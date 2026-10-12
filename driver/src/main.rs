//! Haul's compiler driver: rustc itself — the toolchain's own `librustc_driver`, its LLVM, its
//! release — run through Haul's callbacks. Haul builds it for each toolchain against that
//! toolchain's `rustc-dev` component; with no overrides its outputs are byte for byte rustc's.
//!
//! With `HAUL_DRIVER_INTERFACE_HASH` set, the crate hash (SVH) a library records and its
//! dependents check is computed from what dependents can observe — not from function bodies they
//! cannot see into — so an edit inside such a body leaves it unchanged, and Haul can reuse what
//! the dependents built (early cut-off). The hash covers:
//! - every item's HIR, with bodies left out only for functions whose code stays in this crate:
//!   not generic, not inlinable across crates, not `const`, not `async`, no `impl Trait` in the
//!   return type (their hidden type's layout and auto traits leak); their parameter names stay;
//! - every definition in order, and every macro expansion by number (kind and macro, not call
//!   site): dependents refer to this crate's definitions and expansions by position, so a body
//!   that adds a closure or a macro call shifts them and changes the hash;
//! - positions as file, line and column, wherever dependents can see them: observable bodies,
//!   definitions outside left-out bodies, expansions; byte offsets too for macro definitions,
//!   whose tokens dependents' metadata records by offset;
//! - the symbols dependents may link against instead of instantiating their own;
//! - everything rustc's own crate hash covers besides the HIR (upstream crates, source file
//!   names, options, visibilities, …).
//!
//! Procedural macros, whose bodies run inside their dependents' compilations, keep rustc's hash.
//!
//! With `HAUL_DRIVER_READY_FD` set (Unix), the outputs are linked before rustc saves its
//! incremental state, and the wrapper is told when they are written (see `early_link`).
//! The crate hash goes to `<out-dir>/lib<crate name><extra filename>.haul-svh` for Haul's wrapper.

#![feature(rustc_private)]

extern crate rustc_ast;
extern crate rustc_codegen_ssa;
extern crate rustc_data_structures;
extern crate rustc_driver;
extern crate rustc_hir;
extern crate rustc_interface;
extern crate rustc_metadata;
extern crate rustc_middle;
extern crate rustc_session;
extern crate rustc_span;

use rustc_data_structures::fingerprint::Fingerprint;
use rustc_data_structures::svh::Svh;
use rustc_data_structures::stable_hash::{RawDefId, RawDefPathHash, RawSpan, StableHash, StableHashControls, StableHashCtxt, StableHasher};
use rustc_hir::def::DefKind;
use rustc_hir::def_id::{DefId, LocalDefId, LOCAL_CRATE};
use rustc_span::hygiene::{ExpnId, ExpnKind, LocalExpnId};
use rustc_span::source_map::SourceMap;
use rustc_span::{CachingSourceMapView, Span, SyntaxContext};
use rustc_hir::intravisit::Visitor;
use rustc_hir::{BodyId, ItemLocalId};
use rustc_middle::ty::{TyCtxt, TypeVisitableExt};
use rustc_middle::ich::StableHashState;
use rustc_middle::query::LocalCrate;
use rustc_session::config::{CrateType, OutFileName, OutputType};
use std::collections::{BTreeSet, HashMap, HashSet};
use std::sync::OnceLock;

#[cfg(unix)]
mod early_link;

const INTERFACE_HASH: &str = "HAUL_DRIVER_INTERFACE_HASH";

/// rustc's own `crate_hash` query, for the crates whose bodies their dependents observe.
static STOCK_CRATE_HASH: OnceLock<for<'tcx> fn(TyCtxt<'tcx>, LocalCrate) -> Svh> = OnceLock::new();

struct Callbacks {
    interface_hash: bool,
    /// The pipe on which to say the outputs are written (see `early_link`).
    ready_fd: Option<i32>,
}

