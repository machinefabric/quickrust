# How to try QuickRust before making it your cargo

This guide shows you how to adopt QuickRust in stages, each one reversible, and how to go back to plain Cargo.

None of the stages changes a manifest, a lockfile or a `.cargo/config.toml`.

## Stage 1: compare a build with and without QuickRust

Install QuickRust without letting it take over `cargo`, and make only `qrust` available to your shell:

```bash
qrust setup --no-modify-path
alias qrust="$HOME/.qrust/bin/qrust"
```

Then, in your project:

```bash
qrust doctor               # which toolchain, Cargo and target directory QuickRust would use
qrust upstream build       # the toolchain's own Cargo: no QuickRust settings, nothing recorded
qrust build                # the same build through QuickRust, recorded
qrust analyze              # what QuickRust saw
```

Your `cargo` is still whatever it was before. If `qrust build` fails where `qrust upstream build` succeeds, run it again with `QRUST_MODE=compat` (exactly upstream's build) to tell a fast-mode setting from anything else, and see [How to build exactly as Cargo does](build-exactly-like-cargo.md).

## Stage 2: use qrust instead of cargo by hand

Type `qrust <command>` wherever you would type `cargo <command>`: `qrust test`, `qrust run -- --flag`, `qrust clippy`. Every Cargo command and flag works; unknown ones go to Cargo, which decides whether they are valid. The exceptions are the names QuickRust uses for commands of its own: `qrust fix`, `qrust report`, `qrust help` and `qrust version` are QuickRust's, so for Cargo's `fix` or `report` type `qrust upstream fix` (or, after stage 3, `cargo fix`).

Stay here until your project builds, tests and runs the same way as before.

## Stage 3: make cargo QuickRust

```bash
qrust setup
```

This time setup puts `~/.qrust/bin` first on `PATH`, where `cargo`, `rustc`, `rustfmt`, `cargo-clippy`, `rust-analyzer` and the other toolchain programs are QuickRust. Open a new shell. Editors and other tools that run `cargo` now run QuickRust too.

If a tool must keep using plain Cargo, give it the path of your old Cargo (for example `~/.cargo/bin/cargo`); QuickRust does not change rustup or `~/.cargo`.

## Go back to plain Cargo

To stop using QuickRust for a while, take `~/.qrust/bin` off `PATH` (remove the line `qrust setup` added to `~/.profile` and the other startup files, or start a shell without it). To remove it completely, run `qrust self uninstall --yes`.

Plain Cargo builds into `target/` again, from scratch: QuickRust's builds live in its own directory for each workspace, and the two never touch each other's artifacts.

## Share build output between QuickRust and plain Cargo

If you switch back and forth and do not want plain Cargo to start from scratch, point it at QuickRust's directory:

```bash
CARGO_TARGET_DIR="$(qrust path)" ~/.cargo/bin/cargo build
```

In fast mode the two use different profile settings, so each keeps its own set of artifacts in that directory and neither rebuilds the other's. With `QRUST_MODE=compat` both build the same artifacts and share them.

## If build scripts rerun when you switch

A build script that declares `cargo::rerun-if-env-changed=RUSTC_WRAPPER` reruns every time you alternate between QuickRust and plain Cargo, because QuickRust's rustc wrapper is in that variable. If that matters to you, run QuickRust without its wrapper while you alternate:

```bash
QRUST_MEASUREMENT_ENABLED=false cargo build
```

This also turns off what needs the wrapper: rustc-level observation and the fast-mode settings it applies (the artifact cache, the parallel frontend, early cut-off and the fast linker).

## Related

- [How to install, update and uninstall QuickRust](install.md)
- [How to find build outputs without target/](find-build-outputs.md)
- [Compatibility](../reference/compatibility.md) for exactly what passes through unchanged
- [Why QuickRust is fast by default](../explanation/fast-by-default.md)
