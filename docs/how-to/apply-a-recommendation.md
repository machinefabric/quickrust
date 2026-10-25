# How to review and apply QuickRust's recommendations

This guide shows you how to see what QuickRust recommends for a build, preview the change each recommendation implies, and apply or undo it.

QuickRust never changes your repository on its own. It only writes a file when you apply one specific, previewed change by its id, and only changes to QuickRust's own configuration can be applied that way.

## See the recommendations

```bash
qrust analyze recommendations
qrust analyze recommendations --run latest~1
qrust analyze recommendations --format json
```

Each recommendation has an id, the evidence it rests on, its expected impact, risk and confidence, and how to verify it. When the evidence supports nothing, QuickRust says so:

```text
Recommendations
  None: the recorded evidence supports no recommendation.
```

## Preview the changes

```bash
qrust fix --dry-run                    # every recommendation of the latest run
qrust fix --dry-run <REC_ID>           # one recommendation
qrust fix --dry-run <REC_ID> --option keep-incremental   # one of its alternatives
qrust fix --dry-run --run latest~1
```

For each recommendation the preview shows its class and state and, where a file change goes with it, the remediation id, the file and a unified diff. Nothing is written. A recommendation without a file change gets guidance instead (for example, which `cargo::rerun-if-changed` lines a build script needs).

The class decides what you can do with it:

- `safe`: a change to QuickRust's own configuration (`.qrust/config.toml`). It can be applied with `qrust fix --apply`.
- `review_required`: a change to how the project is built (`Cargo.toml` profiles, dependencies, build scripts). QuickRust only shows it.

## Apply a safe change

```bash
qrust fix --apply rem-1a2b3c4d5e6f
```

```text
OK   Applied rem-1a2b3c4d5e6f to .qrust/config.toml; undo with: qrust fix --rollback rem-1a2b3c4d5e6f
```

QuickRust applies exactly what the preview showed. If the file changed since the preview, it refuses; run `qrust fix --dry-run` again. If the result is not a valid QuickRust configuration, it restores the file and says why.

## Apply a change that needs review

Print only the diffs, read them, and apply them yourself:

```bash
qrust fix --dry-run <REC_ID> --patch > change.patch
git apply change.patch
```

Then verify the change the way the recommendation's `verify:` line says, for example with an experiment (see [How to measure the effect of a build setting](measure-a-change.md)), before you commit it.

## Undo an applied change

```bash
qrust fix --rollback rem-1a2b3c4d5e6f
```

This restores the file as it was before (or removes it, if applying created it). If you edited the file after applying, QuickRust refuses to overwrite your edit; undo the change by hand.

## If you meant Cargo's fix

`qrust fix` is QuickRust's command. For Cargo's `cargo fix` (applying compiler suggestions), run `cargo fix`, or `qrust upstream fix` when `cargo` is not QuickRust.

## Related

- [Commands](../reference/commands.md) for every option of `qrust fix`
- [Security and privacy](../explanation/security.md) for what QuickRust may write and where