impl rustc_driver::Callbacks for Callbacks {
    fn config(&mut self, config: &mut rustc_interface::interface::Config) {
        // As rustc's own driver sets it: it decides how paths are printed into metadata.
        config.opts.trimmed_def_paths = true;
        #[cfg(unix)]
        if let Some(fd) = self.ready_fd {
            let keep = std::env::var_os(early_link::KEEP_OBJECTS).map(std::path::PathBuf::from);
            config.make_codegen_backend = Some(Box::new(move |sess| {
                let early = rustc_session::EarlyDiagCtxt::new(sess.opts.error_format);
                let backend = rustc_interface::util::get_codegen_backend(&early, &sess.opts.sysroot, sess.opts.unstable_opts.codegen_backend.as_deref(), &sess.target);
                Box::new(early_link::EarlyLink::new(backend, fd, keep))
            }));
        }
        if self.interface_hash {
            config.override_queries = Some(|_, providers| {
                let _ = STOCK_CRATE_HASH.set(providers.queries.crate_hash);
                providers.queries.crate_hash = interface_hash;
            });
        }
    }
}

/// What of a function its dependents cannot observe: its bodies (its own and those nested in
/// it: closures, constants) and the definitions inside them.
struct Private {
    bodies: BTreeSet<ItemLocalId>,
    defs: HashSet<LocalDefId>,
    /// Every HIR node inside those bodies: their numbers move with every node added before them.
    ids: HashSet<ItemLocalId>,
}

/// What of `def_id` its dependents cannot observe, or `None` when they observe all of it.
fn private_part(tcx: TyCtxt<'_>, def_id: LocalDefId) -> Option<Private> {
    if !matches!(tcx.def_kind(def_id), DefKind::Fn | DefKind::AssocFn) {
        return None;
    }
    let did = def_id.to_def_id();
    if tcx.generics_of(did).requires_monomorphization(tcx) || tcx.cross_crate_inlinable(def_id) || tcx.is_const_fn(did) || tcx.asyncness(def_id).is_async() || tcx.coroutine_kind(did).is_some() {
        return None;
    }
    if tcx.fn_sig(did).skip_binder().skip_binder().output().has_opaque_types() {
        return None;
    }
    let body = tcx.hir_node_by_def_id(def_id).body_id()?;
    let mut walker = BodyWalker { tcx, private: Private { bodies: BTreeSet::new(), defs: HashSet::new(), ids: HashSet::new() } };
    walker.visit_nested_body(body);
    Some(walker.private)
}

/// Collects a body, every body nested in it and the definitions they hold.
struct BodyWalker<'tcx> {
    tcx: TyCtxt<'tcx>,
    private: Private,
}

impl<'tcx> Visitor<'tcx> for BodyWalker<'tcx> {
    type NestedFilter = rustc_middle::hir::nested_filter::OnlyBodies;
    fn maybe_tcx(&mut self) -> Self::MaybeTyCtxt {
        self.tcx
    }
    fn visit_id(&mut self, id: rustc_hir::HirId) {
        self.private.ids.insert(id.local_id);
    }
    fn visit_nested_body(&mut self, id: BodyId) {
        self.private.bodies.insert(id.hir_id.local_id);
        let body = self.tcx.hir_body(id);
        self.visit_body(body);
    }
    fn visit_expr(&mut self, e: &'tcx rustc_hir::Expr<'tcx>) {
        match e.kind {
            rustc_hir::ExprKind::Closure(c) => {
                self.private.defs.insert(c.def_id);
            }
            rustc_hir::ExprKind::ConstBlock(c) => {
                self.private.defs.insert(c.def_id);
            }
            _ => {}
        }
        rustc_hir::intravisit::walk_expr(self, e);
    }
    fn visit_anon_const(&mut self, c: &'tcx rustc_hir::AnonConst) {
        self.private.defs.insert(c.def_id);
        rustc_hir::intravisit::walk_anon_const(self, c);
    }
}

/// Whether the position of `def_id`'s definition can reach its crate's dependents: not when it
/// is a function whose body they cannot observe (its header moves with every line added above
/// it) or one of its lifetime parameters, nor a definition inside such a body. Decided from the
/// definitions alone: rustc also creates definitions after lowering, which have no HIR.
fn position_observable(tcx: TyCtxt<'_>, def_id: LocalDefId, private: &HashMap<LocalDefId, Option<Private>>) -> bool {
    let mut owner = def_id;
    let part = loop {
        if let Some(part) = private.get(&owner) {
            break part;
        }
        match tcx.opt_local_parent(owner) {
            Some(parent) => owner = parent,
            None => return true,
        }
    };
    let Some(part) = part else { return true };
    !(def_id == owner || part.defs.contains(&def_id) || (tcx.local_parent(def_id) == owner && tcx.def_kind(def_id) == DefKind::LifetimeParam))
}

