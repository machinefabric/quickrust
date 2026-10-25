# Environment variables

The environment variables QuickRust reads, the ones it sets for the Cargo it runs and for the programs Cargo starts, and the ones that are internal to it.

## Read by QuickRust

### QuickRust's own

| Variable | Effect |
|---|---|
| `QRUST_HOME` | QuickRust's home directory; default `~/.qrust` |
| `QRUST_MODE`, `QRUST_OUTPUT`, `QRUST_REPORT`, `QRUST_NESTED`, ... | configuration keys, one variable per key; the full list is in [Configuration](configuration.md#keys) |

Every other variable whose name starts with `QRUST_` is an error, except the [internal variables](#internal-variables).

### Toolchain and build

| Variable | Effect |
|---|---|
| `RUSTUP_TOOLCHAIN` | selects the toolchain, after a `+toolchain` argument and before a toolchain file ([Configuration](configuration.md#toolchain-selection)); a toolchain name or an absolute toolchain directory |
| `CARGO_TARGET_DIR`, `CARGO_BUILD_TARGET_DIR` | a target directory the user chose: used as it is instead of the workspace's directory in QuickRust's cache, unless its value equals `QRUST_MANAGED_TARGET_DIR` (set by an enclosing QuickRust) |
| `RUSTC_WRAPPER`, `CARGO_BUILD_RUSTC_WRAPPER` | a rustc wrapper the user configured (also read from `build.rustc-wrapper` in Cargo's configuration): kept, and run inside `qrust-rustc-wrapper`. With one present, the artifact cache, the parallel frontend, the linker and early cut-off are off, and sccache is not chained |
| `RUSTC_WORKSPACE_WRAPPER` | left to Cargo, which applies it inside the wrapper chain |
| `RUSTC`, `RUSTDOC` | a compiler or rustdoc the user chose: left as it is; otherwise QuickRust sets them to the toolchain's |
| `RUSTC_BOOTSTRAP` | when set by the user, no fast-mode setting that needs `-Z` flags applies (parallel frontend, linker, relinking) |
| `CARGO_INCREMENTAL` | when set, wins over `incremental.local-dev` and `incremental.ci` |
| `CARGO_PROFILE_*` | a profile setting the user made: the fast-mode setting it concerns is left alone ([Fast mode](fast-mode.md)) |
| `CARGO_INSTALL_ROOT` | an error for `qrust install` without `--root` |
| `CARGO_HOME` | read by `qrust setup` to find Cargo's home to import from (default `~/.cargo`); never used for builds, which use `~/.qrust/cargo` (`qrust doctor` notes when it is set) |

### CI detection

A command runs in CI when one of `CI`, `GITHUB_ACTIONS`, `GITLAB_CI`, `BUILDKITE`, `JENKINS_URL`, `TF_BUILD`, `TEAMCITY_VERSION` is set to a value other than empty, `false` or `0`. CI selects `incremental.ci`, is recorded in run reports, and turns off the background toolchain update.

### Terminal

| Variable | Effect |
|---|---|
| `NO_COLOR` (not empty) | no colour in anything QuickRust writes |
| `CARGO_TERM_COLOR` | `never`: no colour; `always`: colour on QuickRust's tagged lines even when they do not go to a terminal |
| `COLUMNS` | the terminal width when it cannot be asked from the terminal (default 100) |

Whether a command's output is rendered depends on stderr being a terminal ([Output](output.md#when-quickrust-presents-the-output)).

## Set for the Cargo QuickRust runs

Every command runs the toolchain's Cargo with these variables. Cargo passes them on to build scripts, external subcommands, `cargo run` programs and tests.

| Variable | Value |
|---|---|
| `CARGO_HOME` | `~/.qrust/cargo` |
| `RUSTUP_TOOLCHAIN` | the selected toolchain's id (`1.99.0-x86_64-unknown-linux-gnu`), so that a nested `cargo` or `rustc` uses the same toolchain |
| `RUSTC`, `RUSTDOC` | the toolchain's `rustc` and `rustdoc`, unless the user set them to something else |
| `PATH` | `~/.qrust/bin` first, then the inherited `PATH`, then the toolchain's `bin` |
| `LD_LIBRARY_PATH` (Linux), `DYLD_FALLBACK_LIBRARY_PATH` (macOS) | the toolchain's `lib` first, then the inherited value |
| `CARGO_CACHE_AUTO_CLEAN_FREQUENCY` | `never`: Cargo does not delete downloaded crates on its own; QuickRust's cache manages that space |
| `CARGO` | `~/.qrust/bin/cargo`, so that a nested build through `$CARGO` runs through QuickRust (except under `qrust upstream`, and with a linked toolchain) |
| `CARGO_TARGET_DIR`, `CARGO_BUILD_BUILD_DIR` | the workspace's directory in QuickRust's cache (`qrust path`), unless the user chose a target directory |
| `CARGO_INCREMENTAL` | `1` or `0` when `incremental.*` is `on` or `off` (and the user did not set it) |

In a recorded build command, also:

| Variable | Value |
|---|---|
| `RUSTC_WRAPPER` | `qrust-rustc-wrapper` |

`+toolchain` is consumed by QuickRust and does not reach Cargo. The fast-mode profile settings and the terminal settings (`term.progress.*`, `term.unicode`, `term.color`) reach Cargo as `--config` arguments, never as environment variables, so they do not reach build scripts, nested Cargo or programs.

## Set for compiler processes

`qrust-rustc-wrapper` runs rustc (or QuickRust's compiler driver) for the units Cargo schedules with:

| Variable | Value |
|---|---|
| `RUSTC_BOOTSTRAP` | `1`, when a fast-mode setting that needs `-Z` flags applies to the unit; on a stable or beta compiler together with `-Zallow-features=`, so that `#![feature]` stays rejected. Never for build scripts or the compiler probes they run, and never when the user set `RUSTC_BOOTSTRAP` |

## Internal variables

QuickRust sets these for its own processes. They are not configuration, and setting them by hand is unsupported.

| Variable | Purpose |
|---|---|
| `QRUST_SESSION_ID` | the session a nested command belongs to |
| `QRUST_EVENT_DIR` | where the rustc wrapper records its events |
| `QRUST_INNER_WRAPPER` | the user's wrapper (or sccache) the rustc wrapper runs inside it |
| `QRUST_WRAPPER_FAIL` | `open` or `closed`: whether a chained cache may be skipped when it cannot start |
| `QRUST_EXEC_GUARD` | recursion detection |
| `QRUST_ENGINE` | the toolchain directory QuickRust set `RUSTC`, `RUSTDOC` and `PATH` for, so that a nested QuickRust can tell its own settings from the user's |
| `QRUST_MANAGED_TARGET_DIR` | marks `CARGO_TARGET_DIR` and `CARGO_BUILD_BUILD_DIR` as set by QuickRust |
| `QRUST_DRIVER_INTERFACE_HASH`, `QRUST_DRIVER_KEEP_OBJECTS`, `QRUST_DRIVER_READY_FD` | between the rustc wrapper and QuickRust's compiler driver |

## Recorded in run reports

With `security.persist-environment = "allowlist"` (the default), a run record keeps the values of these variables when set: `RUSTFLAGS`, `CARGO_ENCODED_RUSTFLAGS`, `RUSTDOCFLAGS`, `CARGO_ENCODED_RUSTDOCFLAGS`, `CARGO_INCREMENTAL`, `CARGO_BUILD_TARGET`, `CARGO_TARGET_DIR`, `CARGO_BUILD_TARGET_DIR`, `CARGO_BUILD_BUILD_DIR`, `CARGO_BUILD_JOBS`, `CARGO_BUILD_RUSTFLAGS`, `RUSTC`, `RUSTC_WRAPPER`, `RUSTC_WORKSPACE_WRAPPER`, `CARGO_BUILD_RUSTC_WRAPPER`, `RUSTUP_TOOLCHAIN`, `RUSTC_BOOTSTRAP`, `CC`, `CXX`, `AR`, `CFLAGS`, `CXXFLAGS`, `LDFLAGS`, `MACOSX_DEPLOYMENT_TARGET`, `SDKROOT`, `PKG_CONFIG_PATH`, `CI`, `CARGO_NET_OFFLINE`, `CARGO_HOME`, `MAKEFLAGS`, and every `CARGO_PROFILE_*`, `CARGO_TARGET_*`, `CARGO_UNSTABLE_*` and `SCCACHE_*` variable. Variables whose names look like credentials are recorded as set, with the value `<redacted>` ([Security](../explanation/security.md#secrets)). `security.persist-environment = "none"` records none.
