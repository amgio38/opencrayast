# Security model

This is the threat model for `opencrayast`. It names what is protected, who is
assumed to be hostile, every threat the design addresses, the mitigation, and the
**test that fails if the mitigation breaks** (the test identifiers are defined in
[`TESTING.md`](TESTING.md#test-catalogue)). It also states what is *not* protected,
because an honest model lists its residual risk.

Every row of that catalogue names a **Target** — a crate integration test file, a
function inside one that carries `#[test]`, or a CI step in a workflow — and
`scripts/check-matrix.sh` checks in CI two things about it: that it exists, and that it
is the *kind* of thing the column claims. So a threat whose mitigation has no test behind
it fails the build in four separate ways: it references an identifier that is not in the
catalogue, it names an identifier nothing references, it is a threat row naming no test
at all, or its catalogue row points at something that is not a test, or at a test that no
longer runs. The last one matters more than it looks: a function that has lost its
`#[test]` attribute is still a function, still compiles and is still lint-clean, so
nothing else in the build notices that cargo has stopped running it. The first version
of the script only compared identifiers between Markdown files and never opened a `.rs`
file, so the entire catalogue could have been deleted with CI still green; the Target
column exists because of that. `scripts/tests/check_matrix_spec.sh` guards the guard:
`SECFIX3-01` fails if the target check stops catching a deleted test, `SECFIX3-07` and
`SECFIX3-08` cover a target that is the wrong kind of thing, and `SECFIX3-06` disables
each check in a copy of the script and requires its own message to stop appearing — so
the suite fails if a check stops carrying its weight rather than merely being present.

Rows for milestones that have not landed yet carry the Target `-`, which the script
accepts only above the `LANDED_MILESTONE` watermark in the script itself (currently
`M4`). Rows at or below the watermark must name a test that exists. The watermark is
the honest boundary between "promised, must be there" and "scheduled, checked when its
milestone arrives".

The tool can **write to source trees on behalf of an AI agent**. That is a larger
trust decision than reading, and the design treats it that way: capability is
explicit, defaults are closed, and every write is reviewable and reversible.

## Assets

| Asset | Why it matters |
|---|---|
| Files in the workspace | Integrity (no unreviewed or unintended change) and confidentiality |
| Files outside the workspace | Must be neither read nor written through this tool |
| Secrets in or near the workspace (`.env`, keys, tokens) | Must not be written, and must not leak into logs |
| The user's account and machine | A parser or tool bug must not become code execution |
| Plans and journals | The integrity of "what was reviewed is what is written" and of undo |
| Availability of the host | The tool must not be a way to exhaust CPU, memory or disk |

## Adversaries

| ID | Adversary | Capability assumed |
|---|---|---|
| A1 | **A misled or prompt-injected agent** — the primary adversary | Can call any exposed tool with any arguments, repeatedly, and can be steered by text in files it reads |
| A2 | **Malicious repository content** | Controls the bytes of files, file names, symlinks and directory layout inside the workspace; any `.git`, config or "helper" files it ships |
| A3 | Another local user | Can see world-readable locations and pre-create predictable paths |
| A4 | **Supply chain** | Compromised dependency, grammar crate, CI action, release artifact or installer |
| A5 | A concurrent editor or tool (not malicious) | Changes files between our reads and writes |

Out of scope as adversaries: someone who already has code execution as the same
user (they do not need this tool), and a hostile operator (the person who launches
the server controls its configuration by design).

## Trust boundaries

```
 agent / MCP client ──(JSON-RPC over stdio)──►  shell  ──(data only)──►  engine
   untrusted input            validates         owns every          no authority:
   (A1)                       and bounds        side effect         no fs, net, env
                                 │
                                 ▼
              workspace files (A2: hostile content)   ·   state dir (user-private)
```

1. **Client → shell.** Everything an agent sends is untrusted: arguments are
   validated against schemas and limits; sizes are capped before parsing.
2. **Shell → engine.** Source text is untrusted. The engine receives bytes and
   returns data; the shell validates what comes back (ranges, counts) rather than
   trusting it.
3. **Workspace.** Contents, names and symlinks are untrusted. Nothing in the
   workspace can change the tool's behaviour (no repo-local configuration).
4. **State directory.** Trusted only if owned by the user with private permissions;
   verified on every start.

## Security invariants

These are the properties the rest of the design exists to preserve.

| ID | Invariant |
|---|---|
| S-1 | **No byte outside the boundary is read or written** through any tool. |
| S-2 | **No write to the workspace happens unless write mode is enabled**, and write code is unreachable without a capability value that only exists in write mode. |
| S-3 | **What is written is exactly what was previewed**: apply performs the recorded edits and verifies the resulting content hash before anything is committed. |
| S-4 | **A failed or interrupted apply leaves the workspace unchanged or recoverable to unchanged.** |
| S-5 | **Nothing from the repository or the agent is executed**: no shell, no build, no evaluation, no network. The only process the tool starts is its own parse worker (the same binary, started without a shell, with `execve` and everything else blocked once it is running). |
| S-6 | **The engine has no ambient authority when it runs in the isolated worker.** It cannot open files or sockets there. In-process execution (tests, and the fallback mode) provides only the *interface* guarantee that the engine is handed bytes and returns data; it is not an OS-enforced guarantee. (Today the isolated worker is planned, not shipped: all parsing is in-process, so this is the *interface* guarantee only. No tool reports which executor is active — `ast_info` emits five fixed lines and names no executor — so the reporting this invariant originally relied on does not exist and would have to be built with the worker.) |
| S-7 | **Every output is bounded** and says when it was truncated. |
| S-8 | **The tool never applies a change on its own initiative.** An apply is always a separate, explicit call on a stored plan. |
| S-9 | **What a reviewer sees is what is there.** Every rendering of file content, names and notes for a human or an agent escapes control, bidirectional and invisible characters, and flags their presence. |
| S-10 | **A reviewed plan cannot be swapped.** Writing requires the full plan id; stored, unexpired plans are never evicted or replaced. |

## Threats, mitigations and tests

Test identifiers refer to the [test catalogue](TESTING.md#test-catalogue). "M#" is
the roadmap milestone in which the mitigation lands.

### Filesystem and boundary

| ID | Threat | Mitigation | Tests | M |
|---|---|---|---|---|
| T-01 | **Path traversal**: `..`, absolute paths or encoded variants read or write outside the workspace | One `Boundary` for all access: reject empty/NUL paths, normalise lexically, require the canonical result to lie under the canonical root | BND-01, BND-02, BND-12, BND-16, BND-17 | M1 |
| T-02 | **Symlink / junction / hard-link escape**: a link (existing or planted by A2) points outside | Canonicalise every existing prefix and re-check containment; a write target's final component must be a regular file with a single hard link and not a link; Windows reparse points are refused if they leave the root. A hard link *read* cannot be told apart from a file by path alone, so S-1 for reads is a path-based guarantee; this is stated, and write targets with `nlink > 1` are refused | BND-03, BND-04, BND-05, BND-06, BND-21 | M1 |
| T-03 | **TOCTOU**: a path is swapped for a link between check and use | Open first, then verify the handle's identity (device+inode / file id) against what was checked; all later operations are relative to verified directory handles (`openat`/`renameat`-style), never to re-resolved path strings; re-verify immediately before each rename; open with no-follow and non-blocking flags and `fstat` to require a regular file | BND-07, BND-08, EDT-18 | M1, M4 |
| T-04 | **Aliasing**: case folding, Unicode normalisation, Windows 8.3 names, alternate data streams, reserved device names, trailing dots/spaces defeat the boundary or the protected list | Compare *canonical* forms from the OS; refuse streams and reserved names; protected-path matching is done on canonical, case-folded paths | BND-09, BND-10, BND-11, BND-13 | M1 |
| T-05 | **Writes to protected targets**: `.git/`, secrets, the state directory, the tool's own configuration | Built-in deny list (VCS directories, secret-like names, the state dir) checked after canonicalisation; extendable but not removable by arguments | BND-13, BND-14, BND-15, BND-19 | M1 |
| T-31 | **Special files and out-of-boundary helper files**: a FIFO or device node in the workspace blocks `open()` forever; a symlinked `.gitignore` is read from outside the root; ignore rules silently hide files so "0 matches" is hollow | Everything is opened through `Boundary` with `O_NONBLOCK`/`O_NOFOLLOW` and an `fstat` regular-file check (no second way to open a file, including ignore files); skipped files are counted and reported, never silent | BND-22, BND-23, OUT-07 | M1, M2 |
| T-20 | **Existence probing**: error messages reveal whether paths outside the workspace exist | Outside-workspace and not-found collapse to a uniform refusal for paths that fail containment; no absolute paths in output; every Windows spelling of "somewhere else" — drive, drive-relative, UNC, device, root-relative, alternate data stream — is refused as `outside_workspace` **lexically**, so none of them can be told apart from each other by which error comes back | BND-02, BND-11, BND-18, OUT-02 | M1 |

### Operator-tunable ceilings (LMT-08)

Exactly one limit is operator-widenable: **`path_max_depth`**. Every other limit in the
central table is a **resource ceiling** — bytes read, bytes returned, results, scan and parse
budgets, plan and journal sizes — and those are **fixed**: a value above the hard maximum is
**refused**, the configuration does not load, and the refusal names the field and the
maximum.

The split is deliberate and is the answer to "can an operator turn a ceiling off?":

| | Limits | Above the hard maximum | Can the ceiling be defeated? |
|---|---|---|---|
| **Tunable** | `path_max_depth` | **Clamped** to `PATH_MAX_DEPTH_HARD` (256); the file loads, the effective ceiling is 256, and the refusal names 256 | **No.** The clamp is applied at both enforcement points — the resolver's size check and the directory walk's depth ceiling — so a request above the ceiling is served the ceiling, not the request. |
| **Fixed** | every other limit | **Refused**; the shell does not start on the file | **No.** No configuration spelling raises one past its maximum. |

**Why depth may be widened.** Path depth is not a budget the operator spends — raising it
costs them nothing and bounds no new work. It is a statement about how deeply a legitimate
repository nests, and the operator who meets a pathological tree needs to widen it.

**Why the resource ceilings may not.** Those are the ceilings that bound actual work. They
are **refused rather than clamped** on purpose: a ceiling that silently became a smaller
ceiling is a guard the operator cannot see, and a shell that starts on a quietly reduced
ceiling is more dangerous than one that refuses to start and says why. Refusing is the
failure mode that keeps the guarantee visible.

**Clamping is observable.** The requested value remains in the parsed configuration, and the
*effective* value is read through `Limits::clamped_path_max_depth()` — the single function
the resolver and the walk both go through, so a message can never name one ceiling while a
check enforces another.

**Zero is refused for every limit, depth included.** Zero is not "unlimited": `0` would
refuse every path at the resolver and, at a check that compares against it, disable the
bound rather than apply it.

### Resource exhaustion

| ID | Threat | Mitigation | Tests | M |
|---|---|---|---|---|
| T-06 | **Oversized inputs**: huge files, plans, patterns or requests exhaust memory/disk | Central `Limits` with hard maxima; size checked before reading; non-UTF-8 and binary content refused | LMT-01, LMT-02, LMT-03, LMT-06, LMT-07, BND-20, LMT-08, LMT-09 | M1 |
| T-07 | **Parser CPU DoS**: deep nesting, giant tokens, pathological grammars | Budgets for size, depth, node count and wall-clock time, with cancellation | PRS-01, PRS-02, PRS-03, PRS-04, PRS-10 | M2 |
| T-09 | **Pattern / regex DoS** | Matching budget (visits, backtracking, time); regex constraints use a linear-time engine with a length cap | PAT-01, PAT-02, PAT-03, PAT-04 | M3 |
| T-10 | **Output flooding** burns the agent's context or the client | Hard output caps, result limits, explicit truncation notices | LMT-04, LMT-05 | M2 |
| T-25 | **Disk exhaustion** via many plans or journals | Quotas and TTL on the plan store (per server process as well as per workspace); journal retention limits; only expired plans and terminal journals are ever evicted | STA-06, EDT-16, EDT-27 | M4 |

### Parsing and memory safety

| ID | Threat | Mitigation | Tests | M |
|---|---|---|---|---|
| T-08 | **Memory corruption in the C parser** triggered by a hostile source file leads to code execution as the user | The engine is a pure function with no authority (S-6) and can run in a worker process with OS restrictions (rlimits/Job Object, Landlock/seccomp/AppContainer where available) and a pipe as its only channel; worker crashes and timeouts are contained and restarted with a bound, and **when the bound is reached the request fails closed, it never falls back to in-process parsing**. Guarantees are stated per executor and per OS (Linux: rlimits + seccomp including `execve` + Landlock; Windows: Job Object limits including memory + AppContainer where available; macOS: rlimits only until a Seatbelt profile is added, and `ast_info` says so). Tree-sitter cannot enforce node or depth budgets *during* a parse, so memory and time are bounded by the OS limit (`RLIMIT_AS` / Job memory limit) and wall clock, not by node counts | PRS-05, PRS-06, PRS-07, PRS-08, PRS-11, PRS-12 | M2 (engine), M6 (isolation) |
| T-27 | **Engine defect** returns out-of-range, overlapping or mid-character edits | The shell validates every edit set (bounds, order, character boundaries, limits) before a plan exists, whichever executor produced it | EDT-01, PAT-07 | M4 |
| T-29 | **Rewrite changes structure or drops content**: a substituted capture changes operator precedence (`$X*2` with `a+b`), comments between captures vanish, re-indenting alters multi-line strings or docstrings, a count-based syntax gate passes an edit that fixes one error and adds another | Captures are parenthesised where precedence requires it; string, template and comment leaves are never re-indented; dropped comments are reported in the risk summary; the syntax gate compares error *locations* in unchanged regions, not just counts | PAT-09, PAT-10, PAT-11, PAT-12, EDT-13 | M3, M4 |
| T-28 | **Wrong or non-reproducible matches**: the plan differs from what the reviewer's request meant, or differs between runs, so the reviewed plan is not the intended one | Differential testing against a reference matcher; fully deterministic ordering and edit sets (nothing time- or build-dependent is hashed, so plan ids are reproducible); invalid patterns fail loudly with guidance instead of matching something else | PAT-05, PAT-06, PAT-08 | M3 |

### Edit integrity

| ID | Threat | Mitigation | Tests | M |
|---|---|---|---|---|
| T-11 | **Plan tampering or forgery**: the stored plan is altered, or a fabricated plan is placed in the store | Plan id is the hash of the canonical plan; apply recomputes it; each file carries a pre- and post-content hash; plans are bound to a workspace id; the plan directory is private to the user; the hash covers the note and every reviewed field | EDT-02, EDT-03, EDT-15, EDT-17, EDT-20, EDT-30 | M4 |
| T-30 | **Reviewed plan swapped for another**: an agent with a shell grinds a note until a malicious plan shares the abbreviated id of a benign one, then floods the store so the benign plan is evicted and the human's `apply <prefix>` resolves to the malicious one | Write operations require the **full** id; abbreviations are read-only conveniences; unexpired plans are never evicted (refuse instead); the CLI shows the diff and asks for confirmation immediately before applying | EDT-26, EDT-27 | M4 |
| T-12 | **Stale / replayed / expired plans** | Pre-hash verified under lock; an applied plan is marked and a second apply is refused; plans expire | EDT-04, EDT-05, EDT-06 | M4 |
| T-13 | **Concurrency**: files change between preview and apply; two applies race | Per-workspace apply lock; per-file advisory locks in sorted order; re-verification just before each rename | EDT-07, EDT-08, EDT-21 | M4 |
| T-14 | **Partial application** after a crash, I/O error or full disk | Journal with originals written *before* any target is touched; temp-file-then-rename; recovery classifies every file first and rolls half-applied work back only when the whole set is consistent; undo is journaled the same way; originals are verified against `pre_hash` before being written back; a failed rollback keeps the journal open and blocks further applies | EDT-09, EDT-10, EDT-19, EDT-23, EDT-24, EDT-25 | M4 |
| T-15 | **Silent corruption**: a rewrite breaks syntax, mangles encoding, line endings or BOM | Syntax gate on the new bytes (no increase in syntax errors); UTF-8 only; mode, BOM, line endings, trailing newline preserved | EDT-13, EDT-14, LMT-06, PRS-09 | M4 |
| T-16 | **Undo destroys later work** | Undo requires each file to still equal its post-apply content; otherwise it refuses the whole operation; an interrupted undo is itself recoverable | EDT-11, EDT-12, EDT-22 | M4 |

### Capability, configuration and state

| ID | Threat | Mitigation | Tests | M |
|---|---|---|---|---|
| T-17 | **Unintended write exposure** | Read-only by default; write tools absent from `tools/list` and rejected as unknown tools; a user-level policy can forbid write mode; type-level write capability | MCP-01, MCP-02, MCP-03, CFG-03 | M5 |
| T-18 | **Configuration injection**: a hostile repository ships a config that enables writing or loosens limits | **No project-level configuration file is read** — neither shell looks for `.opencrayast.toml`, `.mcp.json` or anything else inside `--workspace`; configuration comes from flags, the user file, and built-in defaults. The one deliberate exception is that `--config PATH` is honoured **wherever it points**, including at a file inside the workspace: that is a product decision, not an oversight, and it is paid for with visibility rather than with a refusal — the file must still be `0600` and owned by the user, `ast_info` prints a `config:` line naming the file in force and which route chose it, and `doctor` prints it as its first check and **warns** when the file is inside the workspace | CFG-01, CFG-02, CFG-04, CFG-05 | M1, M5 |
| T-32 | **Repository steers the launch line**: a repository's client config (`.mcp.json`, `.vscode/mcp.json`) starts the server with `--allow-write` and a wider read root | **No project-level config is read by either shell** — only a user-level file (`--config`, else the documented user config path) — so a checked-in file can supply neither half. `--allow-write` has no effect unless the **user-level** file also sets `policy.allow_write = true` (default `false`); `--read-root` (repeatable, read-only, never writable — BND-19) is refused for `/`, a drive root, the home directory itself and key/credential directories, by the same `check_root` the workspace root goes through (CFG-07), and its roots come from the launch line rather than any file in the repository; `ast_info` and `doctor` report **which configuration file** is in force and which route chose it, and `doctor` warns when that file is inside the workspace. What is **not** reported, and was never implemented, is the per-setting provenance T-32 previously claimed — see `docs/CONFIGURATION.md` §Sources and precedence | CFG-06, CFG-07 | M5 |
| T-33 | **Workspace identity confusion**: one tree reached through different canonical roots (bind mount, `subst`, UNC vs drive letter) or nested workspaces get separate locks and journals, so two applies race | The workspace id is derived from the root's device and inode / file id (not only its path) and is 128 bits wide; overlapping workspaces are refused; network filesystems where advisory locks are unreliable are detected and write mode is refused there unless the operator opts in; a network, device or drive-relative root is refused on the **string** before canonicalisation, so naming a share never resolves a host or opens an SMB session (an NTLM challenge sent to a host the caller chose) | STA-07, STA-08, BND-24 | M1, M4 |
| T-34 | **Display deception**: valid UTF-8 content carries ANSI/OSC escapes, `\r`, bidi overrides (Trojan Source) or zero-width characters so a diff, search hit, file name, symbol name or note looks different from what it is | One output sanitiser for every human and agent rendering escapes control, bidi and invisible characters and flags their presence in the risk summary; diffs and match lines are always fenced; the CLI never writes raw file content to a terminal | OUT-04, OUT-05, OUT-06 | M2, M5 |
| T-19 | **Disclosure through logs and errors** | Logs contain codes, counts, durations and plan ids only; no source text or replacement text; log file `0600` | STA-04, STA-05, OUT-03 | M5 |
| T-21 | **State-directory abuse** by another local user (pre-created directory, symlink, readable plans) | State directory created `0700`; verified owner, mode and not-a-symlink at every start; files `0600`; refused and never repaired; ONE creator for it, so the apply lock cannot adopt what the stores refuse | STA-01, STA-02, STA-03, STA-11 | M1, M4 |
| T-21b | **The tool's own state read back as workspace content**: state inside the workspace is walked into, so plans, journals, undo backups, plan ids and before/after hashes are readable through an ordinary `ast_outline`/`ast_get` | State lives in the platform user-state directory, never in the workspace; the workspace-local name is skipped by the walker at any depth; `Settings::boundary_config` sets `state_dir`, so the "never a write target" guard is reachable in every production binary | STA-10, STA-12, BND-15 | M1 |
| T-25b | **Undo history destroyed as a side effect of unrelated work**: every `create` ran the retention pass unconditionally, so applying one plan silently reaped another plan's journal and answered a bare `plan_not_found` | The age pass on the `create` path is bounded by the store's size cap; retention proper is `opencrayast plan gc`, and `doctor` reports what it would remove | STA-13, STA-14 | M4 |

### Protocol and supply chain

| ID | Threat | Mitigation | Tests | M |
|---|---|---|---|---|
| T-23 | **MCP protocol abuse**: malformed JSON-RPC, oversized messages, request floods, cancellation races | Strict parsing, message size cap, bounded concurrency, safe cancellation; no panics on any input | MCP-04, MCP-05, MCP-06, MCP-07, MCP-08 | M5 |
| T-22 | **Supply chain**: dependency, grammar, CI action, release artifact or installer is compromised | `cargo-deny` and scheduled `cargo-audit` (`.github/workflows/audit.yml`; a GitHub `schedule:` trigger only fires on the default branch and is suspended in an inactive public repository, so it is a best-effort backstop, not a guarantee); committed lockfile and `--locked` builds; grammar crates pinned and listed with licences; Actions pinned with minimum permissions; releases only if every platform builds; installers verify checksums and archive contents | SUP-01, SUP-02, SUP-03, SUP-04, SUP-05, SUP-06, SUP-07, SUP-08 | M0, M7 |

### Residual and accepted risks

These cannot be eliminated by this tool. They are documented so an operator can
decide with open eyes.

| ID | Risk | Why it remains | What reduces it |
|---|---|---|---|
| T-24 | **Prompt injection through returned source text.** A comment in a file can say "ignore your instructions and …" and an agent may follow it. | The tool must return source text; it cannot tell instructions from data. | Outputs separate data from tool prose with stable delimiters (OUT-01); nothing is ever applied without an explicit apply call (S-8); apply is annotated destructive so clients can demand human approval; the human-review CLI path removes write capability from the agent entirely. |
| T-26 | **Harmful but well-formed edits.** An agent may be steered to insert a backdoor, or to copy a secret into a public file, with a perfectly valid plan. | Intent cannot be judged mechanically. | Plans are small, bounded and previewable (LMT-03); protected targets are refused (BND-14); read-only mode plus CLI apply puts a person on every write (MCP-01); operators should require client-side approval for apply. |
| T-03r | **A race against our final rename (Unix, macOS).** A process that can already write the workspace could swap a file in the microseconds between our last verification and the rename. | A pure-userspace tool cannot close that window against an attacker who already has local write access to the same files — and such an attacker does not need this tool. | Directory-handle-relative operations and re-verification immediately before each rename; on **Windows the window is closed** by opening targets without `FILE_SHARE_WRITE` and replacing by handle. |
| T-11r | **Same-user plan replacement.** Someone running as the same user can replace a stored plan *and* its id. | Everything running as the user is inside the trust boundary. | Plan id/hash, post-hash and the workspace binding protect against accidents and against other users, not against the user's own account being compromised. |
| T-08r | **Before the isolated worker ships (M6)**, the parser runs in the server process. | Isolation is a hardening milestone, deliberately ordered after the engine exists. | Pre-1.0 releases say so in their notes; budgets and fuzzing apply from M2; 1.0 requires isolation on by default where the OS supports it. |

## Operator guidance

- Prefer **read-only mode** and review plans with the CLI. Enable `--allow-write`
  only for a client that asks you to approve destructive tool calls.
- Run the server for **one workspace** at a time; do not point it at `/` or your
  home directory.
- Keep the user-level configuration file private. Never place configuration in a
  repository you do not control — it will be ignored, by design.
- Treat plan ids as references, not secrets; the plan store is private to your
  account, not encrypted.
- Review the diff, not the description. The description is text an agent wrote.

## Reporting

Vulnerabilities are reported privately; see [`../SECURITY.md`](../SECURITY.md).
