# Edit model

How `opencrayast` changes code: the **plan**, the **preview** that creates it, the
**apply** that commits it, the **journal** that makes it recoverable, **undo**, and
**recovery**. This is the part of the project that carries the most risk, so it is
specified as invariants and failure semantics first, and as an algorithm second.

## The idea in four sentences

An edit is first described as a **plan**: a self-contained record of the exact
byte-range replacements for each file, with the hash of every file before and after.
The plan is shown to a reviewer (an agent, a person, or both) and stored under a
content-addressed id. **Apply** takes only that id: it writes exactly the recorded
edits, and only if every file is still byte-identical to what was reviewed. Every
apply leaves a journal of the originals, so it can be undone and so that a crash
never leaves the workspace half-changed.

## Vocabulary

| Term | Meaning |
|---|---|
| **Edit** | Replace byte range `[start, end)` of a file with `replacement` text |
| **Plan** | The set of edits over one or more files, plus metadata and hashes |
| **Plan id** | A short, content-addressed identifier derived from the canonical plan bytes |
| **Pre-hash / post-hash** | Content hash of a file before the plan / after all its edits are applied |
| **Gate** | A check the new content must pass before it may be written |
| **Journal** | Per-plan record of original file contents and of progress through apply |
| **Workspace id** | Short hash of the canonical workspace root; binds plans to a workspace |

## Invariants

| ID | Invariant | Enforced by |
|---|---|---|
| E-1 | An edit set is valid: every range lies inside the file, ranges do not overlap, boundaries fall on UTF-8 character boundaries, counts and sizes are within `Limits`. | Shell-side validation at preview; re-checked at apply |
| E-2 | `plan_id = hash(canonical plan)`. A plan whose recomputed id differs from its name is rejected. | Apply step 2 |
| E-3 | Apply **never searches**: it applies the recorded edits to a file whose content hash equals `pre_hash`. | Apply step 5 |
| E-4 | After applying a file's edits the resulting content hash equals `post_hash`, or nothing is committed. | Apply step 6 |
| E-5 | Nothing is written unless every file passes every gate. | Apply step 7 |
| E-6 | The journal holds every original file **before** the first target is touched. | Apply step 8 |
| E-7 | Each target is replaced atomically (temp file in the same directory, fsync, rename); the old content is never partially overwritten in place. | Apply step 9 |
| E-8 | After any interruption, recovery returns the workspace to the fully-original state. If a file was changed by someone else in the meantime, recovery stops and names exactly the files a person must resolve; it never silently settles on a mixture. | Recovery |
| E-9 | A plan can be applied at most once while its journal exists. | Journal marker |
| E-10 | Undo restores a file only if it still equals its post-apply content; otherwise the whole undo is refused. | Undo |
| E-11 | A plan is bound to one workspace; applying it elsewhere is refused. | Workspace id |
| E-12 | Preview never writes to the workspace. | Type-level: preview has no write capability |
| E-13 | Undo and recovery are journaled with the same machinery as apply: an interrupted undo or recovery is itself recoverable, and no sequence of crashes leaves a state that no tool can repair. | `undoing` state; Undo, Recovery |
| E-14 | Recovery, rollback and undo first **classify every file**, then act only if the whole set is consistent; they never overwrite content they did not produce, and never write back an original whose hash differs from `pre_hash`. | Recovery |
| E-15 | A reviewed plan cannot be replaced by another plan: apply requires the full plan id, and a stored unexpired plan is never evicted to make room. | Plan store, Apply step 1 |

## Edit kinds

Version 1.0 has two kinds of edit request. Both end as the same plan format.

### `rewrite` — pattern to replacement

```json
{ "kind": "rewrite", "language": "typescript",
  "pattern": "console.log($$$ARGS)", "replacement": "logger.debug($$$ARGS)",
  "paths": ["src/"], "rule": { "not": { "inside": "test_function" } } }
```

Finds every match of the pattern (semantics in [`PATTERNS.md`](PATTERNS.md)) and
replaces each with the expanded replacement text. Replacement text is re-indented
relative to the match site and uses the file's existing line ending.

### `symbol` — target a named symbol

```json
{ "kind": "symbol", "operation": "replace_body", "path": "src/lib.rs",
  "symbol": "Config::load", "text": "{ /* new body */ }" }
```

