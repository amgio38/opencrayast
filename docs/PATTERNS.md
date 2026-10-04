# Pattern and rule language

How `ast_search` and `rewrite` edits describe code by **shape**. The semantics
below are what the project guarantees; the implementation strategy (reuse a
matcher or write one) is decided by the spike in
[ADR-005](DECISIONS.md#adr-005-matching-engine) and must reproduce exactly these
semantics, verified by differential tests.

## Patterns are code

A pattern is a fragment of code in the **target language**, with *metavariables*
standing for unknown parts. It is parsed with the same grammar as the source, so it
means what the language means.

```
console.log($$$ARGS)          // any call to console.log with any arguments
if ($COND) { return $X }      // an if-statement whose body is exactly one return
fn $NAME($$$PARAMS) -> $RET   // a function with an explicit return type
```

### Metavariables

| Form | Matches | Captures |
|---|---|---|
| `$NAME` | Exactly one **named** syntax node | The node |
| `$$$NAME` | Zero or more nodes in a list position (arguments, parameters, statements…), lazily | The list |
| `$_` | Exactly one named node, not captured | – |
| `$$$` | Zero or more nodes, not captured | – |
| `$$` | A literal `$` in the pattern | – |

`NAME` is upper-case letters, digits and `_`, starting with a letter. Using the same
name twice means the two captured nodes must be **token-equal**: the same sequence of
leaf tokens, ignoring whitespace, comments and the kind of the enclosing node
(`$A == $A` matches `x == x` and `(a + b) == (a+b)`, not `x == y`; a repeated `$T` matches
the `T` of `<T>` and the `T` of `: T` even though the grammar names those nodes
differently). Text inside string, regex and template leaves is compared exactly.

### What "matches" means

- Matching is **structural**: node kinds and their ordered children are compared.
  Whitespace and comments between nodes are ignored. Text inside string and comment
  *leaves* is compared exactly.
- A pattern matches a node when its root matches that node. Searches visit every
  node in document order; **nested matches are all reported** (a match inside a
  match is still a match) unless a rule excludes it.
- A pattern must parse to **one root node**. If it parses as several statements or
  as an error, the tool returns `[invalid_pattern]` and says how to fix it (wrap it
  in a block, or give a complete statement). `ast_explain_pattern` shows the parse.
- Syntax errors in the *source* do not abort a search: matches are found in the
  parts that parsed; the file is reported with its error count.

## Rules (constraints)

A `rule` object narrows a pattern. All keys are optional and combine with *and*.

| Key | Value | Meaning |
|---|---|---|
| `kind` | string | The matched node's kind must equal this (language grammar names) |
| `inside` | rule or pattern | The match must be inside a node matching this |
| `has` | rule or pattern | The match must contain a node matching this |
| `not` | rule or pattern | The match must **not** satisfy this |
| `all` / `any` | list of rules | Conjunction / disjunction |
| `where` | `{ "$NAME": { "regex": "…", "kind": "…" } }` | Constraints on captured metavariables |

```json
{ "pattern": "console.log($$$ARGS)",
  "rule": { "not": { "inside": { "kind": "function_declaration",
                                  "where": { "$NAME": { "regex": "^test_" } } } } } }
```

- `regex` uses a **linear-time regular-expression engine** (no backreferences, no
  look-around), with a maximum pattern length. It cannot backtrack catastrophically.
- Relational rules (`inside`, `has`) look only within the current file.
- `kind` names come from the grammar; `ast_explain_pattern` and
  [`LANGUAGES.md`](LANGUAGES.md) list them.

## Rewrite templates

A `rewrite` edit supplies a `replacement` template. In the template:

- `$NAME` is replaced by the **source text of the captured node**; `$$$NAME` by the
  text of the captured list, separated as in the original.
- `$$` is a literal `$`.
- A metavariable used in the template but not bound by the pattern is
  `[invalid_pattern]` at preview (never silently empty).
- The expanded text is inserted at the match's byte range. Multi-line replacements are
  **re-indented** to the indentation of the line where the match starts and use the
  file's line-ending style. Re-indentation never touches the inside of a string,
  template literal, docstring or comment leaf (their bytes are copied as they are).
- Captured text is copied verbatim and is not re-parsed, **but substituting it can
  still change meaning**: `$X * 2` with `$X = a + b` would become `a + b * 2`. So a
  captured *expression* node is wrapped in the language's grouping parentheses when
  the template places it next to an operator of tighter binding, and the preview
  says when it did. Statement and list captures are never wrapped.
- When a match is replaced as a whole, comments that sat *between* captured parts and
  are not themselves captured would disappear. Preview lists every dropped comment in
  the risk summary, and refuses the file (`[comment_loss]`) unless the request sets
  `allow_comment_loss`.
- The *result* is re-parsed by the syntax gate. The gate compares syntax-error
  **locations**, not only counts: errors in regions the edit did not touch must be the
  same errors, so an edit that fixes one error and introduces another is refused.

### Overlap

If two matches overlap (one inside the other), the **outermost** is rewritten and
the inner one is dropped, and the preview says so. Disjoint matches are all rewritten.
The edit set is sorted by position and validated before a plan exists.

## Budgets

A search or rewrite runs under hard budgets that return `[budget_exceeded]` instead
of hanging:

| Budget | Default |
|---|---|
| Nodes visited per file | 2,000,000 |
| Match attempts (pattern steps) per file | 5,000,000 |
| Total wall-clock time per call | 10 s |
| Files scanned per call | 5,000 |
| Matches returned | 200 (hard max 1,000) |

The error names which budget ran out and what to narrow.

## Determinism

For the same bytes and the same request the result — matches, order, captures and
the generated edits — is identical across runs and operating systems. Order is by
path, then start offset, then end offset (outermost first for equal starts). This is
what makes plan ids reproducible (the plan hashes nothing time- or build-dependent) and diffs reviewable.

## Language notes

A pattern is only as expressive as the grammar: some constructs have no single-node
form in some grammars. When a construct cannot be expressed as a pattern, the
language page in [`LANGUAGES.md`](LANGUAGES.md#pattern-notes) documents the
workaround (usually a `kind` plus `has`).

## Examples

Find every `unwrap()` call outside tests (Rust):

```json
{ "pattern": "$X.unwrap()",
  "rule": { "not": { "inside": { "kind": "attribute_item", "has": { "pattern": "test" } } } } }
```

Replace `var` declarations of simple identifiers (JavaScript):

```json
{ "kind": "rewrite", "language": "javascript",
  "pattern": "var $NAME = $VALUE;", "replacement": "let $NAME = $VALUE;" }
```

Swap argument order of a two-argument call (Python):

```json
{ "kind": "rewrite", "language": "python",
  "pattern": "connect($HOST, $PORT)", "replacement": "connect($PORT, $HOST)" }
```
