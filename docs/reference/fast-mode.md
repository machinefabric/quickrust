# Fast mode

The build settings QuickRust applies in its default `mode = "fast"`, when each one applies, and the key that turns it off.

None of them changes what the built program does. `mode = "compat"` (or `QRUST_MODE=compat`) applies none of them: the build is exactly the toolchain's Cargo's, and only observed. Why these settings and not others: [Why QuickRust is fast by default](../explanation/fast-by-default.md).

## Settings

| Setting | What changes | Applies to | Off |
|---|---|---|---|
| `profile.dev.debug = "line-tables-only"` | dev builds keep file and line information (backtraces, stepping, breakpoints) but not local-variable and type information | built-in build commands | set `profile.dev.debug` yourself |
| `profile.dev.package."*".debug = false` | no debug information for dependencies in dev builds | built-in build commands | set `profile.dev.debug` or `profile.dev.package."*".debug` yourself |
| rustc's parallel frontend (`-Zthreads=N`) | compilation from scratch uses several threads | units compiled from scratch | `fast.frontend-threads = "off"` |
| the wild linker | host builds of linked outputs link with `wild` instead of rustc's bundled LLD | Linux glibc hosts with `wild` installed | `fast.linker = "off"` |
| the artifact cache | a unit compiled before is restored instead of compiled | units Cargo schedules | `cache.artifacts = "off"` |
| early cut-off | after an edit its dependents cannot observe, dependents are restored instead of compiled, and binaries only relinked | units Cargo schedules, with the artifact cache on | `fast.early-cutoff = "off"` |

QuickRust's terminal output is separate from fast mode: in fast mode `output` defaults to `qrust`, in compat mode to `cargo` ([Output](output.md)).

## Profile settings

The two profile settings reach Cargo as `--config` arguments placed with the command's own `--config` arguments. They are given to the built-in build commands (`build`, `check`, `test`, `bench`, `run`, `doc`, `rustdoc`, `rustc`, `clippy`, `fix`) and never to other commands or external subcommands (`cargo nextest`, ...), to which Cargo does not pass `--config`.

A setting the user made wins, and QuickRust then leaves that setting, and the settings that depend on it, alone:

| QuickRust's setting | Left alone when the user set |
|---|---|
| `profile.dev.debug` | `profile.dev.debug` |
| `profile.dev.package."*".debug` | `profile.dev.debug` or `profile.dev.package."*".debug` |

A setting counts as the user's when it is made in the workspace's root `Cargo.toml`, in a Cargo configuration file that applies to the workspace, in a `--config` argument, or in a `CARGO_PROFILE_*` environment variable.

## Settings applied by the rustc wrapper

The parallel frontend, the linker, the artifact cache and early cut-off are applied by `qrust-rustc-wrapper` to the compilations of the units Cargo schedules. None of them applies:

- when the user configured a rustc wrapper (`RUSTC_WRAPPER`, `build.rustc-wrapper`, or sccache chained by `cache.provider`);
- with `measurement.enabled = false`;
- to build scripts and the compiler probes they run, which see the compiler exactly as it is.

The parallel frontend, the linker and relinking need `-Z` flags. On a stable or beta compiler the wrapper sets `RUSTC_BOOTSTRAP=1` for the compiler process together with `-Zallow-features=`, so a crate that enables `#![feature]` is rejected as on stable (with rustc's "not in the list of allowed features" error). When the user sets `RUSTC_BOOTSTRAP`, none of these three applies.

### Parallel frontend

| | |
|---|---|
| Key | `fast.frontend-threads`: `auto` (the number of cores, at most 8, on machines with at least 4 cores; off below 4), `off`, or a number of at least 2 |
| Applies to | units compiled from scratch: dependencies, release and other non-incremental builds, and a workspace unit's first compilation in a target directory |
| Does not apply to | incremental rebuilds; units the artifact cache saw compile quickly; units whose arguments already set `-Zthreads` |

### Linker

| | |
|---|---|
| Key | `fast.linker`: `auto`, `off` |
| Applies when | the host is Linux with glibc and `wild` is in `~/.qrust/bin` or on `PATH` |
| Applies to | host builds (no `--target`) of units that link: binaries, tests, proc-macros, dynamic libraries |
| Does not apply to | units where the user chose a linker: `-C linker`, link arguments selecting a linker, or linker features, from Cargo's configuration or `RUSTFLAGS` |
| Link | `~/.qrust/linker/ld` points to the `wild` in use |

### Artifact cache

| | |
|---|---|
| Key | `cache.artifacts`: `on`, `off` |
| Location | `~/.qrust/cache/artifacts/`, managed with the rest of QuickRust's cache ([Files](files.md#quickrusts-cache)) |
| Shared | dependencies across every checkout on the machine; workspace units within the checkout that built them |
| Not stored | linked workspace outputs that compiled faster than `cache.artifacts-min-link-ms` (default 300); a compilation whose inputs changed while it ran |
| Visible difference | a dependency that includes code its build script generated, first built in another target directory, names that directory in panic messages and debug information for the generated code; QuickRust says so once (a `NOTE`) and counts such units in the report (`cache.artifacts.restored_with_foreign_out_dir`) |

How it works: [About the artifact cache](../explanation/artifact-cache.md).

### Early cut-off

| | |
|---|---|
| Key | `fast.early-cutoff`: `auto`, `off` (`QRUST_EARLY_CUTOFF`) |
| Needs | the artifact cache, and QuickRust's compiler driver for the toolchain |
| Driver | the toolchain's own rustc built against its `rustc-dev` component, once per toolchain: by `qrust setup` and when a channel moves to a new release; otherwise in the background after the first build that finds it missing (QuickRust says so in a `NOTE`). `qrust driver status` tells whether it is built, `qrust driver build` builds it |
| Effect | a library's crate hash covers what its dependents can observe, so an edit its dependents cannot observe leaves their cache keys unchanged and they are restored; a binary or test whose own code is unchanged is linked again (`-Zlink-only`) instead of compiled |
| Not applied | to a unit the driver failed on before (rustc compiles it from then on; the report is kept in `~/.qrust/cache/drivers/<key>/failed/`); when Cargo runs another program in rustc's place (`cargo clippy`'s clippy-driver) |

How it works: [About early cut-off](../explanation/early-cutoff.md).

## In the run report

Every run records what fast mode did in `policy.fast` ([Run report schema](report-schema.md#policy)):

| Field | Content |
|---|---|
| `mode` | `fast` or `compat` |
| `applied[]` | each setting applied: `key`, `value`, `why` (the profile settings, `rustc -Zthreads`, `linker`) |
| `yielded[]` | each setting left alone: `key`, and `because` naming the user's setting and where it was made |
| `note` | why nothing applied (compat mode, not a build command) |

`policy.cache.artifacts` says whether the artifact cache was on, and why not.
