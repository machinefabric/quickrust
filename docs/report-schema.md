# Run report schema, version 1.0

One JSON document per recorded run, produced by `HAUL_REPORT=<path> cargo …`,
`measurement.report`, or `haul report --format json`.

Rules (API-001 … API-004):

- `schema_version` is always present. Consumers must ignore fields they do not
  know within the same major version; a breaking change increments the major.
- **`null` means unavailable, never zero.** The reason is in the top-level
  `unavailable` object (keyed by field path) or in an `unavailable` object next
  to the field.
- Durations are nanoseconds (`*_ns`), sizes are bytes (`*_bytes`); wall-clock
  instants are RFC 3339 UTC strings (`*_at`) with the raw value in `*_wall_ns`.
- Exported documents carry tokenized paths (`$WORKSPACE`, `$HOME`,
  `$CARGO_HOME`, `$RUSTUP_HOME`).
- A stored report is immutable.

## Top level

| Field | Content |
|---|---|
| `schema_version` | `"1.0"` |
| `run` | identity, command, result |
| `workspace` | members, digests, directories, source state |
| `toolchain` | Cargo and rustc identity, host, capabilities |
| `environment` | `machine` (OS, CPU, memory, filesystem, `class`) and `variables` (allowlisted, redacted) |
| `policy` | mode, the fast settings applied and left alone, workload class, incremental decision, cache state |
| `metrics` | scalar measurements |
| `cache` | sccache statistics for the run, and what Haul's artifact cache did (`artifacts`) |
| `critical_path`, `critical_path_model` | the longest dependency chain and how trustworthy it is |
| `units`, `edges` | the observed build DAG |
| `rebuilds` | why each compiled unit ran |
| `duplication` | multiple versions and build variants |
| `build_scripts`, `proc_macros`, `codegen` | per-concern analyses |
| `artifacts` | build-directory inventory and GC plan, one entry per directory |
| `recommendations` | ranked findings |
| `budget` | policy evaluation, when one ran |
| `unavailable` | field path → reason |

## `run`

`id` (26-character, time-sortable), `parent_session_id`, `state`
(`completed` \| `failed` \| `cancelled`), `started_at`, `started_wall_ns`,
`duration_ns`, `command` { `family`, `entry` (`cargo-shim` \| `haul`),
`argv_redacted`, `cwd_token`, `profile`, `targets` }, `result`
{ `cargo_exit_code`, `signal`, `policy` (`not-evaluated` unless budgets were
enforced on this build) }, `scenario` (benchmarks only),
`upstream` { `path`, `canonical`, `rule` }, `wrappers` { `haul`, `inner`,
`inner_source`, `workspace_wrapper`, `order` }, `overhead`
{ `preflight_ns`, `post_ns` }, `notes`.

## `metrics`

`wall_time_ns`, `critical_path_ns`, `sum_unit_time_ns`,
`build_script_run_time_ns`, `average_parallelism`, `max_concurrent_compilers`,
`cpu_time_ns`, `peak_rss_bytes`, `units_observed`, `units_compiled`,
`units_restored`, `units_failed`, `units_fresh`, `units_fresh_referenced`, `build_scripts_run`,
`probe_invocations`, `build_script_compiler_invocations`,
`build_script_compiler_time_ns`, `output_bytes_written`, `artifact_logical_bytes`,
`artifact_allocated_bytes`.

`units_compiled` counts units rustc compiled; `units_restored` units Haul's artifact
cache restored instead (Cargo asked for them; nothing was compiled). Compile time and
CPU metrics cover compiled units only.
`units_fresh` is `null` unless Cargo's JSON message stream was observed (it is when
Haul rendered the build's output, or the user requested JSON on a pipe).
`units_fresh_referenced` counts artifacts that compiled units linked against
without them being rebuilt. `build_script_compiler_*` covers rustc processes
that build scripts start themselves to probe the toolchain; they are real cost
but not build units, and are told apart by the absence of Cargo's
`-C metadata`.

## `units[]`

`id` (`unit:<crate>-<cargo hash>`), `stem`, `package`, `version`, `source`
(`path` \| `registry` \| `git` \| `unknown`), `crate_name`, `kind` (`lib`, `bin`,
`test`, `example`, `proc-macro`, `build-script`, `build-script-run`, `cdylib`,
…), `mode` (`check` \| `build` \| `test` \| `run`), `target`, `role` (`host` \|
`target` \| `unknown`), `primary`, `features`, `freshness` (`compiled` \|
`restored` \| `failed` \| `fresh` \| `ran`), `start_wall_ns`, `duration_ns`,
`rmeta_ready_ns`, `cpu_ns`, `max_rss_bytes`, `output_bytes`, `incremental`,
`links`, `codegen` (the `-C` options rustc received), `linker`, `cacheability`,
`on_critical_path`, `fingerprint` (component digests: `compiler`, `target`,
`profile`, `features`, `rustflags`, `environment`, `source`, `generated`,
`dependencies`), `exact_id`, `portable_id`, `logical_key`.

