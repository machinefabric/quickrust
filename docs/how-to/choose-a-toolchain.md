# How to choose and manage toolchains

This guide shows you how to decide which Rust toolchain a build uses, add components and targets to it, use a toolchain you built yourself, and control updates.

QuickRust installs every toolchain it needs on first use, so most of the steps below only state what you want; the next command that needs it installs it. The full list of toolchain commands is in [Commands](../reference/commands.md).

## See which toolchain a directory uses

```bash
qrust toolchain list
```

The last two lines say what the default is and which toolchain the current directory uses, and why:

```text
default: stable
here:    1.80 (from /home/me/app/rust-toolchain.toml)
```

`qrust toolchain which` prints that toolchain's directory, and `qrust toolchain which rustc` the path of one of its programs.

QuickRust chooses in rustup's order: `+<toolchain>` as the first argument, then `RUSTUP_TOOLCHAIN`, then a toolchain file in the directory or the nearest one above it, then the default.

## Pin a toolchain for a project

Put a `rust-toolchain.toml` at the root of the project:

```toml
[toolchain]
channel = "1.98"
components = ["rust-src", "llvm-tools"]
targets = ["wasm32-unknown-unknown"]
profile = "minimal"
```

Only `channel` is required. The next command in that directory installs the toolchain, the listed components and the listed targets' standard libraries. A key the format does not have is an error, and so is a directory holding both `rust-toolchain` and `rust-toolchain.toml`.

`channel` takes `stable`, `beta`, `nightly`, a dated nightly (`nightly-2026-10-11`), a minor release (`1.98`, which follows its patch releases) or an exact release (`1.98.1`), optionally followed by a host triple. If the toolchain lives in a directory of its own, use `path = "../my-toolchain"` instead of `channel`; components, targets and profile do not apply to it.

## Use another toolchain for one command

```bash
cargo +nightly build
cargo +1.80 test
RUSTUP_TOOLCHAIN=beta cargo check
```

Editors and tools that set `RUSTUP_TOOLCHAIN`, including to a toolchain's directory, work the same way.

## Change the default

```bash
qrust toolchain default 1.98
qrust toolchain default            # prints the current default
```

The default applies wherever no `+toolchain`, `RUSTUP_TOOLCHAIN` or toolchain file decides. It can be a channel, a release or the name of a linked toolchain.

## Add components and targets

```bash
qrust toolchain component add rust-analyzer llvm-tools
qrust toolchain target add aarch64-unknown-linux-musl
qrust toolchain target add wasm32-unknown-unknown --toolchain nightly
```

Without `--toolchain`, they change the toolchain the current directory uses. `remove` instead of `add` takes them out again.

You rarely need `target add`: a build with `--target <triple>` installs that target's standard library itself.

## Install toolchains ahead of time

```bash
qrust toolchain install stable 1.80 nightly
qrust toolchain install nightly --profile minimal --component miri --target wasm32-unknown-unknown
```

Do this where the first build must not wait for a download, for example when building a container image. `--profile` is `minimal` (rustc, Cargo, the standard library), `default` (the default: `minimal` with clippy, rustfmt and rust-src) or `complete`.

## Use a toolchain you built yourself

```bash
qrust toolchain link dev ~/src/rust/build/host/stage1
cargo +dev build
```

The directory must contain `bin/rustc` and `bin/cargo`. A linked toolchain is used as it is: QuickRust never installs components or targets into it, and `qrust toolchain unlink dev` only forgets the name.

## Download toolchains from a mirror

If your network cannot reach static.rust-lang.org, point QuickRust at a mirror of it, in `~/.qrust/config.toml`:

```toml
[toolchain]
dist-server = "https://rust-mirror.example.com"
```

or for one shell with `QRUST_DIST_SERVER`. The mirror must serve the same layout (`dist/channel-rust-<channel>.toml` and the archives it names).

## Keep toolchains current

Once a day after a build, QuickRust checks in the background whether the channel the build used (`stable`, `beta`, `nightly`, `1.98`) has a newer release, and installs it for the next build. Toolchains asked for by an exact name (`1.98.1`, `nightly-2026-10-11`) never move.

- To update now: `qrust toolchain update`, or `qrust toolchain update --channel stable` for one channel.
- To turn the background check off, set `toolchain.auto-update = "off"` in `~/.qrust/config.toml` or `QRUST_TOOLCHAIN_AUTO_UPDATE=off`. It never runs in CI.
- To stay on one release, pin it with an exact version in `rust-toolchain.toml`.

A release a channel left behind is removed after a day in which nothing used it. To remove a toolchain yourself:

```bash
qrust toolchain remove nightly                         # the release the channel names
qrust toolchain remove 1.97.0-x86_64-unknown-linux-gnu
```

A removed toolchain is installed again the next time something asks for it.

## Related

- [About owning the Rust environment](../explanation/owning-the-environment.md) for why QuickRust replaces rustup
- [Files and directories](../reference/files.md) for where toolchains are kept
- [Configuration](../reference/configuration.md) for `toolchain.auto-update` and `toolchain.dist-server`
