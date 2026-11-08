# How to use sccache with QuickRust

This guide shows you how to build with sccache through QuickRust, either with the sccache setup you already have or by letting QuickRust add it, and how to read what it did.

Before you start, consider whether you need it: QuickRust's own artifact cache already shares compiled dependencies across all your checkouts on one machine. sccache adds a cache shared between machines (a remote store), at a price: whenever any rustc wrapper is configured, QuickRust turns off its artifact cache, the parallel frontend, early cut-off and the fast linker for those builds. See [Fast mode](../reference/fast-mode.md).

## Keep the sccache setup you have

If you already set `RUSTC_WRAPPER=sccache` in the environment or `build.rustc-wrapper` in a `.cargo/config.toml`, do nothing. With the default `cache.provider = "inherit"`, QuickRust keeps your wrapper, runs its own wrapper outside it, and records sccache's statistics for every build.

## Let QuickRust add sccache

If sccache is installed (on `PATH`) but not configured as a wrapper, ask QuickRust to chain it:

```toml
# .qrust/config.toml (one project) or ~/.qrust/config.toml (all projects)
[cache]
provider = "sccache"
fail = "open"
```

Configure sccache itself (local directory, remote store, credentials) as sccache documents; QuickRust neither reads nor stores those settings.

A wrapper you configured always wins over `provider = "sccache"`: QuickRust keeps yours, warns, and records the conflict.

## Decide what happens when sccache is unavailable

QuickRust checks that sccache answers before each build it chained sccache into.

- `fail = "open"` (default): the build compiles without sccache, QuickRust prints a `WARN` line, and the run is marked `degraded`.
- `fail = "closed"`: the command stops before compiling. Use this where building without the cache would be worse than not building, for example a CI job whose budget assumes cache hits.

A wrapper you configured yourself is never skipped: if it cannot start, the build fails as it would under plain Cargo.

## Choose an incremental policy

sccache does not cache crates compiled incrementally, and Cargo compiles workspace crates incrementally in development profiles. If you want sccache to cache your workspace crates too, turn incremental compilation off, separately for developer machines and CI:

```toml
[incremental]
local-dev = "cargo-default"   # keep incremental rebuilds on your machine
ci = "off"                    # let sccache cache everything in CI
```

Values are `cargo-default` (no change), `on`, `off` and `experiment`. An explicit `CARGO_INCREMENTAL` in the environment always wins. QuickRust treats a run as CI when `CI` or a known CI variable (`GITHUB_ACTIONS`, `GITLAB_CI`, `BUILDKITE`, `JENKINS_URL`, `TF_BUILD`, `TEAMCITY_VERSION`) is set.

To find out which policy is faster for your project, measure both; see [How to measure the effect of a build setting](measure-a-change.md):

```bash
qrust experiment --env CARGO_INCREMENTAL=0 --scenario small-edit --allow-mutation -- build
```

## See what sccache did

```bash
qrust cache status          # sccache's version, location and server statistics, and the last analysed run
qrust analyze cache         # hits, misses and non-cacheable requests of the latest build
```

The numbers are sccache server counters between the start and the end of the build, so other builds using the same server at the same time are included.

## Related

- [Configuration](../reference/configuration.md) for `cache.provider`, `cache.fail` and `incremental.*`
- [About the artifact cache](../explanation/artifact-cache.md)
- [Security and privacy](../explanation/security.md) for how QuickRust composes wrappers
