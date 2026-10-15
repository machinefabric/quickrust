//! rustc's semantic results for a crate, as facts another program can read (`qrust-facts/1`):
//! the crate's definitions, every call in its bodies with the function typeck resolved it to (and,
//! for a trait method, the implementation it selects where the types decide it), and its impls.
//!
//! Written after analysis, with `QRUST_DRIVER_FACTS` set, to `lib<crate name><extra
//! filename>.qrust-facts.json` beside the metadata. Nothing the compiler produces changes.
//!
//! Definitions are identified by their `DefPathHash`, stable across compilations of the same
//! source (never by an index, which the parallel frontend may number differently). Positions are
//! byte offsets into the file as it was read, with the file's path as rustc names it locally.

use rustc_hir::def::{DefKind, Res};
use rustc_hir::def_id::{DefId, LocalDefId, LOCAL_CRATE};
use rustc_hir::intravisit::{self, Visitor};
use rustc_middle::ty::{self, TyCtxt};
use rustc_session::config::{OutFileName, OutputType};
use rustc_span::Span;
use std::collections::BTreeMap;
use std::fmt::Write as _;

pub const ENV: &str = "QRUST_DRIVER_FACTS";
pub const SCHEMA: &str = "qrust-facts/1";

/// A JSON string literal.
fn string(out: &mut String, s: &str) {
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => {
                let _ = write!(out, "\\u{:04x}", c as u32);
            }
            c => out.push(c),
        }
    }
    out.push('"');
}

/// The files spans point into, numbered in the order they are first seen.
#[derive(Default)]
struct Files {
    ids: BTreeMap<String, usize>,
    order: Vec<String>,
}

impl Files {
    fn id(&mut self, name: String) -> usize {
        if let Some(id) = self.ids.get(&name) {
            return *id;
        }
        let id = self.order.len();
        self.ids.insert(name.clone(), id);
        self.order.push(name);
        id
    }
}

/// Where a span is: its file, byte offsets into it, and whether it comes from a macro expansion
/// (then the position is the outermost call site, in source as written).
struct Place {
    file: usize,
    lo: u32,
    hi: u32,
    expanded: bool,
}

fn place(tcx: TyCtxt<'_>, files: &mut Files, span: Span) -> Option<Place> {
    let expanded = span.from_expansion();
    let span = if expanded { span.source_callsite() } else { span };
    if span.is_dummy() {
        return None;
    }
    let data = span.data();
    let file = tcx.sess.source_map().lookup_source_file(data.lo);
    let name = file.name.prefer_local_unconditionally().to_string();
    Some(Place { file: files.id(name), lo: (data.lo - file.start_pos).0, hi: (data.hi - file.start_pos).0, expanded })
}

fn write_place(out: &mut String, p: &Place) {
    let _ = write!(out, "\"file\":{},\"lo\":{},\"hi\":{}", p.file, p.lo, p.hi);
    if p.expanded {
        out.push_str(",\"expanded\":true");
    }
}

/// A definition anywhere (this crate or another): its hash, crate and path.
fn write_def(out: &mut String, tcx: TyCtxt<'_>, def_id: DefId) {
    out.push_str("{\"def\":");
    string(out, &tcx.def_path_hash(def_id).0.to_hex());
    out.push_str(",\"crate\":");
    string(out, tcx.crate_name(def_id.krate).as_str());
    out.push_str(",\"path\":");
    string(out, &tcx.def_path_str(def_id));
    out.push('}');
}

fn kind(k: DefKind) -> Option<&'static str> {
    Some(match k {
        DefKind::Mod => "module",
        DefKind::Struct => "struct",
        DefKind::Union => "union",
        DefKind::Enum => "enum",
        DefKind::Variant => "variant",
        DefKind::Trait => "trait",
        DefKind::TraitAlias => "trait_alias",
        DefKind::TyAlias => "type_alias",
        DefKind::AssocTy => "associated_type",
        DefKind::Fn => "function",
        DefKind::AssocFn => "method",
        DefKind::Const { .. } => "constant",
        DefKind::AssocConst { .. } => "associated_constant",
        DefKind::Static { .. } => "static",
        DefKind::Macro(_) => "macro",
        DefKind::Field => "field",
        DefKind::Impl { .. } => "impl",
        _ => return None,
    })
}

/// How a call reaches its target.
enum Dispatch {
    /// A function, or an inherent method: the callee is the target.
    Static,
    /// A trait method whose implementation the types select: `resolved`.
    Trait(DefId),
    /// A trait method called on a trait object: any implementation.
    Dynamic,
    /// A trait method whose implementation depends on a type parameter of the caller.
    Generic,
    /// A trait method the compiler provides (closures, function pointers, auto traits, …).
    Builtin,
}

