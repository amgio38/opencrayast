# opencrayast reference

Detail that [`SKILL.md`](../SKILL.md) points to. Load this file when you need an
argument range, an error code or a pattern rule that the main instructions do not cover.
The project's normative tool specification is
[`docs/TOOLS.md`](https://github.com/amgio38/opencrayast/blob/main/docs/TOOLS.md). If the
two ever disagree, the `description` the server returns in `tools/list` is the contract.

## Contents

- [Registering the server](#registering-the-server)
- [Arguments](#arguments)
- [Limits and budgets](#limits-and-budgets)
- [Patterns and rewrites](#patterns-and-rewrites)
- [Plans](#plans)
- [Errors](#errors)

## Registering the server

The server speaks MCP over stdio and needs exactly one workspace root:

```text
opencrayast-mcp --workspace <dir> [--read-root <dir>]... [--config <file>] [--allow-write]
```

- `--workspace` is the only tree the server may read and, in write mode, write. `.` means
  the directory the client starts the server in.
- `--read-root` adds an extra **read-only** root. It can be repeated. Files under it are
  shown as `@root1/…`, `@root2/…` in the order the flags were given, and are never
  written. The label is for display only: pass an absolute path to address such a file.
  A preview of an edit there produces a plan that can never be applied. `/`, a drive
  root, the home directory itself and credential directories are refused as roots.
- `--config <file>` reads the user configuration from another location. By default it
  is `$XDG_CONFIG_HOME/opencrayast/config.toml` (`~/.config/…`), or
  `%APPDATA%\opencrayast\config.toml` on Windows.
- `--allow-write` has no effect on its own. Write mode also needs
  `policy.allow_write = true` in the user's configuration file. That decision belongs to
  the user, not to the agent.

Claude Code:

```sh
claude mcp add opencrayast -- opencrayast-mcp --workspace .
```

Clients configured with a JSON `mcpServers` map (Cursor, Claude Desktop and others):

```json
{
  "mcpServers": {
    "opencrayast": {
      "command": "opencrayast-mcp",
      "args": ["--workspace", "/path/to/project"]
    }
  }
}
```

`opencrayast doctor` checks the installation and the configuration, and reports what to
fix.

## Arguments

### `ast_outline`

| Argument | Type | Default | Meaning |
|---|---|---|---|
| `path` | string, required | – | A file or a directory (directories are walked, honouring ignore rules) |
| `depth` | integer 1–6 | 3 | Nesting depth of symbols |
| `kinds` | string[] | all | Filter by portable kind |
| `include_docs` | boolean | false | Add the first line of each doc comment |
| `limit` | integer 1..=`results` limit | 200 | Maximum symbols. A value above the configured limit is refused, not clamped |

### `ast_get`

| Argument | Type | Default | Meaning |
|---|---|---|---|
| `symbol` | string, required | – | Bare or qualified name; 1–256 bytes; one name per call |
| `path` | string | `.` | Restrict to a file or directory; strongly recommended |
| `context_lines` | integer 0–20 | 0 | Surrounding lines to include |
| `include_doc` | boolean | true | Include the leading doc comment |

### `ast_search`

| Argument | Type | Default | Meaning |
|---|---|---|---|
| `pattern` | string, required | – | Code in the target language with metavariables |
| `language` | string | from `paths` | Required when `paths` covers several languages |
| `paths` | string[] | `["."]` | Files or directories. An explicitly empty list is refused |
| `context_lines` | integer 0–5 | 0 | Lines around each match |
| `limit` | integer 1..=`results` limit | 100 | Maximum matches |

`rule` is refused on the MCP path with `[invalid_args]`. So is any other argument a tool
does not have; the message lists the accepted names.

### `ast_explain_pattern`

`pattern` and `language`, both required. Reads no files.

### `ast_edit_preview`

| `kind` | Required | Optional |
|---|---|---|
| `rewrite` | `language`, `paths`, `pattern`, `replacement` | `note` |
| `symbol` | `path`, `symbol`, `operation`; `text` for every operation except `delete` | `note` |

`operation` is one of `replace`, `replace_body`, `delete`, `insert_before`,
`insert_after`. `replace_body` finds the body in the syntax tree. In a brace language
`text` must start with the opening brace and end with the closing one; in Python it is the
body's statements, re-indented to the body. A symbol with no braced body (an arrow function
with an expression body) is refused; use `replace`. `delete` also removes the symbol's
leading doc comment. `note` is at most 1024 bytes by default, and it is part of the plan, so
changing it changes the plan id.

### Plan tools

| Tool | Arguments |
|---|---|
| `ast_plan_list` | `limit` (default 20) |
| `ast_plan_show` | `plan_id` (required; a unique prefix of at least 10 characters is accepted), `file`, `offset`, `limit` (in hunks; omit for all, `0` is refused) |
| `ast_edit_apply` | `plan_id` (required; the full 28-character id) |
| `ast_undo` | `plan_id` (required; the full id) |
| `ast_recover` | none |

### Portable symbol kinds

`module`, `namespace`, `class`, `struct`, `enum`, `interface`, `trait`, `impl`, `fn`,
`method`, `const`, `static`, `type`, `variable`, `macro`. A language that lacks a kind
never produces it. (`field` is reserved in the vocabulary but is not emitted yet.)

## Limits and budgets

`ast_info` prints the file, output, results and plan-size limits in force. The defaults
of all of them are:

| Limit | Default |
|---|---|
| File size | 4 MiB |
| Output per call | 64 KiB (hard maximum 256 KiB) |
| Results per list (`results`) | 200 |
| Plan size | 50 files, 500 edits |
| Plan lifetime, counted from the preview | 15 minutes |
| Unexpired plans per server process | 25 |
| Undo journal retention (the undo window) | 7 days |
| Wall-clock time per `ast_search` or `ast_get` call | 10 s |
| Files scanned per call | 5,000 |

When a budget runs out the call answers `[budget_exceeded]` or `[timeout]` and names what
to narrow; it never hangs. A rewrite preview is bounded by a step budget rather than the
wall clock. Hitting the file-scan limit shows a `[walk truncated at … files]` footer in
`ast_outline` and `ast_search`, and `[limit_exceeded]` in a preview.

## Patterns and rewrites

- A pattern must parse to **one** node. Several statements, or a fragment that parses as
  an error, is `[invalid_pattern]`. `ast_explain_pattern` shows the parse and warns when a
  pattern only parses in a particular context.
- Matching compares node kinds and their children. Whitespace and comments between nodes
  are ignored. Text inside strings and comments is compared exactly.
- Nested matches are all reported by a search. A rewrite changes only the **outermost** of
  two overlapping matches, and the preview says so in a `nested:` line.
- In a replacement, `$NAME` and `$$$NAME` insert the captured source text, and `$$` is a
  literal `$`. A metavariable used in the replacement but not bound by the pattern is
  `[invalid_pattern]`.
- Multi-line replacements are re-indented to the line where the match starts and use the
  file's line endings.
- A captured expression is wrapped in parentheses when the replacement puts it next to an
  operator that binds tighter (`$X * 2` with `$X = a + b` becomes `(a + b) * 2`), and the
  preview says so in a `parenthesised:` line.
- A rewrite that would drop comments sitting between captured parts is refused with
  `[comment_loss]`: capture the part that holds the comment, or narrow the pattern so the
  comment lies outside every match.

Examples:

```json
{"kind": "rewrite", "language": "javascript",
 "pattern": "var $NAME = $VALUE;", "replacement": "let $NAME = $VALUE;", "paths": ["src"]}
```

```json
{"kind": "rewrite", "language": "python",
 "pattern": "connect($HOST, $PORT)", "replacement": "connect($PORT, $HOST)", "paths": ["."]}
```

## Plans

- A plan id is `p-` followed by 26 lower-case base32 characters. It is derived from the
  request and the file contents, so the same request on the same files gives the same id.
- Plans live in the user's state directory, outside the workspace: `$XDG_STATE_HOME/opencrayast`
  or `~/.local/state/opencrayast` on Linux and macOS, `%LOCALAPPDATA%\opencrayast` on
  Windows. Previewing never creates or modifies a file in the workspace.
- `ast_plan_list` shows only unexpired plans. Its `state` column is `ready` when nothing
  has been applied, otherwise the journal's state: `prepared`, `writing`, `applied`,
  `undoing`, `rolled_back` or `undone`.
- A plan expires 15 minutes after the preview by default. Previewing the identical
  request again returns the same plan and does not extend it.
- `ast_plan_show` renders the plan as it was previewed, before and after the apply (after
  it, against the original the journal kept). A file that changed since the preview gets
  `[no diff for <file>: StalePlan]` instead of a diff. It never shows the current file
  contents; use `ast_get` or `ast_outline` for those.
- Each journal keeps a copy of its plan, so `ast_undo` works for the journal's retention
  even after the plan has expired from the plan store.

## Errors

Every error has the form `[code] what is true. Next: what to do.`

| Code | Meaning | What to do |
|---|---|---|
| `invalid_args` | Missing, mistyped or out-of-range argument, or an unknown tool | Fix the call to the form the message gives. An unknown write tool means the server is read-only |
| `outside_workspace` | The path is outside the workspace or cannot be shown to exist | Use a path inside the workspace. Do not try another spelling |
| `protected_path` | The target can never be written (version-control data, secrets) | Choose another file. This cannot be overridden |
| `not_found` | No such file or symbol | Check the name with `ast_outline` |
| `ambiguous` | Several symbols or plans match | Repeat with `path` set to one of the listed candidates (or a longer plan id) |
| `unsupported_language` | No grammar for this file | Read the file the ordinary way |
| `file_too_large` | Over the file size limit | Narrow to a smaller file |
| `not_utf8` | The file is not valid UTF-8 | Not supported; read it another way |
| `invalid_pattern` | The pattern does not parse to one node | Use `ast_explain_pattern`; write a complete fragment |
| `budget_exceeded` / `timeout` | A parse, match or time budget ran out | Narrow `paths` or make the pattern more specific |
| `limit_exceeded` | The plan is too large, or the plan quota (25 unexpired plans per process, 100 in the store) is full | Split a large plan by directory. For a full quota, stop previewing and wait for plans to expire |
| `invalid_edit` | Edits overlap, are out of range or split a character | Submit one edit per region |
| `comment_loss` | The rewrite would drop comments | Capture the commented part or narrow the rewrite |
| `gate_failed` | A safety gate refused the result (for example more syntax errors, or a changed line ending) | Fix the request; the message names the gate and the file |
| `write_disabled` | Writing is off | Hand the plan id to the user |
| `plan_not_found` / `plan_expired` / `plan_corrupt` / `wrong_workspace` | The plan cannot be used | Before an apply: preview again and show the new diff. From `ast_undo`: there is no usable journal any more; tell the user to use version control |
| `stale_plan` | A file changed since the preview | Preview again and show the new diff |
| `already_applied` | The plan was already applied | Nothing to do, or `ast_undo` it |
| `busy` | Another apply holds the lock | Retry shortly |
| `diverged` | Files changed after the apply, so undo or recovery cannot proceed | Do not retry. Tell the user; a person must resolve it |
| `journal_missing` | Reserved; not emitted today. An undo whose journal is gone answers `plan_not_found` | – |
| `unsupported_target` | The file is a symlink, hard-linked, read-only or has attributes that cannot be kept. Nothing was written | Tell the user |
| `replaced_not_durable` | **The file was replaced**, but the change may not survive a crash | Re-read the file before deciding anything. Do not assume the write was lost |
| `rollback_incomplete` | A rollback could not finish; the workspace may be partly applied | Call `ast_recover`; if that fails, tell the user to restore from version control |
| `config_untrusted` | The user's configuration file has unsafe ownership or permissions | Tell the user; do not change the file yourself |
| `io_error` | The filesystem failed | The message says what was restored; tell the user |
| `internal` | A defect in opencrayast | Report it to the user with the plan id |
