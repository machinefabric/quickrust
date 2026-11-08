# How to export a build report

This guide shows you how to get a build's report out of QuickRust, as JSON for tools or as HTML for people, either as part of the build or afterwards.

QuickRust records every build it runs in `~/.qrust`; nothing is written into your repository unless you ask for it as below.

## Write a report with every build

Set `QRUST_REPORT` to a path:

```bash
QRUST_REPORT=build-report.json cargo build
QRUST_REPORT="reports/{session}.json" cargo test --no-run
```

`{session}` is replaced with the run's id, so successive builds write separate files. A relative path is relative to the directory the command runs in. To do this for every build of a project, set it in `.qrust/config.toml`:

```toml
[measurement]
report = "target-reports/{session}.json"
```

The build is analysed right after it finishes, which adds a little time to the command. If the report cannot be written, QuickRust prints a `WARN` line and the build's result stays as it is; set `measurement.strict = true` if a missing report should fail the command.

## Export a report after the build

```bash
qrust report --format json -o build-report.json     # the latest run
qrust report --format html -o report.html           # self-contained page, no scripts
qrust report --run latest~2 --format json           # to stdout
qrust report                                        # every section as text
```

`-o` (or `--output`) writes the file and prints `OK   Wrote <path>`.

## Find the run you want

```bash
qrust runs
qrust runs --limit 5 --format json
```

```text
RUN                         STARTED                   FAMILY       STATE           WALL  OBSERVED  COMMAND
01M4SFR4D0T8A2T0XMX6XDVS4Y  2026-10-13T01:59:02.817Z  build        completed       57ms  rustc     cargo build
01M4SFR4AEWS8GV6Q36CH45KAQ  2026-10-13T01:59:02.735Z  build        failed          42ms  rustc     cargo build
```

Wherever a command takes `--run`, give `latest`, `latest~N` (N runs before the latest), or a run id or any unique prefix or suffix of one. Runs belong to the workspace you are in.

QuickRust keeps the last 200 runs of a workspace (`measurement.keep-runs`); export the ones you need to keep longer.

## Export one section

```bash
qrust analyze timing --format json
qrust analyze rebuilds --format json --filter reason=source
```

Each section of `qrust analyze` prints as JSON with `--format json`, narrowed by `--filter`, `--sort` and `--limit`.

## Keep paths out of shared reports

Exported reports replace the workspace root, your home directory, QuickRust's Cargo home and QuickRust's home with `$WORKSPACE`, `$HOME`, `$CARGO_HOME` and `$QRUST_HOME`. Environment values are kept only for a list of build-relevant variables, and credential-like ones show as `<redacted>`. To keep no environment values at all, set `security.persist-environment = "none"`.

## Related

- [Report schema](../reference/report-schema.md) for every field
- [How to run QuickRust in CI and gate on build time](set-up-ci.md)
- [Security and privacy](../explanation/security.md)