struct Call {
    caller: LocalDefId,
    place: Place,
    callee: DefId,
    dispatch: Dispatch,
}

struct Calls<'a, 'tcx> {
    tcx: TyCtxt<'tcx>,
    owner: LocalDefId,
    typeck: &'tcx ty::TypeckResults<'tcx>,
    files: &'a std::cell::RefCell<Files>,
    out: Vec<Call>,
}

impl<'tcx> Calls<'_, 'tcx> {
    fn dispatch(&self, callee: DefId, args: ty::GenericArgsRef<'tcx>) -> Dispatch {
        if self.tcx.trait_of_assoc(callee).is_none() {
            return Dispatch::Static;
        }
        let typing_env = ty::TypingEnv::post_analysis(self.tcx, self.owner.to_def_id());
        let args = self.tcx.erase_and_anonymize_regions(args);
        match ty::Instance::try_resolve(self.tcx, typing_env, callee, args) {
            Ok(Some(instance)) => match instance.def {
                ty::InstanceKind::Item(def) => Dispatch::Trait(def),
                ty::InstanceKind::Virtual(..) => Dispatch::Dynamic,
                _ => Dispatch::Builtin,
            },
            Ok(None) => Dispatch::Generic,
            Err(_) => Dispatch::Generic,
        }
    }

    fn record(&mut self, span: Span, callee: DefId, args: ty::GenericArgsRef<'tcx>) {
        let Some(place) = place(self.tcx, &mut self.files.borrow_mut(), span) else { return };
        let dispatch = self.dispatch(callee, args);
        self.out.push(Call { caller: self.owner, place, callee, dispatch });
    }
}

impl<'tcx> Visitor<'tcx> for Calls<'_, 'tcx> {
    fn visit_expr(&mut self, e: &'tcx rustc_hir::Expr<'tcx>) {
        match e.kind {
            rustc_hir::ExprKind::MethodCall(segment, ..) => {
                if let Some(callee) = self.typeck.type_dependent_def_id(e.hir_id) {
                    self.record(segment.ident.span, callee, self.typeck.node_args(e.hir_id));
                }
            }
            rustc_hir::ExprKind::Call(callee, _) => {
                if let rustc_hir::ExprKind::Path(ref qpath) = callee.kind {
                    if let Res::Def(DefKind::Fn | DefKind::AssocFn, def) = self.typeck.qpath_res(qpath, callee.hir_id) {
                        // The position of the name called: the path's last segment.
                        let span = match qpath {
                            rustc_hir::QPath::Resolved(_, path) => path.segments.last().map_or(callee.span, |s| s.ident.span),
                            rustc_hir::QPath::TypeRelative(_, segment) => segment.ident.span,
                        };
                        self.record(span, def, self.typeck.node_args(callee.hir_id));
                    }
                }
            }
            _ => {}
        }
        intravisit::walk_expr(self, e);
    }
}

