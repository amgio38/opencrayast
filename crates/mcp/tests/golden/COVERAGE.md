# MCP golden transcripts (MCP-08)

A recorded request/response exchange with the **shipped `opencrayast-mcp` binary**,
one file per tool and per reachable error code, replayed byte for byte.

- **Transcripts:** `crates/mcp/tests/golden/transcripts/*.txt` (35 files)
- **Harness:** `crates/mcp/tests/golden/mod.rs` (drives the real binary; owns the
  format and the normalisation)
- **Tests:** `crates/mcp/tests/mcp8_golden.rs`
- **Regenerate:** `cargo test -p opencrayast-mcp --test mcp8_golden -- --ignored record`

Read the diff before committing a regeneration. A change in these files is a change
to what an MCP client sees, and it should be a deliberate one.

## File format

```text
# golden-transcript v1
# case: error_tool_not_found_path
# covers: error code not_found (no such file)
# responses: 1
# ... (normalisation + regeneration notes) ...
> {"jsonrpc":"2.0","id":2,"method":"tools/call","params":{...}}
{"id":2,"jsonrpc":"2.0","result":{...}}
```

`#` lines are header metadata, `> ` marks a client line, every other non-blank line
is a server response **exactly as the server wrote it**. Nothing is hand-written:
the recorder writes bytes from the server's own stdout.

## Normalisation — exactly one thing

Replay asserts byte equality on every server line. One value is normalised, by the
same rule on both sides:

| Recorded / compared as | Why |
|---|---|
| `(id w-<32 hex>)` → `(id <WORKSPACE_ID>)` | `ast_info` prints a workspace identity **derived from the absolute path of the workspace root**. Recording and replay each run against a freshly created temp directory, so that hex differs every run by construction. |

Nothing else is normalised. No timestamps, durations, pids or absolute paths appear
in the files — the workspace is always a fresh temp dir, so no recorded line can
contain a machine-specific path. If a response ever stops being byte-stable, the
transcript is re-recorded; the comparison is never quietly loosened.

## Coverage

**38 transcripts** = 7 tool + 26 error code + 5 protocol.

Tools: `ast_info`, `ast_outline` (file and directory), `ast_get`, `ast_search`
(one match and zero matches), `ast_explain_pattern`, plus all three write tools
being refused in read-only mode.

`docs/TOOLS.md` §Error code reference documents **30** codes. **12 are pinned** by a
transcript, and the other **18** cannot be, each with a reason recorded in
`UNREACHABLE` in `mcp8_golden.rs` — the test fails if a code moves between those
sets, so the list cannot go stale:

- **13 routed, but the code needs a clock or a real write** — `ast_edit_apply`,
  `ast_undo`, `ast_recover` and the plan tools are in `dispatch.rs`'s match arms, but
  the codes above them (`gate_failed`, `invalid_edit`, `plan_expired`, `stale_plan`,
  …) can only be produced by a write the read-only surface refuses, or by a TTL that
  has to elapse.
- **2 not deterministic** — `timeout`, `budget_exceeded` are reachable only by
  racing a parse/tree budget against host speed. A golden file may not depend on how
  fast the machine is.
- **2 environment-dependent** — `io_error` needs a real filesystem failure;
  `internal` is by definition unexpected.
- `write_disabled` is handler-layer only: per `docs/TOOLS.md`, the MCP catalogue
  answers *unknown tool* instead, and that is what the transcript records.

`coverage_claims_every_code_it_names` and `unreachable_codes_are_documented_not_silently_skipped`
enforce this in both directions. Adding a tool or a code without a transcript is a
**failing** test, not a silent gap.

## The plan tools: what is pinned here, and what is deliberately not

`ast_plan_list`, `ast_plan_show` and `ast_edit_preview` were advertised by
`tools/list` while `dispatch.rs` had no arm for them — every call answered *unknown
tool*. That is fixed (the six edit arms are routed), and the transcript set now
covers the three tools' **refusal** paths: `plan_not_found`, the id-length rule, and
`ast_plan_list`'s own limit range.

**`ast_edit_preview`'s happy path is deliberately not here.** Its output carries
`(expires HH:MM UTC)`, which is a wall clock: a transcript asserting it byte for byte
would go red at every hour boundary, and a golden file that has to be re-recorded on
a clock is pinning the time of day rather than the protocol. That behaviour is
covered in `mcp_edit_plan_spec.rs` instead, which is where a clock-dependent claim
belongs. The reason is recorded in `golden/mod.rs` next to the case list rather than
left as an absence someone will "fix" by loosening the comparison.

## What these transcripts do NOT protect against

They are a **wire-format** regression net, and the gaps are real:

- **No happy path for the plan tools.** The three plan-tool transcripts pin refusals;
  the successful `preview`/`list`/`show` answers are behavioural, not byte-stable
  (see above).
- **No concurrency, ordering or interleaving.** Each transcript is a sequential
  request/response exchange over one pipe. Nothing checks behaviour under parallel
  requests, cancellation races, or two clients.
- **No streaming or progress notifications.** Only whole-line responses are recorded.
- **Nothing about logging.** stderr is discarded; the log-file behaviour the REQ asks
  for (0600, no source text, no argument fragments) is unpinned here.
- **Timing and resource behaviour.** No timeouts, no message-size limits beyond the
  framing errors, no memory pressure. A response that takes 10 s instead of 10 ms
  still passes.
- **`stderr` and exit status** are checked only as "exited 0", never recorded.
- **Normalisation is a blind spot of exactly one value.** The workspace id is
  asserted as a shape, not as a value; a change to how it is *derived* would not be
  caught.
- **They cannot tell a good change from a bad one.** A re-record makes any drift
  green. The value is in the *diff review*, not the test — which is why the format
  says so in every file.