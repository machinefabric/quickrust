# How to find out why crates rebuilt

This guide shows you how to find out why a build compiled crates you expected to be fresh, and what to do about the usual causes.

## Ask QuickRust about the latest build

Right after the build that surprised you:

```bash
qrust analyze rebuilds
```

```text
Rebuild reasons
  REASON  UNITS  TIME
  source  1      42ms
  previously known units rebuilt: 1 of 1 in the index (ratio 1.0)
  UNIT        KIND  REASON  COST  CONFIDENCE  DETAIL
  demo@0.1.0  bin   source  42ms  high        timestamp-only change (content identical): src/main.rs
```

Each rebuilt unit has one reason, the time it cost and how sure QuickRust is. On a large build, narrow the list:

```bash
qrust analyze rebuilds --filter reason=environment
qrust analyze rebuilds --filter package=serde --sort cost_ns:desc --limit 10
qrust analyze rebuilds --format json
```

To look at an earlier build, add `--run latest~1` (or a run id from `qrust runs`).

## Act on the reason

| Reason | What changed | What to do |
|---|---|---|
| `new` | QuickRust has not seen this unit before | Nothing: it is the first build of it. |
| `source` | a file the compiler read; `timestamp-only change (content identical)` means only its modification time | Expected after an edit. If you did not edit it, find what touches it (a code generator, a `git checkout`, a formatter writing unchanged files). |
| `dependency` | a dependency was rebuilt | Look up the dependency's own reason: fix that and this one goes away. |
| `build-script-input` | the package's build script ran again | Check what the script declares with `cargo::rerun-if-changed` / `rerun-if-env-changed` (`qrust analyze scripts`); a script that declares nothing reruns whenever any file of its package changes. |
| `environment` | an environment variable the unit depends on | Keep that variable the same between builds; editors and terminals often differ (`PATH`, `RUSTFLAGS`, tool-specific variables). |
| `rustflags` | `RUSTFLAGS` or target rustflags | Use the same flags in every place you build from, or give the differing tool its own target directory. |
| `feature-set` | the package was built with other features | Building single packages (`-p`) and the whole workspace resolve features differently; `qrust analyze duplication` lists the variants and who asks for them. |
| `profile` | profile settings | Usually alternating between commands or modes with different settings, for example between fast and compat mode. |
| `compiler` | the toolchain | The channel moved to a new release (see below), or `+toolchain` / a toolchain file selected another one. |
| `target` | the compilation target | Expected when you build for another `--target`. |
| `cache-miss` | nothing: an identical unit exists, but not in this build directory, or `cargo clean` removed it | Normal for a new checkout or after a clean; QuickRust's artifact cache restores most such units. |
| `unknown` | QuickRust could not see what changed | See "When the reason is unknown" below. |

## Compare two builds

```bash
qrust compare latest~1 latest
```

prints both runs' metrics side by side and what differs between their environments (toolchain, lockfile, settings). A difference listed as `notable` there is often the cause.

## Rebuilds after switching between QuickRust and plain Cargo

Plain Cargo and QuickRust keep separate build directories, so they never rebuild each other's output. If you share one directory between them, build scripts that declare `cargo::rerun-if-env-changed=RUSTC_WRAPPER` rerun on every switch; [How to try QuickRust before making it your cargo](adopt-gradually.md#if-build-scripts-rerun-when-you-switch) shows how to avoid it.

## Rebuilds after a toolchain update

QuickRust updates channels such as `stable` in the background, and the first build on the new release compiles everything again (reason `compiler`). If you would rather choose when that happens, pin the release in `rust-toolchain.toml` or turn background updates off; see [How to choose and manage toolchains](choose-a-toolchain.md#keep-toolchains-current).

## Units built twice in other worktrees

```bash
qrust analyze worktrees
```

lists the other checkouts and workspaces QuickRust knows, and how many identical units each built that this one built too.

## When the reason is unknown

`unknown` means the unit's identity changed without any component QuickRust can observe changing, or it rebuilt with an unchanged identity (inputs rustc does not list, or artifacts removed outside Cargo). To see what Cargo itself thinks, run the same build with Cargo's fingerprint log:

```bash
CARGO_LOG=cargo::core::compiler::fingerprint=info cargo build 2>&1 | grep -i dirty
```

## Related

- [Report schema](../reference/report-schema.md) for the `rebuilds` records
- [About measuring builds](../explanation/measurement.md) for what QuickRust can and cannot observe
