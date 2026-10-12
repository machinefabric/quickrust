# Haul

Haul is a drop-in replacement for the `cargo` command. It does not reimplement
Cargo: every invocation is delegated, argument for argument, to the real
upstream Cargo and rustc. Haul owns the process boundary around them and uses
it to observe, explain and police builds.

```text
developer / IDE / CI ──► cargo (Haul shim) ──► upstream Cargo ──► haul-rustc-wrapper ──► [your wrapper / sccache] ──► rustc
                              │                                          │
                              └── session record                         └── one event per rustc invocation
```

What you get on top of an unchanged Cargo:

- **timing** per build unit, the observed dependency DAG and its critical path;
- **why each unit rebuilt** (source, dependency, compiler, profile, features, flags, environment, build script, cache miss — or an honest `unknown`);
- **duplication**: several versions of a dependency, and one version built in several variants, with the dependents responsible;
- **build scripts** and **proc macros**: cost, rerun behaviour, what waits on them;
- **cache** behaviour with sccache: hits, misses and non-cacheable requests as separate numbers;
- **storage**: what is in the build directory by category, and a garbage-collection *plan* (nothing is deleted);
- **benchmarks**, A/B **experiments**, **baselines** and CI **regression budgets**;
- ranked, evidence-backed **recommendations** and reviewable **remediation previews**.

The specification this implements is [docs/spec.md](docs/spec.md); the research
behind it is [docs/report.md](docs/report.md).

## Install

```bash
cargo install --locked --path . # installs `haul` and `haul-rustc-wrapper`; never a binary named `cargo`
haul doctor                     # shows what Haul resolved and whether anything is wrong
```

Requires a Rust toolchain (rustup or a plain Cargo on `PATH`); Haul itself builds with
Rust 1.80 or newer. `--locked` uses the tested dependency versions: without it Cargo
picks the newest, and some of those need a newer Rust than 1.80. sccache and Git are
optional.

## Use

Three ways in, from least to most invasive (see [docs/migration.md](docs/migration.md)):

```bash
haul upstream build             # upstream Cargo, nothing else — the escape hatch, always available
haul build                      # Cargo syntax, through Haul
haul install-shim               # then put the printed directory first on PATH: `cargo build` is now Haul
```

After any build that went through Haul:

```bash
haul analyze                    # everything about the latest build run
haul analyze rebuilds           # one section: timing rebuilds duplication scripts proc-macros codegen cache storage recommendations
haul analyze worktrees          # identical units built in other worktrees / workspaces
haul report --format html -o report.html
haul runs                       # recorded runs; refer to them as latest, latest~1, or by id prefix
haul compare latest~1 latest
```

Measuring and gating:

```bash
haul benchmark --scenario clean --reps 5            # also: no-change, small-edit, test-build, release
haul experiment codegen --codegen-units 16 --scenario small-edit --allow-mutation --verify test -- build
haul baseline set main --benchmark <ID>             # writes .haul/baselines/main.json — commit it
haul ci evaluate                                    # exit 3 when a blocking budget fails
```

Storage and policy:

```bash
haul inventory --caches
haul gc --dry-run --older-than 14 --max-size 20GiB
haul policy explain                                 # every effective setting and where it came from
haul cache status
haul fix --dry-run                                  # remediation previews; nothing is written
```

A machine-readable report for any ordinary build:

```bash
HAUL_REPORT=build-report.json cargo build
```

## Fast by default

The goal: `cargo build` through Haul is faster than plain Cargo, on any project, with
no configuration. In the default `mode = "fast"`, Haul applies build settings that keep
your program's behaviour identical:

| Setting | What changes |
|---|---|
| `profile.dev.debug = "line-tables-only"` | dev builds keep file and line information (backtraces, stepping, breakpoints) but not local-variable and type information, the bulk of the debug data that has to be generated, linked and stored |
| `profile.dev.package."*".debug = false` | no debug information for dependencies in dev builds, as Cargo's own build-performance guide recommends |
| rustc's parallel frontend (`-Zthreads`, up to 8) | units compiled from scratch use several threads: dependencies, release and CI builds, and each workspace crate's first compilation in a target directory (a fresh clone or a `cargo clean`). Applied by Haul's rustc wrapper, with `RUSTC_BOOTSTRAP=1` for the compiler processes so it works on stable, and `-Zallow-features=` so `#![feature]` stays rejected as on stable; incremental rebuilds are faster without it. Build scripts and their compiler probes see the compiler exactly as it is. `fast.frontend-threads = "off"` disables it |
| the wild linker, when installed (`cargo install wild-linker`; Linux with glibc) | relinking after an edit is markedly faster than with rustc's bundled LLD (measured: ripgrep 22%, a small binary 34%). For host builds of binaries, tests and proc-macros, unless you configured a linker. `fast.linker = "off"` disables it |
| a global artifact cache | a unit compiled once is restored instead of compiled again: dependencies across all your checkouts, your own crates within the same checkout (after `cargo clean`, when switching branches back and forth). `cache.artifacts = "off"` disables it |

