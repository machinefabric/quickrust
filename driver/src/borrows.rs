//! What rustc's borrow checker decided about the crate's bodies, as facts (`bodies` of
//! `qrust-facts/1`): every borrow of a user variable (shared or mutable) with where it goes out
//! of scope, and every move out of a user variable.
//!
//! Collected with `QRUST_DRIVER_BORROW_FACTS` set, by overriding the `mir_borrowck` query:
//! rustc's own borrow check runs first and decides; only for a body it accepted does the
//! borrow checker's consumer interface (`rustc_borrowck::consumers`) compute the borrows again,
//! on a copy of the body. Nothing the compiler produces changes.

use rustc_borrowck::consumers::{self, BorrowIndex, ConsumerOptions};
use rustc_data_structures::fx::FxIndexMap;
use rustc_hir::def_id::LocalDefId;
use rustc_middle::mir::visit::Visitor;
use rustc_middle::mir::{self, Body, BorrowKind, Location, MutBorrowKind, Operand, Place, PlaceTy, ProjectionElem, TerminatorKind, VarDebugInfoContents};
use rustc_middle::ty::{self, TyCtxt};
use rustc_span::{ErrorGuaranteed, Span};
use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

pub const ENV: &str = "QRUST_DRIVER_BORROW_FACTS";

/// A position, by the file's name as rustc names it locally (see `facts`).
pub struct At {
    pub file: String,
    pub lo: u32,
    pub hi: u32,
    pub expanded: bool,
}

pub struct Borrow {
    /// `shared` or `mut`.
    pub kind: &'static str,
    pub place: String,
    pub at: At,
    /// Where the borrow is no longer live: it goes out of scope there.
    pub dead_at: Vec<At>,
}

pub struct Move {
    pub place: String,
    pub at: At,
}

/// One body's borrows and moves; `owner` is its definition's `DefPathHash`.
pub struct BodyFacts {
    pub owner: String,
    pub borrows: Vec<Borrow>,
    pub moves: Vec<Move>,
}

/// The bodies the borrow checker accepted in this compilation.
pub static BODIES: Mutex<Vec<BodyFacts>> = Mutex::new(Vec::new());

type Borrowck = for<'tcx> fn(TyCtxt<'tcx>, LocalDefId) -> Result<&'tcx FxIndexMap<LocalDefId, ty::DefinitionSiteHiddenType<'tcx>>, ErrorGuaranteed>;

/// rustc's own `mir_borrowck`.
pub static STOCK_MIR_BORROWCK: OnceLock<Borrowck> = OnceLock::new();

/// The `mir_borrowck` query: rustc's, then the facts of the bodies it accepted.
pub fn mir_borrowck<'tcx>(tcx: TyCtxt<'tcx>, def: LocalDefId) -> Result<&'tcx FxIndexMap<LocalDefId, ty::DefinitionSiteHiddenType<'tcx>>, ErrorGuaranteed> {
    let stock = STOCK_MIR_BORROWCK.get().expect("the stock mir_borrowck is recorded before queries run");
    let result = stock(tcx, def);
    // As rustc skips them: trivial constants and bodies the compiler injected have no borrow check.
    if result.is_ok() && !tcx.is_trivial_const(def) && !tcx.mir_promoted(def).0.borrow().should_skip() {
        let bodies = consumers::get_bodies_with_borrowck_facts(tcx, def, ConsumerOptions::RegionInferenceContext);
        let mut facts: Vec<BodyFacts> = bodies.iter().map(|(owner, b)| body_facts(tcx, *owner, &b.body, &b.borrow_set, &b.region_inference_context)).collect();
        facts.sort_by(|a, b| a.owner.cmp(&b.owner));
        BODIES.lock().unwrap_or_else(|e| e.into_inner()).extend(facts);
    }
    result
}

fn at(tcx: TyCtxt<'_>, span: Span) -> Option<At> {
    let expanded = span.from_expansion();
    let span = if expanded { span.source_callsite() } else { span };
    if span.is_dummy() {
        return None;
    }
    let data = span.data();
    let file = tcx.sess.source_map().lookup_source_file(data.lo);
    Some(At { file: file.name.prefer_local_unconditionally().to_string(), lo: (data.lo - file.start_pos).0, hi: (data.hi - file.start_pos).0, expanded })
}

/// The user variables of a body, by the local holding each.
fn user_variables(body: &Body<'_>) -> HashMap<mir::Local, String> {
    body.var_debug_info
        .iter()
        .filter_map(|v| match &v.value {
            VarDebugInfoContents::Place(p) if p.projection.is_empty() && body.local_decls[p.local].is_user_variable() => Some((p.local, v.name.to_string())),
            _ => None,
        })
        .collect()
}

