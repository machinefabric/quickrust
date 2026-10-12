# Security and privacy

## Trust model

Haul adds no network service and no daemon. It runs with the permissions of the
user who runs Cargo and delegates to the Cargo and rustc that user already
trusts. What it adds to the trusted path:

- the `haul` binary, in the position of `cargo`;
- `haul-rustc-wrapper`, in front of every rustc invocation.

Both forward the user's arguments and environment unchanged. What they add, in
fast mode (compat mode adds nothing):

- the profile settings, as `--config` arguments before the subcommand;
- `-Zthreads=N` for Cargo's non-incremental units, with `RUSTC_BOOTSTRAP=1` set for
  those compiler processes only. That variable also lets such a process accept other
  unstable flags and `#![feature]`s; a crate that enables a nightly feature
  unconditionally would compile under Haul where plain stable Cargo rejects it.
  Build scripts and the compiler probes they run never see it;
- the artifact cache (below);
- calling the toolchain's Cargo directly instead of rustup's proxy, with exactly the
  variables the proxy sets, learned by running a probe once through the proxy: Cargo
  executes Haul itself as an external subcommand (`cargo haul-env-probe`, from
  `~/.haul/probe-bin`) to report its environment.

The wrapper writes one event file per invocation into the session's private event
directory, and the build's output is stored next to them.

## Haul's artifact cache

In fast mode Haul restores compiler outputs instead of compiling when the same unit was
compiled before. It is local to the user (`~/.haul/cache/artifacts`, mode 0700), has no
network component, and is never consulted behind a rustc wrapper the user configured.

- **Key.** rustc's identity (path, size, modification time), every argument with
  output and search paths removed and each `--extern` replaced by the content hash of the
  artifact it names, Cargo's per-unit variables, the content of `OUT_DIR`, then the values
  of the variables rustc reported reading and, for workspace units, the content of every
  input file rustc read. Inputs rustc does not report (files or variables a proc macro
  reads without declaring them) are not part of the key: such a macro can yield a stale
  hit, as it can with sccache.
- **Integrity.** Entries are written to a temporary directory and renamed into place;
  a damaged or missing entry is a miss. A compilation whose inputs changed while it ran
  is not stored. Anyone who can write to `~/.haul` can plant an artifact — the same
  trust boundary as `~/.cargo/registry`, which holds the sources.
- **Paths.** A restored unit's dep-info and diagnostics are rewritten to the current
  output and `OUT_DIR` paths. Its binary contents are not: a dependency restored into
  another target directory names the first one's `OUT_DIR` in panic locations and debug
  information of build-script-generated code. Workspace units are only reused within the
  checkout that built them.

## sccache

Haul does not implement sccache's cache or compute its keys. Correctness of
sccache's artifacts is sccache's; Haul composes the wrapper chain and reads
statistics. Consequences:

- a cache outage can only result in local compilation (`cache.fail = "open"`) or
  a failure before compiling (`closed`) — never in a substituted artifact;
- a run degraded by an outage is recorded as `degraded`, and nothing in its
  report is marked as a cache hit;
- a wrapper *you* configured is never skipped: if it cannot start, the build
  fails exactly as under upstream Cargo.

Remote-cache credentials, read-only modes for untrusted CI and cache
administration are sccache configuration today; Haul neither stores nor prints
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

State under `~/.haul` keeps real local paths (directories are created with mode
0700). Everything exported — `HAUL_REPORT`, `--format json`, HTML, baselines —
replaces the workspace root, the home directory, `CARGO_HOME` and `RUSTUP_HOME`
with `$WORKSPACE`, `$HOME`, `$CARGO_HOME`, `$RUSTUP_HOME`
(`security.redact-path-home`, on by default).

## Deleting

This release deletes nothing in build directories. `haul gc` requires
`--dry-run` and produces a plan. The scan never follows symbolic links, counts a
link as an entry of its own, and classifies anything it does not recognise as
`uncertain`, which is never a candidate. While any Cargo holds the build lock or
any other Haul session is alive, the plan reports zero bytes as reclaimable now.

The only directories Haul removes are benchmark build directories it created
itself, identified by a marker file; a directory without the marker is refused.

## Changes to a repository

Haul writes into a repository only on explicit request. Remediations are
previews bound to the exact content they were computed from. Only changes to
Haul's own configuration are classified `safe` and can be applied
(`haul fix --apply <id>`); applying verifies the file is byte-identical to the
previewed preimage, verifies the result, and can be rolled back. Profile,
linker, dependency and `build.rs` changes are `review_required` and are only
ever shown.

## HTML reports

Self-contained: no scripts, no external resources. Every repository-controlled
string (package names, features, paths, diagnostics) is HTML-escaped.

## Build scripts

Haul never executes a build script. Their behaviour is read from what Cargo
leaves on disk after running them.