/// How `Positions` hashes a span.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
enum Spans {
    Off,
    /// File, lines and columns: what diagnostics and debug information show.
    Lines,
    /// Also byte offsets into the file, which metadata records.
    WithOffsets,
}

/// rustc's hashing context, except for spans: hashed by where they are (file, lines and columns)
/// in every build, and so are the expansions they come from. rustc's own hashes a span by its
/// byte offset from its parent definition in an incremental build, which moves with every byte
/// added above that definition, and identifies a local expansion by a hash made that way.
struct Positions<'h, 'a, 's> {
    hcx: &'h mut StableHashState<'a>,
    source_map: CachingSourceMapView<'s>,
    spans: Spans,
    expansions: HashMap<(ExpnId, Spans), Fingerprint>,
}

impl<'h, 'a, 's> Positions<'h, 'a, 's> {
    fn new(hcx: &'h mut StableHashState<'a>, source_map: &'s SourceMap) -> Self {
        Positions { hcx, source_map: CachingSourceMapView::new(source_map), spans: Spans::Lines, expansions: HashMap::new() }
    }

    /// A syntax context, by the expansions it is made of.
    fn hash_ctxt(&mut self, ctxt: SyntaxContext, hasher: &mut StableHasher) {
        let marks = ctxt.marks();
        std::hash::Hash::hash(&marks.len(), hasher);
        for (expn, transparency) in marks {
            self.expansion(expn).stable_hash(self, hasher);
            transparency.stable_hash(self, hasher);
        }
    }

    /// An expansion: a local one by what it is and where (its data, its parent's likewise); one
    /// from another crate by the hash that crate's metadata gives it.
    fn expansion(&mut self, expn: ExpnId) -> Fingerprint {
        if expn == ExpnId::root() {
            return Fingerprint::ZERO;
        }
        if let Some(f) = self.expansions.get(&(expn, self.spans)) {
            return *f;
        }
        let f = match expn.as_local() {
            Some(local) => {
                let mut data = local.expn_data();
                let parent = std::mem::replace(&mut data.parent, ExpnId::root());
                let mut e = StableHasher::new();
                data.stable_hash(self, &mut e);
                self.expansion(parent).stable_hash(self, &mut e);
                e.finish()
            }
            None => {
                let mut e = StableHasher::new();
                expn.expn_hash().stable_hash(self, &mut e);
                e.finish()
            }
        };
        self.expansions.insert((expn, self.spans), f);
        f
    }

    fn spans(&mut self, spans: Spans, f: impl FnOnce(&mut Self)) {
        let before = std::mem::replace(&mut self.spans, spans);
        f(self);
        self.spans = before;
    }
}

impl StableHashCtxt for Positions<'_, '_, '_> {
    fn stable_hash_span(&mut self, raw: RawSpan, hasher: &mut StableHasher) {
        if self.spans == Spans::Off {
            return;
        }
        let span = Span::from_raw_span(raw).data_untracked();
        self.hash_ctxt(span.ctxt, hasher);
        if span.is_dummy() {
            std::hash::Hash::hash(&1u8, hasher);
            return;
        }
        match self.source_map.span_data_to_lines_and_cols(&span) {
            None => std::hash::Hash::hash(&1u8, hasher),
            Some((file, line_lo, col_lo, line_hi, col_hi)) => {
                let start = file.start_pos;
                std::hash::Hash::hash(&(0u8, file.stable_id, line_lo, col_lo.0, line_hi, col_hi.0), hasher);
                if self.spans == Spans::WithOffsets {
                    std::hash::Hash::hash(&((span.lo - start).0, (span.hi - start).0), hasher);
                }
            }
        }
    }

    fn def_path_hash(&self, def_id: RawDefId) -> RawDefPathHash {
        self.hcx.def_path_hash(def_id)
    }

    fn stable_hash_controls(&self) -> StableHashControls {
        StableHashControls { hash_spans: self.spans != Spans::Off }
    }

    fn assert_default_stable_hash_controls(&self, msg: &str) {
        // As rustc's context: what is hashed with spans on (expansions) is never hashed without.
        assert!(self.spans != Spans::Off && self.hcx.stable_hash_controls().hash_spans, "{msg}: hashed with spans off");
    }
}

