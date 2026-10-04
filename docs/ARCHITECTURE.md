# Architecture

This document fixes the structure of `opencrayast`: its layers, the process model,
the data flow of a request, where trust changes, and how the platforms differ. It
is normative; code that disagrees with it is wrong or the document must be changed
first (see [ADR process](DECISIONS.md#how-decisions-are-made)).

## Shape in one paragraph

A single stateless process per client. There is **no daemon and no socket**: a
client launches `opencrayast-mcp` over stdio, requests are served, and the only
state shared between runs lives on disk (stored plans and the apply journal). The
privileged work — resolving paths, reading and writing files, taking locks — is
done by a small **shell**. The interesting work — parsing, matching, rewriting — is
done by a **pure engine**: a function from *bytes and a request* to *a result*, with
no ambient authority. That split is what lets the engine run in an isolated worker
process and is the main structural security decision (ADR-004).

## Principles that shape the structure

1. **Pure engine, privileged shell.** The engine never touches the filesystem, the
   network or the environment. The shell owns every side effect.
2. **One boundary.** Every filesystem access is mediated by `Boundary`; there is no
   second way to open a file.
3. **Plans are data.** An edit is described by a self-contained, serialisable
   plan. Apply consumes plan data; it does not re-derive it.
4. **Layering is enforced.** Dependencies point downwards only, checked by a
   script in CI.
5. **No `unsafe` today.** `[workspace.lints.rust] unsafe_code = "forbid"` applies to
   every current crate. Anything that would need it (OS sandbox calls, FFI glue) is
   planned as an optional tiny crate reviewed line by line (**M6**; not in the tree
   yet). Until then there is no exception.

## Layers and crates

```
                    ┌─────────────────────────────┐
  binaries          │ opencrayast-mcp   opencrayast│   stdio MCP server · human CLI
                    └──────────────┬──────────────┘
                    ┌──────────────▼──────────────┐
  L4 tools          │ opencrayast-tools            │   tool catalogue, schemas, handlers,
                    │                              │   output formatting, mode (read/write)
                    └──────────────┬──────────────┘
                    ┌──────────────▼──────────────┐
  L3 edit           │ opencrayast-edit             │   plans, preview, apply, journal,
                    │                              │   undo, recovery, gates
                    └──────────────┬──────────────┘
                    ┌──────────────▼──────────────┐
  L2 query          │ opencrayast-query            │   outline, get, patterns, search,
                    │                              │   rewrite generation (pure)
                    └──────────────┬──────────────┘
                    ┌──────────────▼──────────────┐
  L1 lang           │ opencrayast-lang             │   language registry, grammars,
                    │                              │   budgeted parsing, worker protocol
                    └──────────────┬──────────────┘
                    ┌──────────────▼──────────────┐
  L0 core           │ opencrayast-core             │   boundary, limits, errors, hashing,
                    │                              │   text, atomic fs, locks, config
                    └─────────────────────────────┘

  opencrayast-e2e       planned, not present: end-to-end tests that depend on the binaries
  opencrayast-sandbox   planned (M6), not present: would hold the only `unsafe` (OS sandbox/rlimit calls)
```

| Crate | Owns | Must not |
|---|---|---|
| `opencrayast-core` | `Boundary` (path policy), `Limits`, `ToolError`/`ErrorCode`, `ContentHash`, `LineIndex`, UTF-8 policy, atomic write, advisory locks, config types and loading | depend on any other workspace crate; parse source code |
| `opencrayast-lang` | language registry and tiers, grammar crates (feature-gated per language), budgeted `parse` (M2's worker protocol and both executors are planned, not present: `crates/lang/src/` today is `language.rs`, `lib.rs` and `parse.rs`) | touch the filesystem; know about plans |
| `opencrayast-query` | outline and symbol queries, the pattern/rule language, matching, generation of edit sets from a rewrite, all as pure functions of bytes | read files; know about stored plans |
| `opencrayast-edit` | plan model and canonical serialisation, plan store, preview, apply, journal, undo, recovery, verification gates | print user-facing text; speak MCP |
| `opencrayast-tools` | tool definitions (JSON Schema), argument validation, handlers wiring shell + engine, output formatting, read-only vs write mode, tool annotations | start processes or open sockets; contain logic that the CLI cannot reuse |
| `opencrayast-mcp` | the stdio JSON-RPC/MCP transport, request limits, cancellation, logging setup | contain tool logic |
| `opencrayast` (CLI) | argument parsing, interactive review and confirmation, `doctor` | contain logic that is not in `tools`/`edit` |

The dependency direction is the table order. `core` depends on nothing in the
workspace; a lower layer never names a higher one. The layering check
script (added in M0) reads every `Cargo.toml` and fails on a violation or on a `path` dependency
that points outside the workspace.

## The engine/shell split

```
 request ──► [ shell ]                                         [ engine ]
              resolve path (Boundary)  ──── bytes, language ─►  parse (budgeted)
              read file (size cap)                              outline / match / rewrite
              hash content                  ◄──── result ─────  (pure; no I/O)
              build plan, store plan
```

- **Engine requests** are plain data: `{ op, language, source bytes, arguments,
  budget }`. **Results** are plain data: symbols, matches, edit sets, error counts.
  They never contain file handles or paths — paths are the shell's concern and the
  engine only ever sees an opaque label for messages.
- An executor runs requests. `InProcess` calls the engine directly (used in tests,
  as the fast path, and as a documented fallback). `Worker` runs the same engine in
  a child process that has been stripped of authority (ADR-004). The two produce
  identical results; a differential test asserts it.
- Because results are data, the **shell can audit them**: before a plan is stored,
  every edit range is checked to lie inside the file, to be non-overlapping, to be
  on character boundaries, and to respect the limits. The engine is not trusted to
  stay within bounds even when it runs in-process.

### Why the verification parse is separate

After computing the new content for a file, apply asks the engine to parse it again
and report the syntax-error count (the *syntax gate*). This is a second, independent
engine request on the *output bytes*, so a defective rewrite is caught by the same
machinery that would catch a hostile input.

## Request lifecycle (read tool)

1. The MCP layer reads one message (size-capped) and validates the JSON-RPC shape.
2. `tools` validates the arguments against the tool's schema and the active `Limits`.
3. For each path: `Boundary::resolve_read` → canonical path inside the workspace
   (or a refusal with a code).
4. The shell opens the file, checks size, reads at most `max_file_bytes`, verifies
   UTF-8, and hashes the content.
5. The shell sends an engine request through the executor.
6. `tools` formats the result: deterministic order, hard output cap, explicit
   truncation notice.
7. The response is written; nothing about the source text is logged.

## Request lifecycle (edit)

```
 ast_edit_preview ─► resolve+read targets ─► engine: rewrite ─► shell: validate edit set
        ─► compute post-content + post-hash ─► build plan ─► store plan (0600, TTL) ─► diff + plan id

 ast_edit_apply  ─► load plan, check id==hash(plan) ─► resolve+open targets (write policy)
        ─► take locks ─► verify pre-hashes ─► apply recorded edits ─► check post-hash
        ─► engine: syntax gate on new bytes ─► journal(prepare) ─► temp files + fsync
        ─► rename each ─► journal(applied) ─► release locks ─► summary
```

The full algorithm, its failure semantics and its invariants are in
[`EDIT-MODEL.md`](EDIT-MODEL.md).

## State on disk

All state lives under the user's state directory (`$XDG_STATE_HOME/opencrayast`,
`~/.local/state/opencrayast` by default; `%LOCALAPPDATA%\opencrayast` on Windows),
created `0700` and verified (owner, mode, not a symlink) on every start:

```
$XDG_STATE_HOME/opencrayast/        # the state BASE: one per user, shared by all workspaces
  ws-<id>/                 # id = 128-bit hash of the root's device+inode / file id and canonical path
    plans/<plan-id>.json   # stored plans, 0600, TTL + quotas
    journal/<plan-id>/     # manifest.json + original file copies, 0600
    apply.lock             # serialises applies for this workspace
  log/opencrayast.log      # 0600, no source content
```

**State is never inside the workspace.** An earlier revision put it at
`<workspace>/.opencrayast`; that is reversed. Three findings forced it: the
directory walker did not skip it, so an ordinary `ast_outline`/`ast_get` walk could
read the tool's own plans, journals, undo backups, plan ids and before/after hashes
back as workspace content; the boundary's state-directory guard was compared against
a value nothing ever set, so a fully supported `preview` → `apply` could write into
the tool's own journal and `ast_undo` would restore it as legitimate; and the
protection that did exist was a `.gitignore` entry, which only this repository had.
For an upgrade, the old `.opencrayast` is skipped by name at any depth, so a stale
one is never walked into, and deleting it costs only undo history.

A machine that cannot say where its state is — no `XDG_STATE_HOME` and no `HOME` —
is **refused**. There is no fallback to the workspace; a fallback is the thing being
removed.

Keying by workspace id means a plan made for one workspace can never be applied to
another, even by accident. Nothing is written *inside* the workspace except the
edited files themselves.

Reclamation is `opencrayast plan gc`, and `opencrayast doctor` reports what it
would remove. Deleting a journal makes that plan's edit permanently unundoable, so
neither runs on a schedule of its own.

## Configuration surface

Operator-controlled only: command-line flags, environment variables and a
user-level configuration file. **Configuration is never read from the workspace.**
A repository an agent opens is untrusted and must not be able to turn on writing,
raise limits or add roots. Details: [`CONFIGURATION.md`](CONFIGURATION.md).

## Modes

| Mode | How selected | Tools exposed |
|---|---|---|
| `read-only` (default) | no flag | `ast_info`, `ast_outline`, `ast_get`, `ast_search`, `ast_explain_pattern`, `ast_edit_preview`, `ast_plan_show`, `ast_plan_list` |
| `write` | `--allow-write` **and** not forbidden by user policy | the above plus `ast_edit_apply`, `ast_undo`, `ast_recover` |

`ast_edit_preview` is available in read-only mode on purpose: it writes only to the
plan store in the state directory, never to the workspace. A caller with other means
of running code (a shell tool) can write to that store as the same user, so the store
is treated as untrusted input at apply time (full-id requirement, recomputed hash,
CLI confirmation), not as a trusted channel. That enables the
**human-in-the-loop path with zero write capability in the agent's process**: the
agent previews, a person reviews with `opencrayast edit show <plan-id>`, and the
person applies with `opencrayast edit apply <plan-id>`.

That last command is itself a gate rather than a writer: it lists the files that will
change and asks, and where there is no terminal to ask — a pipe, cron, CI — it refuses
outright (`[invalid_args]`, exit 1, nothing written) unless `--yes` was given, because a
non-interactive environment has nobody whose answer could be sought. See TOOLS.md
§The human CLI's confirmation gate.

## Concurrency model

- Many server processes may run at once (one per client). They share nothing in
  memory.
- Plans are immutable once stored. Reads of the plan store are lock-free.
- Applies are serialised per workspace by `apply.lock`, and each target file is
  additionally locked with an advisory lock for the duration of its verification
  and replacement. Files are locked in sorted path order to rule out deadlock.
- The workspace id comes from the root's *identity* (device and inode, or Windows file
  id) as well as its canonical path, so two spellings of one tree (bind mount,
  `subst`, UNC versus drive letter) share one lock and one journal. Overlapping
  workspaces (one inside another) are refused. Network filesystems where advisory
  locks are unreliable are detected, and write mode is refused there unless the
  operator opts in.
- A per-file advisory lock is held on the file's handle for verification and, because
  rename replaces the inode, is backed by the workspace lock for mutual exclusion
  between applies; it does not protect the new inode.
- Advisory locks only constrain cooperating processes. Against a non-cooperating
  writer (an editor), apply relies on re-verification immediately before each
  rename, and documents the residual window in [`SECURITY-MODEL.md`](SECURITY-MODEL.md).

## Platform notes

| Concern | Linux | macOS | Windows |
|---|---|---|---|
| Locks | `flock`-style via `std::fs::File::lock` | same | same (`LockFileEx`) |
| Atomic replace | `rename(2)` | `rename(2)` | temp file in the same directory → write → flush → `rename` over the target (NTFS rename is atomic). Identity is `creation_time` rather than dev/ino. `fsync_dir` confirms the directory still exists (no POSIX `fdatasync` on the dir) — weaker than unix, documented on the Windows arms. |
| Symlink/junction policy | `lstat` + `O_NOFOLLOW` + identity check | same | Symlink handling as on Linux; junction/reparse-point detection is **planned, not implemented** — see `TESTING.md` BND-06 |
| Path rules | case-sensitive | usually case-insensitive, normalising | case-insensitive; reserved names; `\\?\`; 8.3 aliases; alternate data streams refused |
| Worker restrictions | **planned, not implemented** — `RLIMIT_AS`/CPU, seccomp (including `execve`), Landlock when available | **planned, not implemented** — rlimits only until a Seatbelt profile lands | **planned, not implemented** — Job Object limits incl. memory; AppContainer when available |
| Replacement | handle/dirfd-relative `renameat`; directory `fsync` | same plus `F_FULLFSYNC` | **not implemented** — see the atomic-replace row; the planned design is to open without `FILE_SHARE_WRITE`, replace by handle, and retry on a bounded number of sharing violations |
| State dir check | owner + mode, and a `dev`/`ino` identity re-check | owner + mode, and a `dev`/`ino` identity re-check | **not** owner or ACL: creating the directory and refusing a link or a non-directory is implemented, but a security descriptor needs an API the standard library does not expose and this workspace forbids `unsafe`. The identity re-check is a **creation time**, which catches a store directory deleted and recreated but not a same-time substitution — see the note on `DirIdentity` in `crates/edit/src/fsutil.rs` |
| State dir | XDG | XDG-style under `~` | `%LOCALAPPDATA%` |

There is no Unix-socket dependency, so native Windows support is a first-class goal
from M0 rather than a later port. **On Windows today the reading tools work and every
write is refused**: the atomic-replace primitive and the isolated parse worker are both
unported, and the rows above say which.

## Observability

Structured logs to a `0600` file. Logged: tool name, outcome code, durations,
counts, plan ids. **Never logged:** source text, replacement text, absolute paths
outside the workspace, tool argument values that contain code. No telemetry, no
network access of any kind.

## Extension points (and their limits)

- **New language:** a grammar crate behind a Cargo feature, an outline query file,
  tier tests. Process in [`LANGUAGES.md`](LANGUAGES.md).
- **New tool:** a definition + handler in `tools`; it must declare its mode
  (read/write), its annotations, its limits and its error codes, and add rows to
  the threat-to-test matrix.
- **New edit kind:** extends the plan model; requires an EDIT-MODEL update, new
  gates if needed, and failure-injection coverage.
- There is intentionally **no plugin API**: loading third-party code into a process
  that can write to a workspace is a risk this project does not take.
