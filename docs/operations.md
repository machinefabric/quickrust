# Operating Haul

## Modes

| `mode` | Builds |
|---|---|
| `fast` (default) | the toolchain's Cargo, plus everything below. Program behaviour is identical. |
| `compat` | exactly the builds of the toolchain's Cargo; Haul only observes |

Set it in `.haul/config.toml` (`mode = "compat"`), `~/.haul/config.toml`, or `HAUL_MODE`.

What fast mode does, and the settings that control it:

| | Setting | Notes |
|---|---|---|
| Profile settings (debug info) | — | `--config` arguments to built-in build commands only. A setting the user made in the root manifest, a Cargo config file, `CARGO_PROFILE_*` or `--config` is never overridden; the report's `policy.fast` says what was applied, what was left alone, and why. External subcommands (`cargo nextest`, …) never receive them: Cargo does not pass `--config` on. |
| rustc's parallel frontend | `fast.frontend-threads` = `auto` (up to 8 on 4+ cores) \| `off` \| a count ≥ 2 | Applied by the rustc wrapper to the units Cargo schedules that are compiled from scratch: dependencies, release and CI builds, and a workspace unit's first compilation in a target directory. Incremental rebuilds never get it: it slows small ones 2–3×; nor does a unit the artifact cache saw compile quickly (a small binary not stored for that reason), whose compilation its start-up would only lengthen. On a stable compiler it needs `RUSTC_BOOTSTRAP=1`, which Haul sets for the compiler processes of all units Cargo schedules (rustc keeps incremental sessions only across compilations made alike), together with `-Zallow-features=` so that `#![feature]` stays rejected. Off behind a user rustc wrapper and when `RUSTC_BOOTSTRAP` is set. |
| Artifact cache | `cache.artifacts` = `on` \| `off`; `cache.artifacts-min-link-ms` (`300`) | Dependencies are shared across all checkouts; workspace units within the same checkout (their debug information names it). Linked workspace outputs that built faster than the threshold are not stored. Its space is managed with the rest of Haul's cache (below). Off behind a user rustc wrapper. |
| Early cut-off | `fast.early-cutoff` = `auto` \| `off` (`HAUL_EARLY_CUTOFF`) | With the artifact cache on: Cargo's units are compiled by Haul's compiler driver — the toolchain's own rustc run through Haul's callbacks (`driver/`), built once per toolchain in the background after the first build that finds it missing (installing the toolchain's `rustc-dev` component; `haul driver build` builds it on demand, `haul driver status` tells). A library's crate hash then covers only what its dependents can observe, so an edit inside a function body they cannot see into (not generic, not inlinable, not `const`/`async`, no `impl Trait` return, adding no closure, macro call or generic instantiation) leaves its dependents' cache keys unchanged: they are restored instead of compiled. When the driver compiles a binary or test executable it keeps the objects (the `.rlink` naming them, the objects hard-linked into `<stem>.haul-objects/`, a `<stem>.haul-relink` record of what they were compiled from), so when only what it links changed it is linked again (`-Zlink-only`) instead of compiled (not for one the cache saw compile quickly). The driver also links before rustc saves its incremental state and tells the wrapper its outputs are written: the build goes on while rustc finishes saving (Unix; not for crates with `#![feature]`, whose unused-feature warnings come later). Units that link are keyed by everything they link. Units whose MIR inlining could copy code across crates (optimised, non-incremental builds) are keyed by content as before. A unit the driver fails on is compiled by rustc instead, and by rustc from then on (`~/.haul/cache/drivers/<key>/failed/` keeps the report). Known limit: rustc's parallel frontend numbers a crate's nested statics in varying order, so such a crate can miss the cut-off once after a cold build. |
| Linker | `fast.linker` = `auto` \| `off` | Linux with glibc: when `wild` is in `~/.haul/bin` or on `PATH`, host builds of binaries, tests, proc-macros and dynamic libraries link with it instead of rustc's bundled LLD. A `-C linker`, linker-selecting link arguments or linker features from the user (Cargo configuration, `RUSTFLAGS`) win. `~/.haul/linker/ld` links to the wild in use. |
| Build output | `output` = `haul` \| `cargo` | In a terminal: status line, errors in full, warnings counted, everything stored (`haul warnings`, `haul errors`, `haul log`). `run`, `test` and `bench` build first (rendered), then run the command with the terminal inherited. compat mode defaults to `cargo`. |

## Toolchains

Haul installs every toolchain it uses (see [migration.md](migration.md#which-toolchain-runs) for how one
is chosen) from the dist server (`toolchain.dist-server`, `HAUL_DIST_SERVER`; default
`https://static.rust-lang.org`), with the `default` profile (rustc, cargo, the standard
library, clippy, rustfmt, rust-src) unless a toolchain file asks for another. `haul toolchain`
lists, installs, updates, removes and links them.

A channel (`stable`, `beta`, `nightly`, `1.98`) keeps naming the release it was installed as.
Once a day after a build that used it, Haul checks in the background whether the channel has a
newer release: a detached, low-priority `haul toolchain update --channel …` fetches the 64-byte
checksum of the channel's manifest and, when it changed, installs the new release (with the
components and targets the old one had) and switches the channel to it for the next build.
The release a channel left is removed once nothing names it and it has gone unused for a day;
releases asked for by their own name (`1.98.1`, `nightly-2026-10-11`) are never removed by Haul.
`toolchain.auto-update = "off"` (`HAUL_TOOLCHAIN_AUTO_UPDATE=off`) turns the background check
off; it never runs in CI. `haul toolchain update` runs it now.

## Haul's cache and disk space

Everything under `~/.haul/cache` can be rebuilt or downloaded again, and Haul keeps it within
two bounds:

| Setting | Default | |
|---|---|---|
| `cache.max-size` (`HAUL_CACHE_MAX_SIZE`) | `auto`: 10% of the disk, between 10 and 100 GiB | the most the cache uses |
| `cache.min-free` (`HAUL_CACHE_MIN_FREE`) | `auto`: 5% of the disk, between 2 and 20 GiB | the free space it leaves the disk |

Both take a size (`40GiB`) or a share of the disk (`10%`). A collection removes first what
nothing can use (build directories of workspaces that are gone, drivers of toolchains that are
gone, week-old unfinished downloads, month-old remembered answers, leftovers), then — while the
cache is above its size, or the disk below its free space — build directories and artifact
entries, least recently used first, until the cache is back under 90% of its size and the disk
has a quarter more than its free space; last, the crates and git checkouts downloaded into
Haul's Cargo home, which Cargo removes itself (`cargo -Zgc clean gc`: it knows when each was
last used and waits for builds reading them) — by its usual ages at every collection, and down
to what the bounds leave when they are still exceeded. Never what was used in the last 15
minutes, a build directory a command holds, or one Cargo is building in. A file the artifact cache and a build
directory share (a restored unit) counts once and is freed with its last link.

