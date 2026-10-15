# QuickRust documentation

QuickRust is a drop-in replacement for the `cargo` command that makes Rust builds faster, manages the Rust toolchains itself, and records every build so that it can explain where the time and the disk space went.

This documentation has four parts, one for each thing you might need from it:

| | Part | Use it when you want to |
|---|---|---|
| Learning | [Tutorials](#tutorials) | get to know QuickRust by using it, step by step |
| Working | [How-to guides](#how-to-guides) | get a particular job done |
| Looking up | [Reference](#reference) | check a command, a setting, a file or a guarantee |
| Understanding | [Explanation](#explanation) | know why QuickRust works the way it does |

New to QuickRust? Start with [Your first build with QuickRust](tutorials/first-build.md).

## Tutorials

Lessons that take you through QuickRust by doing something with it.

- [Your first build with QuickRust](tutorials/first-build.md): create, build, break, mend and test a small program, and meet QuickRust's output, its build directories and its cache.
- [Finding where a build's time goes](tutorials/where-the-time-goes.md): build a program with dependencies, find its critical path, see why a rebuild happened, compare two builds.

## How-to guides

Directions for a task, for when you know what you want to do.

Setting up and moving over:

- [How to install, update and uninstall QuickRust](how-to/install.md)
- [How to try QuickRust before making it your cargo, and how to go back](how-to/adopt-gradually.md)
- [How to choose and manage toolchains](how-to/choose-a-toolchain.md)
- [How to install programs with qrust install](how-to/install-programs.md)

Daily work:

- [How to find build outputs without target/](how-to/find-build-outputs.md)
- [How to see the warnings, errors and full log of a build](how-to/read-warnings-and-errors.md)
- [How to build exactly as Cargo does](how-to/build-exactly-like-cargo.md)
- [How to keep QuickRust's cache within your disk](how-to/free-disk-space.md)
- [How to use sccache with QuickRust](how-to/use-sccache.md)

Investigating and improving builds:

- [How to find out why crates rebuilt](how-to/find-why-crates-rebuilt.md)
- [How to measure the effect of a build setting](how-to/measure-a-change.md)
- [How to review and apply QuickRust's recommendations](how-to/apply-a-recommendation.md)
- [How to export a build report](how-to/export-a-report.md)
- [How to run QuickRust in CI and gate on build time](how-to/set-up-ci.md)
- [How to embed QuickRust in another program](how-to/embed-quickrust.md)

## Reference

Descriptions of the machinery, to consult while you work.

- [Commands](reference/commands.md): every `qrust` command and option, run references, exit statuses
- [Configuration](reference/configuration.md): configuration files, precedence and every setting
- [Environment variables](reference/environment.md): what QuickRust reads, and what it sets for Cargo, rustc and build scripts
- [Fast mode](reference/fast-mode.md): each build setting fast mode applies, when, and how to turn it off
- [Output](reference/output.md): the `OK` / `FAIL` / `WARN` / `NOTE` lines, and when Cargo's own output passes through
- [Files and directories](reference/files.md): what QuickRust keeps where, and what is safe to delete
- [Budgets](reference/budgets.md): the `.qrust/budgets.toml` format and its metrics
- [Benchmarks](reference/benchmarks.md): scenarios, repetitions, and what is discarded
- [Compatibility with Cargo](reference/compatibility.md): what passes through unchanged and what differs
- [Platforms](reference/platforms.md): supported systems, known gaps, and the limits of observation
- [Run report schema](reference/report-schema.md): the versioned JSON report
- [Specification](reference/specification.md) and [Conformance](reference/conformance.md): the requirements QuickRust implements, and where it deviates

## Explanation

Discussion of the ideas and decisions behind QuickRust.

- [How QuickRust works](explanation/architecture.md)
- [Why QuickRust is fast by default](explanation/fast-by-default.md)
- [About the artifact cache](explanation/artifact-cache.md)
- [About early cut-off and QuickRust's compiler driver](explanation/early-cutoff.md)
- [How QuickRust knows a build is up to date](explanation/up-to-date-checks.md)
- [Why QuickRust owns its environment](explanation/owning-the-environment.md)
- [About QuickRust's build output](explanation/build-output.md)
- [About measuring builds](explanation/measurement.md)
- [Security and privacy](explanation/security.md)
- [Reducing Rust compile time and build disk footprint](explanation/compile-time-research.md): the research QuickRust started from

Benchmark results and their method are kept with the data, in [benchmarks/PROGRESS.md](../benchmarks/PROGRESS.md) and [benchmarks/README.md](../benchmarks/README.md).

## How this documentation is written

Each page is one of the four kinds above and does only that kind's job: a tutorial teaches by doing and does not explain at length; a how-to guide gives directions for one goal; reference describes and does not instruct; explanation discusses and does not give steps. Where a page needs something of another kind, it links to it. A new page goes in the part whose job it does, and examples of output are copied from real runs.
