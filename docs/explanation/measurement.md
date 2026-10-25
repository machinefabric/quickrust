# About measuring builds

QuickRust reports numbers about builds (durations, critical paths, rebuild causes, cache hits, bytes on disk) and lets CI fail on them. Numbers that drive decisions have to be trustworthy, and build measurements are easy to get wrong in quiet ways. This page explains the principles QuickRust measures by and why each one is there.

## Unavailable is not zero

The most important rule is about what QuickRust does when it does not know something. A report that says a build had zero fresh units, zero bytes of artifacts or zero cache hits when it simply could not tell is worse than no report: the zero looks like a fact, gets compared against other numbers, and passes a budget it should not.

So in every report, `null` means unavailable, never zero, and the reason travels with it, in an `unavailable` object keyed by the field. A run analysed after its build directory has moved on reports its footprint as unavailable, with that reason. A run without a compiler cache reports cache statistics as unavailable because no cache was active, not as a zero hit rate. A rebuild QuickRust cannot attribute to a cause is reported as `unknown`, with low confidence, rather than assigned to the most likely-looking one. Consumers of the [report schema](../reference/report-schema.md) are expected to handle `null` everywhere a measurement could be missing.

## Record now, analyse when asked

A build is the worst moment to analyse it: the developer is waiting, and anything QuickRust adds is overhead on every build for the sake of reports most builds never get. So the build only records, cheaply: a session record and one small event per rustc invocation. Analysis, which runs `cargo metadata`, reconstructs the dependency graph, classifies rebuilds and inventories the build directory, happens when something asks for it, or right after the build when a report or budget check was requested for it.

The cost of that choice is stated rather than hidden. Some facts can only be observed while the build directory is as the build left it, such as the artifact footprint or whether the working tree was dirty. A run analysed after later builds keeps everything recorded at build time, and reports those facts as unavailable rather than describing today's tree under yesterday's run.

## Comparisons must compare like with like

"The build got 30% slower" is only meaningful if the two builds could have been the same speed. A new compiler release, a different machine class, a release build against a debug build, a cold cache against a warm one: each changes timings for reasons that have nothing to do with the change under review.

So when QuickRust compares two runs, or a run against a baseline, it first checks that they are comparable. Differences in compiler version, host triple, hardware class, command, profile, compilation targets, scenario or cache state make the comparison incomparable: the delta is shown, but it is not a verdict. Differences that may well be the thing under test (Cargo version, build-relevant environment variables, the compiler cache, the source revision, a dirty working tree, the lockfile) make it partially comparable and are listed.

Budgets follow the same logic. A relative threshold ("no more than 10% slower than main") needs a baseline, and is evaluated only when baseline and candidate are comparable; otherwise the result is `incomparable`, never a pass and never a fail. Whether incomparable should block a merge is a policy decision that belongs to the project, so it is a setting (`incomparable = "warn"` or `"fail"`), not a guess. Relative changes within the noise band the budget file sets (`noise-percent`) count as no change, because a budget that fails on noise teaches people to ignore it. The [budgets reference](../reference/budgets.md) gives the format.

## Repetition, and discarding bad samples

One build timing is an anecdote. `qrust benchmark` runs a scenario several times, reports distributions rather than one number, and refuses to let a broken repetition into the result: runs whose build failed, that timed out, during which source files changed, or that used a different toolchain than the rest are discarded and counted, so the report says how many samples it stands on.

Benchmarks also isolate what they measure. Each benchmark builds with an artifact cache of its own, so cold scenarios really are cold (and still pay the cost of storing), and the two sides of an experiment never share results. A scenario that edits a source file journals the original bytes first and restores them on exit, failure or interruption; if QuickRust is killed outright, the next benchmark restores the file, but only if it is still exactly the original plus QuickRust's marker lines, and otherwise stops and says where the original is. A measurement tool that can corrupt the code it measures will not be trusted with real projects. [How to measure a change](../how-to/measure-a-change.md) shows benchmarks and experiments in use.

QuickRust's own speed index is held to stricter rules still, because it decides what becomes a default ([fast by default](fast-by-default.md)). It runs in a sealed environment with a pinned toolchain and nothing from the machine's Cargo configuration; variants run interleaved in a rotating order so drift in machine load affects all of them alike; and a run is invalid, and never recorded, when more than half of a scenario's samples were taken under CPU pressure or a long scenario's samples spread by more than 25%. The method is in [`benchmarks/README.md`](../../benchmarks/README.md).

## What a stable toolchain cannot show

QuickRust observes builds through the seams Cargo offers on a stable toolchain, and some things do not pass through them. Rather than estimate, QuickRust reports these as unavailable:

- which units Cargo considered fresh, which only Cargo's JSON messages say. QuickRust observes that stream only when you already asked for it on a pipe, passing it through untouched, because asking for it in a terminal would change what you see;
- the linker's share of a unit that links;
- time spent expanding procedural macros inside dependent crates;
- rustdoc invocations, since `cargo doc` runs rustdoc outside `RUSTC_WRAPPER`;
- per-unit CPU and memory when a compiler-cache client runs rustc on your behalf, because the process QuickRust sees is the client's.

Some of this is available from nightly-only interfaces (`--unit-graph`, `-Zbuild-analysis`). QuickRust detects them and lists them among the toolchain's capabilities in the report, but nothing depends on them, so every report means the same thing on every toolchain. The [platforms reference](../reference/platforms.md) lists these limits alongside platform differences.
