# Adopting Haul

Haul is adopted in stages. Each stage is reversible, none requires changing a
manifest, a lockfile or `.cargo/config.toml` (MIG-003, MIG-004, MIG-005).

## Stage 1 — observe without replacing anything

```bash
haul doctor
haul upstream build        # exactly the toolchain's Cargo
haul build                 # the same build, recorded
haul analyze
```

`haul doctor` prints the shim path, the active toolchain and what selected it, its
Cargo and rustc, Haul's Cargo home, the `CARGO`, `RUSTC`, `RUSTC_WRAPPER` and
`RUSTC_WORKSPACE_WRAPPER` values, target and build directories, cache policy and
the capability set, and exits non-zero if something is wrong.

## Stage 2 — explicit commands

Use `haul <cargo command>` wherever you would type `cargo`; `cargo` itself is
unchanged. Gate: `cargo test --test compat` passes in this repository, and your own
project builds, tests and runs identically.

## Stage 3 — set up

```bash
haul setup                 # ~/.haul/bin first on PATH; `cargo` is now Haul
```

`haul setup` copies Haul into `~/.haul/bin`, where `cargo`, `rustc`, `rustdoc`,
`rustfmt`, `cargo-clippy`, `rust-analyzer` and the other toolchain programs are Haul
too; adds one line sourcing `~/.haul/env` to `.profile` (and to `.bashrc`,
`.bash_profile` and `.zshenv` where they exist; fish gets `conf.d/haul.fish`); imports
registry credentials and the `[registries] [registry] [source] [net] [http]` tables of
`~/.cargo/config.toml` once into Haul's Cargo home (`~/.haul/cargo`); and installs
stable Rust. Running it again repairs what is missing. In CI, `haul setup
--no-modify-path` and `export PATH="$HOME/.haul/bin:$PATH"`. rustup and `~/.cargo`
are left as they are, and no longer used.

## Which toolchain runs

Haul installs and selects toolchains itself, in rustup's order:

1. `+<toolchain>` as the first argument;
2. `RUSTUP_TOOLCHAIN` (the name editors and tools set; a toolchain directory is accepted);
3. `rust-toolchain.toml` or `rust-toolchain` in the directory or the nearest one above;
4. the default (`haul toolchain default`, `stable` until set).

A toolchain is `stable`, `beta`, `nightly` (optionally dated), a release (`1.98` or
`1.98.1`), or a name given with `haul toolchain link`. It is installed on first use into
`~/.haul/toolchains`, with the components and targets the toolchain file lists and the
standard library of every `--target`. Haul then runs that toolchain's own Cargo with Haul's
Cargo home (`~/.haul/cargo`), `RUSTUP_TOOLCHAIN` naming the toolchain for nested calls, and
`~/.haul/bin` (Haul as `cargo`, `rustc`, `rustfmt`, …) first on `PATH`. If a linked
toolchain's `cargo` turns out to be another copy of Haul, the nested copy detects it and
stops with a diagnostic. rustup and `~/.cargo` are never used.

## Uninstalling

```bash
haul self uninstall --yes  # removes ~/.haul (toolchains, installed programs, caches, records) and the PATH lines
```

Files Haul may have written in a repository, only on request:
`.haul/config.toml`, `.haul/budgets.toml`, `.haul/baselines/*.json`, report
paths you configured, and `target/haul-bench/` (benchmark build directories,
marked with `.haul-owned`; safe to delete).

## Rollback

Remove `~/.haul/bin` from `PATH`, or run `haul self uninstall --yes`. Haul builds
into its own directory for each workspace (`haul path`), so plain Cargo builds into
`target/` again from scratch, and the two never touch each other's artifacts. Pointing
plain Cargo at Haul's directory (`CARGO_TARGET_DIR=$(haul path)`) shares it without
rebuilding: in fast mode their profile settings differ, so each keeps its own set of
artifacts there (Cargo names them by a hash of the settings); with `HAUL_MODE=compat`
both build the same artifacts.

One caveat: build scripts that emit `cargo::rerun-if-env-changed=RUSTC_WRAPPER`
rerun when you switch between Haul and plain Cargo, because Haul's wrapper is in
that variable. Set `HAUL_MEASUREMENT_ENABLED=false` to delegate without the
wrapper if that matters.
