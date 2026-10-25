# How to build exactly as Cargo does

This guide shows you how to turn off what QuickRust changes about a build, all of it or one setting at a time, for example to rule QuickRust out while you chase a problem.

By default QuickRust runs in fast mode, which changes how a build is made but never what the program does. What each setting does is in [Fast mode](../reference/fast-mode.md); why they are safe is in [Why QuickRust is fast by default](../explanation/fast-by-default.md).

## Run the toolchain's Cargo with nothing added

```bash
qrust upstream build
qrust upstream test -- --nocapture
```

`qrust upstream` runs the selected toolchain's own Cargo with your arguments: no QuickRust settings, no rendered output, nothing recorded. It still uses QuickRust's toolchain, its Cargo home (`~/.qrust/cargo`) and the workspace's build directory, so it finds what QuickRust built.

Use it to answer "does this also happen without QuickRust?".

## Build exactly what Cargo builds, still through QuickRust

Compat mode makes QuickRust build exactly what upstream Cargo builds while it keeps recording runs:

```bash
QRUST_MODE=compat cargo build          # for one command
```

```toml
# .qrust/config.toml in the project, or ~/.qrust/config.toml for all projects
mode = "compat"
```

In compat mode the terminal shows Cargo's own output too, unless you also set `output = "qrust"`.

## Turn off one setting

If you have narrowed a difference down to one fast-mode setting, turn off only that one:

| To turn off | In `.qrust/config.toml` | For one command |
|---|---|---|
| rustc's parallel frontend | `[fast]` `frontend-threads = "off"` | `QRUST_FAST_FRONTEND_THREADS=off` |
| early cut-off (QuickRust's compiler driver) | `[fast]` `early-cutoff = "off"` | `QRUST_EARLY_CUTOFF=off` |
| linking with wild | `[fast]` `linker = "off"` | `QRUST_FAST_LINKER=off` |
| the artifact cache | `[cache]` `artifacts = "off"` | `QRUST_CACHE_ARTIFACTS=off` |

For example:

```toml
[fast]
early-cutoff = "off"

[cache]
artifacts = "off"
```

Early cut-off depends on the artifact cache, so turning the artifact cache off turns early cut-off off as well.

## Get full debug information back

Fast mode builds development profiles with line tables only and no debug information for dependencies. If you need full debug information, for example to inspect local variables in a debugger, set it as you would with plain Cargo:

```toml
# Cargo.toml
[profile.dev]
debug = true
```

or for one command, `CARGO_PROFILE_DEV_DEBUG=true cargo build`. A profile setting you make yourself, in `Cargo.toml`, a `.cargo/config.toml`, a `CARGO_PROFILE_*` variable or `--config`, always wins: once you set `profile.dev.debug`, QuickRust leaves both of its debug settings alone.

## Get Cargo's terminal output

To see Cargo's own lines instead of QuickRust's status line and summary:

```bash
QRUST_OUTPUT=cargo cargo build
cargo build -v                     # -v, -q, --message-format and --color also leave Cargo's output alone
```

or `output = "cargo"` in a configuration file. The build itself is unchanged.

## Check what a build applied

Every run records which fast-mode settings it applied, which it left alone and why:

```bash
qrust report --format json | jq .policy.fast
```

`qrust policy explain` lists every QuickRust setting in effect, its value and where that value came from.

## Related

- [Configuration](../reference/configuration.md)
- [How to try QuickRust before making it your cargo](adopt-gradually.md)