/// The spans of every token in `tokens`, delimiters included.
fn token_spans(tokens: &rustc_ast::tokenstream::TokenStream, out: &mut Vec<Span>) {
    for tree in tokens.iter() {
        match tree {
            rustc_ast::tokenstream::TokenTree::Token(token, _) => out.push(token.span),
            rustc_ast::tokenstream::TokenTree::Delimited(dspan, _, _, inner) => {
                out.push(dspan.open);
                out.push(dspan.close);
                token_spans(inner, out);
            }
        }
    }
}

fn interface_hash(tcx: TyCtxt<'_>, krate: LocalCrate) -> Svh {
    // rustc's own hash where dependents observe every body, or check no crate hash at all: a
    // procedural macro's bodies run inside its dependents' compilations, and binaries and build
    // scripts have no dependents (rustc hashes their HIR for incremental compilation alone).
    let types = tcx.crate_types();
    if types.contains(&CrateType::ProcMacro) || !types.iter().any(|t| matches!(t, CrateType::Rlib | CrateType::Dylib)) || !tcx.needs_hir_hash() {
        return STOCK_CRATE_HASH.get().expect("the stock crate_hash provider is saved before queries run")(tcx, krate);
    }
    let items = tcx.hir_crate_items(());
    let private: HashMap<LocalDefId, Option<Private>> = items.owners().map(|o| (o.def_id, private_part(tcx, o.def_id))).collect();
    let resolutions = tcx.resolutions(());
    let mut upstream: Vec<_> = tcx.crates(()).iter().map(|&cnum| (tcx.stable_crate_id(cnum), tcx.crate_hash(cnum))).collect();
    upstream.sort_unstable_by_key(|&(id, _)| id);
    let mut source_file_names: Vec<_> = tcx.sess.source_map().files().iter().filter(|f| f.cnum == LOCAL_CRATE).map(|f| f.stable_id).collect();
    source_file_names.sort_unstable();
    let debugger_visualizers: Vec<_> = tcx.debugger_visualizers(LOCAL_CRATE).iter().map(|v| v.path_erased()).collect();
    // Macro expansions by number, where dependents refer to them by number: the tokens of a
    // macro definition carry the expansions they came from (a macro another macro defined), and
    // a dependent that expands it records them as this crate's expansion number so-and-so. What
    // each one is counts, not where it was called (which moves with every line added above it).
    // Every other expansion (a `println!` in a body) reaches dependents only through spans hashed
    // by what they are (see `Positions`), or through MIR inlined across crates, for which Haul's
    // wrapper keys dependents by content instead.
    let mut numbered = BTreeSet::new();
    for owner in items.owners() {
        if !matches!(tcx.def_kind(owner.def_id), DefKind::Macro(_)) {
            continue;
        }
        if let rustc_hir::Node::Item(item) = tcx.hir_node_by_def_id(owner.def_id) {
            if let rustc_hir::ItemKind::Macro(_, def, _) = item.kind {
                let mut spans = vec![def.body.dspan.open, def.body.dspan.close];
                token_spans(&def.body.tokens, &mut spans);
                for span in spans {
                    for (expn, _) in span.ctxt().marks() {
                        let mut at = expn;
                        while let Some(local) = at.as_local() {
                            if !numbered.insert(local.as_u32()) {
                                break;
                            }
                            at = local.expn_data().parent;
                        }
                    }
                }
            }
        }
    }
    let expansions: Vec<(u32, ExpnKind, Option<DefId>, Option<u32>)> = numbered
        .into_iter()
        .map(|i| {
            let data = LocalExpnId::from_u32(i).expn_data();
            (i, data.kind, data.macro_def_id, data.parent.as_local().map(|p| p.as_u32()))
        })
        .collect();

    let hash: Fingerprint = tcx.with_stable_hashing_context(|mut hcx| {
        let mut h = StableHasher::new();
        let mut p = Positions::new(&mut hcx, tcx.sess.source_map());
        // Spans are hashed only where dependents rely on them: a span's hash includes its length,
        // and every item, impl and module enclosing an edited body grows with it.
        for owner in items.owners() {
            let Some(info) = tcx.lower_to_hir(owner.def_id).as_owner() else { continue };
            let left_out = private[&owner.def_id].as_ref();
            let none = HashSet::new();
            let inside = left_out.map(|part| &part.ids).unwrap_or(&none);
            if matches!(tcx.def_kind(owner.def_id), DefKind::Macro(_)) {
                // Expanded in dependents, its tokens carry their positions into them, which their
                // metadata records as offsets into this crate's source file.
                p.spans(Spans::WithOffsets, |p| info.nodes.node().stable_hash(p, &mut h));
            } else {
                p.spans(Spans::Off, |p| match (left_out, info.nodes.node()) {
                    // A function whose bodies are left out: all of it but its body's number, which
                    // rustc gives the body after lowering its contents.
                    (Some(_), rustc_hir::OwnerNode::Item(&rustc_hir::Item { owner_id, kind: rustc_hir::ItemKind::Fn { sig, ident, generics, body: _, has_body }, span, vis_span, eii })) => {
                        owner_id.stable_hash(p, &mut h);
                        sig.stable_hash(p, &mut h);
                        ident.stable_hash(p, &mut h);
                        generics.stable_hash(p, &mut h);
                        has_body.stable_hash(p, &mut h);
                        span.stable_hash(p, &mut h);
                        vis_span.stable_hash(p, &mut h);
                        eii.stable_hash(p, &mut h);
                    }
                    (Some(_), rustc_hir::OwnerNode::ImplItem(&rustc_hir::ImplItem { ident, owner_id, generics, kind: rustc_hir::ImplItemKind::Fn(sig, _), impl_kind, span })) => {
                        ident.stable_hash(p, &mut h);
                        owner_id.stable_hash(p, &mut h);
                        generics.stable_hash(p, &mut h);
                        sig.stable_hash(p, &mut h);
                        impl_kind.stable_hash(p, &mut h);
                        span.stable_hash(p, &mut h);
                    }
                    (_, node) => node.stable_hash(p, &mut h),
                });
            }
            for (id, body) in info.nodes.bodies.iter() {
                if !left_out.is_some_and(|part| part.bodies.contains(id)) {
                    // A body dependents can observe, positions included (panic locations, debug
                    // information of the code they instantiate).
                    p.spans(Spans::Lines, |p| (*id, *body).stable_hash(p, &mut h));
                }
            }
            p.spans(Spans::Off, |p| {
                // The attributes themselves (their precomputed hash includes positions), and of the
                // maps keyed by HIR node, the entries outside left-out bodies.
                let attrs: Vec<_> = info.attrs.map.iter().filter(|(id, _)| !inside.contains(id)).collect();
                attrs.stable_hash(p, &mut h);
                info.attrs.define_opaque.stable_hash(p, &mut h);
                let mut parenting: Vec<_> = info.parenting.items().filter(|(_, parent)| !inside.contains(parent)).map(|(def, parent)| (tcx.def_path_hash(def.to_def_id()), *parent)).into_sorted_stable_ord();
                parenting.sort_unstable();
                parenting.stable_hash(p, &mut h);
                let ids: Vec<ItemLocalId> = info.trait_map.items().map(|(id, _)| *id).filter(|id| !inside.contains(id)).into_sorted_stable_ord();
                for id in ids {
                    id.stable_hash(p, &mut h);
                    info.trait_map[&id].stable_hash(p, &mut h);
                }
                // Owners lowering made on the side are hashed as owners of their own.
                let mut children: Vec<_> = info.children.keys().map(|def| tcx.def_path_hash(def.to_def_id())).into_sorted_stable_ord();
                children.sort_unstable();
                children.stable_hash(p, &mut h);
                if left_out.is_some() {
                    // Parameter names come from the body's patterns; metadata records them.
                    tcx.fn_arg_idents(owner.def_id.to_def_id()).stable_hash(p, &mut h);
                }
            });
        }
        p.spans(Spans::Lines, |p| {
            upstream.stable_hash(p, &mut h);
            source_file_names.stable_hash(p, &mut h);
            debugger_visualizers.stable_hash(p, &mut h);
            // Every definition by its number (dependents refer to them by number), with its
            // header's position where dependents can see it (diagnostics and debug information
            // point there). Except the generic parameters rustc gives an opaque type after
            // lowering, numbered in an order the parallel frontend varies: dependents never
            // name them, so they count as a set.
            let in_hir: HashSet<LocalDefId> = items.definitions().collect();
            let mut unnumbered = Vec::new();
            let definitions = tcx.untracked().definitions.freeze();
            for (index, _, def_path_hash) in definitions.enumerated_keys_and_path_hashes() {
                let def_id = LocalDefId { local_def_index: index };
                if !in_hir.contains(&def_id) && matches!(tcx.def_kind(def_id), DefKind::LifetimeParam | DefKind::TyParam | DefKind::ConstParam) && matches!(tcx.def_kind(tcx.local_parent(def_id)), DefKind::OpaqueTy) {
                    unnumbered.push(def_path_hash);
                    continue;
                }
                (index.as_u32(), def_path_hash).stable_hash(p, &mut h);
                if def_id != rustc_hir::def_id::CRATE_DEF_ID && position_observable(tcx, def_id, &private) {
                    tcx.def_span(def_id).stable_hash(p, &mut h);
                }
            }
            unnumbered.sort_unstable();
            unnumbered.stable_hash(p, &mut h);
            expansions.stable_hash(p, &mut h);
            // The symbols dependents may link against instead of instantiating their own: with
            // shared generics, the instances the bodies left out above instantiate. Hashed as a set.
            if tcx.sess.opts.output_types.should_codegen() {
                let mut symbols: Vec<Fingerprint> = tcx
                    .exported_non_generic_symbols(LOCAL_CRATE)
                    .iter()
                    .chain(tcx.exported_generic_symbols(LOCAL_CRATE))
                    .map(|entry| {
                        let mut e = StableHasher::new();
                        entry.stable_hash(p, &mut e);
                        e.finish()
                    })
                    .collect();
                symbols.sort_unstable();
                symbols.stable_hash(p, &mut h);
            }
            tcx.sess.opts.dep_tracking_hash(true).stable_hash(p, &mut h);
            tcx.stable_crate_id(LOCAL_CRATE).stable_hash(p, &mut h);
            resolutions.visibilities_for_hashing.stable_hash(p, &mut h);
            rustc_span::with_metavar_spans(|m| m.freeze_and_get_read_spans().stable_hash(p, &mut h));
        });
        h.finish()
    });
    let svh = Svh::new(hash);
    record(tcx, svh);
    svh
}

