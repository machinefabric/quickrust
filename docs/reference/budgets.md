# Budgets

The budget file, its policy and thresholds, the metrics a budget can name, and how a budget is evaluated against a baseline.

Budgets are evaluated by `qrust ci evaluate`, and after every successful build command when `ci.enforce-on-build = true` ([Commands](commands.md#budgets)). Guide: [How to set up QuickRust in CI](../how-to/set-up-ci.md).

## File

`.qrust/budgets.toml` in the workspace root, or the path in `ci.budget-policy` (relative to the workspace root), or the file given with `qrust ci evaluate --budgets FILE` (relative to the current directory).

```toml
[policy]
mode = "warn"            # warn | blocking
incomparable = "warn"    # warn | fail
noise-percent = 3

[[budget]]
metric = "wall_time_ns"
scenario = "clean"
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

Unknown tables, keys and metrics are errors, as is a budget that sets no threshold.

### `[policy]`

| Key | Values | Default | Meaning |
|---|---|---|---|
| `mode` | `warn`, `blocking` | `warn` | whether a failed budget fails the command (exit status 3) or only warns |
| `incomparable` | `warn`, `fail` | `warn` | whether an incomparable result fails the command (exit status 4) |
| `noise-percent` | a number of at least 0 | `0` | a relative change whose size is at most this counts as no change |

### `[[budget]]`

| Key | Value | Meaning |
|---|---|---|
| `metric` | a metric name (required) | what the budget limits |
| `scenario` | a scenario name | the budget applies only to a candidate of this scenario |
| `max-value` | a number, or a string with a unit | the candidate's value must not exceed it |
| `min-value` | a number, or a string with a unit | the candidate's value must not fall below it |
| `max-increase-percent` | a number | the increase over the baseline must not exceed this percentage |
| `max-decrease-percent` | a number | the decrease below the baseline must not exceed this percentage |

A string threshold takes the metric's unit: a duration for `ns` metrics (`500ms`, `1.5s`, `2m`, `1h`, `100ns`, or a bare number of nanoseconds), a size for `bytes` metrics (`20GiB`, `500MB`; powers of 1024). `max-increase-percent` and `max-decrease-percent` need a baseline.

## Metrics

| Metric | Unit |
|---|---|
| `wall_time_ns` | ns |
| `critical_path_ns` | ns |
| `sum_unit_time_ns` | ns |
| `cpu_time_ns` | ns |
| `peak_rss_bytes` | bytes |
| `units_compiled` | count |
| `units_restored` | count |
| `artifact_allocated_bytes` | bytes |
| `artifact_logical_bytes` | bytes |
| `output_bytes_written` | bytes |
| `cache_hits` | count |
| `cache_misses` | count |
| `cache_non_cacheable` | count |
| `cache_hit_ratio` | ratio |
| `cache_lookup_time_ns` | ns |

What each metric measures: [Run report schema](report-schema.md#metrics).

## Baselines

A baseline is a run or benchmark summary written by `qrust baseline set NAME` to `<workspace>/.qrust/baselines/NAME.json`. Budgets are evaluated against the baseline named by `--baseline` or `ci.baseline` (default `main`). A missing baseline is an error only when a budget is relative.

## Comparability

Before a relative threshold is evaluated, baseline and candidate are compared. A difference in any of these makes them incomparable, and every relative threshold's result `incomparable`:

| Field | Difference |
|---|---|
| `environment.rustc_version` | compiler version |
| `environment.host` | host triple |
| `environment.machine_class` | hardware class |
| `environment.command_family` | command |
| `environment.profile` | profile |
| `environment.targets` | compilation targets |
| `scenario.name` | scenario |
| `scenario.cache_state` | cache state (cold or warm) |

A difference in any of these makes them partially comparable; thresholds are still evaluated and the differences are listed:

| Field | Difference |
|---|---|
| `environment.cargo_version` | Cargo version |
| `environment.variables_digest` | build-relevant environment variables |
| `environment.cache_provider` | compiler cache |
| `environment.source_revision` | source revision |
| `environment.dirty` | working-tree dirtiness |
| `environment.lockfile_digest` | lockfile |

## Results

Each threshold check is `pass`, `fail`, `incomparable` (relative threshold without a comparable baseline) or `unavailable` (the metric is unavailable in the candidate or the baseline). A budget's result is its worst check, in the order `fail`, `incomparable`, `unavailable`, `pass`.

| Overall result | When | Exit status of `qrust ci evaluate` |
|---|---|---|
| `fail` | a budget failed and `mode = "blocking"` | 3 |
| `warn` | a budget failed and `mode = "warn"` | 0 |
| `incomparable` | no budget failed and a budget was incomparable | 4 with `incomparable = "fail"`, else 0 |
| `pass` | otherwise | 0 |

`--format json` prints the result with every evaluation, check, baseline and the comparability differences.

### On a build

With `ci.enforce-on-build = true`, every successful build command evaluates the budgets against its own run: a `fail` prints `FAIL budget: ...` on stderr and exits 3, a `warn` prints `WARN budget: ...`, an `incomparable` prints `WARN budget incomparable: ...` (and exits 4 with `incomparable = "fail"`). Missing budgets or a missing baseline a budget needs print a `FAIL` and exit 2. The result is stored in the run report's `budget` field. Cargo's stdout is not touched.
