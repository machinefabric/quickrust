# Why QuickRust owns its environment

A Rust developer usually installs rustup, which installs toolchains, whose Cargo builds into a `target/` directory in every project and keeps downloads in `~/.cargo`, none of which ever shrinks on its own. QuickRust replaces all of that around the build engine: it installs and selects toolchains itself, keeps its own Cargo home, builds every workspace into a directory of its own cache, and keeps that cache within a size it chooses. This page explains why it goes that far, what the decisions were, and what they cost.

## The decision

QuickRust started as a layer over whatever Cargo and rustup a machine had. On 2026-10-12 that changed: QuickRust became the only thing a developer installs. The decisions taken then were:

- **Own everything except the engine.** The toolchain's `cargo` binary stays the build engine, as an internal component nobody needs to meet. QuickRust owns the home directory, the toolchains, build directories, installs, downloads and output. The engine's own pieces may be replaced later, one at a time, where measurement shows a gain.
- **Nothing in the project.** No `target/` beside the sources, not even a symbolic link. Tools find build outputs the way Cargo tells them: `CARGO_TARGET_DIR`, `cargo metadata`, `qrust path`.
- **A clean break, with one import.** QuickRust never calls rustup. When it is set up, it copies registry credentials and registry settings from `~/.cargo` once, says what it imported, and from then on uses only its own.
- **A cache that manages itself.** Everything QuickRust can rebuild or download again lives in one cache, which stays within a size and leaves the disk free space, without the user ever running a cleanup command.

The reason is that the features QuickRust is built for need it. A global artifact cache shared by every checkout, early cut-off through a compiler driver built for each toolchain, an "up to date" verdict that depends on knowing which release a channel names: each of these is fragile when another program decides which compiler runs, where outputs go and when downloads disappear. Owning the environment turns those from guesses into facts QuickRust controls.

## Toolchains without rustup

QuickRust reads the same signals rustup does, in rustup's order: `+toolchain` on the command line, `RUSTUP_TOOLCHAIN` (the name editors and tools already set), `rust-toolchain.toml` or `rust-toolchain` in the nearest directory, and then the default. Keeping rustup's names and precedence means existing projects, editors and CI scripts need no change. A toolchain is downloaded from the Rust dist server on first use, each manifest checked against its published SHA-256 and each archive against the manifest's, and installed into `~/.qrust/toolchains` with the components and targets the project asks for.

Owning toolchains also lets QuickRust keep channels current without anyone running an update. Once a day, after a build that used a channel such as `stable`, a detached, low-priority process checks the 64-byte checksum of the channel's manifest; when it changed, the new release is installed in the background and the channel switches to it for the next build, never during one. A release a channel left is removed once nothing names it and a day has passed unused; a release asked for by its own name, such as `1.98.1`, is never removed by QuickRust. CI never auto-updates, because a CI run should use what it was told to use.

What is lost is rustup's per-directory overrides (`rustup override set`), which have no equivalent; a toolchain file in the project does the same job and travels with the repository. A toolchain built elsewhere, such as a local rustc checkout, is added with `qrust toolchain link`.

## A Cargo home of its own

Every Cargo QuickRust runs gets `CARGO_HOME=~/.qrust/cargo`. The registry index, downloaded crates, git checkouts, credentials and Cargo's own configuration live there, shared by every project. Keeping it separate from `~/.cargo` is what makes the clean break real: rustup's Cargo and QuickRust never fight over the same index lock, and QuickRust can manage that directory's size without surprising another tool. The one-time import at setup carries over what users would otherwise have to redo: credentials and the `[registries]`, `[registry]`, `[source]`, `[net]` and `[http]` tables. Other tables are left where they are and reported as not imported, since settings like `[build]` belong to the projects under that directory, which Cargo still reads from there.

## Build directories outside the project

