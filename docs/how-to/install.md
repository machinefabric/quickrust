# How to install, update and uninstall QuickRust

This guide shows you how to put QuickRust on a machine, keep it current, check that it works, and remove it again.

QuickRust is the only thing you install: it brings its own Rust toolchains, so you do not need rustup. For what setup changes and why, see [About owning the Rust environment](../explanation/owning-the-environment.md).

## Install a release

QuickRust is published in the MachineFabric package repository and as an archive per platform. Each of these puts the `qrust` program on your machine; `qrust setup` then makes it your `cargo`.

With Homebrew (macOS, Linux):

```bash
brew install machinefabric/tap/quickrust
qrust setup
```

With apt (Debian, Ubuntu):

```bash
curl -fsSL https://release.machinefabric.com/packages/keys/repo-signing-prod.asc \
  | sudo gpg --dearmor -o /usr/share/keyrings/machinefabric.gpg
echo "deb [signed-by=/usr/share/keyrings/machinefabric.gpg] https://release.machinefabric.com/packages/apt stable main" \
  | sudo tee /etc/apt/sources.list.d/machinefabric.list
sudo apt update && sudo apt install quickrust
qrust setup
```

With dnf (Fedora, RHEL):

```bash
sudo tee /etc/yum.repos.d/machinefabric.repo <<'REPO'
[machinefabric]
name=MachineFabric
baseurl=https://release.machinefabric.com/packages/rpm
enabled=1
gpgcheck=1
gpgkey=https://release.machinefabric.com/packages/keys/repo-signing-prod.asc
REPO
sudo dnf install quickrust
qrust setup
```

From an archive (any platform, and the way on Windows). Every release is listed in the release manifest, `https://release.machinefabric.com/quickrust/manifest`, with the platforms it was built for and the SHA-256 of each archive. The archive of a platform is at

```text
https://release.machinefabric.com/quickrust/release/<version>/qrust-<version>-<platform>.tar.gz
https://release.machinefabric.com/quickrust/release/<version>/qrust-<version>-windows-x86_64.zip
```

with `<platform>` one of `darwin-arm64`, `darwin-x86_64`, `linux-x86_64`, `linux-arm64` (both Linux builds are static and run on any distribution) (a `.tar.gz` each) and `windows-x86_64` (a `.zip`). Download it, compare its SHA-256 with the manifest's, unpack it and run setup from it:

```bash
version=0.150.0          # the release channel's latest version is in the manifest
platform=linux-x86_64
curl -fLO "https://release.machinefabric.com/quickrust/release/$version/qrust-$version-$platform.tar.gz"
sha256sum "qrust-$version-$platform.tar.gz"      # compare with the manifest's sha256 (macOS: shasum -a 256)
tar xzf "qrust-$version-$platform.tar.gz"          # unpacks quickrust/
quickrust/qrust setup
```

On Windows (PowerShell):

```powershell
$version = "0.150.0"
Invoke-WebRequest "https://release.machinefabric.com/quickrust/release/$version/qrust-$version-windows-x86_64.zip" -OutFile qrust.zip
(Get-FileHash qrust.zip -Algorithm SHA256).Hash      # compare with the manifest's sha256
Expand-Archive qrust.zip -DestinationPath .          # unpacks quickrust\
.\quickrust\qrust.exe setup
```

`qrust setup` copies `qrust` and `qrust-rustc-wrapper` into `~/.qrust/bin` and installs stable Rust, so the unpacked directory can be deleted afterwards, and a copy installed by a package manager is only where setup was started from. Arguments choose what setup does ([below](#choose-what-setup-does)):

```bash
qrust setup --no-modify-path
```

If the manifest does not list your platform for a release, that release has no build for it: install from source instead.

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

It installs the latest release of the channel this QuickRust was published on (`release` or `nightly`; `qrust doctor` says which, on its `release line`), in place of the running one in `~/.qrust/bin`. If you are already on the latest release, it says so and changes nothing.

Nothing is installed on trust. QuickRust reads the release manifest and the signature published beside it, and accepts the manifest only if its signature chains to the root keys this QuickRust was built with: a release key certified by two of those roots, for the right environment, inside its validity, must have signed the manifest's exact bytes. The archive must then be the one the manifest describes (SHA-256 and size), and the binary in it must run and report the version it was published as. If any of these fails, the update stops with the reason and replaces nothing. See [Security and privacy](../explanation/security.md#updates).

A QuickRust built from source has no root keys, and so no release it could check: `qrust self update` says so. Update it by building the newer source.

If your releases come from a mirror of the registry, set `self.update-url` (or `QRUST_UPDATE_URL`) to it: the mirror serves `<url>/quickrust/manifest` and `<url>/quickrust/manifest.sig` as the registry does. A mirror is trusted exactly as the registry is: by the signature, not by where it is.

A copy installed with a package manager is updated by that manager (`brew upgrade quickrust`, `sudo apt upgrade quickrust`, `sudo dnf upgrade quickrust`); it is the copy `qrust setup` was started from, not the one that runs. Run `qrust self update` to update the one that runs.

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
