# How to embed QuickRust in another program

This guide shows you how to run QuickRust from a program of your own (an indexer, a code-analysis service, a build farm agent) so that it builds with exactly what your program gives it: no downloads, no background processes, no settings taken from the repository it builds, and Cargo's output unchanged for your program to read.

## Check what the installed QuickRust offers

```bash
qrust capabilities --format json
```

```json
{"quickrust":"1.151.1495","capabilities":{"embedded":"1","report-schema":"1.0"}}
```

Rely on embedded mode only when `capabilities.embedded` is present: an older QuickRust rejects `QRUST_EMBEDDED` as an unknown variable.

## Give it a home, a toolchain and a Cargo home

```bash
QRUST_EMBEDDED=1 \
QRUST_HOME=/var/lib/myindexer/qrust/trusted \
CARGO_HOME=$HOME/.cargo \
RUSTUP_TOOLCHAIN=$HOME/.rustup/toolchains/1.98.1-x86_64-unknown-linux-gnu \
QRUST_REPORT=/var/lib/myindexer/reports/{session}.json \
qrust check --offline --message-format=json --workspace --all-targets
```

- `QRUST_EMBEDDED=1` makes QuickRust embedded ([`embedded`](../reference/configuration.md#keys)). It can also be set in `$QRUST_HOME/config.toml`; a repository's `.qrust/config.toml` is not read, so the code being built cannot change how QuickRust builds it.
- `QRUST_HOME` is QuickRust's state and cache: build directories, the artifact cache, compiler drivers and run records. Give each trust level its own home. The artifact cache restores outputs between builds; a home shared by builds you trust and builds you do not would let one restore what the other produced.
- `CARGO_HOME` is required: an embedded QuickRust builds with it (its registry, git checkouts and configuration) instead of its own.
- `RUSTUP_TOOLCHAIN` names the toolchain by its directory. An embedded QuickRust never installs a toolchain; a toolchain named by channel or version (`stable`, `1.98`) is refused, including one a repository's `rust-toolchain.toml` would choose.
- `QRUST_REPORT` writes the build's [report](../reference/report-schema.md): units with their timings, freshness and rebuild reasons, build scripts, the critical path.

What Cargo writes reaches your program unchanged: with `--message-format=json`, stdout is Cargo's JSON messages (rustc's diagnostics included), one per line.

## Keep its cache within bounds

An embedded QuickRust collects nothing in the background, since your program decides how long it runs. Run a collection with the same home when it suits your program, for example after a batch of builds:

```bash
QRUST_HOME=/var/lib/myindexer/qrust/trusted qrust gc
```

## Use early cut-off

Early cut-off compiles with QuickRust's compiler driver, built once per toolchain from the toolchain's `rustc-dev` component. An embedded QuickRust does not build it in the background: build it with the same home and toolchain before the builds that should use it.

```bash
rustup component add rustc-dev --toolchain 1.98.1
QRUST_HOME=/var/lib/myindexer/qrust/trusted RUSTUP_TOOLCHAIN=$HOME/.rustup/toolchains/1.98.1-x86_64-unknown-linux-gnu qrust driver build
```

Without the driver, builds still use the artifact cache; early cut-off is off for them.
