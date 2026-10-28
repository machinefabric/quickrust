# Files and directories

Everything QuickRust keeps on disk: its home directory, its cache, what it writes into a repository, and how long records are kept.

QuickRust creates its directories with mode 0700. Paths below use the default home, `~/.qrust`; `QRUST_HOME` moves all of it.

## QuickRust's home

| Path | Content | Deleting it |
|---|---|---|
| `~/.qrust/bin/` | QuickRust (`qrust`, `qrust-rustc-wrapper`): links to a package manager's installation, or QuickRust's own copies when an archive or a source build set it up; `cargo` and the toolchain programs as links to `qrust` ([Commands](commands.md#toolchain-programs)); programs `qrust install` installed | the links are recreated by the next command; installed programs are lost |
| `~/.qrust/config.toml` | the user's QuickRust configuration ([Configuration](configuration.md)) | yours |
| `~/.qrust/setup.json` | what `qrust setup` did: where it installed, the package manager's installation it links to (`installed_by`), which shell files it changed, what it imported, and the digest of every QuickRust binary it or `qrust self update` installed (so an update recognises the proxies an earlier release made, which on Windows are copies of it); read by `qrust doctor`, `qrust self update` and `qrust self uninstall` | `qrust setup` writes it again |
| `~/.qrust/env`, `~/.qrust/env.fish` | the scripts the shell startup files source to put `~/.qrust/bin` first on `PATH` | `qrust setup` writes them again |
| `~/.qrust/installs.json` | what `qrust install` installed, by root and package (`qrust install --list`) | use `qrust uninstall` instead |
| `~/.qrust/toolchains/` | the Rust toolchains QuickRust installed (`qrust toolchain list`); `channels.json` says which release each channel names, the default, and the linked toolchains | use `qrust toolchain remove`; a deleted toolchain is installed again on its next use |
| `~/.qrust/cargo/` | QuickRust's Cargo home: `config.toml`, `credentials.toml` (mode 0600), the registry index and sources, git checkouts | the registry and git directories are downloaded again; `config.toml` and `credentials.toml` are yours |
| `~/.qrust/workspaces/<id>/` | per workspace: `workspace.json` (its root), `sessions/` (run records), `events/` (raw rustc events and stored output), `runs/` (analysed reports), `index/units.json` (last known unit fingerprints), `benchmarks/`, `remediations/`, `restore/` (journals of benchmark edits in progress) | history is lost |
| `~/.qrust/workspaces/<id>/prebuilt/` | stamps of builds that succeeded in a terminal: the files each depended on, with sizes and times (at most 32 per workspace) | the next build asks Cargo |
| `~/.qrust/active/` | one file per running session | only while no QuickRust command runs |
| `~/.qrust/cargo-runs/` | one empty file per QuickRust process that may be running a Cargo with QuickRust's Cargo home, named by its process id, and the `.lock` they are written under. A toolchain program QuickRust runs by its name (`rust-analyzer`, `rustfmt`, …) has none: a Cargo it runs is QuickRust's `cargo`, which has its own, so an editor's rust-analyzer does not keep Cargo's clean from running; a collection runs Cargo's clean only while none of them is running ([Collection](#collection)). A file whose process is gone holds nothing and is removed by the next command or collection. A command that cannot write its file stops before it runs Cargo | only while no QuickRust command runs |
| `~/.qrust/linker/ld` | a link to the `wild` in use ([Fast mode](fast-mode.md#linker)) | recreated by the next build |
| `~/.qrust/notices/` | markers of notes shown once or once an hour | the notes are shown again |
| `~/.qrust/cache/` | QuickRust's cache (below) | see below |

`<id>` is the first 16 hexadecimal digits of the SHA-256 of the workspace root's canonical path.

## QuickRust's cache

Everything under `~/.qrust/cache/` can be rebuilt or downloaded again. QuickRust keeps it within `cache.max-size` and leaves `cache.min-free` of the disk free ([Configuration](configuration.md#keys)), removing what was used least recently.

| Path | Content |
|---|---|
| `builds/<name>-<id>/` | each workspace's target directory (`qrust path`): Cargo's build output. `<name>` is the workspace directory's name (at most 24 characters; other than letters, digits, `-` and `_` replaced by `_`) |
| `builds/<name>-<id>.<variant>/` | the target directory of a [build variant](configuration.md#keys) of that workspace (`build.variant`), with a record and a lock of its own like any build directory |
| `builds/<name>-<id>.hold-<pid>` | a hold on that build directory by the running process `<pid>` (`qrust path --hold-for`); once the process is gone it holds nothing, and the next hold or the next collection removes it |
| `builds/<name>-<id>.json`, `builds/<name>-<id>.lock` | which workspace a build directory belongs to and when it was last used; the lock a command holds while it uses the directory |
| `builds/<name>-<id>/qrust-bench/` | benchmark and experiment build directories, each with a `.qrust-owned` marker and its own artifact cache (`.qrust-artifacts/`) |
| `artifacts/` | the artifact cache |
| `drivers/<commit>-<host>-<source>/` | QuickRust's compiler driver for one compiler and driver source (`qrust driver status`); `build-failed` says why it could not be built, `failed/` keeps the report of each crate it failed on |
| `src/` | sources of programs being or having been installed: crates copied from QuickRust's Cargo home (their published lockfile kept as `.qrust-published.lock`) and `--git` clones; `<copy>.lock` while an install uses one |
| `downloads/` | toolchain archives being downloaded (resumed after an interruption) |
| `memo/rustc/` | each compiler's `rustc -vV`, asked once per binary |
| `memo/roots/` | which workspace a directory belongs to, as `cargo locate-project` said (asked again when a `Cargo.toml` from there up changes) |
| `tmp/` | what a collection is deleting; renamed here first |
| `usage.json` | the cache's last count, the disk's free space then, and what collections removed that the disk did not show free yet; a count QuickRust cannot write fails the collection, and one of an older format is no count (the next collection counts again) |
| `gc-log.jsonl` | one line per collection (the last 200): by kind and reason, how many things went and their bytes; the build directories and program sources that went, by path; Cargo's summary, or why Cargo's downloads were left; the free space it started from and its goal |
| `gc.lock` | held by a running collection |

### Collection

A collection removes, in this order:

1. whatever nothing can use any more: build directories of workspaces that are gone, drivers of toolchains that are gone, week-old unfinished downloads, month-old memos, leftovers in `tmp/`;
2. while the cache is above `cache.max-size` or the disk below `cache.min-free`: build directories, artifact entries and sources of installed programs, least recently used first, until the cache is under 90% of `cache.max-size` and the disk has 125% of `cache.min-free` free;
3. crates and git checkouts downloaded into QuickRust's Cargo home, removed by Cargo itself (`cargo -Zgc clean gc`): by Cargo's usual ages at every collection and, while the bounds are still exceeded, those unused for a month, a week, a day, an hour, 15 minutes in turn. Only while no QuickRust command is running a Cargo (`~/.qrust/cargo-runs/`). Otherwise they are left for the next collection, which `qrust gc` and `qrust cache status` report. Cargo's clean deletes its records of registries it does not find in the Cargo home, such as the temporary one `cargo package` builds against, and a running Cargo that holds one of those records stops with an internal error. A command that starts while Cargo's clean runs waits for it and says so.

A collection counts what it frees itself and does not measure the disk again until it ends, because the disk may not show what it frees. A snapshot (Time Machine's hourly local snapshots on macOS, btrfs or ZFS snapshots) keeps the blocks of every file older than itself, and what was used least recently always is. What the disk does not show at the end is recorded in `usage.json` and counted as free, before and after commands and by the next collection, until the disk shows it. That lasts a day at most, the life of a Time Machine local snapshot. After a day, the disk's own figure counts again.

Never removed: anything used in the last 15 minutes; a build directory a QuickRust command holds or a Cargo holds a lock in; a source an install holds; an artifact entry whose files a build directory in use links. A collection runs for minutes, so just before it removes a build directory it asks again, under the directory's lock, whether it is held or was used: a run that starts during a collection keeps what it holds. Sizes are blocks allocated; a file with several hard links counts once and is freed with its last link.

| Trigger | Collection |
|---|---|
| after every command, when the disk is below `cache.min-free` (counting what collections removed that it does not show yet), the cache above `cache.max-size` by the free space lost since the last count, or a day has passed since the last count; not within 15 minutes of a collection that could not get within the bounds | `qrust gc --background`, detached, at the lowest priority |
| before a command, when the disk is below a quarter of `cache.min-free` (counted the same way) | in the command, for at most ten seconds, with a `NOTE`; Cargo's downloads are left alone |
| `qrust gc` | now |

`qrust cache status` shows the last count and recent collections; `qrust gc --dry-run` lists what a collection would remove.

## Toolchain programs inside a toolchain

| Path | Content |
|---|---|
| `~/.qrust/toolchains/<id>/bin/.qrust-engine` (`.qrust-engine.exe` on Windows) | a hard link of the toolchain's `cargo`, which QuickRust runs so that Cargo passes QuickRust's `cargo` on as `CARGO` ([Compatibility](compatibility.md#nested-invocations)); not for linked toolchains |

## In a build directory

| Path | Content |
|---|---|
| `<target>/<profile>/deps/<unit>.qrust-svh` | a library's interface hash (early cut-off) |
| `<target>/<profile>/deps/<unit>.rlink`, `<unit>.qrust-objects/`, `<unit>.qrust-relink` | a binary's kept objects and what they were compiled from, for relinking |

They go with the rest of the directory (`cargo clean`).

## In a repository

QuickRust writes into a repository only on request.

| Path | Written by |
|---|---|
| `.qrust/config.toml` | you, or `qrust fix --apply` for a `safe` remediation |
| `.qrust/budgets.toml` | you ([Budgets](budgets.md)) |
| `.qrust/baselines/<name>.json` | `qrust baseline set` |
| the path of `measurement.report` / `QRUST_REPORT` | every build, when set |
| the file of `qrust report -o FILE` | that command |
| a source file of the workspace, temporarily | the `small-edit` benchmark scenario, which restores it ([Benchmarks](benchmarks.md#source-edits)) |

No `target/` directory is created in a project.

## Setup

`qrust setup` writes:

- `~/.qrust/bin/` with `qrust`, `qrust-rustc-wrapper` and the program links; it refuses to replace a file there that is not QuickRust's. Run from a package manager's installation, `qrust` and `qrust-rustc-wrapper` are links to the package's, by the path its marker names; run from anything else, they are copies;
- `~/.qrust/env` and `~/.qrust/env.fish`;
- one line marked `# Added by `qrust setup`: QuickRust's bin first on PATH` in `~/.profile`, and in `~/.bash_profile`, `~/.bashrc` and `~/.zshenv` where they exist (`~/.zshenv` also when `zsh` is on `PATH`); `~/.config/fish/conf.d/qrust.fish` when `~/.config/fish` exists; on Windows, the user's `PATH` through PowerShell (none of this with `--no-modify-path`);
- once, from `$CARGO_HOME` or `~/.cargo`: `credentials.toml`, and the `[registries]`, `[registry]`, `[source]`, `[net]` and `[http]` tables of `config.toml`, into `~/.qrust/cargo/` (the tables it did not import are listed);
- `~/.qrust/setup.json`.

`qrust self uninstall --yes` removes `~/.qrust` and the lines and files above that `qrust setup` added. rustup and `~/.cargo` are never changed.

### A package's installation

The Homebrew formula, the deb and the rpm put `qrust-installed-by.json` beside the `qrust` they install (Homebrew: `$(brew --prefix)/opt/quickrust/libexec/`; deb and rpm: `/opt/quickrust/`). It names the package manager, the path of the package's `qrust` that stays the same across its versions, and the commands that update and remove it. QuickRust reads it from beside the file `qrust` resolves to: `qrust setup` then links to that path, and `qrust self update` and `qrust self uninstall` name the manager's command and change nothing. A marker that cannot be read, or that names another binary, is an error, never taken for an installation without one.

## Retention

| Key | Default | Kept per workspace |
|---|---|---|
| `measurement.keep-runs` | 200 | run records and reports |
| `measurement.keep-raw-events` | 20 | raw rustc events and stored output, of analysed runs |

Pruning runs on about one command in 32. Prebuilt stamps: at most 32 per workspace. Collection log: the last 200 entries.