Operations: `replace` (the whole symbol, optionally including its leading doc
comment), `replace_body`, `delete`, `insert_before`, `insert_after`. The symbol must
resolve to exactly one node; otherwise the preview returns an `[ambiguous]` error
listing the candidates so the caller can disambiguate (never a guess).

The `text` a caller supplies is subject to the same rules as a rewrite's
replacement: it is re-indented to the site and put into the file's existing line
ending, so text pasted from another platform cannot leave a file with mixed line
endings (see [§Preserving file properties](#preserving-file-properties)).

### What is deliberately not an edit kind

- **Raw byte ranges supplied by the caller.** Offsets are fragile and invite
  mistakes; edits always come from a pattern match or a resolved symbol.
- **Semantic rename or move-across-files.** That needs a language server.
- **Anything that runs a tool.** No formatter, no code generator, no build.

## Plan format (version 1)

A plan is canonical JSON: keys sorted, no insignificant whitespace, UTF-8, numbers
as integers. The diff shown to a reviewer is *derived* from the plan and is not part
of the hashed content.

```json
{
  "format": 1,
  "workspace_id": "w-5c1e9a07",
  "engine_format": 1,
  "request": { "kind": "rewrite", "summary": "console.log -> logger.debug",
               "note": "optional text written by the caller" },
  "files": [
    {
      "path": "src/a.ts",
      "language": "typescript",
      "pre_hash": "sha256:6f0c…", "pre_size": 4812, "pre_errors": 0,
      "post_hash": "sha256:a91d…", "post_size": 4830, "post_errors": 0,
      "edits": [ { "start": 210, "end": 232, "replacement": "logger.debug(a, b)" } ]
    }
  ]
}
```

- **Everything that affects what is written or what a reviewer is told is inside
  the hashed content**, including `request.summary` and `request.note`. Nothing that
  varies between runs is: the creation time, the expiry and the producing binary
  version live in a separate, unhashed **envelope** next to the plan
  (`<plan-id>.meta.json`). `engine_format` is the version of the edit-generation
  rules, not of the binary, and changes only when those rules change.
- Paths are workspace-relative, normalised, and **re-resolved by the shell at apply
  time**; a stored path is a request, never an authority (E-1, S-1).
- `pre_errors` / `post_errors` are the syntax-error counts used by the syntax gate.
- The plan id is `p-` followed by the first 26 characters of the unpadded, lowercase
  RFC 4648 base32 encoding (alphabet `a-z2-7`) of the 128-bit-truncated SHA-256 of
  the canonical bytes. Read-only inspection (`ast_plan_show`, `edit show`) accepts
  an unambiguous prefix of at least 10 characters. **Apply and undo
  require the full id** (E-15): a 50-bit prefix is a convenience for reading, never
  an authority for writing. `recover` is the exception and takes no id at all: it
  converges whatever the journal store holds in a non-terminal state, so there is no
  single plan for a caller to name — which is why `TOOLS.md` §`ast_recover` lists
  its arguments as none. The example hashes above are abbreviated for
  readability; real ones are 64 hex digits.
- `format` is versioned. Unknown versions are refused, never guessed at.

### Plan store

Stored at `state/ws-<id>/plans/<plan-id>.json`, mode `0600`.

| Property | Default | Notes |
|---|---|---|
| Time to live | 15 minutes | Expired plans are refused and eventually deleted |
| Max plans per workspace | 100 | Preview refuses with `[limit_exceeded]` when full; only **expired** plans are ever deleted to make room |
| Max store size | 64 MiB | Same: refuse, never evict an unexpired plan |
| Per-caller quota | 25 unexpired plans per server process | Stops one agent session from filling the store |

A plan is *in use* from the moment an apply, undo or recovery has opened it until
that operation finishes; such a plan is never deleted, expired or not. An attacker
who can only call the preview tool cannot push a reviewed plan out of the store, and
cannot make a different plan resolve under the reviewer's prefix, because writing
needs the full id and plans are never silently replaced.

## Limits that bound a plan

| Limit | Default | Hard maximum |
|---|---|---|
| Files per plan | 50 | 500 |
| Edits per plan | 500 | 5,000 |
| Changed bytes per plan (inserted + removed) | 1 MiB | 8 MiB |
| File size eligible for editing | 4 MiB | 16 MiB |
| Total original bytes journaled per plan | 64 MiB | 128 MiB |
| Note length | 1 KiB | 4 KiB |

The journal cap is always at least twice the largest allowed plan's originals, so a
plan that passes preview can always be journaled (the journal total cap below applies
to *retained terminal* journals, never to one being written). Exceeding a limit is a `[limit_exceeded]` error at preview that says how to narrow
the request. The tool never silently truncates a plan.

## Preview

1. Validate arguments; resolve every path with the *read* policy; collect candidate
   files (respecting ignore rules and `max_scan_files`).
2. For each file: read (size-capped), verify UTF-8, compute `pre_hash`, ask the
   engine for the edit set and the new content's syntax-error count.
3. **Validate the edit set in the shell** (E-1) regardless of which executor ran.
4. Compute the new content in memory and its `post_hash`.
5. Evaluate the gates *in preview as well*, so a plan that would be refused at apply
   is refused now, with the reason.
6. Build the canonical plan, derive the id, store the plan, render the diff.
7. Return: plan id, per-file summary, the diff (bounded; with a truncation notice
   and a pointer to `ast_plan_show` for the rest), the risk summary, and the
   expiry time.

Preview is deterministic: the same workspace content and request (including the
note) produce the same plan id on every platform, because nothing time- or
build-dependent is hashed. This is tested. Files are deduplicated by **file
identity** (device and inode, or Windows file id), not by path spelling, so a case
alias or a second hard-link name cannot make one file appear twice with conflicting
edits. Everything shown to a human or an agent is rendered through the
[output sanitiser](TOOLS.md#output-sanitising).

### Risk summary

Every preview carries a short, machine-readable and human-readable summary so a
reviewer can see the size of the change at a glance: files, edits, bytes added and
removed, whether any file has pre-existing syntax errors, whether any file was
skipped (too large, not UTF-8, protected) and why.

## Gates

A gate decides whether new content may be written. Gates run at preview and again at
apply, on the bytes that would actually be written.

| Gate | Rule | Can an operator disable it? |
|---|---|---|
| `syntax` | `post_errors <= pre_errors` for every file (an edit may fix errors but may not add them) | No |
| `size` | Plan and file within [limits](#limits-that-bound-a-plan) | Tighten only |
| `path` | Every target is inside the boundary, a regular file, not protected | No |
| `encoding` | UTF-8 only; BOM, line endings and trailing newline preserved | No |
| `stability` | Applying the recorded edits yields exactly `post_hash` | No |

Gates are conservative by construction. A future operator-defined verifier (for
example "run the project's type checker") is out of scope for 1.0 because it means
executing code; verification beyond syntax is the caller's job, and `apply` returns
a list of changed files so a caller can run a language server's diagnostics.

## Apply

Apply is only reachable in write mode. It takes a plan id and nothing else.

```
 1  require write capability and a stored plan named by the id
 2  load plan; recompute id from canonical bytes → mismatch ⇒ [plan_corrupt]
    check format, workspace_id, expiry                        ⇒ [plan_expired] / [wrong_workspace]
    check the journal: already applied?                        ⇒ [already_applied]
 3  take the workspace apply lock (timeout)                    ⇒ [busy]
 4  recover any half-applied plan left by an earlier crash     (see Recovery)
 5  for each file, in sorted path order:
        resolve with the WRITE policy (inside root, regular file, not a link, not protected)
        open; verify handle identity; take the advisory file lock (timeout ⇒ [busy])
        read; hash == pre_hash ?                               ⇒ [stale_plan]  (nothing written yet)
 6  compute new content by applying the recorded edits; hash == post_hash ?   ⇒ [plan_corrupt]
 7  gates on every new content (engine parse of the new bytes)                ⇒ [gate_failed]
 8  journal(prepare): copy every original into the journal, fsync, write manifest(state=prepared)
 9  for each file: write temp file in the same directory (exclusive create, copy mode),
        write, fsync;
    then for each file in order: re-verify identity + pre_hash, rename temp over target
        manifest(state=writing, progress=n) updated durably as renames proceed
10  manifest(state=applied), fsync; fsync each parent directory (F_FULLFSYNC on macOS);
    delete temps; release locks
11  return the changed files and the suggested verification step
```

Steps 8–10 form a **non-cancellable section**: a client cancellation, stdin EOF or
SIGTERM received inside it is deferred until the section ends (success or complete
rollback). SIGKILL and power loss cannot be deferred; they are what recovery is for.
Locks and file identity are held on *directory handles and file handles*, and
renames are done relative to the already-verified parent directory handle
(`openat`/`renameat`-style via `rustix`, or `cap-std`), never by re-resolving a path
string, so swapping an intermediate directory for a link between verification and
rename has no effect. On Windows the target is opened without `FILE_SHARE_WRITE` and
replaced by handle, which closes the verify-to-rename window for cooperating and
non-cooperating writers alike; elsewhere the window is documented as residual.

**Targets must be plain files.** Step 5 refuses a target that has more than one hard
link (`[unsupported_target]`: replacing it would silently break the link) or that is
read-only for the user (the operator's intent is respected on every platform, not just
Windows). It also refuses targets whose extended properties cannot be preserved (see
below).

**Failure semantics.** Steps 1–8 can fail without having touched the workspace; the
result is always "nothing changed" plus a code. A failure in step 9 triggers
immediate rollback using the **same procedure as recovery** (below), so rollback also
classifies each file first and never overwrites content that is neither the post nor
the pre state. A rename that fails mid-apply (for example a Windows sharing
violation caused by an indexer or antivirus) is retried a bounded number of times
with backoff before rollback begins. If the process itself dies in step 9, recovery
performs the same rollback at the next opportunity.

**If rollback itself fails** (disk full, sharing violation, permissions), the journal
stays in state `writing`, the tool returns `[rollback_incomplete]` listing exactly
which files are in which state, and every later apply in the workspace is refused with
`[busy]` until `ast_recover` or `opencrayast doctor --recover` succeeds. The tree is
never reported as clean when it is not.

### Why rollback and not roll-forward

If a crash leaves some files changed and others not, finishing the job would
complete an operation the user may no longer expect, possibly long after review.
Restoring the original state is the only outcome a reviewer can reason about:
"nothing happened". The journal makes this always possible because the originals are
saved before the first target is touched (E-6).

### What atomic means here

Each file is replaced atomically. *Across* files the operation is made
all-or-nothing by the journal plus recovery, not by the filesystem: there is a
bounded window during which some files are new and some are old. That window is
visible only to other processes reading the tree at that instant, and it is closed
by the rollback if the apply does not complete.

### Preserving file properties

Mode bits (and ownership where permitted), BOM, line-ending style (LF/CRLF), the
presence or absence of a trailing newline, and — on Windows — attributes are carried
to the replacement file. Replacement text is adapted to the file's line ending and
to the indentation at the match site.

**Line-ending style is a classification, not a folded string.** A file is `LF`, `CRLF`, or
`Mixed` — anything that is not exactly one style, including a file that mixes LF with CRLF
and a file containing a lone `
`. A `Mixed` file has no single style, so replacement text
takes **the ending in force at its own match site**: the first line terminator after the
edit's start. Editing a mixed file therefore leaves it mixed, and an edit that would flatten
it is refused at the `encoding` gate rather than quietly rewriting the whole file. Folding
the two styles together before comparing them is not permitted: it makes "mixed" and "LF"
compare equal, which is a change of the property passing unnoticed.

Replace-by-rename drops some properties. The exact policy, so that "refuse rather
than silently change" is checkable:

| Property | Policy |
|---|---|
| Mode bits, BOM, line ending, trailing newline | Preserved |
| Ownership | Preserved where permitted; otherwise `[unsupported_target]` if it would change |
| Hard links (`nlink > 1`) | Refused |
| Read-only bit / `0444` | Refused |
| Windows attributes (hidden, system, archive) | Preserved |
| ACLs, extended attributes, SELinux labels | Copied when the platform API allows; if the target has any that cannot be copied, refused |
| Windows alternate data streams | Refused when the target has any |
| Symlinks | The final component being a symlink is refused |
| Junctions, reparse points (including cloud placeholders such as OneDrive) | **Planned — not implemented.** No reparse-point check exists on any platform today; see `TESTING.md` BND-06. |

A property that cannot be preserved is a refusal at preview, not a silent change.
Note that the *read* boundary is path-based: a hard link inside the workspace that
points at a file outside it passes path containment (see T-02 in the security model).

## Journal

`state/ws-<id>/journal/<plan-id>/`:

| File | Content |
|---|---|
| `manifest.json` | Plan id, state (`prepared`, `writing`, `applied`, `undoing`, `rolled_back`, `undone`), per-file pre/post hashes, progress, timestamps |
| `orig/<n>` | Original bytes of each file, mode `0600`; its hash is recorded in the manifest and **checked against `pre_hash` before it is ever written back** |

Retention: 7 days by default and a 256 MiB total cap; the oldest *terminal* journals
are evicted first. A journal in a non-terminal state is never evicted — it is
recovered. Eviction means undo is no longer possible for that plan, and the tool
says so.

## Undo

`ast_undo(plan_id)` (write mode):

1. Take the apply lock; load the journal; require state `applied` (or `undoing`,
   which resumes as recovery below).
2. For every file: resolve with the write policy, lock, read, compare its hash to
   `post_hash`; verify `orig/<n>` hashes to `pre_hash`.
3. If **any** file differs from `post_hash` (someone edited it since), refuse the
   entire undo with `[diverged]` and list which files diverged. Nothing is written.
4. Set the manifest to `undoing` (durably), write the originals back with the same
   atomic replace while recording progress, then mark the journal `undone`.

An interrupted undo is handled by recovery: files already restored equal `pre_hash`,
files not yet restored equal `post_hash`, and recovery completes the *undo* (the
direction the user asked for) or, if asked, rolls back to `applied`. Because both
directions are fully described by the journal, no crash leaves a state no tool can
repair (E-13).

A partial undo is available only through the human CLI (`--only <file>`), because it
is a decision about an inconsistent tree that should be made by a person.

## Recovery

`ast_recover` (write mode), the CLI's `doctor --recover`, and step 4 of every apply
run the same procedure over journals in a non-terminal state:

| Journal state at recovery | Action |
|---|---|
| `prepared` | Nothing in the workspace was touched; delete temps, mark `rolled_back` |
| `writing` | **Classify, then act.** Re-resolve every manifest path with the write policy (a path in a manifest is a request, not an authority). Classify each file as `post` (equals `post_hash`), `pre` (equals `pre_hash`) or `other`. If **any** file is `other`, change nothing and report `[diverged]` with the full classification for a person to resolve. Otherwise restore each `post` file from `orig/<n>` (after checking that original hashes to `pre_hash`) |
| `undoing` | Same classification; completes the undo, as described above |
| `applied`, `undone`, `rolled_back` | Terminal; nothing to do |

Classifying first is what makes E-8 true: a crash can never leave a mixture because
recovery refuses to start rewriting when it could not finish. Recovery is idempotent:
running it twice has the same effect as running it once, and a crash *during*
recovery leaves a state recovery can classify again.

## Concurrency

| Situation | Result |
|---|---|
| Two applies of the **same** plan at once | Exactly one succeeds; the other gets `[busy]` or `[already_applied]` |
| Two applies of **different** plans touching the same file | Serialised by the workspace lock; the second finds the file changed and returns `[stale_plan]` |
| A person edits a file after preview | Apply returns `[stale_plan]`; preview again |
| A person edits a file during apply | Detected by the per-file re-verification before rename (cooperating or not); the apply is rolled back. A swap inside the last microseconds before rename is the documented residual window |

## Error codes (edit)

`[stale_plan]`, `[plan_expired]`, `[plan_not_found]`, `[plan_corrupt]`,
`[wrong_workspace]`, `[already_applied]`, `[gate_failed]`, `[busy]`, `[diverged]`,
`[limit_exceeded]`, `[write_disabled]`, `[protected_path]`, `[unsupported_target]`,
`[rollback_incomplete]`, `[io_error]`. Each message
states what is true and what to do next; for example `[stale_plan]` names the files
that changed and says "preview again".

## Test obligations

Every invariant above has a named test in the
[test catalogue](TESTING.md#test-catalogue): E-1 → EDT-01; E-2 → EDT-02, EDT-03;
E-3/E-4 → EDT-15; E-5 → EDT-13; E-6/E-7/E-8 → EDT-09, EDT-10, EDT-19; E-9 → EDT-05;
E-10 → EDT-11, EDT-12; E-11 → EDT-03; E-13 → EDT-22; E-14 → EDT-23, EDT-24,
EDT-25; E-15 → EDT-26, EDT-27; E-12 → MCP-03 plus a compile-time check that
preview has no write capability. The two properties that matter most —
**all-or-nothing under interruption at every step** (EDT-09) and
**apply-then-undo restores the bytes exactly** (EDT-11) — are tests of the
parametrised, fault-injecting kind and run on all three operating systems.
