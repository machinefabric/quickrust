# Finding where a build's time goes

In this tutorial we will build a small program that uses `serde`, then ask QuickRust where the build spent its time: which crates the build had to wait for, what the procedural macros and build scripts cost, and why a rebuild compiled what it did. At the end we will compare two builds and write a report we can open in a browser.

It takes about fifteen minutes. We assume QuickRust is installed and that you have done [Your first build with QuickRust](first-build.md), so `cargo build` and QuickRust's summary lines are familiar.

## Build a program with dependencies

Create a project and give it two dependencies:

```bash
cargo new inventory
cd inventory
cargo add serde --features derive
cargo add serde_json
```

Each `cargo add` ends in an `OK` line, with the features of the dependency listed above it.

Replace `src/main.rs` with:

```rust
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Debug)]
struct Item {
    name: String,
    count: u32,
}

fn main() {
    let items = vec![Item { name: "apple".into(), count: 3 }, Item { name: "pear".into(), count: 5 }];
    let json = serde_json::to_string_pretty(&items).unwrap();
    println!("{json}");
    let back: Vec<Item> = serde_json::from_str(&json).unwrap();
    println!("{} items", back.len());
}
```

Build it:

```bash
cargo build
```

This build takes a few seconds. The status line counts the crates as they finish; at the end you will see something like:

```text
     Downloaded 10 crates, 924.3 KiB [0.12s]
OK   Built: 17 compiled, 1 restored from QuickRust's cache [2.00s]
```

If you did the first tutorial, one crate (`itoa`, which `serde_json` uses) was restored from QuickRust's cache instead of compiled. Your numbers and versions may differ a little; the steps below work the same.

## See the critical path

QuickRust recorded every compiler run of that build. Ask it about the timing:

```bash
qrust analyze timing
```

```text
Timing
  critical path: 1.78s  [observed]
  ON CRITICAL PATH      KIND              DURATION  STATE     EDGE
  proc-macro2@1.0.107   build-script      141ms     compiled  observed
  proc-macro2@1.0.107   build-script-run  115ms     ran       inferred
  proc-macro2@1.0.107   lib               139ms     compiled  inferred
  quote@1.0.47          lib               91ms      compiled  observed
  syn@3.0.9             lib               435ms     compiled  observed
  serde_derive@1.0.229  proc-macro        706ms     compiled  observed
  serde@1.0.229         lib               125ms     compiled  observed
  inventory@0.1.0       bin               106ms     compiled  observed
  slowest units (15 of 24):
  UNIT                  KIND              MODE   DURATION  CRITICAL  ROLE    CACHE
  serde_derive@1.0.229  proc-macro        build  706ms     yes       host    stored
  syn@3.0.9             lib               build  435ms     yes       host    stored
  ...
  serde_core@1.0.229    lib               build  541ms     no        target  stored
  ...
  peak concurrent compilers: 9   cpu time: 9.13s   peak rss: 372.6 MiB
```

The first table is the critical path: the chain of steps, each waiting for the one before, that decided how long the build took. Read it from the top. `proc-macro2`'s build script is compiled and run, then `proc-macro2` itself, then `quote`, then `syn`, then the `serde_derive` macro, then `serde`, and only then our own program. Together they take 1.78 seconds of the 2.00.

Notice `serde_core` in the second table: it took 541 ms, longer than most steps on the path, but it is marked `no` under `CRITICAL`. It was compiled while the path was busy elsewhere, so making it faster would not have finished the build any sooner.

Notice also the last line: the compilers used 9.13 seconds of CPU time in a build that took 2.00 seconds, because up to 9 of them ran at once.

## Look at the macro and the build scripts

`serde_derive` is the longest step on the path. Ask about procedural macros:

```bash
qrust analyze proc-macros
```

```text
Procedural macros
  MACRO CRATE           STATE     OWN COMPILE  DIRECT USERS  DOWNSTREAM  DOWNSTREAM TIME  CLASS
  serde_derive@1.0.229  compiled  706ms        1             2           230ms            critical-path-candidate
  Proc-macro execution timing
    Unavailable: a stable toolchain does not expose time spent expanding macros in dependent crates; only the macro crate's own compilation is measured
```

The class `critical-path-candidate` says what we saw above: this macro sits on the critical path. The last line is QuickRust being honest about what it cannot measure; it says "unavailable" rather than showing a zero.

Now the build scripts:

```bash
qrust analyze scripts
```