/// The facts of the crate `tcx` compiled, as JSON.
pub fn render(tcx: TyCtxt<'_>) -> String {
    let files = std::cell::RefCell::new(Files::default());
    let mut defs = String::new();
    let mut impls = String::new();
    for def_id in tcx.hir_crate_items(()).definitions() {
        let dk = tcx.def_kind(def_id);
        let Some(kind) = kind(dk) else { continue };
        let span = tcx.def_ident_span(def_id).unwrap_or_else(|| tcx.def_span(def_id));
        let Some(name) = place(tcx, &mut files.borrow_mut(), span) else { continue };
        if !defs.is_empty() {
            defs.push(',');
        }
        defs.push_str("{\"def\":");
        string(&mut defs, &tcx.def_path_hash(def_id.to_def_id()).0.to_hex());
        defs.push_str(",\"path\":");
        string(&mut defs, &tcx.def_path_str(def_id));
        defs.push_str(",\"kind\":");
        string(&mut defs, kind);
        let _ = write!(defs, ",\"public\":{},", tcx.visibility(def_id).is_public());
        write_place(&mut defs, &name);
        defs.push('}');
        if let DefKind::Impl { .. } = dk {
            if !impls.is_empty() {
                impls.push(',');
            }
            impls.push_str("{\"impl\":");
            string(&mut impls, &tcx.def_path_hash(def_id.to_def_id()).0.to_hex());
            impls.push_str(",\"trait\":");
            match tcx.impl_opt_trait_ref(def_id) {
                Some(t) => write_def(&mut impls, tcx, t.skip_binder().def_id),
                None => impls.push_str("null"),
            }
            impls.push_str(",\"self_type\":");
            string(&mut impls, &tcx.type_of(def_id).skip_binder().to_string());
            impls.push('}');
        }
    }
    let mut calls = Vec::new();
    for owner in tcx.hir_body_owners() {
        let typeck = tcx.typeck(owner);
        if typeck.tainted_by_errors.is_some() {
            continue;
        }
        let body = tcx.hir_body_owned_by(owner);
        let mut visitor = Calls { tcx, owner, typeck, files: &files, out: Vec::new() };
        visitor.visit_body(body);
        calls.extend(visitor.out);
    }
    let mut out = String::with_capacity(defs.len() + impls.len() + calls.len() * 160 + 256);
    out.push_str("{\"schema\":");
    string(&mut out, SCHEMA);
    out.push_str(",\"crate\":{\"name\":");
    string(&mut out, tcx.crate_name(LOCAL_CRATE).as_str());
    out.push_str(",\"stable_crate_id\":");
    string(&mut out, &format!("{:016x}", tcx.stable_crate_id(LOCAL_CRATE).as_u64()));
    out.push_str("},\"definitions\":[");
    out.push_str(&defs);
    out.push_str("],\"impls\":[");
    out.push_str(&impls);
    out.push_str("],\"calls\":[");
    for (i, c) in calls.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        out.push_str("{\"caller\":");
        string(&mut out, &tcx.def_path_hash(c.caller.to_def_id()).0.to_hex());
        out.push(',');
        write_place(&mut out, &c.place);
        out.push_str(",\"callee\":");
        write_def(&mut out, tcx, c.callee);
        out.push_str(",\"dispatch\":");
        match c.dispatch {
            Dispatch::Static => out.push_str("\"static\""),
            Dispatch::Trait(target) => {
                out.push_str("\"trait\",\"target\":");
                write_def(&mut out, tcx, target);
            }
            Dispatch::Dynamic => out.push_str("\"dynamic\""),
            Dispatch::Generic => out.push_str("\"generic\""),
            Dispatch::Builtin => out.push_str("\"builtin\""),
        }
        out.push('}');
    }
    out.push(']');
    if std::env::var_os(crate::borrows::ENV).is_some() {
        let bodies = std::mem::take(&mut *crate::borrows::BODIES.lock().unwrap_or_else(|e| e.into_inner()));
        let mut files = files.borrow_mut();
        let mut place = |out: &mut String, a: &crate::borrows::At| {
            let p = Place { file: files.id(a.file.clone()), lo: a.lo, hi: a.hi, expanded: a.expanded };
            write_place(out, &p);
        };
        out.push_str(",\"bodies\":[");
        for (i, b) in bodies.iter().enumerate() {
            if i > 0 {
                out.push(',');
            }
            out.push_str("{\"owner\":");
            string(&mut out, &b.owner);
            out.push_str(",\"borrows\":[");
            for (j, r) in b.borrows.iter().enumerate() {
                if j > 0 {
                    out.push(',');
                }
                out.push_str("{\"kind\":");
                string(&mut out, r.kind);
                out.push_str(",\"place\":");
                string(&mut out, &r.place);
                out.push(',');
                place(&mut out, &r.at);
                out.push_str(",\"dead_at\":[");
                for (k, d) in r.dead_at.iter().enumerate() {
                    if k > 0 {
                        out.push(',');
                    }
                    out.push('{');
                    place(&mut out, d);
                    out.push('}');
                }
                out.push_str("]}");
            }
            out.push_str("],\"moves\":[");
            for (j, m) in b.moves.iter().enumerate() {
                if j > 0 {
                    out.push(',');
                }
                out.push_str("{\"place\":");
                string(&mut out, &m.place);
                out.push(',');
                place(&mut out, &m.at);
                out.push('}');
            }
            out.push_str("]}");
        }
        out.push(']');
    }
    out.push_str(",\"files\":[");
    for (i, name) in files.borrow().order.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        string(&mut out, name);
    }
    out.push_str("]}\n");
    out
}

/// Writes the facts beside the metadata (`lib<crate><extra>.qrust-facts.json`).
pub fn write(tcx: TyCtxt<'_>) {
    if let OutFileName::Real(metadata) = tcx.output_filenames(()).path(OutputType::Metadata) {
        let path = metadata.with_extension("qrust-facts.json");
        // Paths in full: trimmed ones are for diagnostics, and may only be printed for them.
        let facts = rustc_middle::ty::print::with_no_trimmed_paths!(render(tcx));
        if let Err(e) = std::fs::write(&path, facts) {
            tcx.dcx().fatal(format!("qrust: cannot write {}: {}", path.display(), e));
        }
    }
}