After every command Haul checks, at the cost of one `statvfs`, whether a collection is due
(below the free space; above the size by the free space lost since the last count; or a day
after that count) and then runs `haul gc --background` detached, at the lowest priority. When
the disk is nearly full (below a quarter of `cache.min-free`) a command collects first, for at
most ten seconds, and says so. `haul gc` collects now (`--dry-run` says what it would remove,
`--to SIZE` collects down to a size); `haul cache status` shows the last count and collections.

## Where state lives

| Location | Content | Safe to delete |
|---|---|---|
| `~/.haul/` (`HAUL_HOME`) | per-workspace sessions, raw rustc events, stored build output, reports, unit index, benchmarks, remediation records | yes; history is lost |
| `~/.haul/cache/artifacts/` | the artifact cache | yes; the next builds compile instead |
| `~/.haul/cache/usage.json`, `gc-log.jsonl` | the cache's last count, and what collections removed (`haul cache status`) | yes |
| `~/.haul/toolchains/` | the Rust toolchains Haul installed (`haul toolchain list`); `channels.json` says which release each channel names, the default, and linked toolchains | `haul toolchain remove`; a deleted toolchain is installed again on its next use |
| `~/.haul/cargo/` | Haul's Cargo home: `config.toml`, credentials, the registry index and sources, git checkouts | the registry and git directories are downloaded again; `config.toml` and `credentials.toml` are yours |
| `~/.haul/bin/` | Haul as `cargo`, `rustc`, `rustfmt`, `cargo-clippy` and the other toolchain programs | recreated by the next build |
| `~/.haul/cache/downloads/` | toolchain archives being downloaded (resumed after an interruption) | yes |
| `~/.haul/cache/memo/rustc/` | each compiler's `rustc -vV`, asked once per binary | yes; asked again |
| `~/.haul/cache/drivers/<commit>-<host>-<source>/` | Haul's compiler driver for one compiler and driver source (`haul driver status`); `build-failed` says why it could not be built, `failed/` keeps the report of each crate it failed on (compiled by rustc since) | yes; rebuilt in the background after the next build |
| `~/.haul/cache/builds/<workspace>-<id>/` (and `<…>.json`, `<…>.lock`) | each workspace's target directory (`haul path`): Cargo's build output, never a `target/` in the project; the record says which workspace and when it was last used, the lock that a command uses it | yes; the next build of that workspace starts over (its dependencies come back from the artifact cache) |
| `~/.haul/cache/memo/roots/` | which workspace a directory belongs to, as Cargo said (asked again when a `Cargo.toml` from there up changes) | yes |
| `<target>/<profile>/deps/<unit>.haul-svh`, `.rlink`, `.haul-objects/`, `.haul-relink` | a library's interface hash; a binary's kept objects and what they were compiled from, for relinking | yes, with the rest of the target directory (`cargo clean`) |
| `~/.haul/workspaces/<id>/prebuilt/` | stamps of the build phase of `run` / `test` / `bench` in a terminal: the files it depended on, with sizes and times (at most 32 per workspace) | yes; the next such command builds before running |
| `<target>/haul-bench/` | benchmark and experiment build directories | yes |
| `<repo>/.haul/` | configuration, budgets, baselines — written only on request | it is yours |

