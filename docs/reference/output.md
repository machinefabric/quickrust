# Output

What QuickRust writes to a terminal: the line forms, which commands it presents, when Cargo's own output passes through untouched, and what it stores.

Why the output looks like this: [About QuickRust's build output](../explanation/build-output.md).

## Line forms

Everything QuickRust writes for people is ASCII.

| Form | Example |
|---|---|
| An outcome: a tag, padded to four characters and a space, then the message | `OK   Built: 12 compiled, 140 fresh [4.21s]` |
| A line that belongs to an outcome: indented five spaces, to the message column | `     Downloaded 38 crates, 5.6 MiB [1.84s]` |

A message is a head, then its details after `: `, separated by `, `; a duration, when there is one, closes the line in square brackets. A tag with nothing to add stands alone: `OK`, or `OK   [0.42s]` when only the time is worth saying.

| Tag | Colour | Means |
|---|---|---|
| `OK` | bold green | it worked |
| `FAIL` | bold red | it did not: an error, a failed command, an interruption |
| `WARN` | bold yellow | it worked, and something deserves a look |
| `NOTE` | bold cyan | what QuickRust does or knows, for information |

Of an outcome line only the tag is coloured. The status line and detail lines such as `Locked ...` are dim; the words of dependency lines are coloured ([below](#commands-that-do-not-build)). There is no colour when `NO_COLOR` is set (not empty) or `CARGO_TERM_COLOR=never`, and when the line does not go to a terminal, unless `CARGO_TERM_COLOR=always`. Without colour the tag carries the meaning.

Durations are shown as `120ms`, `4.21s`, `2m05.3s`; sizes as `942 B`, `281.0 KiB`, `5.6 MiB`.

## When QuickRust presents the output

QuickRust presents a command's output when all of these hold:

- `output = "qrust"` (the default in fast mode; in compat mode only when set explicitly);
- stderr is a terminal;
- the command is not a Cargo alias;
- the command line has none of `--message-format`, `-v`, `-vv`, `--verbose`, `-q`, `--quiet`, `--color` (before any `--`).

Otherwise Cargo's output reaches the terminal, pipe or file unchanged. Per command:

| Commands | Presentation |
|---|---|
| `build`, `check`, `clippy`, `doc` (and their aliases `b`, `c`, `d`, `rustdoc`), `test --no-run`, `bench --no-run` | the build is rendered: status line, errors in full, warnings counted, summary |
| `run` (for the host, or for one target triple, from `--target` or `build.target`) | the build is rendered, then `cargo run` runs the program with the terminal as it is |
| `test`, `bench` | the build is rendered, then the tests or benchmarks run in a terminal QuickRust relays (Unix) |
| `test --doc`, `bench --doc` | Cargo's output |
| `update`, `fetch`, `add`, `remove` (`rm`), `new`, `init`, `generate-lockfile`, `package`, `publish`, `vendor`, `clean` | Cargo's status lines are rendered from its stderr; stdout and stdin stay the terminal's |
| `tree`, `metadata` (without `--help`) | downloads are rendered from stderr; the command's own output on stdout is unchanged |
| `install`, `uninstall` | QuickRust's own output |
| every other command (`rustc`, `fix`, `login`, `logout`, `owner`, `yank`, `search`, external subcommands, ...) | Cargo's output |

For a presented command QuickRust asks Cargo for its JSON messages (builds) and its progress, and asks it to draw in ASCII (`term.unicode = false`, so `cargo tree` draws `|--` and `` `-- ``). These go to that one Cargo as `--config` arguments; build scripts, nested Cargo and the programs a command runs do not see them.

## Builds

### Status line

One line, redrawn in place, indented to the message column:

```text
     Building [=========>          ] 112/240, 98 compiled, 14 fresh; tokio, syn
     Downloading [=====>              ] 7/25 crates, 2.1 MiB left
     Updating crates.io index: 26 packages looked up
     Waiting for the lock on the package cache (another Cargo holds it)
```

The bar is 20 cells wide. The line is cut to the terminal's width with `...`. It is removed before the summary.

### Lines above the status line

| Line | Example |
|---|---|
| A compiler error: the crate in red, then rustc's rendering, in full, as it arrives | `--- app (bin)` followed by `error[E0425]: cannot find function ...` |
| Downloads, when done | `     Downloaded 1 crate, 4.4 KiB [0.32s]` |
| Lockfile changes | `     Locked 1 package to highest compatible version` |
| Lines QuickRust does not recognise (build script failures, `cargo:warning=` output, resolution errors) | as Cargo printed them |

Warnings are not printed as they arrive; they are counted.

### Summary

| Outcome | Line |
|---|---|
| success | `OK   Built: 1 compiled, 1 fresh, 3 restored from QuickRust's cache [4.21s]` |
| success of `check`, `clippy` | `OK   Checked: 2 compiled [0.80s]` |
| success of `doc`, followed by where the documentation is | `OK   Documented: 1 compiled, 1 fresh [1.10s]` and `     docs <target>/doc/app/index.html` |
| failure with compiler errors | `FAIL Build: 1 error in app (bin), first at src/main.rs:2:27 [1.20s]` |
| failure without compiler errors (Cargo's own error is shown above it) | `FAIL Build [0.10s]` |
| failure of `check`, `clippy`; `doc` | `FAIL Check: ...`; `FAIL Documentation: ...` |
| a full disk | after the `FAIL` line: `NOTE the disk is full: <free> free where QuickRust keeps its cache, which holds <size>; ...` |
| interrupted | `FAIL Interrupted [3.02s]` |
| nothing changed since the last successful build | `OK   Up to date: 152 fresh` ([About up-to-date checks](../explanation/up-to-date-checks.md)) |

Each unit is counted once: `compiled` (rustc ran), `fresh` (Cargo found it up to date) or `restored from QuickRust's cache`. Counts that are zero are left out. Warnings follow the summary in one line, the three most frequent lints first:

```text
WARN 23 warnings in app and util: unused_variables x17, dead_code x4, +2 more; `qrust warnings` shows them
```

### `run`, `test`, `bench`

After the build summary the command itself runs and finds everything fresh. Cargo's "Finished" and "Running" lines of `cargo run` are not shown, nor are warnings Cargo would replay. The program's output is not touched.

For `test` and `bench`, QuickRust shows Cargo's lines its way and closes with the counts of every test program added up:

```text
OK   Built: 1 compiled, 1 fresh [0.42s]
     unittests src/lib.rs
running 132 tests
...
test result: ok. 132 passed; 0 failed; 2 ignored; 0 measured; 0 filtered out; finished in 0.31s
     doc-tests app
...
OK   Tests: 140 passed, 2 ignored [1.12s]
```

| Outcome | Line |
|---|---|
| all passed | `OK   Tests: <counts> [<time>]`; counts in the order failed, passed, ignored, measured, filtered out, zeros left out |
| a test failed | `FAIL Tests: 1 failed, 139 passed [<time>]` |
| a test program failed without counting a failure | `FAIL Tests: a test program failed, 139 passed [<time>]` |
| no counts at all | `none ran` in place of the counts |
| a harness that is not libtest's | nothing on success; `FAIL Tests [<time>]` on failure |
| interrupted | `FAIL Interrupted [<time>]` |

On Windows the test run's lines are Cargo's.

## Commands that do not build

```text
     updated    serde                    1.0.209 -> 1.0.210
     added      hashbrown                0.15.0
     removed    autocfg                  1.3.0
OK   Lockfile updated: 1 updated, 1 added, 1 removed [0.92s]
```

| Line | Form |
|---|---|
| A dependency in the lockfile (`update`, `generate-lockfile`) | `     <word> <name> <change>`: word `updated` (green), `downgraded` (yellow), `added` (green), `removed` (red), `unchanged` (dim), padded to 10; name padded to 24 |
| A manifest edit (`add`, `remove`) | `     added      greet                    =1.0.0 to dependencies`, `     removed    greet                    from dependencies` |
| `cargo add`'s features | `     features +derive -alloc` |
| The lockfile changes of a manifest edit, in one line | `     locked +greet 1.0.0` |
| Downloads | `     Downloaded 1 crate, 4.4 KiB [0.32s]` |
| A result Cargo reports (`Creating`, `Removed`, `Packaged`, `Uploaded`, `Published`, `Vendored`) | `OK   Created binary (application) `tool` package`, `OK   Removed 125 files, 75.8 MiB`, `OK   Packaged 4 files, 1.2 KiB (942 B compressed)` |
| Cargo's `note:` lines | `NOTE see more `Cargo.toml` keys ...` |
| A line QuickRust does not recognise | as Cargo printed it |

The closing line:

| Outcome | Line |
|---|---|
| `update`, `generate-lockfile`, nothing changed | `OK   Lockfile up to date [<time>]` |
| `update`, `generate-lockfile`, changes | `OK   Lockfile updated: <counts> [<time>]` |
| a result line was printed (`new`, `init`, `clean`, `package`, `publish`, `vendor`), or `tree`, `metadata` | none |
| any other success (`add`, `remove`, `fetch`) | `OK   [<time>]` |
| failure | `FAIL cargo <command> [<time>]` |
| interrupted | `FAIL Interrupted [<time>]` |

## QuickRust's own commands

| Kind | Form |
|---|---|
| A QuickRust command that could not run (exit status 2) | `FAIL <message>` on stderr |
| A warning about a build QuickRust could not fully observe or record (it still ran) | `WARN <message>` on stderr |
| Something QuickRust does or knows (the compiler driver is being built, the disk is nearly full, restored crates name another directory) | `NOTE <message>` on stderr |
| The result of a management command (`qrust toolchain ...`, `qrust baseline set`, `qrust fix --apply`, `qrust self uninstall`) | `OK   <message>` on stdout |
| A long-running phase (toolchain install, `qrust setup`, `qrust install`, `qrust gc`, `qrust self update`) | a status line, then `OK   <message> [<time>]` or `FAIL <message> [<time>]` |

Examples:

```text
OK   Rust 1.99.0 ready (1.99.0-x86_64-unknown-linux-gnu) [38.20s]
OK   Installed hello 0.3.0: hello -> ~/.qrust/bin [12.41s]
OK   Freed 1.2 GiB: QuickRust's cache 9.8 GiB -> 8.6 GiB (at most 10.0 GiB; keeps 2.0 GiB of the disk free, 40.1 GiB free now) [0.84s]
FAIL toolchain no-such-toolchain-xyz (from `+toolchain` argument): no toolchain is named `no-such-toolchain-xyz` (`qrust toolchain link no-such-toolchain-xyz <dir>` names one)
```

## Stored output

For every build QuickRust presented, every line Cargo and rustc wrote (at most 200,000 per run) is stored with the run, in `cargo-output.jsonl` in the run's event directory. Status-line redraws are not stored. A run found up to date holds the stored output of the build it is up to date with, whose warnings its `WARN` line repeats. `qrust warnings`, `qrust errors` and `qrust log` print it again ([Commands](commands.md#output-of-a-build)). The stored output is kept as long as the run's raw events (`measurement.keep-raw-events`). Commands that do not build are not stored.