/// Writes the crate hash next to the metadata, `lib<crate name><extra filename>.haul-svh`, for
/// Haul's wrapper. Here rather than after analysis: the query runs (always, being `eval_always`)
/// when rustc needs the hash, which it must not be asked for earlier; forcing it sooner would
/// also force the work it depends on (the exported symbols) out of rustc's own order.
fn record(tcx: TyCtxt<'_>, svh: Svh) {
    if let OutFileName::Real(metadata) = tcx.output_filenames(()).path(OutputType::Metadata) {
        let path = metadata.with_extension("haul-svh");
        if let Err(e) = std::fs::write(&path, format!("{}\n", svh)) {
            tcx.dcx().fatal(format!("haul: cannot write {}: {}", path.display(), e));
        }
    }
}

fn main() -> std::process::ExitCode {
    let early = rustc_session::EarlyDiagCtxt::new(rustc_session::config::ErrorOutputType::default());
    rustc_driver::init_rustc_env_logger(&early);
    rustc_driver::install_ice_hook("https://github.com/jowharshamshiri/haul/issues", |_| ());
    rustc_driver::install_ctrlc_handler();
    #[cfg(unix)]
    let ready_fd = std::env::var(early_link::READY_FD).ok().and_then(|v| v.parse().ok());
    #[cfg(not(unix))]
    let ready_fd = None;
    let mut callbacks = Callbacks { interface_hash: std::env::var_os(INTERFACE_HASH).is_some(), ready_fd };
    rustc_driver::catch_with_exit_code(|| rustc_driver::run_compiler(&rustc_driver::args::raw_args(&early), &mut callbacks))
}
