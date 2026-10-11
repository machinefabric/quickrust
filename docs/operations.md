# Operating Haul

## Modes

| `mode` | Builds |
|---|---|
| `fast` (default) | upstream Cargo, plus build settings that keep program behaviour identical, applied as `--config` arguments to build commands only (see the README). A setting the user made in the manifest, a Cargo config file, `CARGO_PROFILE_*` or `--config` is never overridden. |
| `compat` | exactly upstream Cargo's builds; Haul only observes |

Set it in `.haul/config.toml` (`mode = "compat"`), `~/.haul/config.toml`, or `HAUL_MODE`.
The run report's `policy.fast` lists what was applied and what was left alone, with the
user setting that caused it. External subcommands (`cargo nextest`, …) never receive the
settings, because Cargo does not pass `--config` on to them.

## Where state lives

| Location | Content | Safe to delete |
|---|---|---|
| `~/.haul/` (`HAUL_HOME`) | per-workspace sessions, raw rustc events, reports, unit index, benchmarks, remediation records | yes; history is lost |
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
| 2 | Haul could not delegate: no upstream Cargo, recursion, invalid Haul configuration, invalid management arguments |
| 3 | a blocking budget failed (`haul ci evaluate`, or a build with `ci.enforce-on-build`) |
| 4 | baseline and candidate are incomparable and the policy says `incomparable = "fail"` |
| 1 | a benchmark, experiment or `doctor` finished with a negative result |

A policy status only ever replaces a *successful* Cargo status.

## CI

```bash
haul install-shim --dir "$RUNNER_TEMP/haul-shim" && export PATH="$RUNNER_TEMP/haul-shim:$PATH"
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
`peak_rss_bytes`, `units_compiled`, `artifact_allocated_bytes`,
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
