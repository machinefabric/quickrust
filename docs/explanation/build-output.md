# About QuickRust's build output

In a terminal, a QuickRust build does not look like a Cargo build. There is no `Compiling` line per crate; there is one status line, errors in full, warnings counted rather than printed, and a one-line verdict. This page explains why QuickRust takes over the terminal, how its way of speaking is designed, and where it deliberately leaves Cargo's output alone.

## Why not just show Cargo's output

Cargo's terminal output was designed for a different moment. A `Compiling serde v1.0.210` line per unit made sense when watching a build was interesting; on a project with a few hundred dependencies it is a scroll of names that pushes the one line that matters, the error, off the screen. Warnings are worse: each one takes several lines of source excerpt and notes, so a few dozen of them print hundreds of lines before every build result, so people stop reading them, or stop seeing the error between them.

What a developer needs from a build in a terminal is short: is it working, did it work, and if not, where is the first problem. QuickRust's output answers those, in that order:

```text
OK   Built: 12 compiled, 140 fresh, 37 restored from QuickRust's cache [4.21s]
WARN 23 warnings in app and util: unused_variables x17, dead_code x4, +2 more; `qrust warnings` shows them
```

While the build runs, a single status line shows Cargo's own progress (units done of all, crates being downloaded, a lock being waited for) and is overwritten in place. Errors are printed in full as they arrive, exactly as rustc rendered them, under a header naming the crate, because an error is the one thing that should never be summarised. Warnings are counted by lint and crate, because a count tells you whether anything changed, and the full text is one command away. The verdict closes the build: what was compiled, what was fresh, what came from the artifact cache, and how long it took; or, for a failure, how many errors and where the first one is:

```text
FAIL Build: 1 error in demo (bin), first at src/main.rs:3:18 [78ms]
```

## Nothing is lost

Collapsing output is only acceptable if what was collapsed can be recovered exactly. Every line Cargo and rustc wrote during a build QuickRust rendered is stored with the run. `qrust warnings` prints the warnings as rustc wrote them, filtered by crate or lint if asked; `qrust errors` prints the errors again; `qrust log` prints everything in order, and `qrust log --raw` the unprocessed stream. [How to see warnings and errors](../how-to/read-warnings-and-errors.md) shows them in use. So the summary is a view, not a loss: the warnings are counted on screen and kept in full on disk.

## One way of speaking

QuickRust's output follows one scheme everywhere, for builds, toolchain installs, cache collections, `qrust doctor` and the rest:

- A line that reports an outcome starts with its tag in a column of its own: `OK` (it worked), `FAIL` (an error, a failed command, an interruption), `WARN` (it worked, and something deserves a look) or `NOTE` (what QuickRust does or knows).
- A line that belongs to an outcome, such as a download, a dependency that moved or the live status line, is indented to where messages start, under the outcome it belongs to.
- A message is a head, then its details after `: `, separated by `, `; a duration, when there is one, closes the line in brackets.

The point is that a person scanning a terminal, or a script grepping a log, can tell outcomes from detail by the first column alone, and the same words mean the same thing in every command. A brief "it worked" with nothing to add is just `OK` and the time; a line that merely confirms success never says more than that.

Everything is ASCII. Box-drawing characters, check marks and emoji render differently, or not at all, across terminals, fonts, Windows consoles, CI log viewers and serial consoles, and copied into an issue they turn into noise. Tags are coloured when colour is available (green, red, yellow, cyan), but they are words first, so without colour the tag still says what the colour would. QuickRust asks Cargo for ASCII too (`term.unicode = false`), which is why `cargo tree` draws its tree with `|--` and `` `-- ``.

## Commands that do not build

The same reasoning applies to commands that change a project without compiling it. `cargo update` lists what moved, one dependency per line, and sums it up; `cargo add` shows the dependency and its features in one line; `cargo new`, `clean`, `package` and `publish` end in one result line. Their stdout and stdin stay the terminal's, so anything they print for you to read, or ask you, is untouched. Lines QuickRust does not recognise, such as a build script's failure, a `cargo:warning=` line or a resolution error, are shown exactly as Cargo printed them. A renderer that drops a line it does not understand would be hiding exactly the unusual messages that matter most.

## `run` and `test`: build first, then get out of the way

`cargo run` and `cargo test` mix two kinds of output: the build's and the program's. The program's output belongs to the program, and must reach the terminal exactly as it would without QuickRust. So QuickRust splits the command in two. First it builds exactly what the command needs (for `test`, the same command with `--no-run`; for `run`, Cargo's own selection of the binary, with a runner configured for this one invocation that does not run it), rendered as any build. Then it runs the real command, which finds everything fresh, with Cargo's own build output quieted so the warnings are not replayed a second time.

Test programs then run in a terminal of their own: a pseudo-terminal the size of the real one, so colour, `isatty` and the order of their output are exactly what a terminal would give them. QuickRust relays it byte for byte as it comes, except for Cargo's own lines (its `Running` and `Doc-tests` lines are shown dimmed without the program's path, and `Finished` goes, since the build verdict said it), and closes with the counts of every test program added up:

```text
OK   Built: 2 compiled [113ms]
     unittests src/lib.rs

running 1 test
test tests::it_works ... ok

test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s

     doc-tests lib1

running 0 tests

test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s

OK   Tests: 1 passed [26ms]
```

When nothing the last such build depended on has changed, the build phase is skipped entirely ([up-to-date checks](up-to-date-checks.md)).

## When Cargo's output is left alone

QuickRust renders only where a person is reading. Its output replaces Cargo's in a terminal, and nowhere else:

- Pipes, IDEs and CI logs get Cargo's output byte for byte. A program parsing Cargo's lines, or rust-analyzer reading `--message-format=json`, must see what Cargo prints.
- `-v`, `-q`, `--message-format` and `--color` are requests for Cargo's own output, and get it.
- `cargo test --doc` gets Cargo's output, since doc tests are built while they run and there is no build to show first.
- Compat mode leaves the terminal to Cargo unless `output = "qrust"` is set explicitly; `output = "cargo"` (or `QRUST_OUTPUT=cargo`) turns QuickRust's rendering off in any mode.

Because output is part of the interface, it is tested like one. What the terminal shows for each command is kept as a snapshot under `tests/snapshots/`, compared whole in the test suite, and every snapshot is checked to contain only ASCII. The [output reference](../reference/output.md) lists the line forms.
