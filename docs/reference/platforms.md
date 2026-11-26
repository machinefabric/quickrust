# Platforms

Where QuickRust runs and is tested, what differs between platforms, and what cannot be observed on a stable toolchain.

## Support

| Platform | Release archive (`<platform>` in its name) | Tested |
|---|---|---|
| Linux x86_64 | `linux-x86_64` (static, musl) | every test suite, on Ubuntu; by hand on Debian 12, also as root |
| Linux aarch64 | `linux-arm64` (static, musl) | built for every release; the test suites are not run |
| macOS aarch64 | `darwin-arm64` | every test suite |
| macOS x86_64 | `darwin-x86_64` | built for every release; the test suites are not run |
| Windows x86_64 | `windows-x86_64` | a run sets QuickRust up from nothing (stable Rust from the dist server, the compiler driver), then creates, builds, runs and tests a project; the integration suites are Unix-only |
| FreeBSD x86_64 | `freebsd-x86_64` (FreeBSD 14 and later) | a run on FreeBSD 15.1 installs QuickRust from its release archive, sets it up and builds a project, and runs the library's tests |

An archive is `qrust-<version>-<platform>.tar.gz` (`.zip` for Windows), and unpacks to one directory, `quickrust/`, holding `qrust`, `qrust-rustc-wrapper`, the licence and the README. The release manifest (`https://release.machinefabric.com/quickrust/manifest`) lists, for every release, the platforms it was built for: a platform it does not list has no build of that release.

QuickRust builds with Rust 1.80 or newer. The toolchains it installs are for the host it runs on; on Linux, `-gnu` or `-musl` after the system's C library, whatever QuickRust itself was built against; on FreeBSD, `x86_64-unknown-freebsd`.

## Platform differences

| Area | Linux | macOS | Windows | FreeBSD |
|---|---|---|---|---|
| Toolchain programs in `~/.qrust/bin` | symbolic links | symbolic links | hard links | symbolic links |
| `PATH` set up by `qrust setup` | shell startup files | shell startup files | the user's `PATH`, through PowerShell | shell startup files |
| Toolchain libraries for its programs | `LD_LIBRARY_PATH` | `DYLD_FALLBACK_LIBRARY_PATH` | `PATH` | `LD_LIBRARY_PATH` |
| The wild linker | glibc hosts | no | no | no |
| Test runs (`cargo test`, `cargo bench`) | relayed terminal, totals line | relayed terminal, totals line | Cargo's lines | relayed terminal, totals line |
| Cargo's help names the program it runs | `cargo` | `cargo` | `.qrust-engine.exe` | `cargo` |
| Termination signals | forwarded | forwarded | only what the console delivers to the whole process group | forwarded |
| `qrust benchmark --timeout` | yes | yes | no | yes |
| Inventory: hard-link de-duplication, allocated sizes | yes | yes | no | yes |
| Detection of a Cargo build in progress (inventory) | yes | yes | no | yes |
| Driver: linking before rustc saves its incremental state | yes | yes | no | yes |
| Filesystem type, CPU model, memory in reports | yes | yes | `null`, with a reason | yes |

## Limits of observation on a stable toolchain

These are reported as unavailable (`null` with a reason), never as zero:

- which units Cargo considered fresh: known only when the user passes `--message-format=json` and stdout is not a terminal (QuickRust then reads the stream while passing it through) or when QuickRust renders the build;
- the linker's share of a linking unit;
- time spent expanding procedural macros in dependent crates;
- rustdoc invocations: `cargo doc` runs rustdoc outside `RUSTC_WRAPPER`;
- per-unit CPU time and memory when a cache client (sccache) runs rustc on the wrapper's behalf;
- bytes restored by sccache, which it does not report.

Nightly-only data sources (`--unit-graph`, `-Zbuild-analysis`, section timings) are detected and listed in the report's `toolchain.capabilities`; nothing depends on them. Why: [About measuring builds](../explanation/measurement.md).
