# About the artifact cache

Cargo compiles a crate again in every target directory, even when the same version of the same crate, with the same features and flags, was compiled a minute ago for another project or another checkout. QuickRust's artifact cache is the answer to that waste: a unit compiled once is restored instead of compiled again. This page explains what is cached, how a cached result is known to be the right one, and why the cache is shaped as it is.

## What a unit is

Cargo breaks a build into units: one compilation of one crate target for one profile, target triple and set of features, with one rustc command line. Cargo's own fingerprints decide whether a unit is fresh within one target directory. The artifact cache works one level below that: when Cargo has decided to compile a unit and calls rustc through QuickRust's wrapper, the wrapper first asks whether exactly this compilation has been done before, anywhere on the machine. On a hit it puts the outputs where rustc would have written them, replays the diagnostics rustc printed the first time, and returns rustc's success. Cargo cannot tell the difference, except in the time it took.

The cache lives in `~/.qrust/cache/artifacts`, with the rest of QuickRust's disposable cache, and is shared by every project and checkout of the user. It is part of fast mode: compat mode builds without it, and it is never consulted behind a rustc wrapper you configured, which may be a cache with rules of its own.

## The key

Everything hinges on the key: two compilations with the same key must produce the same outputs, and anything that could change the outputs must be in it. The key is built in two levels.

The first level is everything known before compiling: the cache format, the compiler's identity by its own account (`rustc -vV`: release, commit, LLVM version, host), QuickRust's compiler driver when it compiles the unit, every argument with output and search paths removed, Cargo's per-unit `CARGO_*` variables, and the content of the unit's `OUT_DIR`, where build scripts leave generated code. Each `--extern` argument is replaced by the identity of the artifact it names, which is its content hash, so a dependency compiled differently gives its dependents different keys. (With early cut-off, the identity of a library's metadata is its interface hash instead; see [early cut-off](early-cutoff.md).) A unit that links (a binary, a test, a procedural macro) is keyed by everything it links, including the rlibs its dependencies were compiled against, which the linker finds through search paths without the command line naming them.

The second level is what can only be known after one compilation: the values of the environment variables rustc reported reading (`env!`, `option_env!`, recorded in its dep-info), and, for your own crates, the content of every input file rustc read. A lookup first finds the level-one entry, then checks that those variables and files still have the values they had.

What the key cannot contain is what rustc does not report. A procedural macro that reads a file or an environment variable without declaring it can make a stale hit possible, exactly as it can under sccache. Units that link native libraries by name (`-l`) or ask the compiler to print information are not cached at all.

## Integrity

A cache that may serve a wrong file is worse than none, so every step is defensive. An entry is written to a temporary directory and renamed into place, so a reader sees all of it or none of it. Outputs are hard-linked into the entry when the cache shares their file system, because rustc and the linkers write each output as a new file, so the next build of the unit leaves the entry's copy alone; otherwise they are copied. The dep-info file, which rustc rewrites in place, is always copied.

Every file's content hash (BLAKE3) is recorded with the entry, and every file is checked against it as it is restored. A file rewritten in place through a link, torn by a crash or damaged on disk turns the hit into a miss and removes the entry. A compilation whose input files changed while rustc ran is not stored, since its outputs may not match the inputs as they are now.

Restored files are copies, never links: a later build of that unit, here or in another checkout, would otherwise see a modification time that another checkout set, and Cargo's freshness checks depend on modification times. The restored dep-info and replayed diagnostics have the original build's output and `OUT_DIR` paths replaced with this build's, since Cargo derives its own fingerprints from them.

The guiding rule is that any cache problem means "compile normally". The cache can only save time; it can never be the reason a build fails.

## Dependencies are shared; your crates stay in their checkout

Registry and git dependencies cannot change under a given name and version, so their units are shared across every checkout and project. Your own workspace crates are different. Their sources change all the time, which the second level of the key handles, but their outputs also carry the checkout's absolute path in debug information and panic locations. A crate compiled in `~/work/app` and restored into `~/work/app-feature` would name the wrong directory. So workspace units are keyed by their checkout too: they are reused within the checkout that built them (after `cargo clean`, when switching branches back and forth, in CI with a fixed path) and never in another.

A related wrinkle affects dependencies whose build script generates code that the crate `include!`s, serde among them. Their key includes the content of `OUT_DIR` but not its path, so the unit restored into another target directory names the first one's `OUT_DIR` in panic locations and debug information for the generated code. Behaviour is the same, and QuickRust says so the first time it happens ([fast by default](fast-by-default.md#what-the-levers-cost)).

## Not everything is worth storing

Storing has a price: hashing and linking or copying outputs on every compilation. For libraries the price is low and the value high, because they form the critical path a clean rebuild waits on. For a small workspace binary that links in a moment, copying it into the cache after every edit costs about as much as it would ever save (measured at about 30 ms for a small binary). So a linked workspace output that built faster than `cache.artifacts-min-link-ms` (300 ms by default) is not stored, and QuickRust remembers that the unit is quick. That memory matters elsewhere: such a unit also does not get rustc's parallel frontend or relinking, whose start-up would only lengthen it.

## Living with sccache

sccache and the artifact cache solve overlapping problems from different positions. sccache is a general compiler cache that can be shared between machines through remote storage, and it sits in the `RUSTC_WRAPPER` slot. QuickRust's cache is local to one user, knows Cargo's units and QuickRust's driver, and can key a unit by its dependencies' interfaces rather than their bytes, which is what makes early cut-off possible.

They do not stack. When you configure sccache (or any wrapper), QuickRust runs it as you configured it and steps its own cache aside, so there is never a question of which cache served a file. sccache keeps working as before, and QuickRust reports its statistics. [How to use sccache with QuickRust](../how-to/use-sccache.md) covers the choices.

## Space

The cache is part of QuickRust's cache directory and is kept within the same bounds as everything else there: entries used least recently go first when the cache grows beyond `cache.max-size` or the disk runs below `cache.min-free`. A file shared by an entry and a build directory (a restored unit) counts once and is freed only with its last link. An entry whose files a build directory in use still links is kept, since removing it would free little and lose the entry. [Owning the environment](owning-the-environment.md) explains the whole cache's policy.
