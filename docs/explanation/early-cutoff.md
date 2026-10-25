# About early cut-off and QuickRust's compiler driver

Change one line inside a function in a library crate, and Cargo recompiles that crate, then every crate that depends on it, then every binary and test above them, even when nothing those crates can see has changed. Early cut-off is QuickRust's way of stopping that cascade at the crate that was edited. This page explains why the cascade happens, what QuickRust changes in the compiler to stop it, which edits benefit, and what it costs.

## Why an edit rebuilds everything above it

Every library rustc compiles records a crate hash, the SVH (strict version hash). Dependents record the SVH of each crate they were compiled against, and rustc refuses to load a dependency whose hash differs from the one recorded. rustc's SVH covers the whole crate, function bodies included, so any edit changes it. Cargo, for its part, fingerprints a unit by the outputs of its dependencies. Either way, a changed library means recompiling everything that depends on it, even when the change was a string literal deep inside a private function.

That is safe and simple, and in a large workspace it is most of the time spent after an edit. ripgrep's core-crate edit, in QuickRust's benchmarks, took Cargo about 640 ms; nearly all of it was compiling crates the edit could not affect.

## Hashing what dependents can observe

QuickRust's compiler driver is rustc itself: the toolchain's own `librustc_driver`, with its LLVM and its release, run through a small program that installs QuickRust's callbacks. With no callbacks active, its outputs are rustc's byte for byte, and a test holds it to that. With the interface hash on, one thing changes: the SVH a library records is computed from what its dependents can observe, not from the whole crate.

What dependents can observe is more than the public API. The hash covers every item's HIR, leaving out only the bodies of functions whose code stays in their crate: not generic, not inlinable across crates, not `const`, not `async`, and not returning `impl Trait` (whose hidden type's layout and auto traits leak). It covers every definition in order and every macro expansion by number, because dependents refer to a crate's definitions and expansions by position, so a body that gains a closure or a macro call shifts them and changes the hash. It covers the positions (file, line and column) of everything dependents can see, because their debug information and panic messages name those positions. It covers the symbols dependents may link against instead of instantiating their own, and everything else rustc's own hash covers besides the HIR. Procedural macros keep rustc's hash, since their bodies run inside their dependents' compilations.

So an edit that changes only a left-out body leaves the library's SVH unchanged. The library itself is still compiled. Its dependents, keyed in the [artifact cache](artifact-cache.md) by that interface hash instead of the bytes of the library, find their entries and are restored instead of compiled. The rebuild stops at the edited crate, up to the units that link.

## Which edits are cut off

The rule follows from the hash. Edits inside the body of an ordinary, non-generic, non-inline function, that keep its lines and add no closure, macro call or new generic instantiation, are cut off: changing a literal, a condition, an arithmetic expression, a call to an existing function. Edits that dependents can observe are not, and must not be: a signature, a generic or `#[inline]` function's body, a `const fn`, a struct's fields, a new item. Edits that add or remove lines are often not cut off either, because they move the positions of the observable definitions below them in the same file, which dependents' debug information records. That is a real cost of keeping debug information truthful, not an accident.

## Relinking instead of recompiling

Cut-off stops at units that link. A binary or test executable depends on the bytes of every library it links, so when the edited library's code changes, the binary must be linked again. It does not have to be compiled again: its own code has not changed, and neither have the interfaces it was compiled against.

So when the driver compiles a binary or test executable, it keeps the object files: it writes the `.rlink` file that describes the link, and hard-links the objects into `<stem>.qrust-objects/` before linking deletes them, with a `<stem>.qrust-relink` record of what they were compiled from. When a later build finds that unit compiled from the same sources against the same dependency interfaces, and only what it links has changed, it runs rustc's `-Zlink-only` on the kept objects instead of compiling. For a large binary that is a fraction of the work: ripgrep's binary relinks in about 45 ms of the roughly 350 ms compiling it takes. Units the cache saw compile quickly are not relinked, and neither are optimised, non-incremental builds whose MIR inlining could copy code across crates; those are keyed by content as before.

Together these brought ripgrep's core-crate edit from about 640 ms under Cargo to about 140 ms through QuickRust at checkpoint 2 (medians in [`benchmarks/speed-index.jsonl`](../../benchmarks/speed-index.jsonl); summary in [`benchmarks/PROGRESS.md`](../../benchmarks/PROGRESS.md)). Not every workload gains as much yet: the axum service's core edit took about 980 ms through QuickRust against about 1.03 s under Cargo, a gap the progress log lists as open.

## Outputs before bookkeeping

The driver also reorders one step of rustc's work. rustc saves its incremental state (the dependency graph and query results, around 100 ms for a large crate) when its type context ends, and only then links the outputs. Cargo, and every unit waiting on this one, wait for that save. The driver links inside code generation, while the type context is still alive, then tells QuickRust's wrapper that the outputs are written (one byte on a pipe) and stops writing to stdout and stderr. rustc then saves its incremental state exactly as it would have, while the build goes on. This applies on Unix, and not to crates with `#![feature]`, whose unused-feature warnings rustc emits after code generation.

## Why the driver is built per toolchain

rustc's internal interface changes between releases, so a driver compiled for one compiler cannot load another's `librustc_driver`. QuickRust therefore builds the driver for each toolchain, from sources embedded in QuickRust, against that toolchain's `rustc-dev` component (which it installs when needed), and uses it only once it reports exactly the compiler's own `rustc -vV`. It lives in `~/.qrust/cache/drivers/`, keyed by the compiler's commit and host and the driver's own source.

The driver is part of every artifact-cache key for the units it compiles, so what a build stored without it, builds with it would never find. QuickRust therefore builds it before a toolchain's first use where it can: when `qrust setup` installs a toolchain, and when a channel moves to a new release. Otherwise, the first build that finds it missing starts building it in the background and says so; later builds use it once it is there. `qrust driver status` says where things stand, and `qrust driver build` builds it now.

## When the driver fails

The driver is an optimisation, and it is treated as one. If the driver cannot be built for a toolchain, that toolchain compiles with stock rustc, and the reason is recorded next to where the driver would be; background builds do not keep retrying. If the driver fails on a crate, that crate is compiled by rustc instead, and by rustc from then on; the driver's report is kept under `failed/` for diagnosis. Programs that Cargo runs in rustc's place, such as clippy-driver or a wrapper you configured, run as they are. `fast.early-cutoff = "off"` turns all of this off.

## How it is held to account

Restoring a crate's dependents instead of compiling them is only acceptable if the result is exactly what compiling them would give. The cut-off gate (`qrust-bench cutoff`) checks precisely that: for random edits inside the function bodies of each benchmark workload's crates, it builds as a developer would (build, edit, build again with the same QuickRust state) and also builds the edited source from nothing, and every library, metadata file and executable must be byte-identical. Alongside, an incremental loop appends lines to every source file and builds again, which makes rustc verify, in an incremental session, what it reads from crates whose hash did not change. Results are recorded in `benchmarks/cutoff-gate.jsonl`.

The byte comparison runs without rustc's parallel frontend and without the wild linker, because neither produces the same bytes twice. That points at the one known limit: under several threads rustc numbers a crate's nested statics in varying order, and those numbers are part of what dependents can observe, so the interface hash keeps them. A crate with such statics can therefore miss the cut-off once after a cold build (compiled with the parallel frontend), and then hits as usual.
