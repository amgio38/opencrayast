# Languages

Language support is a *tiered promise*, not a checkbox. A tier says exactly what
has been verified, so users and agents know how far to trust a result.

## Tiers

| Tier | Promise | Required to reach it |
|---|---|---|
| **1** | Outline, get, search and edits are supported on Linux, macOS and Windows. The Windows write path is temp+rename (no directory fsync; see [`ARCHITECTURE.md`](ARCHITECTURE.md) Platform notes) | Outline query + golden tests; pattern tests; edit gates validated on the language (syntax-error counting, indentation and line-ending preservation); pathological-input tests; fuzz target on the parse wrapper; documented pattern notes |
| **2** | Outline, get, search. **Edits are experimental** and refused unless the operator enables them for the language — and refused outright on any platform where the write primitive is unported | Outline query + golden tests; pattern smoke tests; parse budgets |
| **3** | Parse only: syntax-error counts and `ast_explain_pattern`; no outline guarantees | A pinned, licence-checked grammar |

A language is never promoted by assertion: promotion is a change that adds the
tests, updates this table and the matrix in [`TESTING.md`](TESTING.md), and passes
review.

## Initial targets

Language ids in tool output (`ast_info`, outlines) are the six values below.
`typescript` and `tsx` share the `tree-sitter-typescript` crate (two grammars,
one Cargo feature `lang-typescript`).

| Language id | Extensions | Grammar crate (pinned) | Target tier for 1.0 |
|---|---|---|---|
| `rust` | `.rs` | `tree-sitter-rust` | 1 |
| `typescript` | `.ts`, `.mts`, `.cts` (incl. `.d.ts`) | `tree-sitter-typescript` | 1 |
| `tsx` | `.tsx` | `tree-sitter-typescript` (TSX grammar) | 1 |
| `javascript` | `.js`, `.jsx`, `.mjs`, `.cjs` | `tree-sitter-javascript` | 1 |
| `python` | `.py`, `.pyi` | `tree-sitter-python` | 1 |
| `go` | `.go` | `tree-sitter-go` | 1 |

Candidates for Tier 2 after 1.0: Java, C, C++, C#, PHP, Ruby, Kotlin, Swift, Bash.
Data and markup formats (JSON, TOML, YAML, HTML, CSS, Markdown) are Tier 3 until a
concrete use justifies more.

## What a language contributes

1. **A grammar crate** behind a Cargo feature (`lang-rust`, …), so builds can be
   trimmed. The feature list is part of the release notes.
2. **An outline collector** at `crates/query/src/outline/<lang>.rs`, written from an
   actual AST dump of the pinned grammar: which nodes are symbols, what their kind
   and name are, how nesting / owner chains are expressed, how signatures and doc
   blocks are extracted, and the rule that a name may only come from a clean
   identifier (never from `ERROR`/`MISSING` recovery). Optional `.scm` query files
   may help dump or explore a grammar; they are not the normative outline form.
   Golden fixtures plus seeded random broken-input tests are required with the
   collector.3. **Symbol identity rules:** how a qualified name is built (`Config::load` in
   Rust, `Config.load` in TypeScript/Python/Go) and how overloads/impl blocks are
   disambiguated.
4. **Pattern notes:** constructs that need `kind` + `has` instead of a bare pattern.
5. **Tests** matching the tier.

## Outline queries

