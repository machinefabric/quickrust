# How QuickRust works

QuickRust stands where `cargo` stands, yet the builds it runs are still made by the toolchain's own Cargo and rustc. This page explains how both can be true: what QuickRust puts around the toolchain, how it sees every compilation without changing how Cargo works, and why it is built this way rather than as a Cargo fork or a plugin.

## The shape of it

```text
developer / IDE / CI --> cargo (QuickRust) --> toolchain's Cargo --> qrust-rustc-wrapper --> [your wrapper / sccache] --> rustc
                           ^   |                      |                  |
                           |   +-- session record     |                  +-- one event per rustc invocation
                           +--------------------------+  $CARGO: Clippy, nextest, build scripts: nested builds come back to QuickRust
```

Three programs share the work. `qrust` is the one users meet; after `qrust setup` it is also `cargo`, `rustc`, `rustfmt`, `cargo-clippy`, `rust-analyzer` and the other toolchain programs in `~/.qrust/bin`. The toolchain's Cargo is the build engine: QuickRust has not replaced Cargo's resolver, its scheduler or its fingerprints, and runs Cargo for every command with the arguments the user typed. `qrust-rustc-wrapper` sits in front of every rustc invocation that Cargo makes, as Cargo's `RUSTC_WRAPPER`. Since the early cut-off work there is a fourth piece, QuickRust's compiler driver, which is rustc itself run through QuickRust's callbacks ([early cut-off](early-cutoff.md)).

## Why wrap Cargo instead of replacing it

A drop-in replacement lives or dies by compatibility. Cargo's command line, configuration, resolver, lockfile handling, build-script protocol and JSON messages are a large surface that the ecosystem depends on in detail, and every reimplementation of it drifts. Running the toolchain's own Cargo means QuickRust inherits all of that exactly, for every toolchain, including ones released after QuickRust was built. What QuickRust adds has to fit through the seams Cargo already offers: `--config` arguments, environment variables, the target directory and the rustc wrapper. That constraint shapes almost everything below, and it is also why `mode = "compat"` can promise exactly Cargo's builds: with its own additions switched off, QuickRust is only an observer of a stock Cargo.

The decision is not permanent. The plan is to own everything around the engine first, then replace the engine's pieces one at a time where measurement shows a gain ([owning the environment](owning-the-environment.md)).

## One command, one session

When `cargo build` reaches QuickRust, it becomes a session. QuickRust loads its configuration, works out which toolchain the directory uses (`+toolchain`, `RUSTUP_TOOLCHAIN`, a toolchain file, the default), installs it if it is missing, finds the workspace and its build directory, decides what fast mode adds, and runs the toolchain's Cargo. Cargo inherits stdin, stdout and stderr, the full environment and the raw arguments; QuickRust only adds its own variables and `--config` arguments. When Cargo exits, QuickRust writes a small session record, passes Cargo's exit status (or re-raises its signal) and is done.

Not every command needs that much. Commands that only ask Cargo something (`cargo metadata`, `cargo --version`, `cargo locate-project` and the like) are handed straight to the engine with QuickRust's environment, without a session record: rust-analyzer asks such questions constantly, and they must cost nothing. (`cargo tree` and `cargo metadata` in a terminal are the exception: they may download what they describe, so QuickRust shows that progress.) `qrust upstream` goes further: it runs the toolchain's Cargo as itself, in QuickRust's environment (the toolchain, the Cargo home, the workspace's build directory) but with none of QuickRust's settings, no wrapper and no record, and without even reading QuickRust's configuration, so the escape hatch works when that configuration is broken.

## The engine and the name it runs under

Cargo tells everything it starts where Cargo is: external subcommands, build scripts, test binaries and `env!("CARGO")` all receive a `CARGO` variable, and tools such as Clippy, nextest or `cargo llvm-cov` call back through it for nested builds. If QuickRust ran the toolchain's `cargo` binary under its own name, Cargo would hand out that binary's path, and every nested build would bypass QuickRust.

