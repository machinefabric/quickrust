# Semantic facts schema, `qrust-facts/1`

With [`export.facts`](configuration.md#keys) on, each crate of the workspace is compiled with QuickRust's compiler driver. The driver writes what rustc decided about the crate to a file beside the crate's metadata:

```text
<target dir>/<profile>/deps/lib<crate name>-<hash>.qrust-facts.json
```

This is the crate's `.rmeta` with the extension `.qrust-facts.json`. Cargo's `compiler-artifact` message names the `.rmeta` (`filenames`), including for units that are fresh or restored from the artifact cache. The facts are restored together with the rest of the unit's outputs.

Facts are written only for Cargo's primary packages (the workspace's members), only after analysis succeeded, and only where the compilation writes metadata. A unit that fails to compile has no facts. The run report's `facts` section says whether facts were written in the run ([Run report schema](report-schema.md)).

## Rules

- `schema` is always `"qrust-facts/1"`. Fields may be added within version 1; consumers ignore fields they do not know.
- Definitions are identified by `def`: rustc's `DefPathHash`, 32 hexadecimal digits. It is stable across compilations of the same source; indices are never used.
- A position is `file` (an index into `files`), `lo` and `hi`. The offsets are bytes from the start of the file as rustc read it. `expanded: true` marks something that comes from a macro expansion; its position is then the outermost macro call in the source as written.
- `files` names files as rustc names them locally. Paths of the workspace's files are relative to the directory Cargo ran rustc in, as Cargo passed them.
- Paths (`path`) are printed in full, as rustc prints them without trimming. The crate's own items have no crate prefix (`twice`, `<Sq as Shape>::area`).

## Document

| Field | Content |
|---|---|
| `schema` | `"qrust-facts/1"` |
| `crate` | `name`, and `stable_crate_id` (16 hexadecimal digits) |
| `definitions[]` | every item, associated item, field, variant, impl and macro of the crate |
| `impls[]` | every impl block |
| `calls[]` | every call in the crate's bodies whose callee is a function or method |
| `bodies[]` | with [`export.borrow-facts`](configuration.md#keys): what the borrow checker decided about each body it accepted; absent otherwise |
| `files[]` | file names that positions index |

### `definitions[]`

| Field | Content |
|---|---|
| `def` | its `DefPathHash` |
| `path` | its path |
| `kind` | one of the kinds below |
| `public` | whether its visibility is `pub` |
| `file`, `lo`, `hi`, `expanded` | the position of its name, or of the whole definition when it has no name (impls) |

The kinds are:

- `module`, `struct`, `union`, `enum`, `variant`, `field`;
- `trait`, `trait_alias`, `type_alias`, `associated_type`;
- `function`, `method`, `constant`, `associated_constant`, `static`;
- `macro`, `impl`.

### `impls[]`

| Field | Content |
|---|---|
| `impl` | the impl's `DefPathHash` (its position is in `definitions`) |
| `trait` | the trait it implements (a reference, below), or `null` for an inherent impl |
| `self_type` | the type it implements for, printed by rustc |

### `calls[]`

| Field | Content |
|---|---|
| `caller` | the `DefPathHash` of the body the call is in: a function, a closure, or a constant's initializer |
| `file`, `lo`, `hi`, `expanded` | the position of the name called: the method's name, or the last segment of the path |
| `callee` | the function typeck resolved the call to (a reference). For a trait method this is the trait's method. |
| `dispatch` | how the call reaches its target: one of the values below |
| `target` | with `dispatch` `trait`, the implementation selected (a reference) |

`dispatch` is one of:

- `static`: a function or inherent method, which is the target;
- `trait`: a trait method whose implementation the types select, given in `target`;
- `dynamic`: a trait method called through a trait object;
- `generic`: a trait method whose implementation depends on the caller's type parameters;
- `builtin`: a trait method the compiler provides, such as a closure call or `Clone` for a function pointer.

Calls through a function pointer or a closure value, and constructor calls (`Some(x)`, tuple structs), are not listed.

A reference to a definition in any crate is an object with these fields:

- `def`: its `DefPathHash`;
- `crate`: its crate's name;
- `path`: its path.

### `bodies[]`

A body is a function, closure or constant initializer that rustc's borrow checker accepted. Its facts are computed again, on a copy of the body, through the borrow checker's consumer interface (`rustc_borrowck::consumers`) after rustc's own check decided.

Only places in the program's own variables are listed: compiler temporaries, the borrows rustc makes for matches and index expressions, and closure captures are left out.

| Field | Content |
|---|---|
| `owner` | the `DefPathHash` of the body's definition. A closure's is its own; its position is inside its function. |
| `borrows[]` | every borrow of a user variable's place |
| `moves[]` | every move out of a user variable's place |

A place is written as in source: `x`, `x.field`, `x.0`, `*x`, or `x[..]` for an element of `x`.

#### `borrows[]`

| Field | Content |
|---|---|
| `kind` | `shared` (`&`) or `mut` (`&mut`). Borrows rustc makes itself count too, such as the receiver of `v.len()`. |
| `place` | what is borrowed |
| `file`, `lo`, `hi`, `expanded` | the position of the borrowing expression |
| `dead_at[]` | positions (`file`, `lo`, `hi`, `expanded`) where the borrow is no longer live: the first points, on each path, at which it is out of scope |

#### `moves[]`

| Field | Content |
|---|---|
| `place` | what is moved out of |
| `file`, `lo`, `hi`, `expanded` | the position of the moving expression: a call's argument, or the expression read |
