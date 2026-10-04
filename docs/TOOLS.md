# Tool catalogue

The normative specification of the MCP tools. Tool names, argument names, modes,
annotations and error codes here are the contract; the schemas that the server
publishes are generated from the same definitions and checked against this document
in CI by `scripts/check-tools-docs.sh`, which compares this file against
`crates/tools/src/registry.rs` (tool set, mode, the four published annotations, and
each tool's `inputSchema` property names) and against `crates/core/src/error.rs` (the
error-code table), **in both directions** — a tool the catalogue has but this file
does not, and a tool this file has but the catalogue does not. Its own self-test,
`scripts/tests/check_tools_docs_spec.sh`, plants each kind of drift and requires a
non-zero exit, so the claim is checked rather than merely made.

## Conventions

**Descriptions are the contract, verbatim.** The `description` string an agent
receives from `tools/list` is the authoritative statement of what a tool does. Each
tool's section below quotes that string **exactly**, as a blockquote immediately after
its heading, under the label **Published description**. Nothing else in a section may
contradict it, and the quoted string may not be reworded for readability: the sentence
an agent reads is the sentence an agent gets. A change to any `description` in the
catalogue therefore requires the matching change here, and is checked by
`crates/tools/tests/tool_descriptions_spec.rs::ux1_10_every_published_description_is_quoted_verbatim`
in both directions — a quote that drifts from the catalogue and a catalogue entry with
no quote are both failures.

**Targeting by name.** Tools take symbol names (`Config::load`, `Foo.bar`) and file
paths — never line:column — because agents miscount columns and names survive edits.

**Paths** are relative to the workspace root (absolute paths are accepted only if
they resolve inside it). Every path passes through the boundary
([`SECURITY-MODEL.md`](SECURITY-MODEL.md)); outputs always show workspace-relative
paths.

**Extra read roots.** `--read-root DIR` is repeatable and adds a **read-only** root:
a file under one is readable, and is displayed as `@root1/…`, `@root2/…` in the order
the flags were given. Those roots can never be written (BND-19), `/`, a drive root,
the home directory itself and key/credential directories are refused as read roots
(CFG-07), and the roots come from the command line rather than the configuration file
— so a file inside a repository cannot widen what an agent may read. `doctor` prints
one line per root with its label.

**Path rules that hold on every platform**, not only on Windows — one rule, so one
set of tests, and so a shape that is only dangerous on one kernel is still refused
everywhere (BND-02, BND-11, BND-24):

