# How to keep QuickRust's cache within your disk

This guide shows you how to see how much space QuickRust uses, free space now, set the limits QuickRust keeps to by itself, and move the cache to another disk.

Everything under `~/.qrust/cache` (build directories, the artifact cache, compiler drivers, downloads) can be rebuilt or downloaded again, and QuickRust removes what nothing is using, the largest and longest unused first, whenever the cache grows past its size limit or the disk runs short of free space. You only need this guide when the defaults do not suit your machine. How the collection decides is described in [About owning the Rust environment](../explanation/owning-the-environment.md).

## See what the cache holds

```bash
qrust cache status
```

```text
QuickRust's cache: 102.8 MiB, counted just now, /home/me/.qrust/cache
  build directories          92.2 MiB  5
  artifact cache             37.1 MiB  17
  remembered answers        312.0 KiB  84
  leftovers                   4.0 KiB  2
  crate and git downloads     9.7 MiB  2
  shared                     36.6 MiB  in files two kinds share (hard links), counted once in the total
  bounds                   at most 35.1 GiB (cache.max-size = auto), keeps 17.6 GiB of the disk free (cache.min-free = auto), 12.0 GiB free now
```

The figures are those of the last count; each collection counts again. If the first line says `not counted yet`, no collection has run so far; QuickRust counts the cache in the background after the next command, and `qrust gc --dry-run` prints its current size meanwhile.

A line `kept by snapshots` says how much collections removed that the volume's snapshots still keep. A snapshot keeps the blocks of every file that changed before it was taken, until it is deleted: on macOS, Time Machine's hourly local snapshots (`tmutil listlocalsnapshots /`) are kept for a day. QuickRust lists the volume's snapshots and counts what they keep as free while they exist, for a day at most, so it does not remove more to make up for space it has already given back. Space the disk does not show for any other reason was used by other programs, and is not counted. QuickRust sees no snapshots on Linux or Windows. If a btrfs or ZFS snapshot covers `~/.qrust/cache`, collections there give back more than the disk is short of; keep the cache out of snapshots (on btrfs, make it a subvolume of its own). A `collected` line that ends with `QuickRust command was running Cargo` means that collection left Cargo's downloads, because removing them while a Cargo runs can stop that Cargo. The next collection takes them.

## Free space now

```bash
qrust gc --dry-run          # what a collection would remove, removing nothing
qrust gc                    # collect now, within the configured bounds
qrust gc --to 5GiB          # collect until the cache is at most 5 GiB
```

Each removal is listed with why it went; if the collection could not get the cache within its bounds, each thing it kept is listed with why it stayed; then one line sums it up:

```text
     removed 4.5 MiB    build directories  /home/me/.qrust/cache/builds/app-aa6b388726e8cac5 (the bounds needed room, and nothing had used it for 2 days)
     kept    4.5 MiB    build directories  /home/me/.qrust/cache/builds/srv-3c1d8e0b7a924f61 (process 799375 (sleep) works in /home/me/src/srv/src, in its workspace's checkout /home/me/src/srv)
     kept    4.5 MiB    build directories  /home/me/.qrust/cache/builds/tool-f5025e19e2918129 (the work in hand: used 2 hours ago, within the 8 hours of cache.working-window)
OK   Freed 4.5 MiB: QuickRust's cache 13.5 MiB -> 9.0 MiB (at most 5.0 MiB; keeps 2.0 GiB of the disk free, 6.6 GiB free now), short of its goal: 2 kept, in use or within the working window [7ms]
```