Retention is bounded: `measurement.keep-runs` (default 200) reports and
`measurement.keep-raw-events` (default 20) raw event sets per workspace.

Runs are analysed lazily: the build path only records. Analysis happens on the
first `haul analyze`/`report`/`compare`, or right after the build when
`HAUL_REPORT` is set, `measurement.finalize = "sync"`, or budgets are enforced.
A run analysed after later builds keeps its rustc-level evidence but reports
its artifact footprint as unavailable, since the directory has moved on.

## Exit statuses

| Status | Meaning |
|---|---|
| Cargo's own | whenever Cargo ran; signals are re-raised |
| 2 | Haul could not run the command: no toolchain (not installable, unknown name), recursion, invalid Haul configuration, invalid management arguments |
| 3 | a blocking budget failed (`haul ci evaluate`, or a build with `ci.enforce-on-build`) |
| 4 | baseline and candidate are incomparable and the policy says `incomparable = "fail"` |
| 1 | a benchmark, experiment or `doctor` finished with a negative result |

A policy status only ever replaces a *successful* Cargo status.

## CI

```bash
haul setup --no-modify-path && export PATH="$HOME/.haul/bin:$PATH"
HAUL_REPORT="$RUNNER_TEMP/haul/{session}.json" cargo build --locked
haul ci evaluate
```

Haul treats the run as CI when `CI` (or a known CI variable) is set; that
selects the `incremental.ci` policy and is recorded in the report.

### Budgets

`.haul/budgets.toml` (path: `ci.budget-policy`):

```toml
[policy]
mode = "warn"            # warn (default) | blocking
incomparable = "warn"    # warn (default) | fail
noise-percent = 3        # relative changes within ±3% count as no change

[[budget]]
metric = "wall_time_ns"
scenario = "clean"       # optional: only for this scenario
max-increase-percent = 10

[[budget]]
metric = "critical_path_ns"
max-increase-percent = 15

[[budget]]
metric = "artifact_allocated_bytes"
max-value = "20GiB"

[[budget]]
metric = "cache_hit_ratio"
min-value = 0.6
```

Metrics: `wall_time_ns`, `critical_path_ns`, `sum_unit_time_ns`, `cpu_time_ns`,
`peak_rss_bytes`, `units_compiled`, `units_restored`, `artifact_allocated_bytes`,
`artifact_logical_bytes`, `output_bytes_written`, `cache_hits`, `cache_misses`,
`cache_non_cacheable`, `cache_hit_ratio`, `cache_lookup_time_ns`.

