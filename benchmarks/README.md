# Speed index

The number Haul's progress is measured by: how long the same developer loop
takes through Haul compared with upstream Cargo.

```bash
cargo build --release
cargo run --release --example speed_index -- --reps 3 --record
```

`--record` appends one line to [`speed-index.jsonl`](speed-index.jsonl). Record
from a clean checkout of the commit being measured, on an otherwise idle
machine, and commit the new line with the change that moved it.

## What is measured

**Workload.** Haul's own source at commit `b2af135`, with its real registry
dependencies (serde, syn, toml, …; about forty build units). It is pinned so
the index is comparable from one Haul version to the next; Haul itself, the
thing under test, is always the current build.

**Variants.** Each has its own checkouts, target directories and state.

| Variant | What runs |
|---|---|
| `cargo` | upstream Cargo, no Haul on the path |
| `haul` | `haul <command>` with Haul's default configuration |
| `haul+cache` | Haul with `cache.provider = "sccache"`, on an isolated sccache server whose cache starts empty every repetition |

**Scenarios**, run in this order within a repetition, starting with nothing
cached anywhere:

| Scenario | Step |
|---|---|
| `cold-build` | `cargo build` from an empty target directory |
| `no-op` | `cargo build` again, nothing changed |
| `edit-rebuild` | `cargo build` after a one-line edit to a workspace source file |
| `check-after-build` | `cargo check` in the tree that was just built |
| `rebuild-after-clean` | `cargo build` after the target directory was removed |
| `second-checkout` | `cargo build` in a second checkout of the same commit |

Variants are interleaved within each repetition, so drift in machine load
affects all of them alike. The reported time per scenario is the median over
repetitions.

## The index

For each scenario, `speedup = cargo time / variant time`. The index is the
geometric mean of the six speedups: **1.000 is parity with Cargo, above 1 is
faster, below 1 is slower.** The geometric mean keeps one long scenario from
drowning the short ones; the whole-loop ratio (sum of the six medians) is
recorded next to it for the opposite view.

Two indices are tracked:

- **`haul`** — what a user gets by replacing `cargo` and changing nothing else.
  Haul delegates every compilation to Cargo and rustc, so this can only move
  above 1 when Haul removes work on its own; until then it measures Haul's
  overhead and should stay at parity.
- **`haul+cache`** — what Haul delivers today with its cache policy switched
  on. The first build pays for filling the cache; later scenarios show what the
  cache gives back.

## Stability gate

If the samples of any scenario longer than a second differ by more than 25%
(`(max − min) / median`) for any variant, the run is reported as **unstable**
and is not recorded: the machine was busy with something else, and a ratio of
medians taken from such samples is not a measurement.

## Reading it honestly

- It is a ratio on one workload on one machine. The `machine` and `toolchain`
  fields of each record say which; compare records from like machines.
- Differences of a few percent between repetitions are noise on the short
  scenarios (`no-op` is tens of milliseconds). The raw samples are stored with
  every record.
- A change that improves the index by making Cargo's side slower is not an
  improvement; the Cargo times are recorded so that is visible.