`fresh` units have no timing: they were not built in this run. A `restored` unit's
duration is the time restoring it took.

## `edges[]`

`from`, `to` (unit ids, producer → consumer), `kind` (`metadata` \| `link`),
`confidence`, `evidence`.

| Confidence | Meaning |
|---|---|
| `observed` | a `--extern` argument rustc actually received |
| `inferred` | a build-script relation reconstructed from package identity and `OUT_DIR` |
| `modeled` | (critical path only) a linking unit waiting for a transitive dependency |

## `critical_path[]`, `critical_path_model`

Each step: `unit_id`, `package`, `kind`, `freshness`, `duration_ns`,
`edge_confidence`. The model has `completeness` (`observed` \| `partial`),
`partial_reasons`, `method`.

## `rebuilds`

`summary` { `units_compiled`, `previously_known`, `known_units_in_index`,
`invalidated_ratio`, `rebuild_time_of_known_units_ns`, `by_reason` } and
`units[]` { `unit_id`, `package`, `kind`, `mode`, `reason`, `detail`,
`differing`, `evidence`, `confidence`, `cost_ns` }.

`reason` is one of `source`, `dependency`, `compiler`, `profile`,
`feature-set`, `target`, `rustflags`, `build-script-input`, `environment`,
`generated-input`, `cache-miss`, `unknown`, or `new` (first observation; not a
rebuild).

## `cache`

`provider`, `state` (`disabled` \| `local` \| `degraded` \| `conflict`),
`requests`, `cacheable_requests`, `hits`, `misses`, `non_cacheable`,
`non_cacheable_reasons`, `errors`, `timeouts`, `writes`, `read_bytes` (always
`null`: sccache does not report it), `write_bytes`, `lookup_time_ns`,
`write_time_ns`, `units` { `cacheable`, `non_cacheable_linking`,
`non_cacheable_incremental` }, `scope` — all about sccache, `null` with a reason when
none is active.

`artifacts` describes Haul's own artifact cache in this run: `state` (`on`, `off`, or
`off: a rustc wrapper is configured`), `restored`, `stored`, `missed` (looked up and not
found, then compiled without being stored, e.g. a quickly linked workspace binary),
`not_cacheable` (units the cache does not take: those linking native libraries, given
an argument file, or dependency binaries other than build scripts), `restore_time_ns`.

## `policy`

- `fast` { `mode` (`fast` \| `compat`), `applied` [ { `key`, `value`, `why` } ]: the
  profile settings Haul passed to Cargo and the parallel frontend (`rustc -Zthreads`),
  `yielded` [ { `key`, `because` } ]: settings Haul left alone because the user made
  them, naming where }
- `workload` (`local-dev` \| `ci`)
- `incremental`: setting, decision, rationale, observed incremental units, conflict
  with sccache
- `cache`: provider policy and state

## `artifacts[]`

`root`, `files`, `logical_bytes`, `allocated_bytes`,
`hardlink_duplicate_bytes_excluded`, `symlinks_not_followed`, `complete`,
`errors`, `categories` (by `final`, `dependency`, `metadata`, `incremental`,
`debug`, `build-script-binary`, `build-script-output`, `proc-macro`,
`fingerprint`, `documentation`, `package-output`, `unknown-generated`),
`reclaimability` (by `required`, `reusable`, `rebuildable`, `uncertain`),
`active`, `gc_plan` { `mode` (`dry-run`), `stale_days`, `candidate_groups`,
`candidate_bytes`, `reclaimable_now_bytes`, `blocked_by`,
`regeneration_estimate`, `size_budget`, `candidates[]` }.

## `recommendations[]`

`id`, `rule`, `category`, `title`, `status`, `evidence[]`,
`impacted_scenarios[]`, `confidence`, `risk`, `effort`, `impact`
{ `kind` (`measured` \| `modeled` \| `unknown`), `avoidable_time_ns`?,
`reclaimable_bytes`?, `basis` }, `verification`, `compatibility`,
`remediation` (`safe` \| `review_required` \| `none`), `alternatives[]`
(mutually exclusive options), `score`.

A numeric gain is present only when `impact.kind` is `measured` or `modeled`.

## `budget`

`result` (`pass` \| `warn` \| `fail` \| `incomparable` \| `not-evaluated` \|
`error`), `mode`, `baseline`, `comparability` { `state`, `differences[]` },
`evaluations[]` { `metric`, `unit`, `scenario`, `result`, `checks[]` }.

## Other documents

`haul compare --format json`, `haul benchmark --format json`,
`haul experiment --format json`, `haul inventory --format json`,
`haul gc --dry-run --format json`, `haul analyze worktrees --format json` and
`haul fix --dry-run --format json` follow the same conventions and carry
`schema_version` where they are stored.
