# Conformance to the specification

Status of [spec.md](spec.md) in this implementation. "Tested" names where the
behaviour is asserted.

## P0 checklist

| Item | Status | Where |
|---|---|---|
| `cargo` compatibility shim; `haul` management executable | done | `src/cli.rs`, `src/session.rs` · tests/compat.rs |
| Recursion-safe engine (COMP-021, COMP-022) | done: the toolchain's own Cargo; a linked toolchain whose `cargo` is Haul is refused | `src/engine.rs` · `recursion_is_refused…`, `an_unreachable_dist_server…` |
| Toolchains: `+toolchain`, `RUSTUP_TOOLCHAIN`, toolchain files, default (COMP-012, COMP-020) | done: Haul installs and selects toolchains itself (deviation 6) | `src/toolchain/` · `qa003_*`, `toolchain_programs_run_through_hauls_proxies`, `builds_and_their_build_scripts_get_hauls_toolchain…`, `environment_reaches_build_scripts_unchanged` |
| argv / stdin / stdout / stderr / signal forwarding, exact exit status | done | `src/proc.rs` · `qa022`, `qa023`, `stdin_and_exit_codes…` |
| External subcommands, aliases, unknown flags | done | `qa019`, `qa020`, `qa030` |
| Cargo config / environment non-interference | done | `environment_reaches_build_scripts_unchanged`, `no_rebuild_when_alternating…` |
| Nested invocations + opt-out | done, through `PATH` and `$CARGO`; see deviation 1 | `qa019`, `builds_and_their_build_scripts_get_hauls_toolchain…` |
| Workspace, toolchain, environment fingerprints (FR-010) | done | `src/discover.rs` · `qa002`, `report_follows…` |
| Cargo JSON-event collector | done, when the user requests JSON on a pipe | `qa018` |
| rustc wrapper collector; existing wrapper composition | done | `src/wrapper.rs` · `qa010_qa011_qa012`, `wrapper_configured_in_cargo_config…` |
| Observed build-unit DAG; critical path (FR-030) | done | `src/analyze.rs` unit tests |
| Rebuild / invalidation classifier (FR-040) | done | `src/waste.rs` unit tests · `rebuild_causes…`, `toolchain_change…` |
| Version duplication; feature/profile/target variants (FR-050) | done | `src/waste.rs` unit tests |
| Build-script analysis (FR-060); proc-macro identification (FR-070) | done | `qa007_qa008_qa009`, `build_script_without_rerun…` |
| Linker / codegen visibility (FR-080) | done | `haul analyze codegen` |
| Incremental policy evaluator (FR-090) | done | `sccache_policy…` |
| sccache integration and reporting (FR-100) | done | `src/cache.rs` · `sccache_policy…`, `qa026` |
| Artifact inventory, reclaimability, GC (FR-110): `haul gc` keeps the cache within its bounds, `--dry-run` previews | done | `src/inventory.rs`, `src/space.rs` · `inventory_and_its_stale_plan_are_read_only…`, `the_stale_plan_is_blocked_while_a_build_is_running`, `hauls_cache_removes_what_is_gone_then_the_least_recently_used…` |
| Exact upstream `cargo clean` | done | `qa016` |
| Worktree identification (FR-120) | done | `worktrees_report…` |
| Scenario runner (FR-020); comparison (FR-160); experiments | done | `src/bench.rs`, `src/compare.rs` · `benchmark_*`, `experiment_*` |
| Recommendation engine (FR-150) | done | `src/recommend.rs` unit tests |
| Safe remediation preview (FR-170) | done; `safe` class can also be applied and rolled back | `src/remedy.rs` |
| JSON sidecar schema | done | [report-schema.md](report-schema.md) |
| CI regression budgets (FR-130) | done | `budgets_gate_ci…` |
| Secret / path redaction | done | `secrets_never_reach…`, `qa021` |
| Cross-platform process abstraction | done; on Windows a CI smoke run (setup, build, run, test) | `src/proc.rs` · `.github/workflows/ci.yml` |
| Compatibility corpus QA-001…030 | done except QA-024 (Windows) | tests/compat.rs |
| Performance-overhead test | done | `dispatch_overhead_stays_within_budget` |
| Migration / uninstall documentation; `doctor`; escape hatch | done | [migration.md](migration.md) · `doctor_setup_and_uninstall` |

## Not in this release (P1 and later in the specification)

