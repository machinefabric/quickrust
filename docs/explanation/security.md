# Security and privacy

QuickRust sits in front of Cargo and rustc, on every build, with the permissions of the user who runs it. That position deserves scrutiny: what does it add to what a user already trusts, what does it keep about builds, and what can it delete? This page explains the trust model and the reasoning behind each boundary. Where things live on disk is listed in the [files reference](../reference/files.md).

## Trust model

QuickRust adds no network service and no daemon. It runs with the permissions of the
user who runs Cargo and delegates to the Cargo and rustc that user already
trusts. What it adds to the trusted path:

- the `qrust` binary, in the position of `cargo` (and, after `qrust setup`, of `rustc`, `rustfmt` and the other toolchain programs, which it runs from the selected toolchain);
- `qrust-rustc-wrapper`, in front of every rustc invocation;
- in fast mode, QuickRust's compiler driver: rustc's own `librustc_driver` from the toolchain, run through a small program QuickRust builds from sources it carries, against that toolchain's `rustc-dev` component ([early cut-off](early-cutoff.md)).

QuickRust and its wrapper forward the user's arguments and environment unchanged. What
they add, in fast mode (compat mode adds nothing):

- the profile settings, as `--config` arguments before the subcommand;
- `-Zthreads=N` for the units Cargo schedules that are compiled from scratch, with
  `RUSTC_BOOTSTRAP=1` set for the compiler processes of Cargo's units (on a stable
  compiler). That variable would also let such a process accept `#![feature]`s, so
  QuickRust passes `-Zallow-features=` with it: a crate that enables a nightly feature is
  rejected as under plain stable Cargo. Build scripts and the compiler probes they run
  never see it, and nothing of this applies when the user sets `RUSTC_BOOTSTRAP`;
- the artifact cache (below);
- early cut-off: units compiled by the compiler driver, which computes a library's crate
  hash from what its dependents can observe and keeps binaries' object files for
  relinking; with no such setting active its outputs are rustc's byte for byte;
- when the user has installed `wild`, linking with it: the binary found on `PATH` is
  trusted like any tool there, through the link `~/.qrust/linker/ld`;
- installing toolchains: manifests and archives from the dist server
  (`toolchain.dist-server`, HTTPS to static.rust-lang.org by default), each manifest checked
  against its published SHA-256 and each archive against the manifest's; archives are
  unpacked with nothing allowed outside their directory (no absolute paths, `..`, links
  leading out, or entries beneath links; no hard links or devices; a bound on unpacked
  size). Crates that `qrust install` unpacks get the same treatment.

The wrapper writes one event file per invocation into the session's private event
directory, and the build's output is stored next to them.

## Updates

`qrust self update` replaces the program that sits in front of every build, so what it
will install is the narrowest thing on this page.

A released QuickRust is built with four things about its own release line: the release
registry it is published in, its channel, a signing environment, and the public halves of
three **root keys**. It reads none of them at run time, and a QuickRust built from source
has none: it has no release line, and updates nothing.

A release is described by a manifest in the registry, published with a signature beside
it. The signature file carries a **release-key certificate**: the public half of the key
that signs manifests, the environment it is for, and the span of time it is valid. An
update goes ahead only when all of this holds:

- two distinct roots of this build signed the certificate's exact bytes. One root cannot
  authorize a release key, and one lost root does not stop releases;
- the certificate is for this build's environment and the clock is inside its validity;
- the release key it names signed the manifest's exact bytes, as fetched;
- every certificate in the file holds, not only the one that signed: a file carrying one
  that does not is a publishing mistake or tampering, and reading past it would hide either;
- the archive is the one the manifest describes, by SHA-256 and size;
- the binary in it runs, and says it is the version it was published as.

So the registry's host, the CDN in front of it and a mirror named in `self.update-url` are
all untrusted: whoever controls them can withhold an update, and cannot make QuickRust
install anything the release key did not sign. A release key that leaks is contained by
its certificate's expiry and environment, and replaced by certifying another.

What this does not protect against: a manifest that is genuine and old. Someone who
controls the registry can serve an earlier signed manifest, and QuickRust then sees no
newer release. It never installs an older one than it is.

The package managers have their own chain. A copy installed with apt or dnf is verified
by them against the MachineFabric repository's OpenPGP key, and a Homebrew formula carries
the archive's SHA-256.

## QuickRust's artifact cache

In fast mode QuickRust restores compiler outputs instead of compiling when the same unit was
compiled before. It is local to the user (`~/.qrust/cache/artifacts`, inside `~/.qrust/cache`,
which is created with mode 0700), has no network component, and is never consulted behind a rustc wrapper the user configured.

- **Key.** rustc's identity by its own account (`rustc -vV`: release, commit, LLVM,
  host — asked once per compiler binary and toolchain selection), every argument with
  output and search paths removed and each `--extern` replaced by the content hash of the
  artifact it names, Cargo's per-unit variables, the content of `OUT_DIR`, then the values
  of the variables rustc reported reading and, for workspace units, the content of every
  input file rustc read. Inputs rustc does not report (files or variables a proc macro
  reads without declaring them) are not part of the key: such a macro can yield a stale
  hit, as it can with sccache.
