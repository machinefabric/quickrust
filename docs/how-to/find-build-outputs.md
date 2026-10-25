# How to find build outputs without target/

This guide shows you how to find the binaries and other files a build produced, now that there is no `target/` directory in your project, and how to make a script or tool that expects one work.

QuickRust builds each workspace into a directory of its own cache, `~/.qrust/cache/builds/<name>-<id>/`. Why it does so is in [About owning the Rust environment](../explanation/owning-the-environment.md).

## Print the target directory

In the workspace, or any directory inside it:

```bash
qrust path
```

```text
/home/me/.qrust/cache/builds/app-fca2211b77f340ae
```

Everything Cargo would have put in `target/` is there, laid out the same way: `debug/`, `release/`, `<triple>/release/`, `doc/`, and so on.

## Print where a binary is

```bash
qrust path bin app                         # debug build
qrust path bin app --release
qrust path bin app --profile profiling
qrust path bin app --release --target aarch64-unknown-linux-musl
```

The path is where `cargo build` with the same options puts the binary; it adds `.exe` for Windows targets. It does not build anything, and the file exists only once you have built it.

To run a binary you have just built, `cargo run` is still the simplest way:

```bash
cargo run --release -- --help
```

## Update a script that hard-codes ./target

Replace the fixed path with what QuickRust prints:

```bash
# before
cp target/release/app dist/
# after
cp "$(qrust path bin app --release)" dist/
```

If the script runs under Cargo (a build script, an `xtask` started with `cargo run`, a Cargo subcommand), it does not need QuickRust at all: Cargo and every tool under it see the directory in `CARGO_TARGET_DIR`, and `cargo metadata` reports it as `target_directory`:

```bash
cargo metadata --format-version 1 --no-deps | jq -r .target_directory
```

## Keep the build in a directory of your choice

If a tool insists on a fixed location, tell Cargo where to build, as you would without QuickRust. Any of these wins over QuickRust's directory:

```bash
cargo build --target-dir out
cargo --config 'build.target-dir="out"' build
CARGO_TARGET_DIR=out cargo build
```

A directory you choose is used as it is: QuickRust never cleans it up or counts it towards its cache. `qrust path` prints it when you chose it through `CARGO_TARGET_DIR` (or `CARGO_BUILD_TARGET_DIR`) in the environment, and its own directory otherwise. `build.target-dir` in a `.cargo/config.toml` file is the one exception: QuickRust overrides it, so put the setting on the command line or in the environment instead.

## Build one workspace in several configurations

Some workspaces are built in more than one configuration that must not share output: against a staging and a production service whose address is compiled in, for two release channels, with and without a GPU feature. In one build directory they take turns: each build recompiles what the configuration changes and overwrites the binaries of the last, so a program you started from one configuration can be replaced under it by the next build.

Name each configuration a variant, and it gets a build directory of its own:

```bash
QRUST_BUILD_VARIANT=staging cargo build --release
QRUST_BUILD_VARIANT=prod    cargo build --release

QRUST_BUILD_VARIANT=staging qrust path bin app --release
```

```text
/home/me/.qrust/cache/builds/app-fca2211b77f340ae.staging/release/app
```

Every command run with the variant builds there and finds its outputs there: `cargo run`, `cargo test`, `qrust path`, tools that ask `cargo metadata`. A variant's name is at most 32 letters, digits, `-` or `_`. The variant can also be set as `build.variant` in a configuration file, for a checkout that is always one configuration.

A variant's directory is a build directory like any other: it is counted in QuickRust's cache, kept within `cache.max-size` and `cache.min-free`, and removed when it has not been used for a while. What the configurations have in common is not compiled twice: dependencies come from the artifact cache whichever variant builds them first.

## Use a build's outputs for longer than the build

A build directory is part of QuickRust's cache, and the cache keeps itself within its bounds: when it is over its size, or the disk is short of free space, it removes build directories that have not been used for a while, least recently used first. A directory a command is building in is never removed, nor one used in the last 15 minutes. That is enough for `cargo run` and `cargo test`, and for a script that copies a binary out right after building it.

It is not enough for a program that builds once and keeps using what it built: a test harness that starts the binaries an hour into its run, a packager that reads them at the end. Such a program holds the directory for as long as it runs, by asking for the path with its own process id:

```bash
bin=$(qrust path bin app --release --hold-for $$)
```

While that process runs, QuickRust's cache treats the directory as in use. The hold needs no release: it ends when the process does, however it ends. Asking again is harmless. With a build variant in effect, the variant's directory is the one held.

## Clean a workspace's build

```bash
cargo clean
```

This is Cargo's own `clean`, run on the workspace's directory in QuickRust's cache. You rarely need it for space: the cache removes build directories that have not been used for a while by itself (see [How to keep QuickRust's cache within your disk](free-disk-space.md)).

## Related

- [Files and directories](../reference/files.md)
- [Environment variables](../reference/environment.md) for what QuickRust sets for Cargo and the tools under it