Controlled GC execution, remote-cache administration and read-only cache mode,
central service and organization policy, PR annotations, container analysis
(FR-140), the plugin protocol (FR-180), nightly `--unit-graph` /
`-Zbuild-analysis` / section-timing adapters, the metadata-embedding
experiment, and network / disk-I/O telemetry. Nightly capabilities are detected
and listed in `toolchain.capabilities`; nothing depends on them.

## Deviations and decisions

1. **`CARGO` is Haul's `cargo` (COMP-030), by running Cargo under another name.**
   Cargo hands everything it starts (external subcommands, build scripts,
   `env!("CARGO")`, test binaries) its own path, and takes an inherited `CARGO`
   instead only when its own executable is not named `cargo` (Cargo 1.86 and
   later), or, after that, the path in its `argv[0]` when that is named `cargo`.
   Haul therefore runs a hard link of the toolchain's `cargo`, `bin/.haul-engine`,
   with `argv[0]` and `CARGO` set to `~/.haul/bin/cargo` (Windows has no separate
   `argv[0]`: there Cargo's help names the link, `.haul-engine.exe`): a nested build
   through `$CARGO` (Clippy's, nextest's, `cargo llvm-cov`'s) is a Haul session
   linked to its parent (COMP-031), with Haul's settings, like one through
   `PATH`. Either can be opted out with `HAUL_NESTED=upstream` (COMP-032).
   `haul upstream` runs the toolchain's `cargo` as itself, and a linked toolchain
   is never changed, so there nested builds through `$CARGO` reach that Cargo
   directly; their rustc invocations are still observed, because `RUSTC_WRAPPER`
   is inherited.

2. **Run state lives in `~/.haul`, not in the repository.** The specification's
   configuration example writes reports to `.haul/runs/`. Creating files in a
   repository on every `cargo build` is not something a drop-in replacement
   should do, so recording goes to `HAUL_HOME` and a repository report path is
   opt-in (`measurement.report`, `HAUL_REPORT`).

3. **Analysis is lazy by default.** To keep dispatch overhead inside
   NFR-PERF-001/003, the build path only records; reports are produced on
   demand or synchronously when a report or budget enforcement was requested.
   A run analysed after later runs keeps everything recorded at build time
   (units, timings, revision) and reports workspace metadata, dirtiness and the
   artifact footprint as unavailable rather than describing today's tree.

4. **Fresh units need Cargo's JSON stream.** On a stable toolchain, nothing
   else says which units Cargo considered fresh. Haul observes the stream only
   when the user already asked for it and stdout is not a terminal, so terminal
   behaviour is never altered; otherwise `units_fresh` is `null` with a reason.

5. **Build-script run time is taken from files Cargo writes around the run**
   (`invoked.timestamp`, then `stderr` / `root-output`). Cargo back-dates
   `output` to the invocation time, so it cannot be used.

6. **Haul manages the toolchains (decision of 2026-10-12), not rustup (COMP-020).** The
   specification resolves an upstream Cargo through `PATH` and rustup. Haul instead
   installs toolchains from the Rust dist server into `~/.haul/toolchains` and selects
   them in rustup's documented order (`+toolchain`, `RUSTUP_TOOLCHAIN`, toolchain file,
   default; rustup's per-directory overrides have no equivalent). Every command runs that
   toolchain's own Cargo with Haul's Cargo home (`~/.haul/cargo`). `HAUL_REAL_CARGO` and
   `compatibility.real-cargo` no longer exist: a toolchain built elsewhere is used with
   `haul toolchain link`.

7. **Configuration errors apply to every command.** An invalid
   `.haul/config.toml` stops `cargo --version` as well as `cargo build`, with the
   field and file named; `haul upstream` is unaffected.

8. **No `cache.mode` or `artifacts.gc` settings.** The specification's example
   configuration lists them, but with remote-cache administration and GC
   execution out of this release each would have exactly one legal value. A
   read-only cache mode that a running sccache server may not honour would also
   be a false assurance. The keys are rejected as unknown rather than accepted
   and ignored.

9. **Portable unit identity includes source content.** Cargo hashes workspace
   members by their workspace-relative path, so two worktrees share unit hashes
   even on different branches. Cross-worktree duplicates are counted only when
   the inputs are identical too.

10. **JSON message order is not part of the oracle.** Upstream Cargo emits
    independent units in a different order from run to run, so the
    compatibility test compares the set of messages byte for byte, not their
    sequence.

11. **Fast by default (spec revision of 2026-10-11).** In the default `mode =
    "fast"`, Haul changes what upstream Cargo would build in ways that keep program
    behaviour identical: debug-information settings, rustc's parallel frontend for
    units compiled from scratch, a global artifact cache, and its own terminal output. `mode = "compat"` builds exactly upstream's builds; the
    compatibility corpus's byte-for-byte oracle tests run in compat mode, and fast
    mode has tests of its own (settings yield to the user, the parallel frontend
    never reaches an incremental rebuild, `#![feature]` stays rejected on a stable
    compiler, the cache restores correctly and never
    serves an edited source, fast mode and upstream keep separate artifacts instead
    of rebuilding each other). Every lever is measured by the speed index and checked
    against the benchmark workloads' own test suites before it becomes a default.

12. **`RUSTC_BOOTSTRAP` for `-Z` levers.** The stable compiler contains the parallel
    frontend and the linker selection Haul uses behind `-Z`. Haul's wrapper sets
    `RUSTC_BOOTSTRAP=1` for the compiler processes of the units Cargo schedules, with
    `-Zallow-features=` so that `#![feature]` is rejected as on stable (with rustc's
    "not in the list of allowed features" error instead of the stable-channel one).
    Never for build scripts or the compiler probes they run, so crates do not switch to
    nightly code paths; not on a nightly compiler; and no such lever applies when the
    user sets `RUSTC_BOOTSTRAP` themselves.

13. **Haul's compiler driver compiles the units Cargo schedules (fast mode, `fast.early-cutoff`).**
    The driver is the toolchain's own rustc (`librustc_driver`, its LLVM, its release) run
    through Haul's callbacks, built once per toolchain against its `rustc-dev`
    component. With no callbacks active its outputs are rustc's byte for byte (test
    `hauls_compiler_driver_builds_byte_for_byte_what_rustc_builds`). In fast mode three
    things differ from upstream's builds, none of them in program behaviour:
    a library's crate hash (SVH) is computed from what its dependents can observe, so
    libraries' metadata and rlibs differ from rustc's (the hash is an identifier; nothing
    compares it across compilers); outputs are linked before rustc saves its incremental
    state, which it finishes after Cargo has moved on; and binaries' object files are kept
    (hard links next to the output) so that a binary whose own code is unchanged is linked
    again instead of compiled. Restored and relinked units are checked against building the
    same source from nothing by `haul-bench cutoff` (random edits on the benchmark
    workloads, byte for byte; recorded in `benchmarks/cutoff-gate.jsonl`). Programs Cargo
    runs in rustc's place (clippy-driver as a workspace wrapper, a user's wrapper) run as
    they are; a crate the driver fails on is compiled by rustc.

