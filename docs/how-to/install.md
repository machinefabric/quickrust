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
baseurl=https://release.machinefabric.com/packages/rpm/$basearch
enabled=1
gpgcheck=1
repo_gpgcheck=1
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

`qrust setup` makes `~/.qrust/bin` QuickRust's and installs stable Rust. How depends on what installed the `qrust` you run it from:

- **A package manager** (Homebrew, apt, dnf): setup copies nothing. `~/.qrust/bin/qrust`, `qrust-rustc-wrapper`, `cargo` and the other toolchain programs are links to the package's `qrust`, by the path that stays the same across its versions (Homebrew's `opt/quickrust`). There is one `qrust` on the machine, and the package manager's upgrade upgrades the one that runs. A copy an earlier setup put in `~/.qrust/bin` is replaced by the link, and setup says so.
- **An archive or a source build**: setup copies `qrust` and `qrust-rustc-wrapper` into `~/.qrust/bin`, and that copy is your installation, so the unpacked directory or the checkout can be deleted afterwards. It does not set up over a package's installation: remove the package first.

Arguments choose what setup does ([below](#choose-what-setup-does)):

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

`doctor` prints which QuickRust runs and what installed it, the toolchain the current directory uses and what selected it, the Cargo and rustc behind it, the relevant environment variables and the workspace's target directory. It ends with `OK`, or with `FAIL` and the number of problems (exit status 1), each problem on its own `FAIL` line above.

It names every `qrust` or `cargo` that comes before QuickRust's on `PATH` and is another program, and the line of your shell's startup files that puts its directory there (following the files they source). Common ones are `~/.cargo/bin` (rustup's `cargo`), and Homebrew's `bin` from a `brew shellenv` line that runs after `~/.qrust/env`: its `cargo` if Homebrew's `rust` formula is installed, and its `qrust` if `~/.qrust/bin` still holds a copy of QuickRust from before (run `qrust setup` from the Homebrew installation, and the copy becomes a link to it). Move the line that sources `~/.qrust/env` after the one it names, or remove what it names. If `doctor` says no `cargo` on `PATH` is QuickRust's, open a new shell.

## Update QuickRust

```bash
qrust self update
```

It installs the latest release of the channel this QuickRust was published on (`release` or `nightly`; `qrust doctor` says which, on its `release line`), in place of the running one in `~/.qrust/bin`. If you are already on the latest release, it says so and changes nothing.

A QuickRust a package manager installed is updated by that manager, and `qrust self update` changes nothing and names the command: `brew upgrade quickrust`, `sudo apt install --only-upgrade quickrust` or `sudo dnf upgrade quickrust`. Since `~/.qrust/bin` links to the package's `qrust`, that upgrade updates the QuickRust that runs.

Nothing is installed on trust. QuickRust first reads the registry's charter, which must be signed by MachineFabric and say where the release manifest is; then the release manifest and the signature published beside it, and accepts the manifest only if its signature chains to the root keys this QuickRust was built with: a release key certified by two of those roots, for the right environment, inside its validity, must have signed the manifest's exact bytes. The archive must then be the one the manifest describes (SHA-256 and size), and the binary in it must run and report the version it was published as. If any of these fails, the update stops with the reason and replaces nothing. See [Security and privacy](../explanation/security.md#updates).

A QuickRust built from source has no root keys, and so no release it could check: `qrust self update` says so. Update it by building the newer source.

If your releases come from another release registry, set `self.update-url` (or `QRUST_UPDATE_URL`) to its anchor. It is read exactly as the registry this QuickRust was published in: only once MachineFabric has chartered it (its charter is at `<url>/charter.json`, for this QuickRust's environment), and then by the signature, not by where it is.

Toolchains are updated separately: see [How to choose and manage toolchains](choose-a-toolchain.md#keep-toolchains-current).

### Know when there is a new release

QuickRust tells you itself. Once a day, after a build, it checks in the background whether a newer release is published on its channel, reading the same signed manifest `qrust self update` installs from. The next build in a terminal then ends with a note such as:

```text
NOTE QuickRust 1.170.0 is available (this is 1.164.1495): run `qrust self update` to update
```

For a package manager's installation, the note names that manager's command instead, such as `brew upgrade quickrust`. It is shown at most once a day, and not at all once you are on that release.

The check never holds up a build. It runs as a separate low-priority process, each of its requests is one attempt of at most 10 seconds, and its result is only told after a later build. When it cannot be made, for example offline, or behind a proxy that refuses it, or with the registry unreachable, nothing is said and it is tried again the next day. It never runs in CI, and a QuickRust built from source has no release line to check.

To check now, run `qrust self check`: it says whether a newer release is published and what to run, or why it could not tell. `qrust doctor` shows the newest release the last check found, and when it ran.

To turn the background check off, set `self.update-check = "off"` in `~/.qrust/config.toml`, or `QRUST_SELF_UPDATE_CHECK=off` in the environment. The check sends nothing about you or your builds: it reads the registry's charter, the manifest and their signatures, with `qrust/<version>` as its user agent ([Security and privacy](../explanation/security.md#updates)).

## Uninstall QuickRust

```bash
qrust self uninstall --yes
```

This removes `~/.qrust` with everything in it (toolchains, programs installed with `qrust install`, caches, run records) and the lines setup added to your shell startup files (on Windows, the user `PATH` entry). Without `--yes` it only tells you what it would remove.

A QuickRust a package manager installed is removed by that manager (`brew uninstall quickrust`, `sudo apt remove quickrust`, `sudo dnf remove quickrust`), and `qrust self uninstall` changes nothing and names the command. The package manager removes the program only: delete `~/.qrust` and the lines marked `# Added by `qrust setup`` in your shell startup files yourself.

Files QuickRust wrote into a repository at your request stay where they are: `.qrust/config.toml`, `.qrust/budgets.toml`, `.qrust/baselines/` and any report paths you configured. Delete them yourself if you no longer want them.

rustup and `~/.cargo` are never touched by setup or uninstall; if you had them before, plain Cargo works again as soon as QuickRust is gone.

## Related

- [How to try QuickRust before making it your cargo](adopt-gradually.md)
- [How to run QuickRust in CI](set-up-ci.md)
- [Files and directories](../reference/files.md) for everything under `~/.qrust`
- [Commands](../reference/commands.md) for every option of `setup`, `doctor` and `self`
