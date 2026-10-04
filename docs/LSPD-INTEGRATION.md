# LSPD integration specification

How a Language Server Protocol front end integrates this engine, written down so the
integration is built from a specification rather than from guesswork. The public
companion product this document is written against is
[opencraylsp](https://github.com/amgio38/opencraylsp) (a **different repository**).
Below, "LSPD" means that class of front end — not a dependency of this tree.

This repository is the **specification provider**. It supplies documents and a mapping; it
does not gain an LSP client dependency, and nothing in layers L0-L4 changes for this
document. Where LSPD needs a capability the engine does not have, this document says
**no counterpart** and, where it matters, records the request in
[Requests against the engine](#7-requests-against-the-engine). Nothing here promises a
capability this repository has not implemented.

Upstream, normative for everything below:

| Document | What it fixes |
|---|---|
| [`ARCHITECTURE.md`](ARCHITECTURE.md) | the engine/shell split, the layers, the executors |
| [`TOOLS.md`](TOOLS.md) | the tool surface: names, arguments, output shapes, error codes |
| [`EDIT-MODEL.md`](EDIT-MODEL.md) | preview, the gates, plans, apply, undo, recovery |
| [`PATTERNS.md`](PATTERNS.md) | the pattern language `ast_search` takes |
| [`LANGUAGES.md`](LANGUAGES.md) | language ids and tiers |
| [`AGENT-GUIDE.md`](AGENT-GUIDE.md#working-alongside-a-language-server) | the agent-facing view of the same split: who finds candidates, who checks semantics, when to hand over |

The whole tool surface is eleven tools: `ast_info`, `ast_outline`, `ast_get`, `ast_search`,
`ast_explain_pattern`, `ast_edit_preview`, `ast_plan_show`, `ast_plan_list`,
`ast_edit_apply`, `ast_undo`, `ast_recover` (the last three are write mode). Every one of
them appears in the [capability mapping](#3-capability-mapping).

## 1. Startup, handshake, lifecycle

### 1.1 Who starts whom

LSPD starts this engine; the engine never starts LSPD.

| Step | Owner | What happens |
|---|---|---|
| 1 | LSPD | Spawn the engine as a child process and speak **MCP over stdio** to `opencrayast-mcp`, or embed the engine in-process through the same tool functions. |
| 2 | LSPD | Call `ast_info` first, exactly as [`TOOLS.md`](TOOLS.md#ast_info) tells every caller to. It answers with the version, the resolved workspace, the languages built into that binary, the limits in force, and whether write mode is on. |
| 3 | LSPD | Refuse to serve if write-mode tools are needed and `ast_info` reports `write: disabled`. The engine does not have a runtime toggle: `--allow-write` plus a user-level config file decide it at startup ([`ARCHITECTURE.md`](ARCHITECTURE.md#configuration-surface)). |
| 4 | LSPD | Answer the LSP `initialize` request from what `ast_info` returned: the languages the binary really has, and the capabilities in [§3](#3-capability-mapping). |

Step 2 is not optional and not a health check that can be skipped "because the engine just
started": the language list is **per binary**, and a client that advertises a language the
binary cannot parse will get `[unsupported_language]` per request instead of at handshake.

### 1.2 Lifecycle

| LSP method | Engine side |
|---|---|
| `initialize` | `ast_info`, once. Nothing else may be called before it. |
| `initialized` | no counterpart: nothing to push. |
| `shutdown` | no counterpart: the engine holds no per-session state, so there is nothing to tear down. |
| `exit` | LSPD terminates the child process. |

The engine is **stateless between calls**. It has no document store, no open-file table and
no caches that a client is expected to invalidate ([`ARCHITECTURE.md`](ARCHITECTURE.md#the-engineshell-split):
results are plain data). Every call re-reads the file from the workspace through the
boundary. That single fact drives most of the mapping below, and the dirty-buffer problem
in [§3.3](#33-what-didopendidchange-have-no-counterpart-for-and-why).

### 1.3 Degradation paths

| Failure | Engine behaviour | What LSPD must do |
|---|---|---|
| Child process dies mid-session | nothing; the process is gone | Re-spawn and call `ast_info` again. **All plans stored by the previous process are still on disk** (they live under the state directory), but a plan belongs to a workspace id, and a different workspace root is a different id, so a re-spawn against the same root keeps the plans usable. |
| Language server mode but the engine binary lacks a grammar | `[unsupported_language]`, with the supported ids in the message | Advertise the language as unsupported in `initialize`; do not advertise it per-file later. |
| Request exceeds a budget | `[budget_exceeded]` or `[timeout]`, naming the budget | Retry is pointless for the same request; narrow it, as the message says. |
| Write mode not enabled | `[write_disabled]` | Surface it as a read-only session. Do not retry. |
| A gate refuses new content | `[gate_failed]`, naming the gate and the file | Surface it. This is the same refusal `apply` would give, so previewing early is the point. |

There is no "partial mode" inside the engine: either a tool answers or it returns a
`ToolError`. The one executor choice ([`ARCHITECTURE.md`](ARCHITECTURE.md#the-engineshell-split))
— in-process or a stripped child worker — produces identical results, so LSPD does not need
to care which one is in use.

## 2. What LSPD must not assume

- **No incremental document state.** The engine reads the workspace file on every call.
- **No line:column input.** Tools take symbol names and paths; `TOOLS.md` says why ("agents
  miscount columns"). A position-based LSP request has to be turned into a name-based call.
- **No formatting, no code actions, no completions.** Not refusals at runtime: absent from
  the tool surface, so they are not advertised.
- **No semantic rename.** [`EDIT-MODEL.md`](EDIT-MODEL.md#what-is-deliberately-not-an-edit-kind)
  rules it out of 1.0 explicitly, because it needs a language server.
- **No diagnostics with ranges.** The engine reports a syntax-error **count**
  ([`EDIT-MODEL.md`](EDIT-MODEL.md#gates)); see [§5](#5-diagnostics-and-the-syntax-gate).

## 3. Capability mapping

Status vocabulary, used by every row:

| Status | Meaning |
|---|---|
| `exact` | one engine call answers the request, same shape. |
| `composition` | two or more engine calls in a fixed order answer it; the composition is specified here. |
| `partial` | an engine call covers part of the request; the missing part is named. |
| `none` | **no counterpart**. No engine call answers it. The row says why. |

`dir` is `c->s` (client to server) or `s->c` (server to client).

### 3.1 Method-by-method

| LSP method | dir | Engine side | Status | Note |
|---|---|---|---|---|
| `initialize` | c->s | `ast_info` | composition | Call `ast_info` once; answer from its five lines. See [§1.1](#11-who-starts-whom). |
| `initialized` | c->s | - | none | The engine has nothing to push, so there is nothing to initialise. |
| `shutdown` | c->s | - | none | No per-session state to release ([§1.2](#12-lifecycle)). |
| `exit` | c->s | - | none | LSPD kills the child process. |
| `$/cancelRequest` | c->s | - | none | No cancellable request state; a running call runs to its own budget and answers `[timeout]` if it exceeds it. |
| `$/setTrace` | c->s | - | none | No tracing protocol in this build. |
| `window/workDoneProgress/cancel` | c->s | - | none | The engine reports no progress; a long call is bounded by its budget instead. |
| `window/logMessage` | s->c | - | none | The engine returns no log stream. Errors come back as `ToolError` on the call itself. |
| `window/showMessage` | s->c | - | none | Same: a refusal is an error code plus a message, not a notification. |
| `window/showMessageRequest` | s->c | - | none | Nothing to ask the user about. |
| `window/showDocument` | s->c | - | none | No URIs are produced by the engine; paths in tool output are workspace-relative display paths. |
| `textDocument/didOpen` | c->s | - | none | No document store; see [§3.3](#33-what-didopendidchange-have-no-counterpart-for-and-why). |
| `textDocument/didChange` | c->s | - | none | Same, and the dirty-buffer consequence is worse here. |
| `textDocument/didClose` | c->s | - | none | Nothing was held open. |
| `textDocument/didSave` | c->s | - | none | The engine reads the saved file itself on the next call; there is no cache to invalidate. |
| `textDocument/willSave` | c->s | - | none | The engine has no formatter to run ([§2](#2-what-lspd-must-not-assume)). |
| `textDocument/willSaveWaitUntil` | c->s | - | none | Same: no edits are produced for a buffer. |
| `textDocument/hover` | c->s | `ast_outline`, `ast_get` | composition | Resolve the position to a symbol name, then ask for its text with its doc comment. Recipe in [§3.4](#34-position-to-name-the-one-composition-every-request-needs). |
| `textDocument/definition` | c->s | `ast_outline`, `ast_get` | composition | Same recipe; the result is a range, not a target URI, because the name was unique already. |
| `textDocument/declaration` | c->s | `ast_outline`, `ast_get` | composition | Same recipe. "Declaration" and "definition" are the same call here; the engine distinguishes traits from impls in the outline, not declaration from definition. |
| `textDocument/typeDefinition` | c->s | - | none | No type inference in this build; the outline reports kinds and signatures, not types. |
| `textDocument/implementation` | c->s | - | none | No trait/interface resolution; `ast_outline` lists what a file defines, not what implements what. |
| `textDocument/references` | c->s | `ast_search` | partial | `ast_search` finds syntactic matches of a pattern (`$NAME` for a bare identifier). It is **not** a reference index: it has no cross-file semantics, no binding resolution, and it matches text that is not the symbol. |
| `textDocument/documentHighlight` | c->s | `ast_search` | partial | Same limitation, scoped to one file by passing `paths: [file]`. Highlights are occurrences, not reads/writes. |
| `textDocument/documentSymbol` | c->s | `ast_outline` | exact | The outline *is* a document symbol list: nesting, kinds, names, line ranges. |
| `textDocument/documentLink` | c->s | - | none | The engine reports no URLs, imports or module paths as data. |
| `textDocument/foldingRange` | c->s | - | none | Symbol ranges exist, but fold regions are a different thing (import blocks, comments, multi-line literals) and are not in the tool surface. |
| `textDocument/selectionRange` | c->s | - | none | Needs per-node ranges from a selection walk; only symbol extents are exposed. |
| `textDocument/linkedEditingRange` | c->s | - | none | Same. |
| `textDocument/moniker` | c->s | - | none | No VCS information in the engine. |
| `textDocument/inlayHint` | c->s | - | none | No type or parameter hints are computed. |
| `textDocument/inlineValue` | c->s | - | none | Same. |
| `textDocument/codeLens` | c->s | - | none | Nothing is computed lazily; the engine computes no counts, references or test status. |
| `textDocument/codeAction` | c->s | - | none | No quick fixes and no refactorings are proposed; `EDIT-MODEL` rules out anything that runs a tool. |
| `textDocument/completion` | c->s | - | none | No completion engine. |
| `textDocument/signatureHelp` | c->s | - | none | No call-signature extraction; the outline has signatures of definitions, not of call sites. |
| `textDocument/formatting` | c->s | - | none | Deliberately absent: formatting runs a tool, which is not an edit kind. |
| `textDocument/rangeFormatting` | c->s | - | none | Same. |
| `textDocument/onTypeFormatting` | c->s | - | none | Same. |
| `textDocument/prepareRename` | c->s | `ast_outline` | partial | The outline can say whether a position is inside exactly one named symbol. It cannot say whether *every reference* to that name can be renamed, so a `prepareRename` answer from LSPD must be conservative. |
| `textDocument/rename` | c->s | `ast_edit_preview` (kind `symbol`) | partial | A whole-symbol `replace` works when the symbol resolves uniquely. Reference sites are **not** updated: that is semantic rename, excluded from 1.0. LSPD must present this as editing one definition, not as a rename. |
| `textDocument/semanticTokens/full` | c->s | - | none | No semantic token stream in this build. |
| `textDocument/semanticTokens/range` | c->s | - | none | Same. |
| `textDocument/semanticTokens/full/delta` | c->s | - | none | Same; there is no stream to delta. |
| `workspace/semanticTokens/refresh` | s->c | - | none | Nothing to refresh. |
| `workspace/inlayHint/refresh` | s->c | - | none | Nothing to refresh. |
| `workspace/codeLens/refresh` | s->c | - | none | Nothing to refresh. |
| `textDocument/diagnostic` | c->s | `ast_outline`, `ast_info` | partial | Pull diagnostics. The engine has a syntax-error **count** per file, printed by `ast_outline` as `(K syntax errors)`. It has no range, no severity and no message per error, so a pull-diagnostic answer can carry a count and nothing finer. See [§5](#5-diagnostics-and-the-syntax-gate). |
| `textDocument/publishDiagnostics` | s->c | - | none | The engine does not compute diagnostics, so LSPD has nothing to publish that it did not synthesise itself. |
| `workspace/diagnostic/refresh` | s->c | - | none | Same. |
| `workspace/symbol` | c->s | `ast_get`, `ast_outline` | partial | `ast_get` finds one symbol by exact name (or qualified name) with `path` defaulting to the whole workspace; `ast_outline` walks a directory. Neither is a fuzzy workspace query, so `workspace/symbol` answers are exact-name matches only. |
| `workspace/executeCommand` | c->s | - | none | No command registry. |
| `workspace/didChangeConfiguration` | c->s | - | none | Limits come from configuration at startup; there is no live reconfiguration, and no tool accepts a limit argument. |
| `workspace/didChangeWatchedFiles` | c->s | - | none | No cache to invalidate ([§1.2](#12-lifecycle)). |
| `workspace/didChangeWorkspaceFolders` | c->s | - | none | One workspace root per server process; a different root is a different workspace id and a different plan store. |
| `workspace/willCreateFiles` | c->s | - | none | Nothing to prepare; the engine writes only inside its own state directory. |
| `workspace/didCreateFiles` | c->s | - | none | Same. |
| `workspace/willRenameFiles` | c->s | - | none | Same. |
| `workspace/didRenameFiles` | c->s | - | none | Same. |
| `workspace/willDeleteFiles` | c->s | - | none | Same. |
| `workspace/didDeleteFiles` | c->s | - | none | Same. |
| `telemetry/event` | s->c | - | none | No telemetry; `ast_info` reports configuration, nothing is reported spontaneously. |

### 3.2 The other direction: every tool

The table above is indexed by LSP method. This one is indexed by **tool**, because the two
sets are not the same size: six of the eleven tools have no LSP surface at all, and saying so
is the point. An integrator reading only the first table would not learn that `ast_undo`
exists, let alone that an editor cannot reach it.

Columns: tool, its LSP counterpart, status, note.

| Tool | LSP counterpart | Status | Note |
|---|---|---|---|
| `ast_info` | `initialize` | composition | One call, once, before anything else ([§1.1](#11-who-starts-whom)). Its five lines are the handshake. |
| `ast_outline` | `textDocument/documentSymbol` | exact | Same data: nesting, kinds, names, line ranges. Also the first half of every position-based request. |
| `ast_get` | `textDocument/hover`, `textDocument/definition` | composition | One symbol's text by name; also the second half of the recipe in [§3.4](#34-position-to-name-the-one-composition-every-request-needs). |
| `ast_search` | `textDocument/references`, `textDocument/documentHighlight` | partial | Syntactic matches, not references. |
| `ast_explain_pattern` | - | none | A debugging aid for callers writing patterns. No editor surface shows "how this pattern parsed". |
| `ast_edit_preview` | `textDocument/rename` | partial | Produces the plan a rename would need, for the one case a symbol resolves uniquely. Reference sites are not updated, so it is not a rename ([§3.1](#31-method-by-method)). |
| `ast_plan_show` | - | none | Plan inspection. The LSP client already holds the plan the preview returned; there is nothing to look up. |
| `ast_plan_list` | - | none | Same, for the workspace's stored plans. A session that previews everything it needs never needs the list. |
| `ast_edit_apply` | - | none | Applying is what `textDocument/rename` does *through* a preview, not a separate LSP request. An editor that wants "apply this plan" has no standard request to send. |
| `ast_undo` | - | none | LSP has no undo request. A client undoes through its own buffer, which is the right place for it. |
| `ast_recover` | - | none | Crash recovery is a CLI operation, per `EDIT-MODEL`: it needs a person, not a buffer. |

The six `none` rows are the honest answer to "why does my editor not see undo?": the engine
has it, and the protocol has no request for it.

### 3.3 What `didOpen`/`didChange` have no counterpart for, and why

The engine reads every file from the workspace through the boundary on every call
([`ARCHITECTURE.md`](ARCHITECTURE.md#the-engineshell-split)). It never sees an editor
buffer.

The consequence is not academic: for a **dirty** document, every engine answer describes
the file on disk, not what the user is looking at. LSPD must therefore:

1. treat `textDocument/didChange` as advisory only, and
2. refuse, or answer with a warning, any position-dependent request
   (`hover`, `definition`, `documentSymbol`, `references`) for a document whose buffer differs
   from the file, and
3. refuse `textDocument/rename` and any `ast_edit_preview` for such a document outright: a
   plan records byte offsets and hashes of the file **on disk**
   ([`EDIT-MODEL.md`](EDIT-MODEL.md#plan-format-version-1)), so applying it against a
   different buffer is refused as `[stale_plan]` — correctly, but late, after the user has
   reviewed a diff that was never going to apply.

There is no engine call that returns "the file changed since you last looked"; LSPD has to
compare hashes itself if it wants that check.

### 3.4 Position to name: the one composition every request needs

Tools take names, never `line:column`. So every position-based LSP request becomes:

1. `ast_outline` with `path` = the file, `depth` = 6, to get symbol extents as
   `L<start>-<end>` (1-based lines).
2. Choose the **innermost** symbol whose line range contains the position.
3. Call the second tool with that symbol's name (or qualified name).

Step 2 is a decision LSPD makes, and the engine cannot make it for it: `ast_outline` returns
a flat list with nesting depth, not a tree with parent links, so "innermost containing
symbol" is a client-side computation over documented output. When two symbols share a name
in one file, the second call answers `[ambiguous]` and lists the candidates, which is the
intended path ([`TOOLS.md`](TOOLS.md#error-code-reference)).

## 4. Position conversion

### 4.1 The two systems

| | LSP `Position` | Engine |
|---|---|---|
| line | **0-based** | **1-based** (`L<start>`, `Match.start_line`) |
| character / column | **0-based, in UTF-16 code units** | **1-based, in bytes** (`Match.start_col`) |
| absolute position | `Range` is line/character pairs only | byte offsets, internal to the engine and never printed |

Both axes differ, and they differ independently: `line + 1`, and the character axis needs a
real unit conversion. Three traps, in order of how often they bite:

1. **UTF-16 code units are not characters and not bytes.** An astral character (an emoji) is
   2 UTF-16 units and 4 UTF-8 bytes. Treating "character" as "byte" is off by two there, and
   the offset it produces lands *inside* the character.
2. **A non-ASCII character before the target shifts everything.** `α` is 1 UTF-16 unit and 2
   bytes, so every byte offset after it is one ahead of the UTF-16 offset.
3. **CRLF costs a byte per line.** Line starts after the first are off by one for each
   `\r\n`, for every engine line/column pair.

### 4.2 Worked example (executable)

Source, one line, LF, then a newline — `S`:

```
let s = "α😀";
```

`S` is 18 bytes. Byte offsets: `l`=0, `e`=1, `t`=2, ` `=3, `s`=4, ` `=5, `=`=6, ` `=7,
`"`=8, `α`=9-10, `😀`=11-14, `"`=15, `;`=16, `\n`=17. Line 0 has 14 UTF-16 code units.

Columns: LSP line, LSP character, what is there, byte offset, engine `line:col`.

| LSP line | LSP character | At | Byte offset | Engine `line:col` |
|---|---|---|---|---|
| 0 | 8 | the `"` before `α` | 8 | 1:9 |
| 0 | 9 | `α` | 9 | 1:10 |
| 0 | 10 | first surrogate of `😀` | 11 | 1:12 |
| 0 | 12 | the closing `"` | 15 | 1:16 |
| 0 | 14 | end of line | 17 | 1:18 |

Every row above is recomputed from `S` by `crates/tools/tests/lspd_spec.rs`, so the numbers in
this table cannot drift away from what the conversion actually does.

Row 3 is trap 1 and trap 2 at once: character 10 is byte **11**, not 10. A converter that
assumed one byte per unit would return byte 10, which is the second byte of `α` — an offset
that does not lie on a character boundary at all. Row 4 is the same trap with the emoji: 2
units became 4 bytes.

The reverse direction, which LSPD needs for every range it sends back:

Columns: engine `line:col`, byte offset, LSP line, LSP character.

| Engine `line:col` | Byte offset | LSP line | LSP character |
|---|---|---|---|
| 1:12 | 11 | 0 | 10 |
| 1:16 | 15 | 0 | 12 |

Also recomputed by the same test, in the other direction.

So: byte → LSP is `line = line - 1`, then count UTF-16 units from the start of that line;
LSP → byte is `line + 1`, then walk UTF-16 units until the count is spent, refusing (not
clamping) a character offset that would land inside a character.

### 4.3 The same example with CRLF

Source `S2` = `let s = "α😀";\r\n` — **19** bytes, because `\r\n` is two where `S`'s `\n` was
one. Line 1 starts at byte **19**, one byte later than in `S`, where it starts at 18. Every
engine line/column pair on a later line inherits that one-byte difference, so a converter
that indexes lines by `line - 1` without measuring the line terminator is wrong on every
line after the first in a CRLF file.

`crates/core/src/text.rs` exposes `detect_line_ending`, which is what `ast_search`-style
output and this document agree on: mixed files answer with the style of the **first** break.

## 5. Diagnostics and the syntax gate

**Diagnostics are not fed into the syntax gate.** The gate is defined in terms of the
engine's own parse:

> `syntax` | `post_errors <= pre_errors` for every file (an edit may fix errors but may not
> add them) | No

([`EDIT-MODEL.md`](EDIT-MODEL.md#gates)). Both counts come from the engine parsing the bytes
before and the bytes after ([`ARCHITECTURE.md`](ARCHITECTURE.md#why-the-verification-parse-is-separate)),
so the gate compares the engine with itself. A language server's diagnostics are a different
measurement of the same bytes, from a different parser, with a different error model.

The same document settles what happens instead:

> verification beyond syntax is the caller's job, and `apply` returns a list of changed files
> so a caller can run a language server's diagnostics

([`EDIT-MODEL.md`](EDIT-MODEL.md#gates)). So the division is:

| Question | Answered by |
|---|---|
| May this edit add syntax errors? | the engine, in preview, as `gate_failed` |
| Is the result type-correct, lint-clean, test-passing? | LSPD, after apply, with its own language server |

What LSPD may do with diagnostics:

- **Run them after apply**, on the changed-file list apply returns, and report them. That is
  the intended division of labour.
- **Surface the engine's own count** as a pull-diagnostic-free summary: `ast_outline` prints
  `(K syntax errors)` per file. It is a number, not a diagnostic — no range, no severity, no
  message — so it cannot be turned into a `Diagnostic` without inventing all three.

What LSPD may **not** do: pass a diagnostic count into the gate, or treat "the language
server found nothing" as licence to skip the gate. The gate has no argument for it, and
adding one would change L3 semantics, which this document may not do.

## 6. Error code mapping

The engine answers with a `[code]` and a message that states what is true and what to do
next ([`TOOLS.md`](TOOLS.md#error-code-reference)). LSP answers with a JSON-RPC error. The
mapping is on the **JSON-RPC code**, because that is the field a client switches on.

| LSP `ResponseError.code` | Engine `[code]` | When LSPD should send it |
|---|---|---|
| `-32700` `ParseError` | - | LSPD's own JSON is malformed. Never an engine answer. |
| `-32600` `InvalidRequest` | `invalid_args` | The request object itself is wrong. |
| `-32601` `MethodNotFound` | - | The method is not in [§3](#3-capability-mapping); the engine is not involved. |
| `-32602` `InvalidParams` | `invalid_args`, `not_found` | `not_found` when a named symbol or path does not exist. |
| `-32603` `InternalError` | `internal`, `io_error` | `internal` is a defect and should be reported upstream with the plan id. |
| `-32002` `ServerNotInitialized` | - | The engine has no session state to be uninitialised ([§1.2](#12-lifecycle)). |
| `-32001` `UnknownErrorCode` | - | Never sent by an engine-backed method. |
| `-32803` `RequestFailed` | `gate_failed`, `stale_plan`, `wrong_workspace`, `plan_corrupt`, `already_applied`, `diverged`, `rollback_incomplete`, `write_disabled` | A well-formed request the engine refuses for a semantic reason. The engine's message is the payload; the JSON-RPC code only says "failed". |
| `-32802` `ServerCancelled` | - | The engine does not cancel; it returns `[timeout]` or `[budget_exceeded]`, which map to `-32803`. |
| `-32801` `ContentModified` | `stale_plan`, `diverged` | A file changed between preview and apply, or between apply and undo. |
| `-32800` `RequestCancelled` | - | Only in answer to `$/cancelRequest`, which the engine does not implement ([§3.1](#31-method-by-method)). |

Engine codes that have no distinct JSON-RPC code all travel as `-32602` or `-32803`:

| Engine `[code]` | JSON-RPC code | Note |
|---|---|---|
| `outside_workspace`, `protected_path` | `-32602` | A refusal about the argument, not a failure of the request. |
| `unsupported_language`, `file_too_large`, `not_utf8`, `limit_exceeded`, `budget_exceeded`, `timeout` | `-32803` | The request was well-formed; the engine could not do it within its own rules. |
| `ambiguous`, `invalid_pattern`, `comment_loss`, `invalid_edit` | `-32602` | The argument needs to change before it can succeed. |
| `not_found` | `-32602` | Same. |
| `unsupported_target` | `-32602` | A target the engine will not replace (hard links, read-only). |
| `busy` | `-32803` | Another apply holds the lock; retry shortly. |
| `config_untrusted` | `-32602` | The operator's configuration file exists but cannot be trusted — wrong owner, wrong permissions, or not a regular file. Raised while the server starts, so it is reported as a startup refusal rather than through a method; listed here so the code has one meaning on both surfaces. |
| `plan_not_found`, `plan_expired` | `-32602` | |
| `journal_missing` | `-32602` | Retention expired: the plan is still in the store, but its originals are gone, so undo can no longer restore them. |
| `internal` | `-32603` | A defect. Report it with the plan id; the message carries no source text. |
| `replaced_not_durable` | `-32803` | The write happened and may not survive a crash; not in the current tool output surface, listed for completeness. |

The engine's message must survive into `message`, and its `next` step (what to do about it)
into LSP `data`, so a client can show "what to do next" without parsing prose. The engine's
messages never contain file contents or absolute paths outside the workspace
([`TOOLS.md`](TOOLS.md#conventions)), so they are safe to relay verbatim.

## 7. Requests against the engine

Things LSPD would use if the engine had them. **None of these exists**, and this document
does not assume any of them:

| Request | Would unblock |
|---|---|
| Per-error syntax positions, not a count | real `publishDiagnostics` / pull diagnostics |
| A range- or selection-limited `ast_search` | `textDocument/selectionRange`, better `references` |
| A tree-shaped outline with parent links | removing the client-side "innermost symbol" step in [§3.4](#34-position-to-name-the-one-composition-every-request-needs) |
| A file-identity or content hash a client can ask for | cheap dirty-buffer detection instead of LSPD comparing hashes itself ([§3.3](#33-what-didopendidchange-have-no-counterpart-for-and-why)) |
| An offset/UTF-16 conversion helper | the traps in [§4](#4-position-conversion) |

Each of these is an engine change in L2-L4 and therefore outside this repository's role for
this document.

## 8. What this document deliberately does not claim

- It does not claim the engine will ever speak LSP. It speaks MCP; LSPD is the adapter, in
  another repository.
- It does not claim any LSP capability marked `none` above. Those rows exist so the absence
  is explicit rather than a gap someone fills by assumption.
- It does not claim diagnostics, formatting, completion, code actions or semantic rename.
- It does not describe behaviour of the companion integration beyond what the mapping
  requires; that repository owns the transport, the document synchronisation and the UI.