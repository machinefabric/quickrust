# Configuration

QuickRust's own settings, where they are read from, every key with its values and default, and how a command's toolchain is selected.

QuickRust never reads Cargo's configuration (`.cargo/config.toml`) for its own settings, and Cargo never reads QuickRust's. `qrust policy explain` prints every key with its effective value, environment variable and origin; `--format json` prints the same as JSON.

## Sources and precedence

Later sources override earlier ones, key by key:

| Order | Source |
|---|---|
| 1 | built-in defaults |
| 2 | `~/.qrust/config.toml` (`$QRUST_HOME/config.toml`): the user's |
| 3 | `.qrust/config.toml` in the directory the command runs in or the nearest directory above it: the repository's |
| 4 | the key's `QRUST_*` environment variable |

An empty environment variable counts as unset. A configuration file that does not exist counts as empty.

## File format

TOML. `output` and `mode` are top-level keys; every other key is `name` inside the table named by its first part:

```toml
mode = "fast"

[cache]
provider = "sccache"
max-size = "40GiB"

[measurement]
report = ".qrust/runs/{session}.json"
```

## Validation

Invalid configuration stops every command except `qrust upstream`, with exit status 2 and a message that names the key and its source:

- a key that does not exist (`unknown configuration key `cache.providr` in <file>`);
- a value outside the key's type or allowed values (`invalid value "maybe" for `cache.fail` from <file>: expected one of open, closed`);
- a value of the wrong TOML type;
- invalid TOML;
- any environment variable whose name starts with `QRUST_` and is not a key's variable or one of QuickRust's internal variables ([Environment](environment.md#internal-variables)): `unknown configuration variable `QRUST_...``.

## Value types

| Type | Accepted values |
|---|---|
| enum | exactly one of the listed strings |
| bool | `true`, `false`; from a string or the environment also `1`/`0`, `on`/`off`, `yes`/`no` (any case) |
| integer | a non-negative integer |
| string | any string |
| space | `auto`; a size (`40GiB`, `500MB`, `1024`; units `B`, `K`/`KB`/`KiB`, `M`/`MB`/`MiB`, `G`/`GB`/`GiB`, `T`/`TB`/`TiB`, all powers of 1024; more than zero); or a share of the disk (`10%`, more than 0 and at most 100) |
| threads | `auto`, `off`, or a number of at least 2 |

## Keys

| Key | Type and values | Default | Environment variable | Meaning |
|---|---|---|---|---|
| `mode` | `fast`, `compat` | `fast` | `QRUST_MODE` | `fast`: the settings of [Fast mode](fast-mode.md) apply; `compat`: exactly the toolchain's Cargo builds |
| `output` | `qrust`, `cargo` | `qrust` | `QRUST_OUTPUT` | who presents a command's output in a terminal ([Output](output.md)). In compat mode the default is `cargo`; an `output` set explicitly applies in either mode |
| `compatibility.nested-cargo` | `orchestrator`, `upstream` | `orchestrator` | `QRUST_NESTED` | a Cargo started inside a QuickRust session (a build script's or a subcommand's `cargo` or `$CARGO`): a QuickRust session of its own linked to the outer one, or the toolchain's Cargo directly |
| `compatibility.unknown-command` | `delegate`, `instrument` | `delegate` | `QRUST_COMPATIBILITY_UNKNOWN_COMMAND` | external subcommands: delegated, or delegated with the rustc wrapper observing their compilations |
| `measurement.enabled` | bool | `true` | `QRUST_MEASUREMENT_ENABLED` | observe rustc invocations through `qrust-rustc-wrapper`; `false` also turns off everything the wrapper applies (artifact cache, parallel frontend, linker, early cut-off) |
| `measurement.report` | string | empty | `QRUST_REPORT` | write each build's report to this path (relative to the current directory); `{session}` expands to the run id |
| `measurement.finalize` | `lazy`, `sync` | `lazy` | `QRUST_MEASUREMENT_FINALIZE` | analyse a run when it is first asked for, or right after each build |
| `measurement.cargo-json` | `auto`, `off` | `auto` | `QRUST_MEASUREMENT_CARGO_JSON` | read Cargo's JSON messages when the user requested them on a stdout that is not a terminal |
| `measurement.strict` | bool | `false` | `QRUST_MEASUREMENT_STRICT` | a run that could not be recorded or reported makes an otherwise successful command exit 2 |
| `measurement.keep-runs` | integer | `200` | `QRUST_MEASUREMENT_KEEP_RUNS` | recorded runs kept per workspace |
| `measurement.keep-raw-events` | integer | `20` | `QRUST_MEASUREMENT_KEEP_RAW_EVENTS` | analysed runs per workspace whose raw rustc events and stored output are kept |
| `incremental.local-dev` | `cargo-default`, `on`, `off`, `experiment` | `cargo-default` | `QRUST_INCREMENTAL_LOCAL_DEV` | incremental compilation outside CI: Cargo's profiles decide, `CARGO_INCREMENTAL=1`, `CARGO_INCREMENTAL=0`, or Cargo's profiles decide (with a pointer to `qrust experiment`) |
| `incremental.ci` | same | `cargo-default` | `QRUST_INCREMENTAL_CI` | the same in CI |
| `cache.provider` | `inherit`, `none`, `sccache` | `inherit` | `QRUST_CACHE_PROVIDER` | compiler cache: observe the configured `RUSTC_WRAPPER`, add none, or chain `sccache` (from `PATH`) inside QuickRust's wrapper when no wrapper is configured |
| `cache.fail` | `open`, `closed` | `open` | `QRUST_CACHE_FAIL` | when a chained sccache is unavailable: compile without it, or fail before compiling |
| `cache.artifacts` | `on`, `off` | `on` | `QRUST_CACHE_ARTIFACTS` | fast mode: QuickRust's artifact cache |
| `cache.artifacts-min-link-ms` | integer (milliseconds) | `300` | `QRUST_CACHE_ARTIFACTS_MIN_LINK_MS` | linked workspace outputs (binaries, tests, proc-macros) that compiled faster than this are not stored in the artifact cache |
| `build.variant` | string | empty | `QRUST_BUILD_VARIANT` | a name for this configuration of the workspace: it builds into a directory of its own, `builds/<name>-<id>.<variant>`, so that configurations of one workspace keep their output apart ([How to find build outputs](../how-to/find-build-outputs.md#build-one-workspace-in-several-configurations)). At most 32 letters, digits, `-` or `_`; empty is the workspace's one build directory. No effect on a target directory you chose |
| `cache.max-size` | space | `auto` | `QRUST_CACHE_MAX_SIZE` | the most QuickRust's cache uses; `auto` is 10% of the disk, between 10 GiB and 100 GiB |
| `cache.min-free` | space | `auto` | `QRUST_CACHE_MIN_FREE` | the free disk space QuickRust's cache leaves; `auto` is 5% of the disk, between 2 GiB and 20 GiB |
| `fast.frontend-threads` | threads | `auto` | `QRUST_FAST_FRONTEND_THREADS` | fast mode: rustc's parallel frontend; `auto` is the number of cores, at most 8, on machines with 4 or more cores, and off below 4 |
| `fast.early-cutoff` | `auto`, `off` | `auto` | `QRUST_EARLY_CUTOFF` | fast mode with the artifact cache: compile with QuickRust's compiler driver |
| `fast.linker` | `auto`, `off` | `auto` | `QRUST_FAST_LINKER` | fast mode on a Linux glibc host: link with `wild` when it is installed |
| `artifacts.inventory` | bool | `true` | `QRUST_ARTIFACTS_INVENTORY` | include a build-directory inventory when a run is analysed |
| `artifacts.stale-days` | integer (days) | `30` | `QRUST_ARTIFACTS_STALE_DAYS` | age after which unused artifacts count as stale in `qrust inventory` |
| `ci.budget-policy` | string | `.qrust/budgets.toml` | `QRUST_CI_BUDGET_POLICY` | the budget file, relative to the workspace root ([Budgets](budgets.md)) |
| `ci.baseline` | string | `main` | `QRUST_CI_BASELINE` | the baseline budgets are evaluated against |
| `ci.enforce-on-build` | bool | `false` | `QRUST_CI_ENFORCE_ON_BUILD` | evaluate budgets after every successful build command; a blocking failure exits 3 |
| `self.update-url` | string | empty | `QRUST_UPDATE_URL` | a mirror of the release registry `qrust self update` reads QuickRust's releases from: it serves `<url>/quickrust/manifest` and its `.sig`, signed as the registry's are. Empty: the registry this QuickRust was published in (`qrust doctor`, `release line`) |
| `toolchain.auto-update` | `background`, `off` | `background` | `QRUST_TOOLCHAIN_AUTO_UPDATE` | once a day after a build, check in the background whether the channel the build used has a newer release, and install it for the next build; never in CI |
| `toolchain.dist-server` | string | `https://static.rust-lang.org` | `QRUST_DIST_SERVER` | where Rust releases are downloaded from |
| `security.persist-environment` | `allowlist`, `none` | `allowlist` | `QRUST_SECURITY_PERSIST_ENVIRONMENT` | which environment values run records keep: build-relevant variables only, or none |
| `security.redact-path-home` | bool | `true` | `QRUST_SECURITY_REDACT_PATH_HOME` | replace the workspace root and the home, Cargo home and QuickRust home directories with `$WORKSPACE`, `$HOME`, `$CARGO_HOME`, `$QRUST_HOME` in exported reports |
| `embedded` | bool | `false` | `QRUST_EMBEDDED` | QuickRust runs as a component of another program, which gives it the toolchain (by directory: `RUSTUP_TOOLCHAIN=<directory>`; nothing is downloaded), `CARGO_HOME` (required) and a QuickRust home of its own. A repository's `.qrust/config.toml` is not read, nothing runs in the background (compiler driver builds, toolchain updates, cache collection: `qrust gc` with the same home does it), and Cargo's output passes through unchanged ([How to embed QuickRust in another program](../how-to/embed-quickrust.md)). Read from the environment or from `$QRUST_HOME/config.toml`, never from a repository |
| `export.facts` | bool | `false` | `QRUST_EXPORT_FACTS` | write rustc's semantic facts for the workspace's crates beside each crate's metadata (`lib<crate>-<hash>.qrust-facts.json`, [Semantic facts schema](facts-schema.md)): their definitions, every call with the function the compiler resolved it to and the impl it selects, their impls. The workspace's units are compiled with QuickRust's compiler driver, which is built for the toolchain before the build when it is missing (it needs the toolchain's `rustc-dev` component; an embedded QuickRust requires it to be there). The run report's `facts` section says whether facts were written, and why not |
| `benchmark.allow-mutation` | bool | `false` | `QRUST_BENCHMARK_ALLOW_MUTATION` | let the `small-edit` benchmark scenario edit a source file without asking |

A key's environment variable is `QRUST_` followed by the key in upper case with `.` and `-` replaced by `_`, except for the keys whose variable is listed otherwise above (`QRUST_MODE`, `QRUST_OUTPUT`, `QRUST_NESTED`, `QRUST_REPORT`, `QRUST_EARLY_CUTOFF`, `QRUST_CACHE_MAX_SIZE`, `QRUST_CACHE_MIN_FREE`, `QRUST_UPDATE_URL`, `QRUST_TOOLCHAIN_AUTO_UPDATE`, `QRUST_DIST_SERVER`).

A run counts as CI when one of `CI`, `GITHUB_ACTIONS`, `GITLAB_CI`, `BUILDKITE`, `JENKINS_URL`, `TF_BUILD`, `TEAMCITY_VERSION` is set to a value other than empty, `false` or `0`. CI selects `incremental.ci` over `incremental.local-dev` and turns off `toolchain.auto-update`. An explicit `CARGO_INCREMENTAL` in the environment always wins over both `incremental.*` keys.

## Toolchain selection

The toolchain of a command is the first of:

| Order | Source | Shown as |
|---|---|---|
| 1 | `+<toolchain>` as the first argument (`cargo +nightly build`, `rustc +1.98 -V`); consumed by QuickRust, never passed to Cargo | `` `+toolchain` argument `` |
| 2 | `RUSTUP_TOOLCHAIN` | `RUSTUP_TOOLCHAIN` |
| 3 | a toolchain file in the current directory or the nearest directory above it | the file's path |
| 4 | the default toolchain (`qrust toolchain default`; `stable` until set) | `default toolchain` |

`qrust toolchain list` and `qrust doctor` show the selected toolchain and what selected it. rustup's per-directory overrides have no equivalent.

### Toolchain names

| Form | Example | Moves to newer releases |
|---|---|---|
| a channel | `stable`, `beta`, `nightly` | yes |
| a dated nightly | `nightly-2026-10-11` | no |
| a minor release | `1.98` (its latest patch release) | yes |
| a release | `1.98.1` | no |
| any of the above with a host | `stable-x86_64-unknown-linux-gnu` | as above |
| the name of a linked toolchain | `my-build` (`qrust toolchain link my-build <dir>`) | no; never changed by QuickRust |
| an absolute path to a toolchain directory | `/opt/rust` | no; used as it is |

Anything else is an error before any download. An undated `nightly` resolves to the newest nightly of the last seven days that has every component and target asked for.

### Toolchain files

`rust-toolchain.toml`, or `rust-toolchain` (the same TOML, or a single line naming the toolchain). A directory holding both is an error.

```toml
[toolchain]
channel = "1.98"                       # or path = "../my-toolchain", never both
components = ["rust-src", "llvm-tools"]
targets = ["wasm32-unknown-unknown"]
profile = "minimal"                    # minimal | default | complete
```

| Key | Content |
|---|---|
| `channel` | a toolchain name |
| `path` | a toolchain directory, relative to the file; used as it is, so `components`, `targets` and `profile` are errors with it |
| `components` | components installed with the toolchain |
| `targets` | target triples whose standard library is installed with the toolchain |
| `profile` | the profile a new install of the toolchain gets |

Any other key or table is an error. The toolchain, its components and targets, and the standard library of every `--target` a command builds for are installed on first use.

### Toolchain profiles

| Profile | Components |
|---|---|
| `minimal` | rustc, cargo, the standard library |
| `default` | `minimal` with clippy, rustfmt and rust-src |
| `complete` | every component the release has for the host |

A toolchain installed without a profile gets `default`.

## Files QuickRust reads in a repository

| File | Content |
|---|---|
| `.qrust/config.toml` | QuickRust configuration (above) |
| `.qrust/budgets.toml` (`ci.budget-policy`) | [Budgets](budgets.md) |
| `.qrust/baselines/<name>.json` | baselines written by `qrust baseline set` |
| `rust-toolchain.toml`, `rust-toolchain` | toolchain selection (above) |
