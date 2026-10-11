# Conformance to the specification

Status of [spec.md](spec.md) in this implementation. "Tested" names where the
behaviour is asserted.

## P0 checklist

| Item | Status | Where |
|---|---|---|
| `cargo` compatibility shim; `haul` management executable | done | `src/cli.rs`, `src/session.rs` · tests/compat.rs |
| Recursion-safe real-Cargo resolver (COMP-020…022) | done | `src/resolve.rs` · `recursion_is_refused…`, `missing_upstream…` |
| rustup `+toolchain` and directory overrides | done, by delegating to rustup's proxy | `qa003_*` |
| argv / stdin / stdout / stderr / signal forwarding, exact exit status | done | `src/proc.rs` · `qa022`, `qa023`, `stdin_and_exit_codes…` |
| External subcommands, aliases, unknown flags | done | `qa019`, `qa020`, `qa030` |
| Cargo config / environment non-interference | done | `environment_reaches_build_scripts_unchanged`, `no_rebuild_when_alternating…` |
| Nested invocations + opt-out | done via `PATH`; see deviation 1 | `qa019` |
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
| Artifact inventory, reclaimability, GC dry-run (FR-110) | done | `src/inventory.rs` · `inventory_and_gc_plan…`, `gc_plan_is_blocked…` |
| Exact upstream `cargo clean` | done | `qa016` |
| Worktree identification (FR-120) | done | `worktrees_report…` |
| Scenario runner (FR-020); comparison (FR-160); experiments | done | `src/bench.rs`, `src/compare.rs` · `benchmark_*`, `experiment_*` |
| Recommendation engine (FR-150) | done | `src/recommend.rs` unit tests |
| Safe remediation preview (FR-170) | done; `safe` class can also be applied and rolled back | `src/remedy.rs` |
| JSON sidecar schema | done | [report-schema.md](report-schema.md) |
| CI regression budgets (FR-130) | done | `budgets_gate_ci…` |
| Secret / path redaction | done | `secrets_never_reach…`, `qa021` |
| Cross-platform process abstraction | done; Windows type-checked, not run | `src/proc.rs` |
| Compatibility corpus QA-001…030 | done except QA-024 (Windows) | tests/compat.rs |
| Performance-overhead test | done | `dispatch_overhead_stays_within_budget` |
| Migration / uninstall documentation; `doctor`; escape hatch | done | [migration.md](migration.md) · `doctor_install_and_uninstall` |

## Not in this release (P1 and later in the specification)

Controlled GC execution, remote-cache administration and read-only cache mode,
central service and organization policy, PR annotations, container analysis
(FR-140), the plugin protocol (FR-180), nightly `--unit-graph` /
`-Zbuild-analysis` / section-timing adapters, the metadata-embedding
experiment, and network / disk-I/O telemetry. Nightly capabilities are detected
and listed in `toolchain.capabilities`; nothing depends on them.

## Deviations and decisions

1. **`CARGO` is not pointed at the shim (COMP-030).** The specification assumes
   Cargo forwards an inherited `CARGO` to build scripts and subcommands. The
   Cargo this was built against (1.98.1) does so only when its own executable is
   not named `cargo`; a real `cargo` always exports its own path. Setting the
   variable therefore has no effect, and Haul does not set it. Nested
   invocations stay inside Haul when they call `cargo` through a `PATH` that has
   the shim first; they are then linked to their parent session (COMP-031) and
   can be opted out with `HAUL_NESTED=upstream` (COMP-032). Nested builds started
   through `$CARGO` bypass the shim, but their rustc invocations are still
   observed, because `RUSTC_WRAPPER` is inherited.

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

6. **Resolution order follows COMP-020 literally**: rustup is preferred over a
   non-rustup `cargo` that happens to be earlier on `PATH`. Set
   `HAUL_REAL_CARGO` to choose otherwise.

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

## Open decisions of the specification, as resolved here

| | Decision |
|---|---|
| OQ-001 minimum Rust/Cargo | Haul declares `rust-version = 1.80` (built and tested here with 1.98); it delegates to whatever Cargo is selected and needs only `cargo metadata` format 1 and `RUSTC_WRAPPER` |
| OQ-002 platforms | macOS and Linux release-blocking; only macOS has been run so far, Linux and Windows are type-checked |
| OQ-003 central service | none; all contracts are local files |
| OQ-004 cache provider | whatever sccache is configured with |
| OQ-005 fail-open vs fail-closed | `cache.fail`, default `open` |
| OQ-006 remediation trust | only Haul's own configuration is `safe` |
| OQ-007, OQ-008 | measured and reported; no emulation of upstream features |