/// A place in source terms (`x`, `x.field`, `*x`, `x.0`, `x[..]`), when it is in a user variable.
fn place_name<'tcx>(tcx: TyCtxt<'tcx>, body: &Body<'tcx>, names: &HashMap<mir::Local, String>, place: Place<'tcx>) -> Option<String> {
    let mut out = names.get(&place.local)?.clone();
    let mut ty = PlaceTy::from_ty(body.local_decls[place.local].ty);
    for elem in place.projection.iter() {
        match elem {
            ProjectionElem::Deref => out = format!("*{}", out),
            ProjectionElem::Field(f, _) => {
                let name = match ty.ty.kind() {
                    ty::Adt(def, _) => {
                        let variant = match ty.variant_index {
                            Some(v) => def.variant(v),
                            None if !def.is_enum() => def.non_enum_variant(),
                            None => return Some(out),
                        };
                        variant.fields[f].name.to_string()
                    }
                    _ => f.index().to_string(),
                };
                out = format!("{}.{}", out, name);
            }
            ProjectionElem::Index(_) | ProjectionElem::ConstantIndex { .. } | ProjectionElem::Subslice { .. } => out = format!("{}[..]", out),
            _ => {}
        }
        ty = ty.projection_ty(tcx, elem);
    }
    Some(out)
}

/// Moves out of user variables: an operand that moves a place.
struct Moves<'a, 'tcx> {
    tcx: TyCtxt<'tcx>,
    body: &'a Body<'tcx>,
    names: &'a HashMap<mir::Local, String>,
    out: Vec<Move>,
}

impl<'a, 'tcx> Moves<'a, 'tcx> {
    fn record(&mut self, place: Place<'tcx>, span: Span) {
        if let (Some(place), Some(at)) = (place_name(self.tcx, self.body, self.names, place), at(self.tcx, span)) {
            self.out.push(Move { place, at });
        }
    }
}

impl<'a, 'tcx> Visitor<'tcx> for Moves<'a, 'tcx> {
    fn visit_operand(&mut self, operand: &Operand<'tcx>, location: Location) {
        if let Operand::Move(place) = operand {
            self.record(*place, self.body.source_info(location).span);
        }
    }

    fn visit_terminator(&mut self, terminator: &mir::Terminator<'tcx>, location: Location) {
        match &terminator.kind {
            // A call's arguments carry their own positions.
            TerminatorKind::Call { func, args, .. } | TerminatorKind::TailCall { func, args, .. } => {
                self.visit_operand(func, location);
                for arg in args.iter() {
                    if let Operand::Move(place) = &arg.node {
                        self.record(*place, arg.span);
                    }
                }
            }
            _ => self.super_terminator(terminator, location),
        }
    }
}

/// The borrows of a body, in `BorrowIndex` order.
#[cfg(qrust_rustc_ge_99)]
fn borrows_of<'a, 'tcx>(set: &'a consumers::BorrowSet<'tcx>) -> impl Iterator<Item = &'a consumers::BorrowData<'tcx>> {
    set.iter()
}

/// The borrows of a body, in `BorrowIndex` order (the order of the map by location).
#[cfg(not(qrust_rustc_ge_99))]
fn borrows_of<'a, 'tcx>(set: &'a consumers::BorrowSet<'tcx>) -> impl Iterator<Item = &'a consumers::BorrowData<'tcx>> {
    set.location_map().values()
}

fn body_facts<'tcx>(tcx: TyCtxt<'tcx>, owner: LocalDefId, body: &Body<'tcx>, borrow_set: &consumers::BorrowSet<'tcx>, regioncx: &consumers::RegionInferenceContext<'tcx>) -> BodyFacts {
    let names = user_variables(body);
    // Where each borrow goes out of scope.
    let mut dead: HashMap<BorrowIndex, Vec<Location>> = HashMap::new();
    for (location, borrows) in consumers::calculate_borrows_out_of_scope_at_location(body, regioncx, borrow_set) {
        for b in borrows {
            dead.entry(b).or_default().push(location);
        }
    }
    let mut borrows = Vec::new();
    for (index, data) in borrows_of(borrow_set).enumerate() {
        let location = data.reserve_location();
        let kind = match data.kind() {
            BorrowKind::Shared => "shared",
            BorrowKind::Mut { kind: MutBorrowKind::Default | MutBorrowKind::TwoPhaseBorrow } => "mut",
            // Fake borrows (matches, index expressions) and closure captures are the compiler's own.
            BorrowKind::Fake(_) | BorrowKind::Mut { kind: MutBorrowKind::ClosureCapture } => continue,
        };
        let (Some(place), Some(at_)) = (place_name(tcx, body, &names, data.borrowed_place()), at(tcx, body.source_info(location).span)) else { continue };
        let mut dead_at: Vec<At> = dead.get(&BorrowIndex::from_usize(index)).into_iter().flatten().filter_map(|l| at(tcx, body.source_info(*l).span)).collect();
        dead_at.sort_by(|a, b| (&a.file, a.lo, a.hi).cmp(&(&b.file, b.lo, b.hi)));
        dead_at.dedup_by(|a, b| a.file == b.file && a.lo == b.lo && a.hi == b.hi);
        borrows.push(Borrow { kind, place, at: at_, dead_at });
    }
    let mut moves = Moves { tcx, body, names: &names, out: Vec::new() };
    moves.visit_body(body);
    BodyFacts { owner: tcx.def_path_hash(owner.to_def_id()).0.to_hex(), borrows, moves: moves.out }
}
