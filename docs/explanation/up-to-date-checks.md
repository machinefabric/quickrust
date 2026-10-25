# How QuickRust knows a build is up to date

Run `cargo build` twice in a terminal through QuickRust and the second time it answers at once:

```text
OK   Up to date: 1 fresh
```

Cargo was not started. This page explains how QuickRust can give that answer without asking Cargo, why it is willing to, and what keeps the answer honest.

## Why not just ask Cargo

Asking Cargo is correct and not free. Even with nothing to do, Cargo loads the manifests and lockfile, resolves the dependency graph and checks every unit's fingerprint; on the axum benchmark service that is about 120 ms. Through QuickRust there is also the session around it. For the commonest command a developer types, after saving a file that did not change anything, or before running a program that is already built, that delay is the whole experience.

QuickRust already knows, from the build it just rendered, everything that build read. If none of it has changed, the build has nothing to do, and the answer can be given from that record: 21 ms instead of 123 ms on axum.

## The stamp

After a build QuickRust rendered in a terminal succeeds, it writes a stamp: a list of files with their sizes and modification times, under a key.

The key holds everything besides files that decides what the build does: the exact arguments, the working directory, every environment variable (except the shell's own bookkeeping, `OLDPWD` and `_`, which change without changing anything a build reads), the environment QuickRust gives Cargo, and the identity of both QuickRust's binary and the toolchain's Cargo. A different command line, a different variable or a new QuickRust release is a different key, with a stamp of its own.

The files are everything the build's freshness depends on that QuickRust can name:

- the sources each unit's compiler read, from rustc's dep-info;
- each unit's outputs and executables, so a deleted program is noticed;
- for each local build script, the inputs it declared with `rerun-if-changed`, a directory meaning everything beneath it; for a script that declared nothing, every file of its package, because that is what Cargo watches;
- the manifests, the workspace's `Cargo.toml` and `Cargo.lock`;
- Cargo's configuration files, present or absent, in every directory Cargo reads them from, and in QuickRust's Cargo home;
- the toolchain files (`rust-toolchain.toml`, `rust-toolchain`) in every directory up from the working directory, the record of which release each channel names, and the compiler itself.

Registry and git sources are left out: they live in QuickRust's Cargo home under names that include their version, and never change in place.

A file is recorded as absent when it does not exist, so creating a `.cargo/config.toml` that was not there is as visible as editing one. A workspace keeps stamps for up to 32 distinct command lines; the least recently written go first.

## When QuickRust judges

The next time the same command runs, with the same key, QuickRust checks each stamped file's size and modification time. If every one is as recorded, the build is up to date, and QuickRust prints the count of fresh units and the warnings line that build had, so a warning does not disappear just because nothing was rebuilt. The stamp keeps that build's stored output beside it, and the up-to-date run holds it as its own, so the `qrust warnings` the line points to shows those warnings.

It only does this where it renders the build, which means in a terminal with QuickRust's output on. A pipe, an IDE, a CI log, `-v`, `-q`, `--message-format`, or compat mode with Cargo's output all get Cargo's own answer every time; there, Cargo's output is the interface, and QuickRust does not stand in for it.

What happens next depends on the command:

- For `build`, `check` and `clippy`, QuickRust's verdict is the answer, and Cargo is not started.
- For `run`, `test` and `bench`, QuickRust normally builds first and then runs the command ([build output](build-output.md)). When the stamp says the build is up to date, the build phase is skipped and the command runs at once. Cargo still checks freshness itself in that command, so a stamp that missed an input can only mean the build happens in Cargo's words instead of QuickRust's, never that a stale program runs.

## Why size and modification time are enough

The stamp compares what Cargo compares. Cargo decides that a local source changed by its modification time against the outputs', not by its content; touching a file without changing it makes Cargo rebuild, and the stamp agrees, because the modification time changed. Content hashing would be slower and would sometimes call a build up to date that Cargo would not, which is the one direction QuickRust must never err in. The size is recorded alongside, at no extra cost, since both come from the same file-system call.

## What keeps it honest

A verdict that is wrong once would teach users to distrust every "Up to date" after it, so this feature has a gate of its own. The test `test0231_qrust_says_up_to_date_only_when_cargo_would_build_nothing` builds a small workspace with two kinds of build script, one that declares nothing and one that declares a directory, then applies a series of changes: editing and touching sources, editing a build script's undeclared input, editing another file of its package, adding a file to a declared directory, editing a file the script did not declare, setting an environment variable the program reads, adding and removing Cargo configuration, changing a manifest, deleting the built program, and changing nothing. After each, it builds through QuickRust in a terminal. Whenever QuickRust says up to date, the test asks Cargo directly, through a pipe where QuickRust never judges, and fails if Cargo finds any work. It also requires that QuickRust does answer when nothing changed, which is the point of judging at all.

The design admits one kind of miss on purpose: an input QuickRust cannot name, such as a file a procedural macro reads without declaring it. Cargo cannot see those either, so in that case QuickRust's answer and Cargo's agree, and both are stale in the same way that plain Cargo is.
