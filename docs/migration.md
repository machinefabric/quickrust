# Adopting QuickRust

QuickRust is adopted in stages. Each stage is reversible, none requires changing a
manifest, a lockfile or `.cargo/config.toml` (MIG-003, MIG-004, MIG-005).

## Stage 1 — observe without replacing anything

```bash
qrust doctor
qrust upstream build       # exactly the toolchain's Cargo
qrust build                # the same build, recorded
qrust analyze
```

`qrust doctor` prints where QuickRust is set up, the active toolchain and what selected
it, its Cargo (and the file QuickRust runs it as) and rustc, QuickRust's Cargo home, the `CARGO`, `RUSTC`, `RUSTC_WRAPPER` and
`RUSTC_WORKSPACE_WRAPPER` values, target and build directories, cache policy and
the capability set, and exits non-zero if something is wrong.

## Stage 2 — explicit commands

Use `qrust <cargo command>` wherever you would type `cargo`; `cargo` itself is
unchanged. Gate: `cargo test --test compat` passes in this repository, and your own
project builds, tests and runs identically.

## Stage 3 — set up

```bash
qrust setup                # ~/.qrust/bin first on PATH; `cargo` is now QuickRust
```

`qrust setup` copies QuickRust into `~/.qrust/bin`, where `cargo`, `rustc`, `rustdoc`,
`rustfmt`, `cargo-clippy`, `rust-analyzer` and the other toolchain programs are QuickRust
too; adds one line sourcing `~/.qrust/env` to `.profile` (and to `.bashrc`,
`.bash_profile` and `.zshenv` where they exist; fish gets `conf.d/qrust.fish`); imports
registry credentials and the `[registries] [registry] [source] [net] [http]` tables of
`~/.cargo/config.toml` once into QuickRust's Cargo home (`~/.qrust/cargo`); and installs
stable Rust. Running it again repairs what is missing. In CI, `qrust setup
--no-modify-path` and `export PATH="$HOME/.qrust/bin:$PATH"`. rustup and `~/.cargo`
are left as they are, and no longer used.

## Which toolchain runs

QuickRust installs and selects toolchains itself, in rustup's order:

1. `+<toolchain>` as the first argument;
2. `RUSTUP_TOOLCHAIN` (the name editors and tools set; a toolchain directory is accepted);
3. `rust-toolchain.toml` or `rust-toolchain` in the directory or the nearest one above;
4. the default (`qrust toolchain default`, `stable` until set).

A toolchain is `stable`, `beta`, `nightly` (optionally dated), a release (`1.98` or
`1.98.1`), or a name given with `qrust toolchain link`. It is installed on first use into
`~/.qrust/toolchains`, with the components and targets the toolchain file lists and the
standard library of every `--target`. QuickRust then runs that toolchain's own Cargo with QuickRust's
Cargo home (`~/.qrust/cargo`), `RUSTUP_TOOLCHAIN` naming the toolchain for nested calls, and
`~/.qrust/bin` (QuickRust as `cargo`, `rustc`, `rustfmt`, …) first on `PATH`. If a linked
toolchain's `cargo` turns out to be another copy of QuickRust, the nested copy detects it and
stops with a diagnostic. rustup and `~/.cargo` are never used.

## Uninstalling

```bash
qrust self uninstall --yes # removes ~/.qrust (toolchains, installed programs, caches, records) and the PATH lines
```

Files QuickRust may have written in a repository, only on request:
`.qrust/config.toml`, `.qrust/budgets.toml`, `.qrust/baselines/*.json` and report
paths you configured. `qrust bench` keeps its build directories in the workspace's
build directory (`$(qrust path)/qrust-bench/`), part of QuickRust's cache.

## Rollback

Remove `~/.qrust/bin` from `PATH`, or run `qrust self uninstall --yes`. QuickRust builds
into its own directory for each workspace (`qrust path`), so plain Cargo builds into
`target/` again from scratch, and the two never touch each other's artifacts. Pointing
plain Cargo at QuickRust's directory (`CARGO_TARGET_DIR=$(qrust path)`) shares it without
rebuilding: in fast mode their profile settings differ, so each keeps its own set of
artifacts there (Cargo names them by a hash of the settings); with `QRUST_MODE=compat`
both build the same artifacts.

One caveat: build scripts that emit `cargo::rerun-if-env-changed=RUSTC_WRAPPER`
rerun when you switch between QuickRust and plain Cargo, because QuickRust's wrapper is in
that variable. Set `QRUST_MEASUREMENT_ENABLED=false` to delegate without the
wrapper if that matters.
