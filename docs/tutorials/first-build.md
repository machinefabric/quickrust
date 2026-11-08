# Your first build with QuickRust

In this tutorial we will create a small Rust program, build and run it through QuickRust, break it and mend it, give it a dependency and a test. Along the way we will meet QuickRust's build output, the build directory it keeps for every project, its record of every warning, and its cache.

It takes about ten minutes. You need a terminal and an internet connection; you do not need to know Cargo well.

## Before we start

QuickRust must be installed. If it is not, follow [How to install QuickRust](../how-to/install.md) and open a new terminal afterwards.

Check that `cargo` is now QuickRust:

```bash
qrust doctor
```

The command prints what QuickRust uses (its home, the toolchain, Cargo and rustc) and ends with a line that says only:

```text
OK
```

If it ends with `FAIL` instead, the lines above it say what is wrong; [How to install QuickRust](../how-to/install.md) covers the usual causes.

## Create a program

We will work in a new project called `hello`. In a directory where you keep projects, run:

```bash
cargo new hello
cd hello
```

QuickRust answers with one line, and a note from Cargo:

```text
OK   Created binary (application) `hello` package
NOTE see more `Cargo.toml` keys and their definitions at https://doc.rust-lang.org/cargo/reference/manifest.html
```

Notice how each line starts: `OK` for something that worked, `NOTE` for something worth knowing. Every line QuickRust prints about an outcome starts with such a word, in the same column.

## Build and run it

Run the program:

```bash
cargo run
```

For a moment a status line shows what is being built; then it is replaced by a single summary, and the program runs:

```text
OK   Built: 1 compiled [59ms]
Hello, world!
```

The time in brackets will differ on your machine. Where plain Cargo prints a `Compiling` line for every crate, QuickRust prints one line for the whole build.

Now list the project's files:

```bash
ls
```

```text
Cargo.lock  Cargo.toml  src
```

There is no `target` directory. QuickRust keeps each project's build output in its own cache, outside the project. Ask it where:

```bash
qrust path
```

```text
/home/you/.qrust/cache/builds/hello-38cd7ad91fa9d702
```

Your path ends in a different number. Run the program again:

```bash
cargo run
```

```text
OK   Up to date: 1 fresh
Hello, world!
```

Nothing changed since the last build, and QuickRust says so before the program starts.

## Make a warning

Open `src/main.rs` and replace its contents with:

```rust
fn main() {
    let name = "world";
    println!("Hello, world!");
}
```

We have made a variable and not used it. Run the program:

```bash
cargo run
```

```text
OK   Built: 1 compiled [58ms]
WARN 1 warning in hello (bin): unused_variables x1; `qrust warnings` shows them
Hello, world!
```

The build worked, so the summary still says `OK`, and a `WARN` line counts the warning by its kind instead of printing it. Let's read it:

```bash
qrust warnings
```

```text
warning: unused variable: `name`
 --> src/main.rs:2:9
  |
2 |     let name = "world";
  |         ^^^^ help: if this is intentional, prefix it with an underscore: `_name`
  |
  = note: `#[warn(unused_variables)]` (part of `#[warn(unused)]`) on by default
```

This is the compiler's own message, kept by QuickRust when the build ran.

## Make an error

Now change the `println!` line so that it uses the variable, but misspell it:

```rust
fn main() {
    let name = "world";
    println!("Hello, {}!", nme);
}
```

Run the program:

```bash
cargo run
```

This time the error is shown in full, under a header naming the crate it belongs to, and the summary says `FAIL`:

```text
--- hello (bin)
error[E0425]: cannot find value `nme` in this scope
 --> src/main.rs:3:28
  |
3 |     println!("Hello, {}!", nme);
  |                            ^^^
  |
help: a local variable with a similar name exists
  |
3 |     println!("Hello, {}!", name);
  |                             +

FAIL Build: 1 error in hello (bin), first at src/main.rs:3:28 [41ms]
```

The program did not run. Correct `nme` to `name` and run it again:

```bash
cargo run
```

```text
OK   Built: 1 compiled [59ms]
Hello, world!
```

The warning has gone too, because the variable is used now.

## Add a dependency

Our program will print a number with the help of a small crate, `itoa`. Add it:

```bash
cargo add itoa
```

QuickRust shows the dependency it added and the features it offers, then the result:

```text
     added      itoa                     1.0.18 to dependencies
     features -no-panic
     locked +itoa 1.0.18
OK   [117ms]
```

You may see a newer version than `1.0.18`. Lines that belong to an outcome are indented under it, and the `OK` line has nothing to add but the time: the lines above already say what happened.

Replace `src/main.rs` with a program that uses the crate:

```rust
fn main() {
    let mut buffer = itoa::Buffer::new();
    println!("The answer is {}", buffer.format(answer()));
}

fn answer() -> u32 {
    6 * 7
}
```

Run it:

```bash
cargo run
```

QuickRust downloads the crate, reports the download in one line, and builds both crates:

```text
     Downloaded 1 crate, 15.6 KiB [0.08s]
OK   Built: 2 compiled [241ms]
The answer is 42
```

## Clean and build again

Remove the build output:

```bash
cargo clean
```

```text
OK   Removed 123 files, 21.7 MiB
```

Then run the program once more:

```bash
cargo run
```

```text
OK   Built: 1 compiled, 1 restored from QuickRust's cache [119ms]
The answer is 42
```

Only our own crate was compiled. QuickRust had kept `itoa` from the first build and restored it instead of compiling it again; every project on this machine that uses the same crate, built the same way, shares it. (Why this is safe is the subject of [About the artifact cache](../explanation/artifact-cache.md).)

## Add a test

Add a test to the end of `src/main.rs`:

```rust
#[test]
fn the_answer_is_42() {
    assert_eq!(answer(), 42);
}
```

Run the tests:

```bash
cargo test
```

QuickRust builds first, in the same way as before; then the tests run and their output is shown as the test harness printed it, and a last line adds up the results:

```text
OK   Built: 1 compiled, 1 fresh [73ms]
     unittests src/main.rs

running 1 test
test the_answer_is_42 ... ok

test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s

OK   Tests: 1 passed [12ms]
```

## What we have done

We have created a program, built and run it, read a warning QuickRust kept for us, fixed an error from its full message, added a dependency, seen QuickRust restore it from its cache after a clean, and run a test. Throughout, every outcome was one line starting with `OK`, `WARN` or `FAIL`.

Everything we typed was an ordinary Cargo command: QuickRust stands in for `cargo` and runs your Rust toolchain's own Cargo underneath.

## Where to go next

- [Finding where a build's time goes](where-the-time-goes.md), the next tutorial, looks inside a larger build.
- [How to see the warnings, errors and full log of a build](../how-to/read-warnings-and-errors.md) and [How to find build outputs without target/](../how-to/find-build-outputs.md) for daily work.
- [About QuickRust's build output](../explanation/build-output.md) and [How QuickRust works](../explanation/architecture.md) for the reasons behind what you saw.
