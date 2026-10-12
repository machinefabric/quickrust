# Speed index

The number Haul's progress is measured by: how long the same developer loop takes
through Haul compared with upstream Cargo, on real projects.

```bash
cargo build --release --workspace
target/release/haul-bench fetch                                  # once; the only step that touches the network
target/release/haul-bench run --quick --record --note "what changed"   # every iteration, ~2 min
target/release/haul-bench run --full --record                    # checkpoint, ~30 min
```

(`target/` is wherever your Cargo puts build output.) Two tiers:

- **Quick**, every iteration: the `haul` workload; `cargo` vs `haul`; `cold-build no-op
  edit-leaf edit-core rebuild-after-clean second-checkout`; 3 repetitions, no warm-up;
  about two and a half minutes.
  `--record` appends to [`speed-index-quick.jsonl`](speed-index-quick.jsonl) with the `--note`,
  so every iteration leaves a trace.
- **Full**, a checkpoint every ~10 iterations and before a step is called done: all three
  workloads, all variants and scenarios, 5 repetitions plus a warm-up; about half an hour.
  `--record` appends to [`speed-index.jsonl`](speed-index.jsonl), and
  [`PROGRESS.md`](PROGRESS.md) gets one human-readable line.

Every run prints a table and writes its complete samples to `<work-dir>/results/<time>.json`.

## Workloads

Defined in [`workloads.toml`](workloads.toml), each pinned to a commit and its committed
lockfile:

| Workload | Shape |
|---|---|
| `haul` | Haul's first commit `b2af135`: one package, lib + 2 bins, ~40 units. The quick workload. |
| `ripgrep` | ripgrep 15.2.0: a 9-crate workspace, a real CLI |
| `axum-todos` | axum 0.8.9's `todos` example: tokio, axum (built from source), tower-http, serde, tracing |

Every workload defines two edits, both real code changes inside a function body (a
literal changes, numbered per edit): `leaf`, in a crate nothing depends on, and `core`,
in a crate others build on.

## Variants

| Variant | What runs |
|---|---|
| `cargo` | upstream Cargo: the denominator |
| `cargo-tuned` | upstream Cargo with the settings from Cargo's own build-performance guide (`debug = "line-tables-only"`, dependencies `debug = false`), passed as `--config`. What a user gets without Haul by editing configuration. Keeps Haul's index honest. |
| `haul` | Haul, default configuration: **the headline number** |
| `haul-compat` | Haul with `mode = compat` (once that setting exists): pure overhead, must stay ≥ 0.99 |

`--define 'NAME=BASE;env:K=V;config:K=V'` adds an ad-hoc variant (base `cargo` or `haul`)
for trying a lever before it becomes a default, or for measuring one lever on its own
(for example Haul with one setting switched off). Defined variants may be recorded. `--aa` adds a second plain Cargo
(`cargo-aa`): an A/A test whose index must be 1.00 within noise.

## Scenarios

One repetition runs, per variant, the whole loop from nothing built:

| Scenario | Step |
|---|---|
| `cold-build` | `cargo build`: empty target directory and Haul state (the OS page cache stays warm) |
| `no-op` | `cargo build` again, nothing changed; each sample is the mean of 10 runs |
| `edit-leaf` | leaf edit, `cargo build` |
| `edit-core` | core edit, `cargo build` (rebuilds everything above that crate) |
| `check-after-edit` | another leaf edit, `cargo check` |
| `test-build` | `cargo test --no-run` |
| `rebuild-after-clean` | `cargo clean`, then `cargo build` |
| `second-checkout` | `cargo build` in a second, untouched checkout of the same commit |

`--quick` runs the `haul` workload, variants `cargo` and `haul`, scenarios
`cold-build no-op edit-leaf edit-core rebuild-after-clean second-checkout`, 3 repetitions. `--full` runs
everything, 5 repetitions after one untimed warm-up repetition.

## Sealed environment

Each command starts from an empty environment: a pinned toolchain (`--toolchain`, default
`stable`) that Haul installs into the work directory (`<work-dir>/toolchain-home`), whose `bin`
and the system tools are all of `PATH`; plain Cargo is that toolchain's own `cargo`. Cargo
gets a Cargo home of the benchmark's own (`<work-dir>/cargo-home`, no `config.toml`, populated
by `fetch`, used offline), which every Haul home of the run links as its own. Every variant
and checkout has its own `CARGO_TARGET_DIR` and `HAUL_HOME`.
Nothing from the machine's Cargo configuration, such as a global `build.target-dir` or
a rustc wrapper, can reach a measurement. The work directory (default
`/mnt/shared/nvme2p3/haul-bench`, `--work-dir` or `HAUL_BENCH_DIR`) is refused if any parent
has a `.cargo/config.toml`.

Checks that invalidate a run, which is then reported and never recorded:

- before measuring, `cargo metadata` under each variant's environment must report that
  variant's own target directory;
- `haul-compat` and `cargo-aa` must run exactly the units Cargo runs (counted from the
  `Compiling`/`Checking` lines); an edit that compiled nothing is an error;
- more than half of a scenario's samples taken while CPU pressure (`/proc/pressure/cpu`,
  `some avg10`) exceeded 10%;
- samples of a scenario longer than a second spread by more than 25% (`(max − min) / median`).

Variants run interleaved within each repetition, in an order that rotates every
repetition, so drift in machine load and caches affects all of them alike.

## The index

Per scenario, `speedup = cargo median / variant median` of wall time. A workload's index
is the geometric mean over its scenarios; the speed index is the geometric mean over
workloads. **1.000 is parity; above 1 is faster than Cargo.** The same is reported for
CPU time (user + system of the whole process tree), which is much less sensitive to other
load on the machine. Every sample also records peak RSS of the largest process, units run
and the target directory's allocated bytes.

## The correctness gate

Speed counts only if program behaviour stays identical. `haul-bench verify` runs each
workload's own test suite (`cargo test --no-fail-fast`, with the workload's `test-args`
when its build selection has no tests) under upstream Cargo and under
`haul` (plus any `--define` variants), from fresh checkouts, in two rounds: the first from
empty Haul state, the second keeping the artifact cache, so tests also run against binaries
built from restored units. A test whose outcome differs between the rounds of one variant
is reported as flaky, and so is one that differs from upstream Cargo but changes outcome
when run alone four more times on either side; any other difference from upstream Cargo
fails the gate (exit status
1), and so does a suite under which upstream Cargo runs no test at all. `--record` appends the result to [`verify.jsonl`](verify.jsonl). No lever becomes a
default without a clean verify.

## Reading it honestly

- A ratio on three workloads on one machine. Each record names the machine and the
  toolchains; compare like with like.
- `no-op` is tens of milliseconds; a few percent there is process start-up noise.
- `cargo-tuned` shows how much of a win plain configuration would give. Haul's value is
  what it adds beyond that, and doing it without anyone editing configuration.
- A change that "improves" the index by making Cargo's side slower is not an
  improvement; Cargo's times are in every record.