Cargo writes every build of a workspace into `<workspace>/target`, which is why every Rust project on a disk carries gigabytes of build output that nothing ever removes. QuickRust builds each workspace (the one `cargo locate-project --workspace` names) into `~/.qrust/cache/builds/<name>-<id>/` and tells Cargo, and every tool running under it, through `CARGO_TARGET_DIR` and `CARGO_BUILD_BUILD_DIR`. `cargo metadata` then reports that directory, so tools that ask, as rust-analyzer, nextest and `cargo llvm-cov` do, find it without knowing QuickRust exists.

Putting build output in the cache rather than the project has several consequences that QuickRust relies on. The build directories sit on the same file system as the artifact cache, so restoring a unit can be a cheap copy, or a reflink on file systems that support it. They are disposable by design: the cache may remove one that has not been used for a while, and the next build recreates it, its dependencies coming back from the artifact cache. A project that is moved or deleted leaves an orphan the cache recognises and removes first. Each git worktree is a workspace of its own and gets its own directory.

A target directory you choose is yours: `--target-dir`, `--config build.target-dir=...` on the command line, or `CARGO_TARGET_DIR` set in the environment are used as they are, and QuickRust never manages or removes them. `build.target-dir` in a Cargo configuration file is overridden. That is a judgement: a setting in a configuration file is typically a standing habit, often a machine-wide one meant to keep `target/` out of projects, which QuickRust already does; honouring it would put every project's output back into one unmanaged place, whereas a target directory given for one command is a deliberate choice for that command.

The cost is borne by scripts that hard-code `./target/debug/app`. They need to ask instead, with `qrust path` or `cargo metadata`; [How to find build outputs](../how-to/find-build-outputs.md) shows how. Going back to plain Cargo is unaffected: it builds into `target/` from scratch, and the two never touch each other's artifacts.

## A cache that keeps itself small

All of `~/.qrust/cache` can be rebuilt or downloaded again: build directories, the artifact cache, compiler drivers, sources of installed programs, toolchain downloads and remembered answers. It is kept within two bounds: `cache.max-size`, the most it uses (by default 10% of the disk, between 10 and 100 GiB), and `cache.min-free`, the free space it leaves the disk (by default 5%, between 2 and 20 GiB). The second bound matters as much as the first: a cache within its size is still a problem on a disk that something else has filled.

Checking costs almost nothing: after a command, QuickRust compares free space and its last count, at the price of one `statvfs` and one small file read. When a collection is due it runs in a detached process at the lowest priority, so the command that triggered it is not slowed. Only when the disk is nearly full, below a quarter of `cache.min-free`, does a command collect before it runs, for at most ten seconds, and say so.

A collection removes things in an order that reflects what they cost to get back:

1. First, whatever nothing can use any more, regardless of space: build directories of workspaces that no longer exist, drivers of toolchains that are gone, stale downloads and remembered answers, leftovers.
2. Then, while the cache is above its size or the disk below its free space, what can be rebuilt locally: build directories, artifact entries and sources of installed programs, least recently used first.
3. Last, what needs the network to come back: crates and git checkouts in QuickRust's Cargo home. Cargo removes these itself (`cargo -Zgc clean gc`), because it knows when each was last used and waits for builds reading them. QuickRust lets it apply its usual ages at every collection and, only while the bounds are still exceeded, asks for progressively shorter ones.

Some things are never removed: anything used in the last 15 minutes, a build directory a QuickRust command or a Cargo build holds, a source an install holds, and an artifact entry whose files a build directory in use still links. Removal renames a directory aside before deleting it, so no build ever sees one half deleted, and every removal is logged. Sizes are counted as blocks allocated, with a hard-linked file counted once, so the numbers match what the disk loses and gains.

The same rule applies everywhere here: QuickRust deletes only inside its own cache, whose contents it created. It never deletes in a project, in a target directory you chose, or among the files you own in its Cargo home ([security](security.md#deleting)). The [disk-space how-to](../how-to/free-disk-space.md) covers adjusting the bounds and collecting by hand.

## The engine, for now

Owning the environment is a step, not the destination. Cargo remains the engine because its resolver, scheduler and fingerprints are exactly right for compatibility and not where most build time goes. With toolchains, output locations, caches and output under QuickRust's control, a piece of the engine can be replaced when a benchmark shows it is worth it, without anything around it changing.
