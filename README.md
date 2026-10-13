# QuickRust

QuickRust is a drop-in replacement for the `cargo` command that makes Rust builds faster, with nothing to configure. It is the only thing you install: it downloads and manages the Rust toolchains itself (`+toolchain`, `RUSTUP_TOOLCHAIN` and `rust-toolchain.toml` choose, as with rustup) and runs every command, argument for argument, through the toolchain's own Cargo and rustc. Around them it adds what makes builds fast, and it records every build so that it can explain where the time and the disk space went.

```text
OK   Built: 12 compiled, 140 fresh, 37 restored from QuickRust's cache [4.21s]
WARN 23 warnings in app and util: unused_variables x17, dead_code x4, +2 more; `qrust warnings` shows them
```

What you get on top of an unchanged Cargo:

- **faster builds by default**: lighter debug information, rustc's parallel frontend, the wild linker when installed, a global artifact cache shared by every checkout, and early cut-off, which stops a rebuild at a crate whose interface an edit did not change. The program you build behaves exactly the same; settings you make yourself always win.
- **no `target/` in your projects**: builds live in QuickRust's cache, which keeps itself within a size and a free-space bound.
- **readable output**: one status line, errors in full, warnings counted and kept, one line per outcome (`OK`, `FAIL`, `WARN`, `NOTE`), plain ASCII.
- **answers about builds**: timing and the critical path, why each crate rebuilt, duplicated dependencies, build scripts and procedural macros, cache behaviour, disk use.
- **measurement**: benchmarks, A/B experiments of build settings, baselines and CI budgets.

How much faster, measured against plain Cargo on real projects: [benchmarks/PROGRESS.md](benchmarks/PROGRESS.md). At the latest checkpoint the speed index (the geometric mean of Cargo's time over QuickRust's, across a developer's build loop) is 2.16.

## Install

```bash
curl -fsSL https://github.com/jowharshamshiri/quickrust/releases/latest/download/install.sh | sh
# Windows: irm https://github.com/jowharshamshiri/quickrust/releases/latest/download/install.ps1 | iex
```

From source, with Rust 1.80 or newer:

```bash
cargo build --release --locked
target/release/qrust setup
```

Then open a new shell: `cargo` is now QuickRust. Details, updates and uninstalling: [How to install, update and uninstall QuickRust](docs/how-to/install.md).

## A first look

```bash
cargo build                      # through QuickRust
qrust warnings                   # the warnings it counted, in full
qrust analyze                    # everything about the latest build
qrust analyze rebuilds           # why each crate rebuilt
qrust path                       # where the build went
qrust upstream build             # the toolchain's own Cargo with nothing added
```

[Your first build with QuickRust](docs/tutorials/first-build.md) takes ten minutes and shows the rest.

## Documentation

The [documentation](docs/README.md) has four parts:

- **Tutorials**, to learn by doing: [Your first build with QuickRust](docs/tutorials/first-build.md), [Finding where a build's time goes](docs/tutorials/where-the-time-goes.md).
- **How-to guides**, for a task at hand: installing, adopting QuickRust gradually, toolchains, finding build outputs, building exactly as Cargo does, disk space, sccache, CI budgets, measuring a change, and more.
- **Reference**: [commands](docs/reference/commands.md), [configuration](docs/reference/configuration.md), [environment variables](docs/reference/environment.md), [fast mode](docs/reference/fast-mode.md), [output](docs/reference/output.md), [files](docs/reference/files.md), [compatibility with Cargo](docs/reference/compatibility.md), the [run report schema](docs/reference/report-schema.md), the [specification](docs/reference/specification.md) and its [conformance](docs/reference/conformance.md).
- **Explanation**: [how QuickRust works](docs/explanation/architecture.md), [why it is fast by default](docs/explanation/fast-by-default.md), the [artifact cache](docs/explanation/artifact-cache.md), [early cut-off](docs/explanation/early-cutoff.md), [security and privacy](docs/explanation/security.md), and the [research](docs/explanation/compile-time-research.md) it started from.

## Tracking

Work is tracked on the private [project board](https://github.com/users/jowharshamshiri/projects/6): delivered work, findings and decisions as cards carrying their context, and the remaining work by release (P0, P1, P2, Future).

## Development

```bash
cargo test                      # unit tests, the compatibility corpus and the management-surface suite (real builds)
cargo clippy --all-targets
```

The suites build with toolchains QuickRust installs into a store shared between runs (`QRUSTTEST_TOOLCHAINS`, default `~/.cache/qrust-test-toolchains`; the first run downloads them), and with crates from a registry served by the test itself. The documentation follows the four-part structure described in [docs/README.md](docs/README.md#how-this-documentation-is-written); output shown in it is copied from real runs.
