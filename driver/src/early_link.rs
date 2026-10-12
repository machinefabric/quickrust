//! Outputs before bookkeeping: rustc's own code generation backend, with the link moved ahead of
//! the incremental save.
//!
//! rustc saves its incremental state (the dependency graph and query results, ~100 ms for a
//! large crate) when its type context ends, and only then links the outputs (`Linker::link`):
//! Cargo, and every unit waiting on this one, wait for the save. Wrapped here, the backend joins
//! code generation and links inside `codegen_crate`, which runs while the type context lives,
//! then tells Haul's wrapper the outputs are written (one byte on the pipe it passed in
//! `HAUL_DRIVER_READY_FD`) and stops writing to stdout and stderr. rustc then saves its
//! incremental state, its work products (handed back by `join_codegen`) and finalizes the
//! session exactly as it would have, while the build goes on.
//!
//! Only when nothing can follow the outputs that a build would need to see: no `#![feature]`
//! (whose unused-feature warnings rustc emits after code generation), and the crate's metadata
//! emitted as a file (Cargo emits it for every library) or not needed at all (binaries).
//!
//! For a unit Haul relinks, `HAUL_DRIVER_KEEP_OBJECTS` names a directory: before linking (which
//! deletes the object files), the `.rlink` naming them is written and they are hard-linked there.

use rustc_codegen_ssa::traits::CodegenBackend;
use rustc_codegen_ssa::{CompiledModules, CrateInfo, TargetConfig};
use rustc_metadata::creader::MetadataLoaderDyn;
use rustc_metadata::EncodedMetadata;
use rustc_middle::dep_graph::WorkProductMap;
use rustc_middle::ty::TyCtxt;
use rustc_middle::util::Providers;
use rustc_session::config::{self, CrateType, OutFileName, OutputFilenames, OutputType, PrintRequest};
use rustc_session::Session;
use rustc_span::Symbol;
use std::any::Any;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

pub const READY_FD: &str = "HAUL_DRIVER_READY_FD";
pub const KEEP_OBJECTS: &str = "HAUL_DRIVER_KEEP_OBJECTS";

pub struct EarlyLink {
    inner: Box<dyn CodegenBackend>,
    ready_fd: i32,
    keep_objects: Option<PathBuf>,
    /// The work products of the code generation joined early, for rustc's `join_codegen`.
    linked: Mutex<Option<WorkProductMap>>,
    /// The outputs were linked in `codegen_crate`.
    linked_early: AtomicBool,
}

/// What `codegen_crate` hands rustc when it linked already.
struct Linked;

impl EarlyLink {
    pub fn new(inner: Box<dyn CodegenBackend>, ready_fd: i32, keep_objects: Option<PathBuf>) -> EarlyLink {
        EarlyLink { inner, ready_fd, keep_objects, linked: Mutex::new(None), linked_early: AtomicBool::new(false) }
    }

    /// The metadata to link with, as rustc's own code generation would pass it: read back from
    /// the file it emitted, or none for a crate that needs none. `None` when it cannot be had
    /// that way (metadata not emitted, or kept apart from the rlib).
    fn metadata(tcx: TyCtxt<'_>, outputs: &OutputFilenames) -> Option<EncodedMetadata> {
        let sess = tcx.sess;
        if !tcx.needs_metadata() {
            // An empty file reads back as no metadata.
            let empty = outputs.with_extension(&format!("haul-no-metadata-{}", std::process::id()));
            std::fs::File::create(&empty).ok()?;
            let metadata = EncodedMetadata::from_path(empty.clone(), None, None).ok();
            let _ = std::fs::remove_file(&empty);
            return metadata;
        }
        if !sess.opts.output_types.contains_key(&OutputType::Metadata) || !sess.opts.unstable_opts.embed_metadata || tcx.crate_types().contains(&CrateType::ProcMacro) {
            return None;
        }
        match outputs.path(OutputType::Metadata) {
            OutFileName::Real(path) => EncodedMetadata::from_path(path, None, None).ok(),
            OutFileName::Stdout => None,
        }
    }

    /// Writes the `.rlink` naming this compilation's objects and hard-links them into `dir`;
    /// nothing is kept (and the directory is gone) when that cannot be done completely.
    fn keep(sess: &Session, dir: &Path, compiled: &CompiledModules, crate_info: &CrateInfo, metadata: &EncodedMetadata, outputs: &OutputFilenames) {
        let _ = std::fs::remove_dir_all(dir);
        let kept = (|| -> std::io::Result<()> {
            std::fs::create_dir_all(dir)?;
            CompiledModules::serialize_rlink(sess, &outputs.with_extension(config::RLINK_EXT), compiled, crate_info, metadata, outputs)?;
            for m in compiled.modules.iter().chain(compiled.allocator_module.iter()) {
                for object in [&m.object, &m.global_asm_object, &m.dwarf_object].into_iter().flatten() {
                    let name = object.file_name().ok_or_else(|| std::io::Error::other("an object file without a name"))?;
                    std::fs::hard_link(object, dir.join(name))?;
                }
            }
            Ok(())
        })();
        if kept.is_err() {
            let _ = std::fs::remove_dir_all(dir);
        }
    }

    /// What rustc's own `join_codegen` gets for a compilation linked early: no modules (linked
    /// already), and the work products of the code generation joined in `codegen_crate`.
    fn handed_back(&self) -> (CompiledModules, WorkProductMap) {
        let work_products = self.linked.lock().unwrap_or_else(|e| e.into_inner()).take().expect("the work products of the code generation joined early");
        (CompiledModules { modules: vec![], allocator_module: None }, work_products)
    }

