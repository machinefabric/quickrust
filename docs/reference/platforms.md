# Platforms

Where QuickRust runs and is tested, what differs between platforms, and what cannot be observed on a stable toolchain.

## Support

| Platform | Release archive | Tested |
|---|---|---|
| Linux x86_64 | `qrust-x86_64-unknown-linux-musl.tar.gz` (static) | every test suite in CI (`ubuntu-latest`), and with QuickRust built by Rust 1.80, its minimum; by hand on Debian 12, also as root |
| Linux aarch64 | `qrust-aarch64-unknown-linux-musl.tar.gz` (static) | built by the release workflow; the test suites are not run |
| macOS aarch64 | `qrust-aarch64-apple-darwin.tar.gz` | every test suite in CI (`macos-latest`) |
| macOS x86_64 | `qrust-x86_64-apple-darwin.tar.gz` | built by the release workflow; the test suites are not run |
| Windows x86_64 | `qrust-x86_64-pc-windows-msvc.tar.gz` | a CI run sets QuickRust up from nothing (stable Rust from the dist server, the compiler driver), then creates, builds, runs and tests a project; the integration suites are Unix-only |

QuickRust builds with Rust 1.80 or newer. The toolchains it installs are for the host it runs on; on Linux, `-gnu` or `-musl` after the system's C library, whatever QuickRust itself was built against.

## Platform differences

| Area | Linux | macOS | Windows |
|---|---|---|---|
| Toolchain programs in `~/.qrust/bin` | symbolic links | symbolic links | hard links |
| `PATH` set up by `qrust setup` | shell startup files | shell startup files | the user's `PATH`, through PowerShell |
| Toolchain libraries for its programs | `LD_LIBRARY_PATH` | `DYLD_FALLBACK_LIBRARY_PATH` | `PATH` |
| The wild linker | glibc hosts | no | no |
| Test runs (`cargo test`, `cargo bench`) | relayed terminal, totals line | relayed terminal, totals line | Cargo's lines |
| Cargo's help names the program it runs | `cargo` | `cargo` | `.qrust-engine.exe` |
| Termination signals | forwarded | forwarded | only what the console delivers to the whole process group |
| `qrust benchmark --timeout` | yes | yes | no |
| Inventory: hard-link de-duplication, allocated sizes | yes | yes | no |
| Detection of a Cargo build in progress (inventory) | yes | yes | no |
| Driver: linking before rustc saves its incremental state | yes | yes | no |
| Filesystem type, CPU model, memory in reports | yes | yes | `null`, with a reason |

## Limits of observation on a stable toolchain

These are reported as unavailable (`null` with a reason), never as zero:

- which units Cargo considered fresh: known only when the user passes `--message-format=json` and stdout is not a terminal (QuickRust then reads the stream while passing it through) or when QuickRust renders the build;
- the linker's share of a linking unit;
- time spent expanding procedural macros in dependent crates;
- rustdoc invocations: `cargo doc` runs rustdoc outside `RUSTC_WRAPPER`;
- per-unit CPU time and memory when a cache client (sccache) runs rustc on the wrapper's behalf;
- bytes restored by sccache, which it does not report.

Nightly-only data sources (`--unit-graph`, `-Zbuild-analysis`, section timings) are detected and listed in the report's `toolchain.capabilities`; nothing depends on them. Why: [About measuring builds](../explanation/measurement.md).