14. **No `target/` in the project (decision of 2026-10-12).** Upstream Cargo builds into
    `<workspace>/target` unless told otherwise; Haul tells it otherwise for every command:
    each workspace (as `cargo locate-project --workspace` names it) builds into
    `~/.haul/cache/builds/<name>-<id>/`, given to Cargo and every tool under it through
    `CARGO_TARGET_DIR` and `CARGO_BUILD_BUILD_DIR`, so `cargo metadata` reports it too. A
    target directory the user chose (`--target-dir`, `--config build.target-dir`,
    `CARGO_TARGET_DIR`/`CARGO_BUILD_TARGET_DIR`) is used as it is; `build.target-dir` in a
    Cargo configuration file is overridden. Scripts that hard-code `./target/…` must ask
    `haul path` instead. Tests: `builds_go_to_hauls_cache_and_never_into_the_project`,
    `a_target_directory_the_user_chose_is_used…`, `a_build_script_building_another_workspace…`.

## Open decisions of the specification, as resolved here

| | Decision |
|---|---|
| OQ-001 minimum Rust/Cargo | Haul declares `rust-version = 1.80`, built and tested with exactly 1.80 in CI from the lockfile (`cargo install --locked`); it delegates to whatever Cargo is selected and needs only `cargo metadata` format 1 and `RUSTC_WRAPPER` |
| OQ-002 platforms | macOS and Linux release-blocking; the suites pass on macOS and Linux x86_64 in CI; Windows has a CI smoke run, the suites are Unix-only |
| OQ-003 central service | none; all contracts are local files |
| OQ-004 cache provider | whatever sccache is configured with |
| OQ-005 fail-open vs fail-closed | `cache.fail`, default `open` |
| OQ-006 remediation trust | only Haul's own configuration is `safe` |
| OQ-007, OQ-008 | measured and reported; no emulation of upstream features |