Cargo takes an inherited `CARGO` (or the path in its `argv[0]` when that is named `cargo`) only when its own executable is not called `cargo`. So QuickRust runs a hard link of the toolchain's `cargo` named `.qrust-engine`, with `argv[0]` and `CARGO` both pointing at QuickRust's own `cargo` in `~/.qrust/bin`. Cargo then hands QuickRust's path to everything it starts, and its help and errors still say `cargo`. On Windows there is no separate `argv[0]`, so Cargo's help names the link there. A linked toolchain is never modified, so its Cargo runs as itself.

The rest of the engine's environment follows the same idea of keeping nested programs on the same footing as the outer one. `CARGO_HOME` is QuickRust's (`~/.qrust/cargo`), so registry sources and credentials are shared by every project on the machine and rustup's `~/.cargo` is no longer consulted. `RUSTUP_TOOLCHAIN` names the exact toolchain, so a build script's `rustc`, or `rustfmt` run from a subcommand, reaches QuickRust's proxies and gets the same toolchain even if the channel moves to a new release mid-build. `~/.qrust/bin` comes first on `PATH`. Cargo is told never to clean its download caches by itself (`CARGO_CACHE_AUTO_CLEAN_FREQUENCY=never`), because QuickRust manages that space ([owning the environment](owning-the-environment.md)).

## Nested builds

Because nested Cargo invocations come back to QuickRust, through `PATH` or through `$CARGO`, each becomes a session of its own, linked to the outer one by `QRUST_SESSION_ID`. A nested build inside an instrumented session writes its rustc events into the parent's stream, so a `cargo clippy` that runs Cargo again is still one picture. If that is unwanted, `compatibility.nested-cargo = "upstream"` (`QRUST_NESTED=upstream`) hands nested invocations straight to the toolchain's Cargo.

A nested QuickRust must never end up running itself as its own engine, for example through a linked "toolchain" whose `cargo` is a copy of QuickRust. QuickRust marks the engine it starts with its own process id (`QRUST_EXEC_GUARD`); a QuickRust whose parent process is the one that meant to start Cargo knows it was started in Cargo's place, and stops with a diagnostic instead of looping. A legitimate nested build, started by Cargo or by a build script, has a different parent and is unaffected.

## The rustc wrapper

The wrapper is where QuickRust sees compilations. Cargo runs `qrust-rustc-wrapper <rustc> <args...>` for every unit; the wrapper runs the user's own wrapper if one is configured (sccache, for instance), otherwise rustc, with untouched arguments, environment and stdio, and then drops one small JSON event file: what was compiled, how long it took, with what result, what the cache did. One file per invocation means compiler processes never contend on a shared lock, and a crashed process loses only its own event.

Observation is strictly fail-open. A problem writing an event, reading the cache or computing a key can only mean that the unit is compiled normally and recorded less well; it can never change the compiler's result. This is also where fast mode does most of its work, as the wrapper is the one place that sees each unit before it is compiled: it consults the [artifact cache](artifact-cache.md), adds rustc's parallel frontend to units compiled from scratch, selects the wild linker when present, and hands units to the compiler driver ([fast by default](fast-by-default.md)). A wrapper the user configured always runs, and QuickRust's own levers step aside behind it, since such a wrapper may be a cache with rules of its own.

## Recording now, analysing later

Everything QuickRust can say about a build (timing, the observed dependency graph and its critical path, why each unit rebuilt, duplication, footprint, recommendations) comes from the session record and its events. Turning those into a report takes time that a developer waiting for `cargo build` should not pay, so by default a build only records. Analysis happens the first time something asks for it: `qrust analyze`, `qrust report`, `qrust compare`, or right after the build when a report path is set (`QRUST_REPORT`), `measurement.finalize = "sync"`, or budgets are enforced on build. A report, once produced, is immutable.

Lazy analysis has a cost, which QuickRust states rather than hides: a run analysed after later builds still has everything recorded at build time, but the build directory has moved on, so its artifact footprint and the workspace's state at that moment are reported as unavailable ([measurement](measurement.md)).

## Where to go next

- [Fast by default](fast-by-default.md) explains what QuickRust changes about a build, and why those changes are safe.
- [Owning the environment](owning-the-environment.md) explains toolchains, the Cargo home and build directories.
- [Compatibility](../reference/compatibility.md) states the drop-in contract precisely.
- [Environment variables](../reference/environment.md) lists what QuickRust reads and what it sets for Cargo.