| Shape | Examples | Why |
|---|---|---|
| Drive letter (outside) | `C:\Windows\…`, `D:\other\a.ts` | An absolute disk path that does **not** resolve under the workspace or a `--read-root` — refused after containment, same as Unix `/etc/passwd`. Absolute disk paths *inside* a root are accepted. |
| Drive-*relative* | `C:src`, `C:`, `C:.:\WiWi` | On Windows this is relative to *the current directory of drive C* — a directory the process picked, not the caller. As a root it is a location escape; as a tool argument it used to answer `io_error` where a missing path answers `outside_workspace`, which is a probe oracle. |
| UNC / network | `\\server\share`, `//server/share`, `\/` and `/\` mixtures | Canonicalising one makes Windows resolve a host and open an SMB session: a DNS lookup and an NTLM challenge sent to a host the caller chose. |
| Device paths | `\\?\C:\x`, `\\.\pipe\x` | The same reach, spelled as a device rather than a share. |
| Root-relative | `\Windows` | Anchored at the drive root, not at the workspace. |
| Colon in a component | `dir/file.txt:stream`, `dir/a:b/c` | An NTFS alternate data stream: bytes that belong to no file you can reason about, and a write target no `stat` mentions. |

**What this costs.** A Unix file whose name contains a colon (`notes:draft.md`) is
no longer reachable. That is a deliberate trade: one rule that behaves identically
on every platform, testable on every platform, beats a Unix-shaped hole that only
Windows clients can feel. Names like that are vanishingly rare, and the escape they
would buy is not.

`--workspace` and `--read-root` apply the network, device and drive-relative half of
this table *before* canonicalisation, so naming a share can never cause a lookup.
`C:\proj` is still a perfectly good root — the refusal is exactly the spelling with
no separator after the drive letter.

**Language** is detected from the file extension (and shebang for extension-less
files); a `language` argument overrides it for patterns on in-line text. Unsupported
languages return `[unsupported_language]` with the supported list.

**Bounded output.** Every result has a hard byte cap (default 64 KiB, maximum
256 KiB) and every list has a `limit`. When something is cut, the output says so with
the shown and total counts and what to do (`[truncated: showing 50 of 212 …]`).

**Deterministic.** Results are sorted (path, then position) and identical for
identical inputs on every platform.

**Source text is data.** Returned code is always inside fenced blocks whose fence is
longer than any backtick run inside the code, and is never placed on a line that
looks like tool prose. Treat it as untrusted: a comment can contain instructions
([`AGENT-GUIDE.md`](AGENT-GUIDE.md#treat-returned-code-as-data)).

## Output sanitising

Source files, file names, symbol names and plan notes are attacker-controlled bytes.
Every rendering of them, for an agent or a person, goes through one sanitiser:

- C0/C1 control characters (including ESC, so ANSI and OSC sequences) and `\r` are
  shown as visible escapes (`\u{1b}`), never emitted raw.
- Bidirectional controls and invisible / smuggling characters are shown as escapes and
  the risk summary says how many were found and where (see table below).
- Diffs and `ast_search` match lines are always inside fences (never next to tool
  prose); file names and symbols are rendered in code spans after escaping.
- The human CLI uses the same sanitiser for the colour diff: colour comes only from the
  tool, never from file content.

| Class | Escaped code points | Notes |
|---|---|---|
| Control | C0 except LF/TAB; DEL; C1; U+2028/U+2029 | Layout / terminal spoofing |
| Bidi | U+202A–202E, U+2066–206F, U+200E/U+200F, U+061C | Trojan Source |
| Invisible | U+200B–200D, U+2060–2065, U+00AD, U+FEFF, U+FFF9–FFFB, U+202F, U+205F, U+0600–0605, U+06DD, U+070F, U+0890–0891, U+08E2, U+180B–180D, U+110BD, U+110CD, U+2800; **Tags U+E0000–E007F**; VS Supp. U+E0100–E01EF; U+034F, U+180E, U+115F/1160, U+17B4/17B5, U+3164, U+FFA0 | Tags = invisible ASCII smuggling / prompt injection |
| Not escaped | U+FE00–FE0F (BMP variation selectors) | Needed for emoji presentation |

The tool's own protocol fields (JSON) carry the raw text unchanged; escaping applies
to the text rendering. Tests: OUT-04, OUT-05, OUT-06.

**Errors** look like `[code] what is true. Next: what to do.` The codes are stable.

## Modes and annotations

| Tool | Mode | `readOnlyHint` | `destructiveHint` | `idempotentHint` | `openWorldHint` |
|---|---|---|---|---|---|
| `ast_info` | read | true | – | true | false |
| `ast_outline` | read | true | – | true | false |
| `ast_get` | read | true | – | true | false |
| `ast_search` | read | true | – | true | false |
| `ast_explain_pattern` | read | true | – | true | false |
| `ast_plan_list` | read | true | – | true | false |
| `ast_plan_show` | read | true | – | true | false |
| `ast_edit_preview` | read | **false** (writes the plan store under `<workspace>/.opencrayast`; never the workspace's *files*) | false | true | false |
| `ast_edit_apply` | **write** | false | **true** | false | false |
| `ast_undo` | **write** | false | **true** | false | false |
| `ast_recover` | **write** | false | **true** | true | false |

In read-only mode the three write tools are not listed and calling one returns the
same *unknown tool* error as any other unknown name.

That rule governs the **catalogue boundary**. It is deliberate: a read-only server does
not advertise write tools, so a name the caller guessed is indistinguishable from a name
that does not exist. It is **not** the same layer as the `[write_disabled]` code in the
per-tool error lists below: a caller that already knows the tool exists — the CLI's
`edit apply`, or any in-process caller — reaches the handler directly, and there the
honest answer is `[write_disabled]` with the next step. Both are correct for their own
layer; neither replaces the other.

---

## `ast_info`

**Published description** (`tools/list`, quoted exactly — see [Conventions](#conventions)):

> What is running and what it will allow (mode, workspace, languages, limits).

What is running and what it will allow. Call it first when unsure what mode you are in.

**Arguments:** none.

**Output** (exactly five lines; byte sizes as `N MiB` / `N KiB` / `N B`; separator is
U+00B7 surrounded by spaces):

```
opencrayast 0.20261002.1 (mode: read-only)
workspace: . (id w-5c1e9a07...)
languages: rust (tier 1), typescript (tier 1), tsx (tier 1), javascript (tier 1), python (tier 1), go (tier 1)
limits: file 4 MiB · output 64 KiB · results 200 · plan 50 files / 500 edits
write: disabled (needs BOTH --allow-write AND policy.allow_write = true in the server config; together they expose ast_edit_apply, ast_undo, ast_recover)
```

`languages` lists only grammars built into this binary, in `Language::all()` order.
With write mode the last line is
`write: enabled (ast_edit_apply, ast_undo, ast_recover)`.

Write mode alone does not make a write tool *callable*: the handler needs a capability minted
from a configuration-issued permission, so the three tools appear in the write-mode listing and
answer `[write_disabled]` until the operator's own configuration enables writing.

**Write mode takes two things, not one.** The server must be started with `--allow-write` **and**
`policy.allow_write = true` must be set in its configuration file; the flag alone leaves the
server read-only. This line says both, because the alternative — telling a caller to pass a flag
it has already passed — is an instruction that cannot work. The `write: disabled` line above is
the authoritative statement of what *this* server can do, which is why the refusal from a
read-mode server points the caller back at `ast_info` rather than at a remedy it can apply itself.

## `ast_outline`

**Published description** (`tools/list`, quoted exactly — see [Conventions](#conventions)):

> The skeleton of a file or directory — symbols with kinds, line ranges and signatures. Lists names only, no source; to read a symbol's text use ast_get, by symbol name. `limit` is 1..=`limits.max_results` (`results` in ast_info's limits line; 200 by default).

The skeleton of a file or directory — the cheap alternative to reading it.

| Argument | Type | Default | Meaning |
|---|---|---|---|
| `path` | string (required) | – | A file or a directory (directories are walked, honouring ignore rules) |
| `depth` | integer 1–6 | 3 | Nesting depth of symbols to show |
| `kinds` | string[] | all | Filter, e.g. `["fn","struct"]` (names per language, see below) |
| `include_docs` | boolean | false | Add the first line of each doc comment |
| `limit` | integer 1..= `limits.max_results` | 200 | Maximum symbols in the output. The cap is the configured `max_results` — `ast_info`'s `limits` line prints the effective value, and it can be raised by configuration, so a request above the printed value is refused rather than silently clamped |

**Output format** (compact; no column padding; golden-tested):

```
<rel>  <language id>  <N> lines[ (<K> syntax errors)]
<indent><kind> <name> L<start>[-<end>]  <signature>
```

- `N` is the line count of the file text; ` (K syntax errors)` only when `K > 0`
  (`1 syntax error` for a single error).
- `indent` is two spaces per nesting depth (depth 1 = two spaces).
- `<name>` is the bare symbol name (not qualified). `L<start>` alone when
  `start == end`; otherwise `L<start>-<end>`.
- When `include_docs` is true and a doc exists, a following line
  `<indent>  /// <doc first line>` uses the symbol indent plus two spaces.
