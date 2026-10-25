# Benchmarks

The scenarios of `qrust benchmark`, the options and verdicts of `qrust experiment`, how runs are taken and discarded, and where results are stored.

Guide: [How to measure the effect of a build setting](../how-to/measure-a-change.md).

## Scenarios

`qrust benchmark --list` prints them:

```text
clean       cold  cargo build            full build from an empty build directory
no-change   warm  cargo build            rebuild with nothing changed (build-system overhead)
small-edit  warm  cargo build            rebuild after a one-line edit to a workspace source file
test-build  cold  cargo test --no-run    build all test targets from an empty build directory
release     cold  cargo build --release  optimized build from an empty build directory
```

| Cache state | Each repetition starts from |
|---|---|
| `cold` | an empty build directory and an empty artifact cache |
| `warm` | the tree the previous run left; a priming build (never a sample) runs first |

Cargo arguments after `--` replace the scenario's default command (`-- check --target wasm32-unknown-unknown`). The command must be a build command (`build`, `check`, `test`, `bench`, `run`, `doc`, `rustc`, `clippy`); `--target-dir` is refused.

## `qrust benchmark`

| Option | Default | Meaning |
|---|---|---|
| `--scenario NAME` | `no-change` | the scenario |
| `--reps N` | `3` | measured repetitions (at least 1) |
| `--warmups N` | `1` | unmeasured repetitions before them |
| `--edit FILE` | the first workspace member's library root, else its binary root | the file `small-edit` edits; it must be a `.rs` file inside the workspace |
| `--allow-mutation` | off (`benchmark.allow-mutation`) | let `small-edit` edit the file without asking |
| `--timeout DURATION` | none | stop a run after `DURATION` (`90s`, `5m`); Unix only |
| `--format text\|json` | `text` | the result as a table or as JSON |

Progress goes to stderr, one indented line per run: `     Benchmark clean: measured 2/3 [41.2s]`. The exit status is 0 when the benchmark completed and 1 otherwise.

### Build directories

Each benchmark builds in `<target>/qrust-bench/<scenario>-<label>/` inside the workspace's target directory (`<label>` is `default`, or `baseline` and `candidate-<name>` in an experiment), with an artifact cache of its own in `.qrust-artifacts/` there. Cold scenarios empty both before every repetition, so a clean build pays for storing into the cache and never restores from it, and an experiment's baseline and candidate never share results. QuickRust deletes such a directory only when it holds its `.qrust-owned` marker.

### Source edits

`small-edit` appends one comment line, `// qrust-benchmark-edit <n>`, to the file before each repetition after the priming build.

- Without `--allow-mutation` or `benchmark.allow-mutation = true`, QuickRust asks on a terminal (`NOTE the small-edit scenario will temporarily append a comment to <file> and restore it afterwards. Proceed? [y/N]`) and refuses without one.
- Before the first edit, the original bytes and a journal are written to `~/.qrust/workspaces/<id>/restore/`.
- The file is restored byte for byte on normal exit, on failure and on interruption; a restore that fails prints `FAIL SOURCE FILE NOT RESTORED: ...` with the path of the saved original.
- If QuickRust was killed, the next `qrust benchmark` restores the file, but only when it is exactly the original followed by QuickRust's marker lines; otherwise it stops and names the original. `qrust doctor` reports a journal left behind.

### Runs that are discarded

| Run | Effect |
|---|---|
| the build failed, was terminated, timed out or was interrupted | the run is invalid |
| a source file changed during the run (other than `Cargo.lock`) | the run is invalid |
| the run could not be analysed | the run is invalid |
| a priming or warm-up run is invalid | the benchmark stops with state `invalid` |
| a measured run used a different toolchain or machine class than the others | excluded from the sample group, with a `WARN` |

The result counts `samples_valid` and `samples_discarded`. A benchmark with no valid measured run is `invalid`.

### Result

| Field | Content |
|---|---|
| `id` | the benchmark id (`--benchmark ID`, `benchmark:ID`) |
| `state` | `complete`, `invalid` or `interrupted` |
| `failure` | why it is not complete |
| `scenario` | name, cache state, label, `--config` overlays and environment variable names |
| `command` | the Cargo command, redacted |
| `build_directory` | where it built |
| `requested` | repetitions, warm-ups, timeout |
| `runs[]` | each run: run id, phase (`prime`, `warmup`, `measured`), iteration, wall time, exit, whether valid and why not |
| `samples_valid`, `samples_discarded`, `excluded_heterogeneous` | the sample group |
| `mutation` | the edited file and whether it was restored |
| `summary` | the metrics of the group, with distributions |

Results are stored in `~/.qrust/workspaces/<id>/benchmarks/<id>.json`.

## `qrust experiment`

An experiment benchmarks the same scenario twice, as baseline and as candidate with the candidate settings, and optionally checks that the candidate still works.

| Option | Meaning |
|---|---|
| `codegen`, `linker` | names the experiment (otherwise `candidate`); no other effect |
| `--name NAME` | the experiment's name: letters, digits, `-`, `_` |
| `--set KEY=VALUE` | a Cargo `--config` assignment for the candidate (TOML `key = value`); repeatable |
| `--env KEY=VALUE` | an environment variable for the candidate; repeatable |
| `--codegen-units N` | `profile.<p>.codegen-units = N` |
| `--debug VALUE` | `profile.<p>.debug = VALUE` |
| `--opt-level VALUE` | `profile.<p>.opt-level = VALUE` |
| `--lto VALUE` | `profile.<p>.lto = VALUE` |
| `--linker NAME` | `target.<host>.rustflags = ["-C", "link-arg=-fuse-ld=NAME"]` |
| `--verify '<cargo args>'` | a Cargo command run with the candidate settings after the candidate's benchmark (`--verify 'test'`) |
| `--scenario NAME` | default `clean` |
| `--reps N`, `--warmups N`, `--edit FILE`, `--allow-mutation`, `--timeout DURATION`, `-- <cargo args>` | as for `qrust benchmark` |

`<p>` is `release` when the Cargo arguments after `--` contain `--release` or `-r`, and `dev` otherwise. Numbers and `true`/`false` are written bare, other values as TOML strings. At least one candidate setting is required. Settings are applied through `--config` and the environment for the experiment's runs only; no file is changed.

### Verdicts

| Status | When |
|---|---|
| `invalid` | the candidate did not build, or the verification command failed (exit status 1) |
| `inconclusive` | baseline and candidate are incomparable, or the wall-time difference is within the run-to-run noise (the larger relative standard deviation of the two, at least 1%) |
| `verified` | measured outside the noise, and the verification command passed |
| `measured-unverified` | measured outside the noise, with no verification command |

The result also lists each setting's known effect on the built program (`dimensions.runtime_and_debugging`), the change in output size, and, for `verified` and `measured-unverified`, the settings as a proposal for review. Profile, linker and flag changes are never applied automatically. Results are stored with the benchmarks.

## qrust-bench

`qrust-bench` (in `bench/`) is a separate harness that measures QuickRust against upstream Cargo on pinned real projects and computes the speed index. Its workloads, scenarios, tiers and records are described in [benchmarks/README.md](../../benchmarks/README.md); results are in [benchmarks/PROGRESS.md](../../benchmarks/PROGRESS.md).