- **Integrity.** Entries are written to a temporary directory and renamed into place.
  Outputs are hard-linked into an entry when the cache shares their file system (rustc
  and the linkers write each output as a new file, so a later build does not touch the
  entry's) and copied otherwise; dep-info, which rustc rewrites in place, is always
  copied. Every file's content hash (BLAKE3) is recorded and checked as the file is
  restored, so a file rewritten in place through a link, torn by a crash or damaged on
  disk makes the entry a miss and removes it. A compilation whose inputs changed while
  it ran is not stored. Anyone who can write to `~/.qrust` can plant an artifact — the same
  trust boundary as `~/.cargo/registry`, which holds the sources.
- **Paths.** A restored unit's dep-info and diagnostics are rewritten to the current
  output and `OUT_DIR` paths. Its binary contents are not: a dependency restored into
  another target directory names the first one's `OUT_DIR` in panic locations and debug
  information of build-script-generated code. Workspace units are only reused within the
  checkout that built them.

## sccache

QuickRust does not implement sccache's cache or compute its keys. Correctness of
sccache's artifacts is sccache's; QuickRust composes the wrapper chain and reads
statistics. Consequences:

- a cache outage can only result in local compilation (`cache.fail = "open"`) or
  a failure before compiling (`closed`) — never in a substituted artifact;
- a run degraded by an outage is recorded as `degraded`, and nothing in its
  report is marked as a cache hit;
- a wrapper *you* configured is never skipped: if it cannot start, the build
  fails exactly as under upstream Cargo.

Remote-cache credentials, read-only modes for untrusted CI and cache
administration are sccache configuration today; QuickRust neither stores nor prints
them.

## Secrets

- Environment values are persisted only for an allowlist of build-relevant
  variables (`RUSTFLAGS`, `CARGO_PROFILE_*`, `CARGO_TARGET_*`, …). Set
  `security.persist-environment = "none"` to persist none.
- Variables whose names look like credentials (`*TOKEN*`, `*SECRET*`,
  `*PASSWORD*`, `*_KEY`, `AWS_*`, `SCCACHE_*`, …) are recorded as "set" with the
  value `<redacted>`.
- Command lines are redacted before they are stored: values of `--token` and
  similar flags, `key=value` arguments with credential-like keys, the positional
  token of `cargo login`, and user-info in URLs.
- Environment values rustc reports as inputs (`env!`) are stored as truncated
  digests, and not at all for credential-like names.
- Source code is never collected; local source files are represented by path and
  content digest.

The test suite plants credentials and asserts they appear in no stored file and
no rendered output.

## Paths

State under `~/.qrust` keeps real local paths (the directories holding run records and
QuickRust's cache are created with mode 0700). Everything exported — `QRUST_REPORT`, `--format json`, HTML, baselines —
replaces the workspace root, the home directory, QuickRust's Cargo home and QuickRust's home
with `$WORKSPACE`, `$HOME`, `$CARGO_HOME`, `$QRUST_HOME`
(`security.redact-path-home`, on by default).

## Deleting

QuickRust deletes only inside its own cache (`~/.qrust/cache`), whose content it created and can
rebuild or download again: workspaces' build directories, artifact cache entries, compiler
drivers, sources of installed programs, unfinished downloads, remembered answers. Crates and git
checkouts downloaded into QuickRust's Cargo home are removed by Cargo itself (`cargo -Zgc clean gc`,
which waits for builds reading them), at QuickRust's request; QuickRust never deletes anything else
there, and never your configuration or credentials. It never deletes in a project, in a target
directory the user chose, or among toolchains (`qrust toolchain remove`, and channels' superseded
releases after a day unused, aside). A collection never removes
anything used in the last 15 minutes, a build directory a QuickRust command holds (its lock) or one
a Cargo holds a lock in; it renames what it removes into `cache/tmp` before deleting it, so
nothing is ever seen half deleted, and logs what each collection removed (`cache/gc-log.jsonl`:
counts and bytes by kind, and the build directories and program sources by path). The scans
never follow symbolic links. `qrust gc --dry-run` says what a collection would remove.

`qrust inventory` plans within a build directory and deletes nothing: it classifies anything it
does not recognise as `uncertain`, which is never a candidate, and while any Cargo holds the
build lock or any other QuickRust session is alive, the plan reports zero bytes as reclaimable now.
Benchmark build directories QuickRust created itself are removed only with their marker file.

## Changes to a repository

QuickRust writes into a repository only on explicit request. Remediations are
previews bound to the exact content they were computed from. Only changes to
QuickRust's own configuration are classified `safe` and can be applied
(`qrust fix --apply <id>`); applying verifies the file is byte-identical to the
previewed preimage, verifies the result, and can be rolled back. Profile,
linker, dependency and `build.rs` changes are `review_required` and are only
ever shown.

## HTML reports

Self-contained: no scripts, no external resources. Every repository-controlled
string (package names, features, paths, diagnostics) is HTML-escaped.

## Build scripts

QuickRust never executes a build script. Their behaviour is read from what Cargo
leaves on disk after running them.
