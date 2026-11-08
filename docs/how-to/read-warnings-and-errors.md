# How to see the warnings, errors and full log of a build

This guide shows you how to read everything a build printed, after QuickRust has summed it up in a terminal.

In a terminal, QuickRust shows every error in full as it arrives, but only counts warnings:

```text
OK   Built: 1 compiled [100ms]
WARN 2 warnings in demo (bin): dead_code x1, unused_variables x1; `qrust warnings` shows them
```

Nothing is lost: the whole output of every build QuickRust rendered is stored with its run.

## Show the warnings

```bash
qrust warnings
```

prints every warning of the latest build as rustc wrote it. To narrow it down:

```bash
qrust warnings --crate demo
qrust warnings --lint dead_code
qrust warnings --crate demo --lint unused_variables
```

If nothing matches, it says so (`No warnings in run <id> matching the filter.`).

## Show the errors again

```bash
qrust errors
qrust errors --crate demo
```

This is useful when the errors have scrolled away, or to pipe them somewhere:

```bash
qrust errors | less -R
```

## Show everything, in order

```bash
qrust log            # Cargo's lines and rustc's messages, as a plain build would have printed them
qrust log --raw      # the stored stream as Cargo emitted it, including its JSON messages
```

## Look at an earlier build

Every one of these commands takes `--run`:

```bash
qrust warnings --run latest~1        # the build before the latest
qrust log --run 01M4SFQZ             # a run id, or a unique prefix or suffix of one
```

`qrust runs` lists the recorded runs of the workspace with their ids.

## Get Cargo's own output instead

If you want Cargo to print a build itself, line by line:

- for one command, pass `-v` (also `-q`, `--message-format`, or `--color`): QuickRust leaves Cargo's output untouched;
- for one shell, `export QRUST_OUTPUT=cargo`;
- for good, `output = "cargo"` in `~/.qrust/config.toml` or the project's `.qrust/config.toml`.

Output written to a pipe or a file is always Cargo's own, so `cargo build 2>&1 | tee build.log` and CI logs need nothing.

## If the output was not stored

```text
FAIL this run's output was not stored: it was not rendered by QuickRust (not a terminal, or output = "cargo"), or its raw events were pruned
```

Output is stored only for builds QuickRust rendered in a terminal, and only for the most recent runs (`measurement.keep-raw-events`, 20 by default). A build whose output went to a pipe, ran with `-v`, or ran with `output = "cargo"` printed Cargo's output directly. Run the build again in a terminal, or read the log you captured.

## Related

- [Terminal output](../reference/output.md) for every kind of line QuickRust prints
- [About QuickRust's build output](../explanation/build-output.md)