A collection takes first what frees the most for how long it has gone unused: a large build directory nobody has built in for hours goes before a small one used half an hour ago. It never removes anything used in the last 15 minutes, nor a build directory that a running command or Cargo holds, that a running program was started from, or that a process works in: in the directory itself, or anywhere in its workspace's checkout (a shell, an editor or an agent's session open there). Nor does it remove the work in hand to keep the cache under its size: a build directory whose workspace is still there and that was built in within the last 8 hours stays until the disk runs short of free space or you ask for the room with `qrust gc --to` (see [Leave room for builds that run at once](#leave-room-for-builds-that-run-at-once)). If it frees less than you expected, the `kept` lines say what holds each directory; end those processes, or run it again when those builds have finished. The rule in full is in [Files and directories](../reference/files.md#collection).

When a collection has removed a workspace's build directory, the next QuickRust command in that workspace says so, once:

```text
NOTE QuickRust's cache removed this workspace's build directory /home/me/.qrust/cache/builds/app-aa6b388726e8cac5 just now (the bounds needed room, and nothing had used it for 3 hours; 4.5 MiB): what was built there is built again by the next build
```

Build again before you run its programs. A script that keeps using a build's programs after the build has ended holds the directory instead ([How to find build outputs](find-build-outputs.md#use-a-builds-outputs-for-longer-than-the-build)).

## Set the limits

In `~/.qrust/config.toml`:

```toml
[cache]
max-size = "40GiB"   # the most the cache uses
min-free = "10%"     # the free space it leaves on the disk
```

Both take a size (`500MB`, `40GiB`), a share of the disk (`10%`) or `auto`. The defaults are `auto`: at most 10% of the disk, between 10 and 100 GiB, and at least 5% of the disk free, between 2 and 20 GiB. This file is the place for a limit that holds on the machine: every command run as you reads it, whoever starts it (a script, an editor, another tool's build). For one shell or a CI job, use `QRUST_CACHE_MAX_SIZE` and `QRUST_CACHE_MIN_FREE`. `qrust policy explain` shows which one is in force and where it comes from.

If the disk is shared with other large data, raise `min-free`; QuickRust then gives space back as the disk fills up, whatever its own size. It gives back what the disk is short of and a quarter of `min-free` more, and again when the disk has lost that margin. On a disk that is usually below `min-free` because of other data, lower `min-free` to what you want kept free: the cache otherwise stays smaller by the difference.

## Leave room for builds that run at once

On a machine where several workspaces build at once, such as a build server running a branch in each worktree, each workspace has a build directory of its own, often tens of GiB. While one of them is the work in hand (its workspace is still there, and a command used it within `cache.working-window`), `cache.max-size` does not remove it. So one workspace's build does not throw away another's between its builds, and none of them starts cold because the others filled the cache. What bounds them is the disk, through `cache.min-free`.

1. Check that the disk the cache is on has room for the build directories in use at the same time, plus `cache.min-free`. `qrust cache status` shows the free space and the bounds. If it does not, [move the cache](#move-the-cache-to-another-disk) to a disk that does: on a disk that is too small, the free space takes the work in hand too, the largest and longest unused first.
2. If a workspace often waits more than 8 hours between two builds and should keep its build directory, lengthen the window in `~/.qrust/config.toml`:

   ```toml
   [cache]
   working-window = "24h"
   ```

   It takes `off` or a length of time with its unit (`90m`, `8h`). `working-window = "off"` puts every build directory back under `cache.max-size`.

## Keep a script's builds from starting a collection

After a command, QuickRust may start a collection in a process of its own, which goes on after the command has ended. It holds none of the command's standard input, output or error: a script can read a pipe from the build to its end, and append to or remove a file the build's output was redirected to, while the collection runs. A test harness that builds with QuickRust and then checks that its tests left no process running sees that collection as one of theirs. Set `QRUST_CACHE_GC=off` for those builds:

```sh
QRUST_CACHE_GC=off qrust build
```

A build with it starts no collection. A collection that was due stays due, and the next command run without it starts it, so the cache is still kept within its limits. A disk nearly full is still collected before a command, inside it, which leaves nothing behind. To turn the background collection off on a machine, put `gc = "off"` under `[cache]` in `~/.qrust/config.toml`, and run `qrust gc` yourself.

Two other things QuickRust does in the background have switches of their own: `QRUST_TOOLCHAIN_AUTO_UPDATE=off` and `QRUST_SELF_UPDATE_CHECK=off`. A build with all three set leaves no process running behind it. The compiler driver for [early cut-off](../explanation/early-cutoff.md) is built inside the build that needs it, never behind it.

Ending a collection part way, by a signal or by ending its process tree, never leaves the cache broken ([Ending a collection](../reference/files.md#ending-a-collection)).

## Move the cache to another disk

When the disk `~/.qrust` is on is short of space, or a faster one is attached, move the cache there:

```bash
qrust cache move /Volumes/Fast/qrust-cache
```

```text
OK   Moved QuickRust's cache from /Users/me/.qrust/cache to /Volumes/Fast/qrust-cache: 18.4 GiB in 212034 files [2m14.07s]
```

Every command after it uses the cache there, and nothing in it is lost: builds stay as fresh as they were, and the artifact cache keeps every entry. The directory must not exist yet, or must be empty. The move waits for running QuickRust commands to finish, and stops while a program holds a build directory (`qrust path --hold-for`). From then on the limits above apply to the disk the cache is on, and `qrust cache status` shows where it is.

If that disk is not mounted, QuickRust commands stop and say so instead of building into a new, empty cache: mount it, or move the cache elsewhere, where it starts empty. `qrust cache move ~/.qrust/cache` brings it back. Details: [Files and directories](../reference/files.md#where-the-cache-is).

## Look inside one workspace's build directory

```bash
qrust inventory
qrust inventory --older-than 14 --max-size 2GiB
qrust inventory --caches
```

`inventory` breaks the workspace's build directory down by category and lists what in it is stale (unused for `--older-than` days, 30 by default), with an estimate of what removing it would save and cost to rebuild. It never deletes anything. `--caches` adds the shared caches (Cargo's registry and git downloads).

To empty one workspace's build directory, run `cargo clean` in it.

## Remove other large things

- Toolchains are not part of the cache. List them with `qrust toolchain list` and remove the ones you no longer use with `qrust toolchain remove <name>`; releases a channel moved away from are removed by QuickRust a day after their last use.
- Programs installed with `qrust install` stay until you `qrust uninstall` them; their build sources are part of the cache.
- A target directory you chose yourself (`--target-dir`, `CARGO_TARGET_DIR`) is never collected: clean it with `cargo clean`.

Deleting `~/.qrust/cache` by hand also works, while no build is running: the next builds compile and download again what they need. A cache you moved stays where you put it: empty its directory rather than delete it, or QuickRust takes it for a disk that is not mounted.

## If a build runs out of space

When the disk is nearly full (below a quarter of `cache.min-free`), a command first collects for up to ten seconds and says so. If a build still fails with a full disk, the summary says how much is free and how much QuickRust's cache holds; free space with `qrust gc --to <size>` or elsewhere on the disk, then build again.

## Related

- [Configuration](../reference/configuration.md) for `cache.max-size` and `cache.min-free`
- [Files and directories](../reference/files.md) for what lives where under `~/.qrust`
