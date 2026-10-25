# Compatibility with Cargo

What QuickRust passes to and from Cargo unchanged, what it adds or changes, and how nested invocations, target directories and Cargo's configuration are handled.

The test suite checks these against the toolchain's own Cargo as the oracle (`tests/compat.rs`, the QA corpus of the [specification](specification.md)). Requirement identifiers (`COMP-...`) refer to the specification; where this implementation departs from it, [Conformance](conformance.md) says how.

## Unchanged

| What | Behaviour |
|---|---|
| Arguments | passed to Cargo as given, except a leading `+toolchain`, which QuickRust consumes (COMP-001, COMP-051) |
| Unknown flags and commands | forwarded; Cargo decides whether they are valid and resolves external subcommands (COMP-002, COMP-003) |
| Aliases, package selection, `--manifest-path`, profiles, targets, features, lockfile behaviour | Cargo's (COMP-008) |
| Environment | every variable passes through to Cargo; QuickRust adds the variables listed in [Environment](environment.md#set-for-the-cargo-quickrust-runs) and never rewrites the user's `RUSTFLAGS`, `CARGO_INCREMENTAL` (unless an `incremental.*` policy is set and the user did not set it), proxy or registry settings (COMP-040, COMP-041) |
| Jobserver | `CARGO_MAKEFLAGS` and the jobserver descriptors are inherited (COMP-042) |
| Exit status | Cargo's, whenever Cargo ran; a terminating signal is re-raised (COMP-006) |
| `--message-format=json` | the output on stdout is what the delegated Cargo prints; QuickRust reads it, when stdout is not a terminal, while passing it on unchanged (COMP-005) |
| Cargo's configuration | never interpreted by QuickRust for execution; it reads a subset (profile keys, `build.rustc-wrapper`, aliases, target directory) only to decide its own settings and for analysis (COMP-050, COMP-052) |
| `cargo clean` | Cargo's `clean`, on the workspace's target directory (COMP-009) |
| `qrust upstream <args>` | the toolchain's Cargo with no fast-mode settings, no wrapper, no rendering and no record (COMP-013) |

## Added or changed

| What | Behaviour |
|---|---|
| Build settings | in fast mode, the settings of [Fast mode](fast-mode.md); in compat mode, none |
| Terminal output | in a terminal, QuickRust presents the output ([Output](output.md)); pipes, `-v`, `-q`, `--message-format`, `--color`, compat mode and `output = "cargo"` leave Cargo's output untouched (COMP-007) |
| Toolchains | QuickRust installs and selects toolchains itself, in rustup's order (`+toolchain`, `RUSTUP_TOOLCHAIN`, toolchain file, default); rustup is never called and its per-directory overrides have no equivalent (COMP-012, COMP-020) |
| Cargo home | `~/.qrust/cargo`, not `~/.cargo`; `~/.cargo/config.toml` is still read by Cargo for projects below the home directory, as any configuration file in a parent directory |
| Target directory | each workspace builds into `~/.qrust/cache/builds/<name>-<id>/`, never `target/` in the project (below) |
| `cargo install`, `cargo uninstall` | QuickRust's own: Cargo's resolver picks the release, QuickRust builds it like any build and puts its binaries into `~/.qrust/bin` (or `--root`), never over QuickRust's own programs or another package's without `--force` ([Commands](commands.md#programs)) |
| `RUSTC_WRAPPER` | set to `qrust-rustc-wrapper` for recorded build commands; a wrapper the user configured runs inside it |
| `CARGO` | set to QuickRust's `cargo` (below) |

## Wrapper chain

For a recorded build command, a compilation runs through:

```text
qrust-rustc-wrapper -> the user's RUSTC_WRAPPER (or sccache chained by cache.provider) -> RUSTC_WORKSPACE_WRAPPER (applied by Cargo) -> rustc
```

A wrapper the user configured (`RUSTC_WRAPPER`, `CARGO_BUILD_RUSTC_WRAPPER`, `build.rustc-wrapper`) is never removed or skipped: if it cannot start, the build fails as under the toolchain's Cargo. Only sccache chained by `cache.provider = "sccache"` may be skipped, under `cache.fail = "open"`.

## Nested invocations

A Cargo started inside a QuickRust session (by a build script, an external subcommand such as Clippy, nextest or `cargo llvm-cov`, an `xtask`, a test) reaches QuickRust:

- through `PATH`, where `~/.qrust/bin/cargo` comes first;
- through `CARGO`, which Cargo hands to everything it starts. QuickRust runs a hard link of the toolchain's `cargo` named `.qrust-engine`, with `argv[0]` and `CARGO` set to `~/.qrust/bin/cargo`, so that Cargo passes QuickRust's `cargo` on (COMP-030). On Windows, which has no separate `argv[0]`, Cargo's help names `.qrust-engine.exe`.

A nested invocation is a QuickRust session of its own, linked to its parent through `QRUST_SESSION_ID` (COMP-031), with the same toolchain (`RUSTUP_TOOLCHAIN`). Its rustc events are recorded with the parent's. `compatibility.nested-cargo = "upstream"` (`QRUST_NESTED=upstream`) hands nested invocations to the toolchain's Cargo directly (COMP-032).

Under `qrust upstream` and with a linked toolchain, Cargo runs as itself and nested builds through `$CARGO` reach that Cargo directly; their compilations are still observed when the outer command set `RUSTC_WRAPPER`.

A linked toolchain whose `cargo` is QuickRust itself is refused with exit status 2 (COMP-022).

## Target directory

| The user chose | Target directory |
|---|---|
| `--target-dir DIR` | `DIR`, used as it is |
| `--config build.target-dir=DIR` on the command line | `DIR`, used as it is |
| `CARGO_TARGET_DIR` or `CARGO_BUILD_TARGET_DIR` in the environment | its value, used as it is (unless an enclosing QuickRust set it for another workspace) |
| `build.target-dir` in a Cargo configuration file | overridden |
| nothing | `~/.qrust/cache/builds/<name>-<id>/`, given to Cargo and every tool under it through `CARGO_TARGET_DIR` and `CARGO_BUILD_BUILD_DIR` |

The workspace is the one `cargo locate-project --workspace` names for the directory the command starts in (after `-C` and `--manifest-path`). `cargo metadata` reports the directory, and `qrust path` prints it. A directory the user chose is never managed or removed by QuickRust's cache.

## Writes

QuickRust writes into a repository only on request ([Files](files.md#in-a-repository)). State lives in `~/.qrust`.

A build never fails because QuickRust could not record or analyse it: the failure is a `WARN`, and Cargo's status stands, unless `measurement.strict = true`.

## Platform differences

See [Platforms](platforms.md).