Kind mapping from grammar nodes to the portable kinds in
[`TOOLS.md`](TOOLS.md#symbol-kinds-in-outlines). The Python / Go / ECMA columns
match the M2 collectors (node names from grammar dumps). The Rust column is the
mapping `crates/query/src/outline/rust.rs` implements, dispatched at
`outline/mod.rs` for `Language::Rust` — the collector has landed. Empty names from
`ERROR`/`MISSING` recovery are never emitted.

| Portable kind | Rust | TypeScript / TSX / JavaScript | Python | Go |
|---|---|---|---|---|
| `fn` | `function_item` | `function_declaration`; `function_signature` (ambient); const/arrow with function init | `function_definition` (module) | `function_declaration` |
| `method` | `function_item` in `impl`/`trait` | `method_definition`; `method_signature` in interfaces | `function_definition` in class | `method_declaration`; `method_elem` in `interface_type` |
| `struct` | `struct_item` | – | – | `type_spec` whose type is `struct_type` |
| `class` | – | `class_declaration`, `abstract_class_declaration` | `class_definition` | – |
| `enum` | `enum_item` | `enum_declaration` | – | – |
| `trait` | `trait_item` | – | – | – |
| `interface` | – | `interface_declaration` | – | `type_spec` whose type is `interface_type` |
| `impl` | `impl_item` | – | – | – |
| `const` | `const_item` | `lexical_declaration` with `const` (non-function init) | `NAME = …` when `NAME` is all-caps style | `const_spec` |
| `static` | `static_item` | – | – | – |
| `variable` | – | – | other simple `NAME = …` (incl. `f = lambda …`) | `var_spec` |
| `type` | `type_item` | `type_alias_declaration` | – | `type_alias`; other `type_spec` |
| `module` | `mod_item` | `module` | – | – |
| `namespace` | – | `internal_module` | – | – |

Also outlined when present: `export_statement` (unwraps `declaration`, including
`export declare` / `ambient_declaration`), `decorated_definition` (Python, field
`definition`). **Not outlined this milestone:** class fields
(`public_field_definition` and similar), Go struct fields.

Qualified names use `.` for TypeScript/TSX/JavaScript/Python/Go (`Config.load`)
and `::` for Rust (`Config::load`).

## Pattern notes

Filled in per language as it reaches Tier 1. Each page records the grammar's
quirks that affect patterns (for example, in some grammars a statement pattern needs
a trailing `;`, or a type pattern must be wrapped in a declaration).

## Syntax errors and parse behaviour

tree-sitter produces a tree even for invalid code, marking `ERROR` and `MISSING`
nodes. The tool counts them and reports the count with every file it reads. The
**syntax gate** for edits compares counts: an edit may leave a broken file as broken
as it was, or fix it; it may not make it worse.

M2-confirmed behaviour of the parse wrapper and grammars in use:

- **tree-sitter 0.27 has no `set_timeout`.** Wall-clock cancel uses
  `ParseOptions::progress_callback` only; when the budget expires the parse returns
  `[timeout]`.
- **`root.end_byte() == source.len()`** holds for clean parses of all six language
  ids (byte offsets, not character counts).
- **BOM and CRLF:** a leading UTF-8 BOM and CRLF line endings parse with
  `error_count == 0` for all six; the BOM remains part of the source length.
- **TSX empty JSX attribute expressions:** `<div className={}>` is tolerated by the
  TSX grammar (no syntax error). Callers must not assume every odd JSX form raises
  `error_count`.
- **Go ERROR recovery:** a broken `function_declaration` does absorb later sibling
  symbols into a corrupted `parameter_list` or into the function body, so a naive walk of
  `source_file`'s direct children would omit symbols that still appear in the source text
  after the break. The collector recovers them, so the outline does **not** lose them: a
  `const`/`var`/`type` recovered from a broken body is outlined normally, while a `func`
  recovered from the parameter list has no recoverable receiver or signature and is
  qualified by its bare name with `/*?*/` in the signature. Symbols before the break are
  unaffected; empty names are still refused; a healthy function's own locals are never
  promoted to top-level symbols.
- **Python `lambda` assignments:** `f = lambda …` is outlined as `variable` or
  `const` (by name style), never as `fn`. Only `function_definition` nodes become
  `fn`/`method`.

## Adding or updating a grammar

Checklist (also in `CONTRIBUTING.md`):

- Licence is MIT, Apache-2.0 or compatible with the allow-list in `deny.toml`;
  recorded in [`THIRD-PARTY-LICENSES.md`](../THIRD-PARTY-LICENSES.md) with its
  upstream project, the upstream commit it was published from, and what its build
  script does. `scripts/check-grammar-provenance.sh` holds that table against
  `Cargo.lock` and refuses a grammar that is unpinned, so a new grammar cannot be
  added without provenance.
- The crate and version are pinned; the update review looks at the grammar's diff and
  at whether its generated C code changed in ways that affect the parse worker.
- Golden, pathological-input and (for Tier 1) fuzz coverage exist and pass on all
  three operating systems.
- The binary-size and parse-time impact is recorded in the pull request.
