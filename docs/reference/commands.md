# Commands

Every command the `qrust` binary accepts, the toolchain programs it stands in for, and the exit statuses it returns.

`qrust help` prints the same list in short form. Options of QuickRust's own commands are strict: an option a command does not have is an error (`` unexpected argument "..."; see `qrust help` ``), and `--format` takes only the values listed for the command. Everything after a literal `--` is passed to Cargo.

## Entry points

| Invocation | What runs |
|---|---|
| `cargo <args...>` | any Cargo command through QuickRust, when `~/.qrust/bin` is first on `PATH` (`qrust setup`) |
| `qrust <cargo-command> [args...]` | the same, by QuickRust's own name; any first argument that is not a QuickRust command below (including `+toolchain` and Cargo's global flags) is Cargo syntax |
| `qrust upstream <args...>` | the toolchain's own Cargo, run as itself: no fast-mode settings, no rustc wrapper, no rendering, nothing recorded; QuickRust's configuration is not read, so it works when that configuration is invalid. It still uses QuickRust's toolchain, Cargo home and the workspace's build directory |
| `qrust help`, `qrust --help`, `qrust -h`, `qrust` | the command list |
| `qrust version`, `qrust --version`, `qrust -V` | `qrust <version>` |

### Toolchain programs

`~/.qrust/bin` holds `cargo` and these names as links to the `qrust` binary. Run under one of them, QuickRust runs that program of the toolchain the current directory selects, installing the toolchain, and the component that provides the program, on first use. A leading `+<toolchain>` argument selects the toolchain, as for `cargo`.

| Program | Component installed on first use |
|---|---|
| `rustc`, `rustdoc`, `rust-gdb`, `rust-gdbgui`, `rust-lldb` | none (part of the toolchain) |
| `rustfmt`, `cargo-fmt` | `rustfmt` |
| `cargo-clippy`, `clippy-driver` | `clippy` |
| `rust-analyzer` | `rust-analyzer` |
| `cargo-miri` | `miri` |

