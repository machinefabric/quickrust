//! Speed index: how long the same developer loop takes through QuickRust compared with upstream Cargo.
//!
//!     cargo build --release --workspace
//!     target/release/qrust-bench fetch               # once; the only step that uses the network
//!     target/release/qrust-bench run --quick         # ~minutes, while iterating
//!     target/release/qrust-bench run --full --record # the recorded measurement
//!
//! See benchmarks/README.md for the method. It measures on Linux and macOS, where it reads the CPU
//! time and peak memory of a command's whole process tree as the system accounts them; elsewhere it
//! builds, and refuses to run.

#[cfg(any(target_os = "linux", target_os = "macos"))]
mod cutoff;
#[cfg(any(target_os = "linux", target_os = "macos"))]
mod index;
#[cfg(any(target_os = "linux", target_os = "macos"))]
mod measure;
#[cfg(any(target_os = "linux", target_os = "macos"))]
mod sealed;
#[cfg(any(target_os = "linux", target_os = "macos"))]
mod verify;
#[cfg(any(target_os = "linux", target_os = "macos"))]
mod workload;

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn main() {
    index::main()
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn main() {
    eprintln!("FAIL qrust-bench measures on Linux and macOS only: it cannot measure on {}", std::env::consts::OS);
    std::process::exit(1)
}
