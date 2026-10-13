# How to install, update and uninstall QuickRust

This guide shows you how to put QuickRust on a machine, keep it current, check that it works, and remove it again.

QuickRust is the only thing you install: it brings its own Rust toolchains, so you do not need rustup. For what setup changes and why, see [About owning the Rust environment](../explanation/owning-the-environment.md).

## Install a release

On Linux and macOS:

```bash
curl -fsSL https://github.com/machinefabric/quickrust/releases/latest/download/install.sh | sh
```

On Windows (PowerShell, x86_64):

```powershell
irm https://github.com/machinefabric/quickrust/releases/latest/download/install.ps1 | iex
```

The script downloads the build for your machine (static on Linux, so any distribution works), checks it against its published checksum and runs `qrust setup`. Arguments after `sh -s --` are passed on to `qrust setup`:

```bash
curl -fsSL https://github.com/machinefabric/quickrust/releases/latest/download/install.sh | sh -s -- --no-modify-path
```

If the script stops with "no release for ...", your platform has no published build: install from source instead.

When setup has finished, open a new shell, or source the file it names:

```bash
. "$HOME/.qrust/env"
```

## Install from source

You need a Rust toolchain of 1.80 or newer for this one build:

```bash
git clone https://github.com/machinefabric/quickrust
cd quickrust
cargo build --release --locked
target/release/qrust setup
```

Keep `--locked`: without it Cargo picks the newest dependency versions, and some of them need a newer Rust than 1.80. `qrust setup` copies `qrust` and `qrust-rustc-wrapper` into `~/.qrust/bin`, so the checkout can be deleted afterwards.

## Choose what setup does

- If you manage `PATH` yourself (dotfiles, CI, containers), pass `--no-modify-path` and put `~/.qrust/bin` first on `PATH` yourself:

  ```bash
  qrust setup --no-modify-path
  export PATH="$HOME/.qrust/bin:$PATH"
  ```

- If you want a different first toolchain than stable, pass it: `qrust setup --toolchain 1.98`. If you want none installed now, pass `--toolchain none`; the first build then installs the toolchain it needs.
- If you want QuickRust's home somewhere other than `~/.qrust`, set `QRUST_HOME` before running setup and keep it set in every shell.

Setup imports, once, what is yours from `~/.cargo` (or `$CARGO_HOME`): registry credentials and the `[registries]`, `[registry]`, `[source]`, `[net]` and `[http]` tables of its `config.toml`. It prints what it imported and what it did not. A table it did not import (for example `[build]`) still applies to projects under your home directory, because Cargo reads `~/.cargo/config.toml` as the configuration of every directory below it; for projects elsewhere it does not apply. If you want such a table for every build, copy it into `~/.qrust/cargo/config.toml`. Running `qrust setup` again repairs a broken installation and never imports a second time.

If setup stops because an entry in `~/.qrust/bin` "exists and is not QuickRust", move that file away and run setup again.

## Check the installation

```bash
qrust doctor
```

`doctor` prints which QuickRust runs, the toolchain the current directory uses and what selected it, the Cargo and rustc behind it, the relevant environment variables and the workspace's target directory. It ends with `OK`, or with `FAIL` and the number of problems (exit status 1), each problem on its own `FAIL` line above.

If it reports that `cargo` on `PATH` is not QuickRust's, open a new shell; if that does not help, another directory (often `~/.cargo/bin`) comes before `~/.qrust/bin` in your startup files.

## Update QuickRust

```bash
qrust self update
```

It installs the latest published release in place of the running one, after checking its checksum and that the new binary runs and reports the version it was published as. If you are already on the latest release, it says so and changes nothing.

If your releases come from a mirror, set `self.update-url` (or `QRUST_UPDATE_URL`) to the directory that holds `version.txt` and the `qrust-<target>.tar.gz` archives.

Toolchains are updated separately: see [How to choose and manage toolchains](choose-a-toolchain.md#keep-toolchains-current).

## Uninstall QuickRust

```bash
qrust self uninstall --yes
```

This removes `~/.qrust` with everything in it (toolchains, programs installed with `qrust install`, caches, run records) and the lines setup added to your shell startup files (on Windows, the user `PATH` entry). Without `--yes` it only tells you what it would remove.

Files QuickRust wrote into a repository at your request stay where they are: `.qrust/config.toml`, `.qrust/budgets.toml`, `.qrust/baselines/` and any report paths you configured. Delete them yourself if you no longer want them.

rustup and `~/.cargo` are never touched by setup or uninstall; if you had them before, plain Cargo works again as soon as QuickRust is gone.

## Related

- [How to try QuickRust before making it your cargo](adopt-gradually.md)
- [How to run QuickRust in CI](set-up-ci.md)
- [Files and directories](../reference/files.md) for everything under `~/.qrust`
- [Commands](../reference/commands.md) for every option of `setup`, `doctor` and `self`