- Paths, names, signatures and docs go through the output sanitiser.

**Example:**

```
src/config.rs  rust  212 lines
  struct Config L12-30  pub struct Config
    fn load L32-58  pub fn load(path: &Path) -> Result<Config, Error>
    fn validate L60-91  fn validate(&self) -> Result<(), Error>
  enum Error L94-110  pub enum Error
src/main.rs  rust  48 lines
  fn main L5-47  fn main()
[truncated: showing 7 of 7 symbols in 2 files; narrow `path`, lower `depth` or filter `kinds`]
```

Footer lines, each only when applicable, in this order:

```
[truncated: showing <shown> of <total> symbols in <F> files; narrow `path`, lower `depth` or filter `kinds`]
[skipped: <a> ignored, <b> links, <c> special, <d> unsupported language, <e> too large, <f> not utf-8, <g> unreadable]
[walk truncated at <max_scan_files> files; narrow `path`]
[escaped: <c> control, <b> bidi, <i> invisible characters in names or paths]
```

The `[skipped: …]` line lists only nonzero parts. The **ignored** count includes
paths skipped by ignore rules, **built-in VCS directories** (`.git`, `.hg`, `.svn`,
`.bzr`), and entries below the **path depth ceiling** — not only `.gitignore`
matches. Links, special nodes, unsupported languages, oversize files, non-UTF-8
and unreadable entries are counted separately when nonzero.

