//! Haul's compiler driver: rustc itself — the toolchain's own `librustc_driver`, its LLVM, its
//! release — run through Haul's callbacks. Haul builds it for each toolchain against that
//! toolchain's `rustc-dev` component; with no overrides its outputs are byte for byte rustc's.

#![feature(rustc_private)]

extern crate rustc_driver;
extern crate rustc_interface;
extern crate rustc_session;

struct Callbacks;

impl rustc_driver::Callbacks for Callbacks {
    fn config(&mut self, config: &mut rustc_interface::interface::Config) {
        // As rustc's own driver sets it: it decides how paths are printed into metadata.
        config.opts.trimmed_def_paths = true;
    }
}

fn main() -> std::process::ExitCode {
    let early = rustc_session::EarlyDiagCtxt::new(rustc_session::config::ErrorOutputType::default());
    rustc_driver::init_rustc_env_logger(&early);
    rustc_driver::install_ice_hook("https://github.com/jowharshamshiri/haul/issues", |_| ());
    rustc_driver::install_ctrlc_handler();
    rustc_driver::catch_with_exit_code(|| rustc_driver::run_compiler(&rustc_driver::args::raw_args(&early), &mut Callbacks))
}
