# QuickRust

This is the first public release of QuickRust. It was developed inside MachineFabric for our own
Rust builds, and it is now available to everyone.

## What it is

- QuickRust is Cargo, faster: it runs every Cargo command, and manages the Rust toolchains, a
  build cache shared by all your projects, and the build output, within bounds you set.
- `qrust setup` makes it your `cargo`. Projects, manifests, lock files and scripts stay as they
  are.

## Installing

- `brew install machinefabric/tap/quickrust`, `apt install quickrust`, `dnf install quickrust`,
  or an archive for macOS (Apple Silicon and Intel), Linux (x86_64 and arm64, static) and Windows
  (x86_64). Each puts the `qrust` program on the machine; `qrust setup` does the rest.
- An archive is `qrust-<version>-<platform>.tar.gz` (`.zip` for Windows) and unpacks to one
  directory, `quickrust/`, holding `qrust`, `qrust-rustc-wrapper`, the licences and the README.

## Updating

- `qrust self update` installs the latest release of the channel this QuickRust was published
  on, and only a release its registry signed: the release manifest's signature must chain to the
  root keys QuickRust was built with, the archive must be the one the manifest describes, and
  the program in it must report the version it was published as. Anything else stops the update
  with the reason and replaces nothing. It never installs an older release.
- `qrust doctor` shows the release line: the channel and registry updates come from.
- A QuickRust built from source has no release line, and `qrust self update` says so.
- `self.update-url` (`QRUST_UPDATE_URL`) names a mirror of the registry. A mirror is trusted as
  the registry is: by the signature, not by where it is.