The profile settings reach Cargo as `--config` arguments; nothing in your repository changes. A setting
you made yourself, in `Cargo.toml`, a `.cargo/config.toml`, a `CARGO_PROFILE_*` variable or
`--config`, always wins: Haul then leaves it, and the settings that depend on it, alone.
Want full debug info back? Set `[profile.dev] debug = true` as you would with Cargo.
Every run records what Haul applied, what it left alone and why (`policy.fast` in the
report).

`mode = "compat"` (or `HAUL_MODE=compat`) builds exactly what upstream Cargo builds.
`haul upstream` is upstream Cargo with no Haul at all.

How much faster, measured on real projects against plain Cargo:
[benchmarks/PROGRESS.md](benchmarks/PROGRESS.md) (method: [benchmarks/README.md](benchmarks/README.md)).

## Build output

In a terminal, Haul shows a build the way you need it: one live status line instead of a
`Compiling` line per crate, every error in full as it arrives (rustc's own rendering),
warnings counted by lint and crate instead of dumped, and a one-line summary:

```text
✓ Built 4.21s · 12 compiled · 140 fresh · 37 restored from Haul's cache
⚠ 23 warnings in app, util: unused_variables ×17, dead_code ×4, +2 more · `haul warnings` to show them
```

Nothing is lost. Every line is stored with the run:

```bash
haul warnings                       # all warnings of the latest build, as rustc wrote them
haul warnings --crate app --lint dead_code
haul errors                         # the errors again
haul log [--raw]                    # everything, in order
```

Lines Haul does not recognise (build script failures, `cargo:warning=…`, resolution errors)
are shown as Cargo printed them. Pipes, IDEs, CI logs, `-v`, `-q`, `--message-format`,
`cargo run` and `cargo test` (other than `--no-run`) get Cargo's output untouched.
`output = "cargo"` (or `HAUL_OUTPUT=cargo`) turns it off; compat mode leaves the terminal
to Cargo unless you set `output = "haul"`.

## What "drop-in" means here

- Arguments, stdin, stdout, stderr, the environment, the exit status and
  terminating signals pass through unchanged. `--message-format=json` output is
  byte-for-byte what the delegated Cargo prints.
- In fast mode, the build settings above differ from Cargo's defaults and nothing
  else does; in compat mode the build is Cargo's own.
- Unknown flags and commands are forwarded; Cargo decides whether they are valid.
- `cargo +toolchain`, `rust-toolchain.toml` and every other rustup rule are
  honoured by delegating to rustup's own proxy.
- An existing `RUSTC_WRAPPER` (from the environment or `.cargo/config.toml`) and
  `RUSTC_WORKSPACE_WRAPPER` keep working; Haul's wrapper runs outside them.
- `cargo clean` is upstream `cargo clean`. Haul never deletes build output.
- Nothing is written into your repository unless you ask (`HAUL_REPORT`,
  `haul baseline set`, `haul fix --apply`). State lives in `~/.haul`.
- A build never fails because Haul could not record or analyse it.

The test suite checks these against upstream Cargo as the oracle
(`tests/compat.rs`, the QA corpus of the specification).

## Configuration

Haul has its own namespace and never interprets Cargo's configuration for
execution:

```text
built-in defaults  <  ~/.haul/config.toml  <  <repo>/.haul/config.toml  <  HAUL_* environment
```

```toml
# .haul/config.toml
[cache]
provider = "sccache"     # inherit (default) | none | sccache
fail = "open"            # open: compile locally if the cache is unavailable | closed

[incremental]
local-dev = "cargo-default"
ci = "off"               # recorded with its rationale in every run

[measurement]
report = ".haul/runs/{session}.json"
```

Unknown keys, values and `HAUL_*` variables are errors that name the field and
its source. `haul policy explain` lists every key with its environment variable.

## Documentation

- [docs/migration.md](docs/migration.md) — adopting Haul in stages, uninstalling, rollback
- [docs/operations.md](docs/operations.md) — CI, budgets, sccache, exit statuses, platform notes
- [docs/report-schema.md](docs/report-schema.md) — the versioned JSON report
- [docs/security.md](docs/security.md) — trust model, secrets, state on disk
- [docs/conformance.md](docs/conformance.md) — what of the specification is implemented, and where reality forced a deviation

## Tracking

Work is tracked on the private [project board](https://github.com/users/jowharshamshiri/projects/6):
delivered work, findings and decisions as cards carrying their context, and the
remaining work by release (P0, P1, P2, Future).

## Development

```bash
cargo test                      # unit tests + compatibility corpus + management-surface suite (real builds, offline)
cargo clippy --all-targets
```
