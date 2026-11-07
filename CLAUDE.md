# Working on QuickRust

QuickRust (`qrust`) replaces the `cargo` command: it builds faster by default, manages Rust toolchains itself, and records every build. The user documentation in `docs/` describes what it does and why. This file holds what someone changing the code needs that the documentation does not say.

## Names

The project was called Haul until 2026-10-13. Use QuickRust in prose. The command and the other programs are `qrust`, `qrust-rustc-wrapper`, `qrust-rustc-driver` and `qrust-bench`. The package and the library are `quickrust`. Environment variables start with `QRUST_` (test-only variables with `QRUSTTEST_`), and the home is `~/.qrust`. The `haul` names in `benchmarks/workloads.toml` are deliberate: the `qrust` workload builds the project's first commit, whose crate and files were called haul at the time. Do not rename them.

## What QuickRust owns

QuickRust owns the whole build environment. It installs and selects toolchains, keeps build directories in its cache (`~/.qrust/cache/builds`, never a `target/` in the project), installs programs, and renders all output. The toolchain's `cargo` is only an internal engine. QuickRust never calls rustup and has no rustup fallback ([owning the environment](docs/explanation/owning-the-environment.md)). The default policy is fast, as long as the program behaves the same; `mode = "compat"` and `qrust upstream` give exactly what Cargo would build ([fast by default](docs/explanation/fast-by-default.md)).

## Code style

The code is formatted by hand, with long lines. There is no rustfmt configuration, and no rustfmt setting reproduces the existing style. Never run `cargo fmt` or `rustfmt`, not even `--check` through anything that could write files. Match the style of the surrounding code: a format pass buries real changes in churn and breaks blame.

## Output

Everything QuickRust prints is ASCII. An outcome line starts with a tag padded to four characters plus a space: `OK  `, `FAIL`, `WARN` or `NOTE`. Lines that belong to an outcome are indented by five spaces (`ui::INDENT`). A message has a head, then `: ` and details separated by `, `, and the duration last in brackets (`[1.20s]`). A result that adds nothing beyond success is just `OK   [920ms]`. The bar is `[====>   ]`; use `->`, `...` and `x3` instead of other symbols. All new output goes through `ui::say`, `ui::tagged` and `ui::message`. `tests/screens.rs` fails on any screen that is not ASCII. Documentation prose is not bound by this.

## Configuration and environment

QuickRust treats every `QRUST_*` variable it does not know as a configuration error (`src/config.rs`). So a variable that only the tests read starts with `QRUSTTEST_`. For example, `QRUSTTEST_TOOLCHAINS` is the toolchain store the integration suites share, by default `~/.cache/qrust-test-toolchains`. QuickRust fills it from the network the first time a test needs it, and it holds a few GB.

## The compiler driver

`driver/` is linked against the toolchain's own `librustc_driver` (the `rustc-dev` component of exactly that release). rustc's internal interface changes between releases. When a release changes something the driver uses, add its minor version to `INTERFACE_BREAKS` in `src/driver.rs`; the driver code then uses `#[cfg(qrust_rustc_ge_<minor>)]`. When stable moves (every six weeks), install the new release through QuickRust (`qrust toolchain install 1.<n>.0 --component rustc-dev`) and build the driver for it (`qrust driver build --rustc "$(qrust toolchain which rustc --toolchain 1.<n>.0)"`). Then run the driver tests and `qrust-bench cutoff` against it. Never change a machine's default toolchain to do this.

## Measuring speed

The speed index ([`benchmarks/README.md`](benchmarks/README.md)) measures QuickRust's progress. Never claim a speedup without a recorded run. Record the quick tier on every iteration. Record the full tier as a checkpoint: about every ten iterations, and before a speed step is called done. Checkpoints also go in [`benchmarks/PROGRESS.md`](benchmarks/PROGRESS.md). When the bench rejects a run as noisy (CPU pressure, a spread over 25%), run it again on a quiet machine. Never loosen the checks so that a run passes.

Byte-for-byte comparisons need both the parallel frontend and the wild linker switched off: `[fast] frontend-threads = "off"` and `linker = "off"` in the home's `config.toml`. This covers the early cut-off gate, which sets both itself, and any reproducibility check. rustc's parallel frontend numbers nested statics in a varying order, and wild 0.10 writes `.rodata`'s `sh_entsize` as 0 or 1. Neither difference changes how the program behaves. Do not fix this by leaving definition numbers out of the interface hash, because dependents can name them.

## Documentation

`docs/` follows the Diataxis framework. `docs/README.md` is the index, and it is divided into `tutorials/`, `how-to/` (titles "How to ..."), `reference/` (austere) and `explanation/`. Each page does the job of one kind and links to the others. The root `README.md` is a short introduction that points into `docs/`. Every page has an Italian translation beside it (`<page>.it.md`), except `reference/specification.md` and `explanation/compile-time-research.md`. A change to a page changes its translation in the same commit. A behaviour change updates every page that describes it, usually the reference page and the how-to guide that uses it. Output samples are copied from real runs. Write one paragraph per line, in ASCII.
