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

## Stage 2 — explicit wrapper

Use `haul <cargo command>` wherever you would type `cargo`. Upstream `cargo`
stays the default executable. Gate: `cargo test --test compat` passes in this
repository, and your own project builds, tests and runs identically.

## Stage 3 — CI shim

```bash
haul install-shim --dir "$RUNNER_TEMP/haul-shim"
export PATH="$RUNNER_TEMP/haul-shim:$PATH"
cargo build --locked       # now Haul
```

## Stage 4 — project launcher

Put the shim directory first on `PATH` in the project's dev shell, devcontainer
or equivalent. `haul upstream <args>` remains the escape hatch.

## Stage 5 — machine-wide (optional)

Put the shim directory first on `PATH` in your shell profile. Haul never
replaces rustup's own `~/.cargo/bin/cargo`: `install-shim` refuses to overwrite
any existing `cargo`.

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
haul uninstall-shim        # removes exactly what install-shim created (recorded in ~/.haul/install.json)
cargo uninstall haul
rm -rf ~/.haul             # recorded runs, indexes, benchmarks
```

Files Haul may have written in a repository, only on request:
`.haul/config.toml`, `.haul/budgets.toml`, `.haul/baselines/*.json`, report
paths you configured, and `target/haul-bench/` (benchmark build directories,
marked with `.haul-owned`; safe to delete).

## Rollback

Remove the shim directory from `PATH`, or run `haul uninstall-shim`. Builds made
through Haul and builds made by upstream Cargo share the same target directory
without rebuilding each other's artifacts. In fast mode their profile settings
differ, so each keeps its own set of artifacts there (Cargo names them by a hash
of the settings); `cargo clean` or `haul gc --dry-run` show the cost. With
`HAUL_MODE=compat` both build the same artifacts.

One caveat: build scripts that emit `cargo::rerun-if-env-changed=RUSTC_WRAPPER`
rerun when you switch between Haul and plain Cargo, because Haul's wrapper is in
that variable. Set `HAUL_MEASUREMENT_ENABLED=false` to delegate without the
wrapper if that matters.