A file that parses with errors is still outlined; the symbols found are real, and
the error count tells the caller to trust the result less. A single-file argument
that cannot be outlined is an error; inside a directory walk such files contribute
to `[skipped: …]`. A directory with no outlinable file returns footer lines only.

**Milestone note:** the portable kind `field` is defined (see below) but **not
emitted** by outline collectors in this milestone.

**Errors:** `[outside_workspace]`, `[not_found]`, `[unsupported_language]`,
`[file_too_large]`, `[not_utf8]`, `[budget_exceeded]`, `[timeout]`.

## `ast_get`

**Published description** (`tools/list`, quoted exactly — see [Conventions](#conventions)):

> One symbol's source, by symbol name (not by path), as fenced data. Exactly one name per call; for several, call it once per name, or list candidates with ast_outline first.

One symbol's source, by name.

| Argument | Type | Default | Meaning |
|---|---|---|---|
| `symbol` | string (required) | – | Name or qualified name (`Config::load`, `Config.load`); 1..=256 bytes, no control characters |
| `path` | string | whole workspace (`.`) | Restrict to a file or directory; strongly recommended |
| `context_lines` | integer 0–20 | 0 | Lines of surrounding code to include |
| `include_doc` | boolean | true | Include the leading doc comment |

**Output** (exactly one match):

````
src/config.rs:32-58  fn Config::load  (rust)
```rust
    /// Load the configuration from `path`.
    pub fn load(path: &Path) -> Result<Config, Error> {
        …
    }
```
````

Header form: `<rel>:<first>-<last>  <kind> <qualified>  (<language id>)`, then a
fenced block of lines `first..=last` (doc and context included when requested).
`first`/`last` are the lines of the returned text, not of the bare symbol alone.
The header is sanitised like outline text.

**Zero matches** → `[not_found]`:
`No symbol named `<symbol>` found in <N> files.` Next: `Check the name with
ast_outline.`

**More than one match** → `[ambiguous]`:
`<N> symbols match `<symbol>`:` followed by lines
`<i>. <rel>:L<start> <kind> <qualified>` (at most 20 listed, then
`... and <M> more`). Next: `Repeat the call with `path` set to one of these
files.`

Search walks `path` as `ast_outline` does and stops at `call_timeout_ms`
(`[timeout]`). Path-argument errors match `ast_outline`.

**Errors:** as `ast_outline`, plus `[ambiguous]`.

## `ast_search`

**Published description** (`tools/list`, quoted exactly — see [Conventions](#conventions)):

> Structural search: matches for a pattern in files under `paths`. `paths` defaults to the workspace root when omitted. `limit` is 1..=`limits.max_results` (`results` in ast_info's limits line; 200 by default). If the pattern is rejected as unparseable, use ast_explain_pattern, which reads no files.

Search by syntactic shape. The pattern language is specified in
[`PATTERNS.md`](PATTERNS.md); `ast_explain_pattern` shows how a pattern parses.

| Argument | Type | Default | Meaning |
|---|---|---|---|
| `pattern` | string (required) | – | Code in the target language with metavariables (`$X`, `$$$XS`) |
| `language` | string | from `paths` | Required if `paths` has mixed languages |
| `paths` | string[] | `["."]` | Files or directories |
| `context_lines` | integer 0–5 | 0 | Lines around each match |
| `limit` | integer 1..= `limits.max_results` | 100 | Maximum matches. Same operator-configurable cap as `ast_outline` (`results` in `ast_info`'s limits line; 200 by default) |

`paths` is optional in the schema and its documented default is `["."]`, so a bare
`{"pattern": "..."}` searches the workspace root. An **explicitly empty** `paths: []` is a
different thing and is refused (`paths has 0 entries`) — omitting the argument is the default,
passing an empty list is a request to search nothing.

**`rule` is not accepted over MCP.** The library handler takes a `rule`
(`crates/tools/src/search.rs`) and it is published for `ast_edit_preview`, but the
`tools/call` path **refuses** it with `invalid_args` rather than silently dropping
it — an unknown-shaped argument that is ignored is exactly the kind of quiet
misbehaviour this tool set avoids. Callers on MCP should omit it; the rule language
is reachable through the CLI.

**Output:**

```
Found 3 matches in 2 files for: console.log($$$ARGS)
src/a.ts:12:5-12:31   console.log("start", id)         $$$ARGS = "start", id
src/a.ts:48:9-48:25   console.log(err)                 $$$ARGS = err
src/b.ts:7:3-7:19     console.log(x)                   $$$ARGS = x
```

Zero matches is a normal, successful result that says what was searched (`0 matches
in 14 files (typescript)`), so the caller can tell "none" from "searched nothing".

**Errors:** `[invalid_pattern]` (with the position and a suggestion),
`[budget_exceeded]` (names which budget), plus the path and size errors above.

## `ast_explain_pattern`

**Published description** (`tools/list`, quoted exactly — see [Conventions](#conventions)):

> Explain a pattern: parse it, list captures, and say what would match. Reads no files — use it when ast_search rejects a pattern as unparseable, since both fail with the same invalid_pattern code.

Show how a pattern is understood — the debugging aid for `ast_search` and rewrites.

| Argument | Type | Meaning |
|---|---|---|
| `pattern` | string (required) | The pattern |
| `language` | string (required) | Its language |

**Output:** the parsed pattern as a tree of node kinds with metavariables marked,
and any warnings (for example "this pattern parses as an ERROR node; quote it as a
complete statement").

## `ast_edit_preview`

**Published description** (`tools/list`, quoted exactly — see [Conventions](#conventions)):

> Preview an edit as a plan: the diff, the plan id, and what applying it would change. THIS CALL PERSISTS the plan under `<workspace>/.opencrayast` — the workspace's files are never modified, but the returned plan_id only exists because a file was written, which is why readOnlyHint is false. kind=rewrite needs language, pattern, replacement and paths; kind=symbol needs path, symbol and operation, plus text for replace, replace_body, insert_before and insert_after (text must include the surrounding braces for replace_body, and is unused by delete).

Produce a **plan**. The plan is **persisted** under the workspace's state directory
`<workspace>/.opencrayast` — that path is inside the user's tree, so this call creates files
there even though it never modifies the workspace's own content. That is what `readOnlyHint:
false` records, and it is why the tool still appears in the read-only listing.

| Argument | Type | Meaning |
|---|---|---|
| `kind` | `"rewrite"` \| `"symbol"` (required) | See [`EDIT-MODEL.md`](EDIT-MODEL.md#edit-kinds) |
| `language`, `paths`, `pattern`, `replacement` | | For `rewrite` — all four are required. `rule` is **not** accepted on the `tools/call` path and is no longer published in the schema |
| `path`, `symbol`, `operation` | | For `symbol` — all three are required |
| `text` | string | For `symbol`, **conditionally**: required by `replace`, `replace_body`, `insert_before` and `insert_after`, and unused by `delete`. For `replace_body` it must include the surrounding braces. The JSON schema cannot express a conditional, so the tool's shipped description states it in prose |
| `note` | string (optional, ≤ `note_max_bytes` bytes, default 1024, hard max 4096) | A label stored with the plan for reviewers. The limit is `limits.note_max_bytes` and it is **enforced in bytes** at preview time. The JSON schema publishes **no** bound on `note`: the limit is operator-configurable, so any number written into the schema would drift from the enforcement. It is part of the hashed plan (changing it changes the plan id) and is shown sanitised and marked as written by the caller, not by the tool |

`summary` is **not** an argument: the plan carries one, hashed, so the tool derives it from the
request — `rewrite <pattern>` or `symbol <symbol>` — deterministically. Two different rewrites
therefore never differ only in their edits with an identical reviewer line.

**Output (example):**

```
plan p-7k2m9xq4ab  (expires 09:45 UTC)  — 2 files, 3 edits, +41 −39 bytes
  src/a.ts   2 edits   syntax errors 0 → 0
  src/b.ts   1 edit    syntax errors 0 → 0
skipped: 1 file too large (docs/generated.ts)

--- a/src/a.ts
+++ b/src/a.ts
@@ -12,1 +12,1 @@
-  console.log("start", id)
+  logger.debug("start", id)
…
Next: review the diff, then apply with ast_edit_apply plan_id=p-7k2m9xq4ab
      (write mode) or with `opencrayast edit apply p-7k2m9xq4ab` (CLI).
```

A request that matches nothing is **a normal, successful result**, not an error: the
call gets no plan id, `files` is empty and the message says so.

```
0 matches — nothing to change in src/a.ts
Next: widen the pattern, or preview a directory instead of one file.
```

The diff is bounded; `ast_plan_show` returns the rest.

A file whose line endings are **mixed** (anything that is not exactly LF or exactly CRLF,
including a file with a lone `\r`) keeps its own endings: replacement text takes the ending
in force at the match site, so a mixed file stays mixed. An edit that would flatten a mixed
file to one style is refused with `[gate_failed]`, naming the `encoding` gate and the file.

**Errors:** everything above, plus `[limit_exceeded]` (plan too large — narrow the
request) and `[ambiguous]` (symbol edits). A gate refusal names the gate and the file, and
for `encoding` it names which property changed — `encoding (trailing newline)`,
`encoding (line ending)` or `encoding (byte order mark)`.

## `ast_plan_show` / `ast_plan_list`

**Published description** (`tools/list`, quoted exactly, one per tool — see [Conventions](#conventions)):

> One plan's summary and diff, paged by hunk. This is the plan **as stored before it was applied**, not the current content of the files — to read current code use ast_get or ast_outline. A plan that has already been applied still renders its original diff. Omit `limit` for every hunk; `limit: 0` is refused.

> Stored edit plans for this workspace: id, state, expiry, files, edits.

| Tool | Arguments | Result |
|---|---|---|
| `ast_plan_show` | `plan_id` (required), `file` (optional), `offset`/`limit` for the diff | The summary and diff of a stored plan, paged. **`limit` is a hunk count: omit it to get every hunk, and note that `limit: 0` is refused** (`limit 0 would show nothing`) — the schema publishes `minimum: 1`, which is why omitting and passing zero are not the same thing |
| `ast_plan_list` | `limit` (default 20) | Stored plans for this workspace: id, note, files, edits, state, expiry |

`state` is the plan's **journal state**: `ready` when no journal exists (nothing was ever applied
to this plan), otherwise the journal's own state — `prepared`, `writing`, `applied`, `undoing`,
`rolled_back` or `undone`. It is never rounded up to `applied`: a `prepared` journal means
originals are saved and **no target has been touched**, and a `rolled_back` one means every target
is back to its original. An **expired** plan does not appear here at all — the plan store lists
only unexpired plans — and `ast_plan_show` answers `[plan_expired]` for one.

**`ast_plan_show` returns the plan as stored, before it was applied** — the diff of the change
that *was* proposed, rendered even after the plan has been applied and the files have moved on.
It is not a reader for current code. To see the current contents of a file, use `ast_get`
(a symbol by name) or `ast_outline` (names and line ranges). A caller that wanted current source
and reached for this would otherwise get the old version and have no way to tell.

**Errors:** `[plan_not_found]`, `[plan_expired]`, `[wrong_workspace]`.

## `ast_edit_apply` *(write mode)*

**Published description** (`tools/list`, quoted exactly — see [Conventions](#conventions)):

> Apply a stored plan to the workspace. Needs the full plan id; takes a lock and is not idempotent.

Apply a previewed plan. Takes only the plan id.

| Argument | Type | Meaning |
|---|---|---|
| `plan_id` | string (required) | The **full** id from `ast_edit_preview`. Abbreviations are accepted only by the read-only plan tools, never here (EDIT-MODEL E-15) |

**Output:**

```
Applied p-7k2m9xq4ab — 2 files changed.
  src/a.ts   syntax errors 0 → 0
  src/b.ts   syntax errors 0 → 0
Undo with ast_undo plan_id=p-7k2m9xq4ab (kept 7 days).
Next: verify the semantics — for example run the language server's diagnostics
      (lsp_diagnostics) on the changed files.
```

Those paths are the handoff list, and the `lsp_diagnostics` line is a real instruction, not
decoration: a plan that clears the syntax gate has not been verified. This tool **does not
call a language server** — there is no `lspd` dependency and no socket. For what the split
means and when to hand over, see
[AGENT-GUIDE.md §Working alongside a language server](AGENT-GUIDE.md#working-alongside-a-language-server)
and, for the offset conversion a front end will need,
[LSPD-INTEGRATION.md §Position conversion](LSPD-INTEGRATION.md#4-position-conversion).

**Errors:** `[write_disabled]`, `[plan_not_found]`, `[plan_expired]`,
`[plan_corrupt]`, `[wrong_workspace]`, `[already_applied]`, `[stale_plan]` (names the
changed files; "preview again"), `[gate_failed]` (names the gate and file),
`[busy]`, `[protected_path]`, `[outside_workspace]`, `[io_error]` (and what was
rolled back).

## `ast_undo` *(write mode)*

**Published description** (`tools/list`, quoted exactly — see [Conventions](#conventions)):

> Revert a plan that was applied, from its journal. Needs the full plan id; refuses when a file has changed since the apply.

Revert an applied plan, if and only if nothing touched those files since.

| Argument | Type | Meaning |
|---|---|---|
| `plan_id` | string (required) | The applied plan (full id) |

**Errors:** `[write_disabled]`, `[plan_not_found]`, `[diverged]` (lists the files
that changed after the apply; nothing is reverted), `[journal_missing]`
(retention expired), `[busy]`, `[io_error]`.

## `ast_recover` *(write mode)*

**Published description** (`tools/list`, quoted exactly — see [Conventions](#conventions)):

> Finish or roll back an interrupted apply. Takes no arguments and is idempotent: running it twice is the same as once.

Converge any half-applied plan after a crash. Normally unnecessary — every apply
does it first — but useful after a reported crash.

**Arguments:** none. **Output:** what was found and what was done, or "nothing to
recover". **Errors:** `[diverged]` when a person must decide.

---

## The human CLI's confirmation gate

`opencrayast edit apply <plan-id>` is the only way a workspace gets written through this
toolchain, and it does not apply a plan because it was asked to. It applies one because a
person agreed to it.

| Environment | `--yes`? | What happens |
|---|---|---|
| a terminal | no | prints the files that will change, asks, and applies only on `y`/`yes` |
| a terminal | yes | applies without asking — the consent was given in advance |
| **no terminal** (pipe, cron, CI) | yes | applies |
| **no terminal** | **no** | **refuses**: `[invalid_args]`, exit 1, nothing written |

The refusal is deliberate rather than a fallback. A non-interactive environment has nobody to
ask, so an unanswered question is not a slow question — it is a no. The message says what to do
next and names `--yes`, because a refusal that does not say what to do gets `--yes` added by
reflex.

Two properties are asserted rather than documented on trust, by
`crates/cli/tests/cli2_confirm_spec.rs`:

- **Nothing is written on either refusal.** The refusal is checked before any store is opened, so
  there is nothing to roll back — the file and the journal are asserted unchanged by reading them,
  not inferred from the exit code.
- **`--yes` is not a default.** Without it, a non-interactive apply does not happen.

`--yes` means a person has already read the plan. It is not a way to make a script stop asking.

## Symbol kinds in outlines

Kind names are normalised across languages so filters are portable:
`module`, `namespace`, `class`, `struct`, `enum`, `interface`, `trait`, `impl`,
`fn`, `method`, `const`, `static`, `type`, `field`, `variable`, `macro`. A language
that lacks a kind simply never produces it. **`field` is in the vocabulary but is
not emitted this milestone** (class/struct fields are skipped). The per-language
mapping is in [`LANGUAGES.md`](LANGUAGES.md#outline-queries).

## Error code reference

| Code | Meaning | Next step the message gives |
|---|---|---|
| `invalid_args` | Missing, mistyped or out-of-range argument | The valid form |
| `config_untrusted` | The user configuration file exists but cannot be trusted: owned by another user, executable, readable or writable by the group or others, or not a regular file. Distinct from `invalid_args` because the file is not wrong — the machine it sits on is — so it is the environment exit status, not the user's | `chmod 600` the file, or remove it; see [Configuration](CONFIGURATION.md) |
| `outside_workspace` | Path resolves outside the boundary (also used when a path cannot be shown to exist) | Use a path inside the workspace |
| `protected_path` | Target is a protected path | Choose another file; this cannot be overridden by arguments |
| `not_found` | No such file or symbol | Check the name with `ast_outline` |
| `ambiguous` | More than one symbol matches | Repeat with `path` |
| `unsupported_language` | No grammar for the file | Supported languages listed |
| `file_too_large` | Over the size limit | Narrow to a smaller file |
| `not_utf8` | File is not valid UTF-8 | Not supported |
| `invalid_pattern` | The pattern does not parse | See `ast_explain_pattern` |
| `budget_exceeded` | A parse or match budget ran out | Narrow `paths`/pattern |
| `timeout` | Wall-clock limit | Narrow the request |
| `limit_exceeded` | A plan or store limit | Narrow the request |
| `write_disabled` | A write tool was reached while writing is off (handler layer; the MCP catalogue answers with an unknown-tool error instead) | Use the CLI to apply, or enable write mode |
| `plan_not_found` / `plan_expired` / `plan_corrupt` / `wrong_workspace` | Plan problems | Preview again |
| `already_applied` | Plan already applied | `ast_undo`, or preview a new plan |
| `stale_plan` | A file changed since the preview | Preview again |
| `gate_failed` | A safety gate rejected the new content | Fix the request; the message names the gate |
| `diverged` | Undo/recovery found a file changed after the apply | A person must resolve (CLI) |
| `busy` | Another apply holds the lock | Retry shortly |
| `journal_missing` | Retention expired | Undo no longer possible |
| `unsupported_target` | The target cannot be replaced without losing a property: a symlink, a non-regular file, a hard-linked or read-only file, or extended attributes that cannot be copied. **Nothing was written** | Edit a plain, single-linked, writable file; copy the attributes yourself if the target has them |
| `replaced_not_durable` | **The file WAS replaced**, but the parent directory could not be synced, so the change may not survive a crash. Not a "nothing happened" error | Re-read the file to see whether the new contents are present, then decide whether to retry; do not assume the change was lost |
| `rollback_incomplete` | A rollback could not finish, so the workspace may be partly applied | Run `ast_recover`; if it cannot finish, restore from version control and report the plan id |
| `comment_loss` | The rewrite would drop comments and the request did not allow it | Pass the option that permits comment loss, or narrow the rewrite |
| `invalid_edit` | An edit set violates E-1: out of range, overlapping, or splitting a character | Narrow the edit to whole characters; submit one edit per region |
| `io_error` | The filesystem failed | The message says what was restored |
| `internal` | A defect; never expected | Report it with the plan id; includes no source text |
