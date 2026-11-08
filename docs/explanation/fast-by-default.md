# Why QuickRust is fast by default

QuickRust changes how your project is built without being asked, and the changes go further than a tool that merely runs Cargo might be expected to make. This page explains why it makes them, what limits it puts on itself, how a change earns its place as a default, and what each one costs.

## The thesis

QuickRust began as an observer: a Cargo that records and explains builds, at parity with Cargo's speed. On 2026-10-11 the goal changed. Bun showed what happens when a JavaScript runtime and package manager is simply faster than the tools it replaces, with no configuration: people switch, and feel the difference on the first command. QuickRust aims at the same for Rust. Replace `cargo` with QuickRust on any project and builds should be noticeably faster than Cargo's, with nothing to configure.

Much of that speed was available all along. Cargo's own build-performance guide recommends lighter debug information; rustc has a parallel frontend; faster linkers exist; compiled dependencies could be shared between checkouts. Few projects use any of it, because each lever means reading documentation, editing configuration and convincing yourself the change is harmless. A default is the only way most builds will ever get them.

## The rule: the program must behave identically

The licence QuickRust takes is narrow. It may change anything about how a program is built, as long as the program it builds behaves the same: the same output for the same input, the same panics, the same file and line numbers in backtraces, the same results from its tests. It may not change what the program does, and it may not change the source, the manifest, the lockfile or anything else in the repository.

Within that rule, fast mode currently does five things:

- **Lighter debug information.** Dev builds keep line tables (`profile.dev.debug = "line-tables-only"`), so backtraces, stepping and breakpoints by line still work, and dependencies get no debug information at all, as Cargo's guide recommends. Generating, linking and storing full variable and type information is much of what makes debug builds slow and large.
- **rustc's parallel frontend** for units compiled from scratch: dependencies, release and CI builds, and a workspace crate's first compilation in a target directory.
- **The wild linker**, when it is installed, for host builds on Linux with glibc. Relinking after an edit was measured markedly faster than with rustc's bundled LLD: by 22% for ripgrep and 34% for a small binary.
- **A global artifact cache**: a unit compiled once is restored instead of compiled again ([artifact cache](artifact-cache.md)).
- **Early cut-off**: after an edit that dependents cannot observe, their compilations are skipped ([early cut-off](early-cutoff.md)).

The [fast-mode reference](../reference/fast-mode.md) gives each one's exact conditions and switches.

## Your settings always win

A default is a guess about what most people want. Anyone who has made a decision about a setting has made a better one for their own project, so QuickRust never overrides it. If the root manifest, a Cargo configuration file, a `CARGO_PROFILE_*` variable or a `--config` argument sets a profile key, QuickRust leaves that key, and the keys that depend on it, alone. If you configured a rustc wrapper, QuickRust's cache and `-Z` levers step aside behind it. If you set `RUSTC_BOOTSTRAP` yourself, QuickRust uses no lever that needs it. If you chose a linker, wild is not used.

The profile settings reach Cargo only as `--config` arguments to built-in build commands. They are not written to any file and not placed in the environment, so they cannot leak into build scripts, into programs run by `cargo run`, or into nested Cargo invocations that did not ask for them. Every run records what fast mode applied, what it left alone and why (`policy.fast` in the [run report](../reference/report-schema.md)), so "what did QuickRust do to my build?" always has a written answer.

## Escape hatches

Two ways out exist, at different depths. `mode = "compat"` (or `QRUST_MODE=compat`) makes QuickRust build exactly what the toolchain's Cargo builds, while still recording and analysing: it is the mode the compatibility tests run in, byte for byte against Cargo as the oracle. `qrust upstream <args>` runs the toolchain's Cargo as itself, with none of QuickRust's settings, wrapper or recording (it still uses QuickRust's toolchain, Cargo home and build directory). Both exist so that "is QuickRust the cause?" can always be answered in one command. [How to build exactly as Cargo does](../how-to/build-exactly-like-cargo.md) shows how to use them, and how to turn off one lever at a time.

## How a lever becomes a default

"Faster" is a claim that needs a number, and "behaves identically" is a claim that needs evidence. Every lever has to pass three gates before it becomes a default:

- **The speed index.** `qrust-bench` measures the same developer loop (cold build, no-op, edits in a leaf and in a core crate, check, test build, rebuild after clean, a second checkout) through QuickRust and through plain Cargo, on three real workloads: QuickRust's own first commit, ripgrep, and an axum service. The index is the geometric mean of Cargo's time divided by QuickRust's. A variant called `cargo-tuned` applies the settings any user could put in their own configuration, so the index also shows how much QuickRust adds beyond what configuration alone could do. Checkpoints are recorded in [`benchmarks/PROGRESS.md`](../../benchmarks/PROGRESS.md): at checkpoint 2 (2026-10-13) the index was 2.158 against Cargo, where `cargo-tuned` reached 1.066. The method is described in [`benchmarks/README.md`](../../benchmarks/README.md).
- **The correctness gate** (`qrust-bench verify`). Each workload's own test suite is built and run by upstream Cargo and by QuickRust, from fresh checkouts, twice for QuickRust (the second time from a warm artifact cache, so tests also run against binaries built from restored units). A test whose outcome differs consistently between Cargo and QuickRust fails the gate.
- **The cut-off gate** (`qrust-bench cutoff`), for early cut-off: after random edits inside function bodies, what the developer loop restored and relinked must be byte for byte what building the edited source from nothing produces.

A measurement can also remove a lever or narrow it. The parallel frontend was first applied to every unit, and checkpoint 1 showed it slowing ripgrep's core-crate edit to 0.58 of Cargo's speed: on small incremental rebuilds its start-up costs more than it saves. It is now applied only to compilations from scratch, and that edit runs at 4.60 times Cargo's speed at checkpoint 2.

## What the levers cost

The rule says program behaviour is identical; it does not say every byte is. Each lever has a visible difference, and they are stated here rather than discovered later.

**Line tables only.** A debugger stepping through a dev build sees files and lines but not local variables or their types. That is the setting's whole point, and also the one most likely to be noticed. Anyone who needs full debug information sets `[profile.dev] debug = true` as they would with Cargo, and QuickRust then leaves debug settings alone.

**`RUSTC_BOOTSTRAP` and `-Zallow-features=`.** The parallel frontend and the linker selection are behind `-Z` flags, which a stable compiler accepts only with `RUSTC_BOOTSTRAP=1`. That variable would also let the compiler accept `#![feature]` attributes, which would quietly turn a stable build into a nightly one. QuickRust therefore sets it only for the compiler processes of units Cargo schedules, always together with `-Zallow-features=`, an empty list of allowed features, so a crate that enables a nightly feature is still rejected (with rustc's "not in the list of allowed features" error rather than the stable-channel wording). Build scripts, and the compiler probes they run to detect features, never see it, so crates do not switch to nightly code paths. The variable is also part of an incremental session's identity, so QuickRust sets it for every compilation of such a unit, including incremental rebuilds that get no `-Z` flags, to keep their sessions.

**A foreign `OUT_DIR`.** Most crates restored from the artifact cache are exactly what compiling them here would produce. One kind is not quite: a dependency that `include!`s code its build script generated, as serde does, and that was first compiled in another target directory. Its panic messages and debug information for the generated code name that other directory. Behaviour is unaffected. QuickRust says so the first time it happens (once per QuickRust home), and the run report counts such units (`cache.artifacts.restored_with_foreign_out_dir`).

**Different crate hashes.** With early cut-off, libraries are compiled by QuickRust's driver, whose crate hash covers only what dependents can observe. Libraries' metadata and rlibs therefore differ from rustc's in that hash, which is an identifier that nothing compares across compilers ([early cut-off](early-cutoff.md)).

**Separate artifacts.** Because the profile settings differ, Cargo names fast-mode artifacts differently from plain Cargo's. If both share a target directory, each keeps its own set, and alternating between them rebuilds nothing.

## Where the line is drawn

Anything that changes what a program does is out, however fast: lowering optimisation levels or changing a profile's panic strategy would make a dev build a different program. Ideas that pass the rule still have to pass the gates, and some do not. Seeding a new worktree's build directory with a copy of a sibling's was measured and rejected: the copy cannot save compiling the workspace's own crates, whose outputs name their checkout, and a copy whose sources look older than the sibling's outputs could wrongly pass as up to date ([`benchmarks/README.md`](../../benchmarks/README.md)). The defaults are what the benchmarks have shown to be both safe and faster, and nothing else.