    /// Tells the wrapper the outputs are written; from here on the build sees nothing of this
    /// process (rustc saving its incremental state).
    fn ready(&self) {
        use std::io::Write;
        use std::os::fd::{AsRawFd, FromRawFd};
        extern "C" {
            fn dup2(old: i32, new: i32) -> i32;
        }
        if let Ok(null) = std::fs::OpenOptions::new().write(true).open("/dev/null") {
            unsafe {
                dup2(null.as_raw_fd(), 1);
                dup2(null.as_raw_fd(), 2);
            }
        }
        // The descriptor is this process's, given by the wrapper for this one write.
        let mut ready = unsafe { std::fs::File::from_raw_fd(self.ready_fd) };
        let _ = ready.write_all(b"1");
    }
}

impl CodegenBackend for EarlyLink {
    fn name(&self) -> &'static str {
        self.inner.name()
    }
    fn init(&self, sess: &Session) {
        self.inner.init(sess)
    }
    fn print(&self, req: &PrintRequest, out: &mut String, sess: &Session) {
        self.inner.print(req, out, sess)
    }
    fn target_config(&self, sess: &Session) -> TargetConfig {
        self.inner.target_config(sess)
    }
    fn supported_crate_types(&self, sess: &Session) -> Vec<CrateType> {
        self.inner.supported_crate_types(sess)
    }
    fn print_passes(&self) {
        self.inner.print_passes()
    }
    fn print_version(&self) {
        self.inner.print_version()
    }
    fn replaced_intrinsics(&self) -> Vec<Symbol> {
        self.inner.replaced_intrinsics()
    }
    fn fallback_intrinsics(&self) -> Vec<Symbol> {
        self.inner.fallback_intrinsics()
    }
    fn thin_lto_supported(&self) -> bool {
        self.inner.thin_lto_supported()
    }
    fn has_zstd(&self) -> bool {
        self.inner.has_zstd()
    }
    fn has_mnemonic(&self, sess: &Session, mnemonic: &str) -> bool {
        self.inner.has_mnemonic(sess, mnemonic)
    }
    fn metadata_loader(&self) -> Box<MetadataLoaderDyn> {
        self.inner.metadata_loader()
    }
    fn provide(&self, providers: &mut Providers) {
        self.inner.provide(providers)
    }
    fn target_cpu(&self, sess: &Session) -> String {
        self.inner.target_cpu(sess)
    }
    fn print_pass_timings(&self) {
        self.inner.print_pass_timings()
    }
    fn print_statistics(&self) {
        self.inner.print_statistics()
    }
    fn print_statistics_json(&self) -> String {
        self.inner.print_statistics_json()
    }

    fn codegen_crate<'tcx>(&self, tcx: TyCtxt<'tcx>) -> Box<dyn Any> {
        let ongoing = self.inner.codegen_crate(tcx);
        let sess = tcx.sess;
        let links = sess.opts.output_types.contains_key(&OutputType::Exe) && !sess.opts.unstable_opts.no_link && !sess.opts.unstable_opts.link_only;
        if !links || !tcx.features().enabled_features().is_empty() {
            return ongoing;
        }
        let outputs = tcx.output_filenames(());
        let Some(metadata) = Self::metadata(tcx, outputs) else { return ongoing };
        let crate_info = CrateInfo::new(tcx, self.inner.target_cpu(sess));
        #[cfg(haul_rustc_ge_99)]
        let (compiled, work_products) = self.inner.join_codegen(ongoing, sess, tcx.incr_comp_session, outputs, &crate_info);
        #[cfg(not(haul_rustc_ge_99))]
        let (compiled, work_products) = self.inner.join_codegen(ongoing, sess, outputs, &crate_info);
        sess.dcx().abort_if_errors();
        if let Some(dir) = &self.keep_objects {
            Self::keep(sess, dir, &compiled, &crate_info, &metadata, outputs);
        }
        self.inner.link(sess, compiled, crate_info, metadata, outputs);
        sess.dcx().abort_if_errors();
        *self.linked.lock().unwrap_or_else(|e| e.into_inner()) = Some(work_products);
        self.linked_early.store(true, Ordering::SeqCst);
        self.ready();
        Box::new(Linked)
    }

    #[cfg(haul_rustc_ge_99)]
    fn join_codegen(&self, ongoing: Box<dyn Any>, sess: &Session, incremental: Option<&rustc_session::IncrCompSession>, outputs: &OutputFilenames, crate_info: &CrateInfo) -> (CompiledModules, WorkProductMap) {
        match ongoing.downcast::<Linked>() {
            Ok(_) => self.handed_back(),
            Err(ongoing) => self.inner.join_codegen(ongoing, sess, incremental, outputs, crate_info),
        }
    }

    #[cfg(not(haul_rustc_ge_99))]
    fn join_codegen(&self, ongoing: Box<dyn Any>, sess: &Session, outputs: &OutputFilenames, crate_info: &CrateInfo) -> (CompiledModules, WorkProductMap) {
        match ongoing.downcast::<Linked>() {
            Ok(_) => self.handed_back(),
            Err(ongoing) => self.inner.join_codegen(ongoing, sess, outputs, crate_info),
        }
    }

    fn link(&self, sess: &Session, compiled_modules: CompiledModules, crate_info: CrateInfo, metadata: EncodedMetadata, outputs: &OutputFilenames) {
        // Linked already, before the incremental save.
        if self.linked_early.load(Ordering::SeqCst) {
            return;
        }
        self.inner.link(sess, compiled_modules, crate_info, metadata, outputs)
    }
}