```text
Build scripts
  PACKAGE              THIS RUN  RUN TIME  COMPILE  RERUN-IF       OUT_DIR  RUNS SEEN  STATE
  proc-macro2@1.0.107  ran       115ms     141ms    3 file, 1 env  0 B      1          observed
  quote@1.0.47         ran       13ms      137ms    1 file, 0 env  0 B      1          observed
  serde@1.0.229        ran       12ms      151ms    1 file, 0 env  4.0 KiB  1          observed
  serde_core@1.0.229   ran       15ms      166ms    1 file, 0 env  4.0 KiB  1          observed
  serde_json@1.0.154   ran       2ms       69ms     1 file, 0 env  0 B      1          observed
  zmij@1.0.23          ran       14ms      132ms    1 file, 0 env  0 B      1          observed
  No broad invalidation detected.
```

Six crates have build scripts. Each one is compiled and then run before its crate can be compiled; the `RERUN-IF` column says what each declared it depends on, which decides when it runs again.

## Change one line and rebuild

Open `src/main.rs` and change `count: 3` to `count: 4`. Build again:

```bash
cargo build
```

```text
OK   Built: 1 compiled, 17 fresh [154ms]
```

Only our program was compiled. Ask why:

```bash
qrust analyze rebuilds
```

```text
Rebuild reasons
  REASON  UNITS  TIME
  source  1      90ms
  previously known units rebuilt: 1 of 18 in the index (ratio 0.056)
  UNIT             KIND  REASON  COST  CONFIDENCE  DETAIL
  inventory@0.1.0  bin   source  90ms  high        changed: src/main.rs (modified)
```

QuickRust names the reason (a source file changed), the file, and how sure it is. When a build rebuilds more than you expected, this is where to look.

## Compare the two builds

Every build is a recorded run. List them:

```bash
qrust runs
```

```text
RUN                         STARTED                   FAMILY       STATE           WALL  OBSERVED  COMMAND
01M4SFW9PX1526541E7WM1ZJ9F  2026-10-13T02:01:19.327Z  build        completed      154ms  rustc     cargo build
01M4SFW1V3NZTKFD0YHGA3GXES  2026-10-13T02:01:11.267Z  build        completed      2.00s  rustc     cargo build
01M4SFW1JF0TMXB0QNFFZFA7PF  2026-10-13T02:01:10.991Z  passthrough  completed      246ms  session   cargo add serde_json
01M4SFVZWTN755HZZ5TKTQ5W32  2026-10-13T02:01:09.274Z  passthrough  completed      1.69s  session   cargo add serde --features derive
```

We do not need the long identifiers: `latest` is the last run and `latest~1` the one before it. Compare the full build with the rebuild:

```bash
qrust compare latest~1 latest
```

```text
Comparison: run 01M4SFW1V3NZTKFD0YHGA3GXES  ->  run 01M4SFW9PX1526541E7WM1ZJ9F
  comparability: comparable
  METRIC                    BASELINE     CANDIDATE    DELTA
  wall_time_ns              2.00s        154ms        -92.3%
  critical_path_ns          1.78s        90ms         -95.0%
  sum_unit_time_ns          3.61s        90ms         -97.5%
  cpu_time_ns               9.13s        134ms        -98.5%
  peak_rss_bytes            372.6 MiB    126.3 MiB    -66.1%
  units_compiled            17           1            -94.1%
  units_restored            1            0            -100.0%
  ...
```

Notice the second line: QuickRust first checks that the two runs can be compared at all (same toolchain, same machine, same kind of command) before it shows any difference.

## Write a report

Finally, write the full build's analysis as a web page:

```bash
qrust report --run latest~1 --format html -o report.html
```

```text
OK   Wrote report.html
```

Open `report.html` in your browser. It holds everything we looked at, every section on one page, and needs no network.

## What we have done

We built a program with real dependencies and found its critical path, saw that a procedural macro and the crates it needs decided most of the build's time, looked at the build scripts, found out exactly why a rebuild compiled one crate, compared two builds, and wrote a report.

## Where to go next

- [How to find out why crates rebuilt](../how-to/find-why-crates-rebuilt.md) when a rebuild is larger than it should be.
- [How to measure the effect of a build setting](../how-to/measure-a-change.md) to try a change, such as a different codegen setting, with repetitions and a verdict.
- [Commands](../reference/commands.md) lists every analysis section and its options, including `--filter`, `--sort` and `--limit`.
- [About measuring builds](../explanation/measurement.md) explains what QuickRust can and cannot observe, and why some values are "unavailable".
