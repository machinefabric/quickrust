# How to run QuickRust in CI and gate on build time

This guide shows you how to build with QuickRust in a CI job, keep a report of every build, and fail (or warn) when a change makes the build slower or bigger than you allow.

## Install QuickRust in the job

Pin the release a job builds with, and check the archive against the SHA-256 the release manifest (`https://release.machinefabric.com/quickrust/manifest`) gives for it:

```bash
version=0.150.0
sha256=<the manifest's sha256 of qrust-$version-linux-x86_64.tar.gz>
curl -fLO "https://release.machinefabric.com/quickrust/release/$version/qrust-$version-linux-x86_64.tar.gz"
echo "$sha256  qrust-$version-linux-x86_64.tar.gz" | sha256sum -c
tar xzf "qrust-$version-linux-x86_64.tar.gz"
quickrust/qrust setup --no-modify-path
export PATH="$HOME/.qrust/bin:$PATH"
```

On GitHub Actions, put the directory on `PATH` for the following steps with `echo "$HOME/.qrust/bin" >> "$GITHUB_PATH"` instead of the `export`. A job that adds the MachineFabric package repository can `apt install quickrust` (or `brew install machinefabric/tap/quickrust`) and run `qrust setup --no-modify-path`: see [How to install QuickRust](install.md#install-a-release).

Setup installs stable Rust; a `rust-toolchain.toml` in the repository makes the first build install the toolchain it names instead. To avoid downloading toolchains on every run, cache `~/.qrust/toolchains` between runs (for example with `actions/cache`, keyed by the toolchain file).

In CI, QuickRust never updates toolchains in the background, and it applies the `incremental.ci` policy instead of `incremental.local-dev`. It recognises CI by `CI` or `GITHUB_ACTIONS`, `GITLAB_CI`, `BUILDKITE`, `JENKINS_URL`, `TF_BUILD` or `TEAMCITY_VERSION`.

## Keep a report of every build

```bash
QRUST_REPORT="$RUNNER_TEMP/qrust/{session}.json" cargo build --locked
```

`{session}` becomes the run's id, so several builds in one job do not overwrite each other; a relative path is relative to the directory the command runs in. Upload the directory as an artifact if you want the reports after the job. The format is described in [Report schema](../reference/report-schema.md); see also [How to export a build report](export-a-report.md).

Writing the report never changes the build's result. If you want a report failure to fail the job, set `measurement.strict = true`.

## Gate on budgets

### 1. Write the budgets

Create `.qrust/budgets.toml` in the repository:

```toml
[policy]
mode = "warn"            # warn: report only; blocking: fail the job (exit status 3)
incomparable = "warn"    # when baseline and candidate cannot be compared: warn, or fail (exit status 4)
noise-percent = 3        # relative changes within +/-3% count as no change

[[budget]]
metric = "wall_time_ns"
scenario = "clean"
max-increase-percent = 10

[[budget]]
metric = "artifact_allocated_bytes"
max-value = "20GiB"
```

Start with `mode = "warn"` and switch to `blocking` once the numbers have proved stable. The metrics and keys are listed in [Budgets](../reference/budgets.md).

### 2. Record a baseline on the CI hardware

Budgets with `max-increase-percent` or `max-decrease-percent` compare against a baseline, which must come from the same kind of machine as the CI runs. Run a benchmark there (for example in a manually triggered job), then promote it:

```bash
qrust benchmark --scenario clean --reps 5 --format json > bench.json
qrust baseline set main --benchmark "$(jq -r .id bench.json)"
```

This writes `.qrust/baselines/main.json`. Commit it. `main` is the name budgets look for unless you set `ci.baseline` or pass `--baseline`.

### 3. Evaluate in every CI run

```bash
qrust benchmark --scenario clean --reps 3 --format json > bench.json
qrust ci evaluate --benchmark "$(jq -r .id bench.json)"
```

```text
Budget result: PASS  (mode blocking, policy /work/app/.qrust/budgets.toml)
  baseline: baseline main   comparability: comparable
  METRIC        SCENARIO  THRESHOLD                 OBSERVED      RESULT  NOTE
  wall_time_ns  clean     max-increase-percent 10%  86ms (+3.3%)  PASS
```

Without `--benchmark`, `qrust ci evaluate` judges the latest build run (`--run` picks another). Budgets with a `scenario` apply only to benchmarks of that scenario.

If the result is `incomparable`, baseline and candidate differ in compiler, host, hardware class, command, profile, targets, scenario or cache state, and relative budgets are not judged. Record a new baseline after you change any of those on purpose (a toolchain update, new runners).

### Gate the ordinary build instead

If you would rather not run a separate benchmark, evaluate the budgets after every build command:

```toml
# .qrust/config.toml
[ci]
enforce-on-build = true
```

A blocking budget that fails then turns the build's successful exit status into 3. The verdict goes to stderr; Cargo's stdout is untouched.

## Related

- [Commands](../reference/commands.md) for `benchmark`, `baseline` and `ci evaluate`, and every exit status
- [How to measure the effect of a build setting](measure-a-change.md)
- [About measuring builds](../explanation/measurement.md) for why baselines must come from the same hardware
