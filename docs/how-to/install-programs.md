# How to install programs with qrust install

This guide shows you how to install Rust programs from crates.io, another registry, a Git repository or a local directory, and how to list, update and remove them.

`cargo install` and `qrust install` are the same command. QuickRust builds the program like any other build (through its artifact cache, so dependencies you have built before are not compiled again) and puts the binaries into `~/.qrust/bin`, which `qrust setup` put on your `PATH`.

## Install from crates.io

```bash
cargo install ripgrep
cargo install ripgrep@14.1.1          # an exact version
cargo install ripgrep --version '^14' # a version requirement
cargo install cargo-nextest --locked  # with the lockfile the crate was published with
```

Without a version, QuickRust installs the newest release that is not yanked, not a pre-release, and works with the toolchain's Rust version.

Use `--locked` when the program's authors recommend it: the build then uses the dependency versions they published. If the crate was published without a lockfile, QuickRust says so and resolves dependencies afresh.

Several crates at once install one after the other; a summary line says how many succeeded, and the exit status is non-zero if any failed:

```bash
cargo install ripgrep fd-find bat
```

## Install from another registry

```bash
cargo install mytool --registry internal
```

The registry must be configured in QuickRust's Cargo home, `~/.qrust/cargo/config.toml`, where `qrust setup` imported your registries; its token comes from `~/.qrust/cargo/credentials.toml` or the environment, as for Cargo. A project's own `.cargo/config.toml` is not consulted, since the release is resolved outside any project. Source replacement (`[source]` with `replace-with`) applies as in a build. `--index URL` is not supported: name the registry in the configuration and use `--registry`.

## Install from Git or a local directory

```bash
cargo install --git https://github.com/BurntSushi/ripgrep
cargo install --git https://github.com/BurntSushi/ripgrep --tag 14.1.1
cargo install --git https://example.com/tools.git --branch main mytool   # name the package when the repository has several
cargo install --path .
```

`--git` needs `git` on `PATH`. `--path` builds the package where it is; if the directory is a workspace with several packages that have binaries, name the one you want.

## Choose what gets built and where it goes

- `--bin NAME` (repeatable) or `--bins`, `--example NAME` or `--examples` select the targets.
- `--features F`, `--all-features`, `--no-default-features` select features.
- `--profile P` or `--debug` change the profile (release by default); `--target T` cross-compiles.
- `--root DIR` installs into `DIR/bin` instead of `~/.qrust/bin`. `CARGO_INSTALL_ROOT` is not used: if it is set, the command stops and asks for `--root`.
- `-j`, `--config`, `--offline`, `--frozen`, `--ignore-rust-version`, `-q` and `-v` are passed to the build.

## Replace or reinstall

Installing a program again with the same version, source, features, profile and target does nothing:

```text
OK   tool 0.1.0 is installed already (--force installs it again) [10ms]
```

A newer version, or different options, installs over the old one. QuickRust refuses to overwrite a file in the bin directory that another package installed, or that it did not install at all, unless you pass `--force`. It never installs over its own programs (`cargo`, `rustc`, `qrust` and the other toolchain names), even with `--force`.

## List and remove programs

```bash
cargo install --list
cargo uninstall ripgrep
cargo uninstall ripgrep --bin rg      # one binary of a package
cargo install --list --root ~/tools   # with --root, for that root
```

```text
tool v0.1.0 (/home/me/src/tool):
    tool
```

## Related

- [Commands](../reference/commands.md) for every option of `install` and `uninstall`
- [Files and directories](../reference/files.md) for `installs.json` and the install sources in the cache
