# How to measure the effect of a build setting

This guide shows you how to measure how long your project takes to build in a given situation, and whether a change of build settings (codegen units, debug information, a linker, an environment variable) makes it faster without breaking it.

Both commands build in directories of their own and change no file in your repository, except for the `small-edit` scenario, which edits a source file and restores it.

## Measure the build as it is

```bash
qrust benchmark --scenario clean --reps 5
```

```text
Benchmark 01M4SGKSDHS59VDFVQBMMSVXJC  scenario clean (cold): COMPLETE
  command: cargo build
  PHASE     #  WALL   SAMPLE  RUN
  warmup    1  211ms  valid   01M4SGKSDJ6GQVGSECXMFXEX1D
  measured  1  143ms  valid   01M4SGKSN537TSEAYC2Z4W6EZK
  measured  2  137ms  valid   01M4SGKST98MBZWGN1BVRY7A01
  measured  3  138ms  valid   01M4SGKSZ8SYQ2S5QYFRKNT4WJ
  measured  4  137ms  valid   01M4SGKT47E8GAD5QVT18MBVVC
  measured  5  147ms  valid   01M4SGKT9500QKMBDAPNFG6KN1
  wall time: median 138ms  (min 137ms, max 147ms, stddev 4ms, n=5)
  critical path (median): 101ms   units compiled (median): 2
  samples: 5 valid, 0 discarded
```

Pick the scenario that matches the situation you care about (`qrust benchmark --list` shows them):

| Scenario | Measures |
|---|---|
| `clean` | a full build from an empty build directory |
| `no-change` | a rebuild with nothing changed (the default) |
| `small-edit` | a rebuild after a one-line edit to a workspace source file |
| `test-build` | building all test targets from an empty build directory |
| `release` | an optimised build from an empty build directory |

- To measure another command, put its Cargo arguments after `--`: `qrust benchmark --scenario clean -- check --target wasm32-unknown-unknown`.
- `--reps N` and `--warmups N` set the number of measured and discarded repetitions. More repetitions narrow the noise.
- `--timeout 5m` gives up on a repetition that takes longer (Unix only).
- `--format json` prints the result as JSON, with its `id`, for scripts.

`small-edit` appends a comment to a source file before each repetition. It asks first; pass `--allow-mutation` (or set `benchmark.allow-mutation = true`) to skip the question in scripts, and `--edit src/lib.rs` to choose the file. The file is restored afterwards, also on failure and interruption. If QuickRust was killed outright, the next `qrust benchmark` restores it, or, if the file changed since, stops and tells you where the original is.

Repetitions whose build failed, timed out, saw source files change, or used another toolchain are discarded and counted in the `samples` line.

## Compare a setting against the current build

```bash
qrust experiment codegen --codegen-units 16 --reps 5 --verify test
```

An experiment benchmarks the current settings (the baseline) and the changed ones (the candidate) in the same scenario, `clean` unless you pass `--scenario`, and, with `--verify`, runs a Cargo command with the candidate settings to check the result still works. It ends with a verdict and the two benchmarks side by side:

```text
Experiment `codegen`: VERIFIED
  measured and functionally verified
  candidate settings: profile.dev.codegen-units=16
  applied through `cargo --config` and the environment for these runs only; no file in the repository was changed
  build time: -9.7%  (run-to-run noise +/-7.5%)
  runtime and debugging:
    profile.dev.codegen-units=16: more codegen units parallelise code generation but can reduce generated-code performance; fewer do the opposite
  verification: passed
```

Ways to describe the candidate:

```bash
qrust experiment --debug line-tables-only                     # profile settings: --codegen-units, --debug, --opt-level, --lto
qrust experiment --opt-level 1 -- build --release             # with --release, they change the release profile
qrust experiment linker --linker mold                         # link with another linker (passes -fuse-ld)
qrust experiment --set 'profile.dev.split-debuginfo="unpacked"'   # any Cargo configuration value
qrust experiment --env CARGO_INCREMENTAL=0 --scenario small-edit --allow-mutation
qrust experiment --name lto-thin --lto thin -- build --release
```

`--set` and `--env` can be repeated. `--name` labels the experiment (letters, digits, `-` and `_`).

### Read the verdict

| Verdict | Meaning | What to do |
|---|---|---|
| `VERIFIED` | faster or slower beyond the noise, and `--verify` passed | Adopt the setting if it is faster. |
| `MEASURED-UNVERIFIED` | a difference beyond the noise, no `--verify` given | Run again with `--verify` before you adopt it. |
| `INCONCLUSIVE` | within run-to-run noise, or the two runs are not comparable | Run again with more `--reps`, on a quiet machine. |
| `INVALID` | the candidate did not build, or `--verify` failed | Do not adopt it; the exit status is 1. |

To adopt a setting, put it into your `Cargo.toml` profile or `.cargo/config.toml` yourself; QuickRust never writes it there.

## Compare any two runs or benchmarks

```bash
qrust compare latest~1 latest
qrust compare benchmark:01M4SFS07EDAVZ5DSWCZQM1HZ6 benchmark:01M4SFS0JWGYG8MBA5NM6C8Q4N
qrust compare baseline:main latest
```

The first lines say whether the two are comparable and what differs between their environments; then every metric with its change. A metric one side could not observe reads `unavailable`, never zero.

## Related

- [Benchmarks](../reference/benchmarks.md) for scenarios and result fields
- [How to run QuickRust in CI and gate on build time](set-up-ci.md)
- [About measuring builds](../explanation/measurement.md) for noise and comparability