How the toolchain is selected: [Configuration](configuration.md#toolchain-selection).

## Cargo commands

QuickRust classifies each Cargo command into a family. The family decides whether a session is recorded, whether fast-mode settings apply and how the terminal output is presented ([Output](output.md)).

| Family | Commands (and Cargo's aliases) | Recorded, observed through the rustc wrapper | Fast-mode profile settings |
|---|---|---|---|
| build | `build` (`b`), `check` (`c`), `test` (`t`), `bench`, `run` (`r`), `doc` (`d`), `rustdoc`, `rustc`, `clippy`, `fix` | yes | yes |
| clean | `clean` | recorded | no |
| passthrough | `add`, `remove` (`rm`), `update`, `fetch`, `vendor`, `generate-lockfile`, `publish`, `package`, `login`, `logout`, `owner`, `yank`, `new`, `init` | recorded | no |
| query | `metadata`, `tree`, `search`, `info`, `locate-project`, `pkgid`, `verify-project`, `read-manifest`, `version`, `help`, `config`, `report`, `fmt` | no: Cargo replaces QuickRust (`tree` and `metadata` in a terminal are rendered and recorded) | no |
| external | any other command (`cargo nextest`, `cargo llvm-cov`, ...) | recorded; observed only with `compatibility.unknown-command = "instrument"` | no |

`cargo install` and `cargo uninstall` are QuickRust's own (below). `cargo fix` is Cargo's; QuickRust's `qrust fix` is a different command.

## Programs

```text
qrust install <CRATE>[@VERSION]... [--git URL [--branch B | --tag T | --rev R] | --path DIR]
              [--locked] [--force] [--features F] [--bin NAME]... [--root DIR] [--profile P] [--target T]
qrust install --list
qrust uninstall <CRATE>... [--bin NAME]... [--root DIR]
```

`cargo install` and `cargo uninstall` reach the same commands. QuickRust builds the package through its own build path and places its binaries into `~/.qrust/bin`, or `<DIR>/bin` with `--root`. Guide: [How to install programs](../how-to/install-programs.md).

| Option | Meaning |
|---|---|
| `<CRATE>[@VERSION]` | a package from the registry; several may be given. `@VERSION` (or `--version`/`--vers` with exactly one crate) is a version requirement; a bare version is exactly that version |
| `--git URL`, with `--branch`, `--tag` or `--rev` | a package from a Git repository (needs `git` on `PATH`) |
| `--path DIR` | a local package, built in place |
| `--registry NAME` | a registry named in Cargo's configuration; `--index` is rejected |
| `--locked` | build with the lockfile: for a registry crate, the one it was published with; otherwise the package's own `Cargo.lock`. Without it, a registry crate's dependencies are resolved afresh |
| `-f`, `--force` | replace binaries another package installed |
| `-F`, `--features F`, `--all-features`, `--no-default-features` | features |
| `--bin NAME`, `--bins`, `--example NAME`, `--examples` | which executables to install |
| `--root DIR` | install into `DIR/bin` instead of `~/.qrust/bin` |
| `--profile P`, `--debug` | build profile (default `release`; `--debug` is `dev`) |
| `--target T` | target triple |
| `--list` | what is installed, in Cargo's format |
| `--no-track` | do not record the install in `installs.json` |
| `-j`, `--jobs`, `--config`, `--offline`, `--frozen`, `--ignore-rust-version`, `-q`, `--quiet`, `-v`, `-vv`, `--verbose` | passed to the build |

Any other option is an error.

A set `CARGO_INSTALL_ROOT` without `--root` is an error. Binaries named like QuickRust's own programs (`cargo`, `qrust`, the toolchain programs) are never installed. `qrust uninstall` takes `--bin NAME` and `--root DIR`. Installs are recorded in `~/.qrust/installs.json`.

## Output of a build

Stored for every build QuickRust rendered in a terminal ([Output](output.md#stored-output)). `--run REF` selects the run (default `latest`; see [Run references](#run-references)).

| Command | Prints |
|---|---|
| `qrust warnings [--run REF] [--crate NAME] [--lint LINT]` | the warnings of the run as rustc rendered them; `--crate` matches the target name or package, `--lint` the lint name (`dead_code`, `clippy::len_zero`) |
| `qrust errors [--run REF] [--crate NAME]` | the errors of the run |
| `qrust log [--run REF] [--raw]` | every stored line in order, without colour; `--raw` prints the lines as Cargo wrote them (Cargo's JSON messages included) |

## Analysis

| Command | Prints |
|---|---|
| `qrust analyze [SECTION] [--run REF] [--format text\|json] [--filter FIELD=VALUE]... [--sort FIELD[:asc\|desc]] [--limit N]` | the analysis of a recorded run: every section, or one |
| `qrust report [--run REF] [--format text\|json\|html] [-o FILE]` | the whole report; `-o` (or `--output`) writes it to `FILE` instead of stdout. `json` and `html` are exported documents (tokenized paths, [Run report schema](report-schema.md)) |
| `qrust runs [--limit N] [--format text\|json]` | the recorded runs of this workspace, newest first (default 20) |
| `qrust inspect [--format text\|json]` | the workspace, toolchain and environment fingerprint, measured now |
| `qrust driver [status\|build] [--rustc PATH]` | QuickRust's compiler driver for the toolchain used here (or the compiler at `PATH`): `status` (default) says whether it is built, `build` builds it and prints its path |
| `qrust compare <BASE> <CANDIDATE> [--format text\|json]` | the metrics of two runs, baselines or benchmarks side by side, with their comparability |

Sections of `qrust analyze`: `timing`, `rebuilds`, `duplication`, `scripts`, `proc-macros`, `codegen`, `cache`, `storage`, `recommendations`, and `worktrees` (identical units built in other worktrees and workspaces; it takes no `--run`, `--filter`, `--sort` or `--limit`).

`--filter`, `--sort` and `--limit` apply to the records of one section and need a section:

- `--filter FIELD=VALUE` keeps records whose field equals the value or, for a list field, contains it; several filters must all match; a field no record has is an error that names the fields there are.
- `--sort FIELD[:asc|desc]` orders by a field: numbers descending and text ascending unless a direction is given.
- `--limit N` keeps the first `N` records (`N` at least 1).

```bash
qrust analyze timing --filter source=registry --sort duration_ns --limit 10
```

Analysis is lazy: a run is analysed when one of these commands first needs it. A run analysed after later builds reports its artifact footprint as unavailable.

## Measurement

```text
qrust benchmark [--scenario NAME] [--reps N] [--warmups N] [--edit FILE] [--allow-mutation]
                [--timeout DURATION] [--format text|json] [-- <cargo args>]
qrust benchmark --list
qrust experiment [codegen|linker] [--name NAME] [--set KEY=VALUE]... [--env KEY=VALUE]...
                 [--codegen-units N] [--debug VALUE] [--opt-level VALUE] [--lto VALUE] [--linker NAME]
                 [--scenario NAME] [--reps N] [--warmups N] [--verify '<cargo args>'] [-- <cargo args>]
```

Scenarios, defaults, the mutation journal, discarded runs and experiment verdicts: [Benchmarks](benchmarks.md).

## Budgets

| Command | Does |
|---|---|
| `qrust baseline set NAME [--run REF \| --benchmark ID]` | writes `<workspace>/.qrust/baselines/NAME.json` from a run (default `latest`) or a benchmark; a failed run is refused |
| `qrust baseline list` | the baselines in `.qrust/baselines/` |
| `qrust baseline show NAME` | one baseline as JSON |
| `qrust ci evaluate [--run REF \| --benchmark ID] [--baseline NAME] [--budgets FILE] [--format text\|json]` | evaluates the budgets (default `ci.budget-policy`) for a run or benchmark against a baseline (default `ci.baseline`); a missing baseline is an error only when a budget is relative |

`--run` and `--benchmark` exclude each other. Budget file format and verdicts: [Budgets](budgets.md).

## Storage

| Command | Does |
|---|---|
| `qrust inventory [--caches] [--older-than DAYS] [--max-size SIZE] [--format text\|json]` | what this workspace's build directory holds by category, and a stale plan (what is older than `DAYS`, default `artifacts.stale-days`, or over `SIZE`); deletes nothing. `--caches` adds the shared caches |
| `qrust gc [--dry-run] [--to SIZE] [--format text\|json]` | collects QuickRust's cache now ([Files](files.md#quickrusts-cache)); `--dry-run` lists what it would remove, `--to SIZE` collects down to `SIZE` |

Sizes are a number with an optional unit `B`, `K`/`KB`/`KiB`, `M`/`MB`/`MiB`, `G`/`GB`/`GiB`, `T`/`TB`/`TiB` (all powers of 1024).

## Policy and remediation

| Command | Does |
|---|---|
| `qrust policy explain [--format text\|json]` | every configuration key with its effective value, environment variable, origin and description ([Configuration](configuration.md)) |
| `qrust cache status [--format text\|json]` | QuickRust's cache: its size by kind as last counted, its bounds, free space, recent collections; then the compiler cache (sccache): policy, state, server statistics, the latest analysed run's cache results |
| `qrust fix --dry-run [REC_ID] [--run REF] [--option ALTERNATIVE] [--patch] [--format text\|json]` | remediation previews for the recommendations of a run (one with `REC_ID`); `--patch` shows them as patches; nothing is written |
| `qrust fix --apply REMEDIATION_ID` | applies a previewed remediation of class `safe` (QuickRust's own configuration only); the file must be byte-identical to the previewed one |
| `qrust fix --rollback REMEDIATION_ID` | undoes an applied remediation |

## Build output location

| Command | Prints |
|---|---|
| `qrust path` | this workspace's target directory (that of the build variant in effect, `build.variant`) |
| `qrust path bin <NAME> [--release \| --profile P] [--target T]` | the path where `cargo build` puts binary `NAME` (`.exe` added for Windows targets) |
| `qrust path [bin …] --hold-for <PID>` | the same answer, and the build directory is held while process `PID` runs: QuickRust's cache does not collect it, however long it has been idle ([How to find build outputs](../how-to/find-build-outputs.md#use-a-builds-outputs-for-longer-than-the-build)). The hold ends when the process does. `PID` must be running. A target directory you chose is never collected, and holding it does nothing |

Profiles `dev` and `test` map to `debug`, `release` and `bench` to `release`, any other profile to its own name. `--release` and `--profile` exclude each other.

## Toolchains

| Command | Does |
|---|---|
| `qrust toolchain` or `qrust toolchain list` | installed and linked toolchains, the default, and the toolchain selected here with what selected it |
| `qrust toolchain install <TOOLCHAIN>... [--profile minimal\|default\|complete] [--component C]... [--target T]...` | installs toolchains, components and targets |
| `qrust toolchain update [--channel C]` | brings `stable`, `beta`, `nightly` and `1.xx` channels (or only `C`) to their latest release, then removes releases no channel names that went unused for a day |
| `qrust toolchain remove <TOOLCHAIN>...` | removes toolchains; a channel name removes the release it names |
| `qrust toolchain default [<TOOLCHAIN>]` | prints or sets the default toolchain (a channel, a release or a linked name; not a path) |
| `qrust toolchain link <NAME> <DIR>` | names a toolchain built elsewhere; `DIR` must hold `bin/rustc` and `bin/cargo`, and neither may be QuickRust. QuickRust never changes a linked toolchain |
| `qrust toolchain unlink <NAME>` | removes the name |
| `qrust toolchain which [<TOOL>] [--toolchain T]` | the directory of the selected (or named) toolchain, or the path of one of its programs |
| `qrust toolchain component add\|remove <NAME>... [--toolchain T]` | components of the selected (or named) toolchain |
| `qrust toolchain target add\|remove <TRIPLE>... [--toolchain T]` | standard libraries for targets |

Profiles: `minimal` (rustc, cargo, the standard library), `default` (`minimal` with clippy, rustfmt and rust-src), `complete` (every component the release has for the host). Toolchain names: [Configuration](configuration.md#toolchain-names).

## Installation

| Command | Does |
|---|---|
| `qrust doctor` | prints the installation and its release line (the channel and registry `qrust self update` reads, or that this QuickRust was built from source), the active toolchain and what selected it, its Cargo and rustc, QuickRust's Cargo home, the Cargo and rustc variables of the environment, the workspace's target and build directories, the state directory, cache policy and sccache; each problem is a `FAIL` line, and the last line is `OK` or `FAIL <n> problem(s)` |
| `qrust setup [--no-modify-path] [--toolchain T\|none]` | installs QuickRust for this user ([Files](files.md#setup)); `--toolchain` names the toolchain to install (default `stable`, `none` for no toolchain); running it again repairs what is missing |
| `qrust self update` | installs the latest release of this QuickRust's channel in place of the running one, from the release registry it was published in (or the registry in `self.update-url`), after verifying the registry's charter, its signed manifest, the archive and the binary ([How to install, update and uninstall QuickRust](../how-to/install.md#update-quickrust)); a QuickRust built from source has no release line and says so |
| `qrust self uninstall --yes` | removes `~/.qrust` and the `PATH` lines `qrust setup` added; without `--yes` it only says what it would remove |

## Run references

`--run REF` and the arguments of `qrust compare` accept:

| Reference | Means |
|---|---|
| `latest` | the newest recorded build run of this workspace |
| `latest~N` | the `N`th build run before it |
| a run id, or a unique prefix or suffix of one | that run |
| `baseline:NAME` | a baseline (`qrust compare` only) |
| `benchmark:ID` | a benchmark result (`qrust compare` only) |

`latest` and `latest~N` count runs the rustc wrapper observed directly; nested sessions whose events went to their parent are skipped.

## Exit statuses

| Status | Meaning |
|---|---|
| Cargo's own | whenever Cargo ran; a terminating signal is re-raised |
| 1 | a benchmark that did not complete, an experiment whose verdict is `invalid`, or `qrust doctor` with a problem |
| 2 | QuickRust could not run the command: no usable toolchain, recursion into QuickRust, invalid QuickRust configuration, invalid arguments to a QuickRust command; also a failed record or report with `measurement.strict = true` |
| 3 | a blocking budget failed (`qrust ci evaluate`, or a build with `ci.enforce-on-build`) |
| 4 | baseline and candidate are incomparable and the budget policy says `incomparable = "fail"` |

A policy status (2 from `measurement.strict`, 3, 4) only ever replaces a successful Cargo status.