Relative thresholds need a baseline (`ci.baseline`, default `main`) stored in
`.haul/baselines/`. They are only evaluated when baseline and candidate agree on
compiler version, host, hardware class, command, profile, targets, scenario and
cache state; otherwise the result is `incomparable`, never a pass or a fail.

A sound setup takes the baseline from a benchmark on the CI hardware class:

```bash
haul benchmark --scenario clean --reps 5
haul baseline set main --benchmark <ID>      # commit .haul/baselines/main.json
# in CI:
haul benchmark --scenario clean --reps 3 --format json > bench.json
haul ci evaluate --benchmark "$(jq -r .id bench.json)"
```

`ci.enforce-on-build = true` evaluates budgets after each ordinary build command
instead. The verdict goes to stderr; Cargo's stdout is not touched.

## sccache

- `cache.provider = "inherit"` (default): Haul changes nothing. If your
  `RUSTC_WRAPPER` is sccache, its statistics are collected per run.
- `cache.provider = "sccache"`: Haul chains sccache inside its own wrapper when
  no other wrapper is configured. A wrapper you configured is never replaced.
- `cache.fail = "open"`: if sccache cannot be reached before the build, the
  build proceeds without it, a warning is printed and the run is marked
  `degraded`. `closed` stops before compiling.

Statistics are sccache-server counters between the start and end of the run,
so concurrent builds on the same server are included. sccache does not report
bytes restored; that field is `null` with a reason.

Incremental compilation and sccache exclude each other per unit. Haul reports
the conflict and never turns incremental off on its own; `incremental.local-dev`
/ `incremental.ci` do, and each run records the decision and where it came from.
An explicit `CARGO_INCREMENTAL` always wins.

## Benchmarks

| Scenario | Starts from | Default command |
|---|---|---|
| `clean` | empty build directory each repetition | `build` |
| `no-change` | fully built tree | `build` |
| `small-edit` | fully built tree, one appended comment per repetition | `build` |
| `test-build` | empty build directory | `test --no-run` |
| `release` | empty build directory | `build --release` |

Pass your own command after `--` (for example `-- check --target wasm32-unknown-unknown`).

`small-edit` edits a source file. It asks first, or needs `--allow-mutation` /
`benchmark.allow-mutation = true`. The original bytes are journaled before the
edit and restored on normal exit, on failure and on interruption. If Haul is
killed outright, the next `haul benchmark` restores the file — but only if it is
still exactly the original plus Haul's marker lines; otherwise it stops and
tells you where the original is.

Each benchmark builds with an artifact cache of its own inside its build directory:
cold scenarios start every repetition with it empty (a clean build is cold, and still
pays for storing), and an experiment's baseline and candidate never share results.

Runs whose build failed, that timed out, during which source files changed, or
that used a different toolchain than the rest are discarded and counted.

## Platform notes

- **macOS**: fully supported; every test suite runs here.
- **Linux**: every test suite passes on `x86_64-unknown-linux-gnu` (Debian 12, run by
  hand, also as root; not yet in CI). aarch64 has not been run.
- **Windows**: the code is written against a process abstraction and
  type-checks for `x86_64-pc-windows-gnu`, but is not exercised by the test
  suite. Known gaps: no forwarding of termination signals beyond what the
  console delivers to the whole process group, no `--timeout` for benchmarks,
  no hard-link de-duplication or allocated-size accounting in the inventory, no
  active-build lock detection.
- Filesystem type, CPU model and memory are reported on macOS and Linux and are
  `null` with a reason elsewhere.

## Limits of observation on a stable toolchain

Reported as unavailable, never as zero:

- units Cargo considered fresh — only known when you pass
  `--message-format=json` on a pipe (Haul then observes the stream while
  passing it through untouched);
- the linker's share of a linking unit;
- time spent expanding proc macros in dependent crates;
- rustdoc invocations (`cargo doc` runs rustdoc outside `RUSTC_WRAPPER`);
- per-unit CPU and memory when a cache client runs rustc on your behalf.
