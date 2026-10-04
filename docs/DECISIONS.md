# Decisions

The architecture decision records (ADRs) for `opencrayast`, and the checklist that
defines 1.0. An ADR is short: the context, the decision, the alternatives we
rejected and why, and the consequences we accept.

## How decisions are made

- A decision that changes a **guarantee** (anything in
  [`SECURITY-MODEL.md`](SECURITY-MODEL.md) or [`EDIT-MODEL.md`](EDIT-MODEL.md)), the
  **crate layering**, the **plan or tool contract**, or the **dependency policy**
  needs an ADR *before* the code changes.
- ADRs are numbered, never deleted. A reversal is a new ADR that supersedes the old
  one, which stays with its status updated.
- Status: `Proposed` → `Accepted` → (`Superseded by ADR-n`). `Proposed` means the
  direction is chosen but a named spike or review must confirm it.
- Security-relevant ADRs get an independent adversarial review before they are
  `Accepted`; the reviewer's findings are summarised in the ADR.

---

## ADR-001: Rust

**Status:** Accepted

**Context.** The tool parses hostile text and writes to source trees. Memory safety
and a single dependency-free binary matter more than development speed. The sibling
project (`opencraylsp`) is Rust, and its boundary, installer and release machinery
are directly reusable.

**Decision.** Rust (edition 2024), `unsafe_code = forbid` workspace-wide. Isolating
OS-sandbox calls in one small crate remains the accepted design for when such calls
exist; **that crate is not implemented today** (planned with the M6 isolated worker)
and until it exists the forbid is absolute — there is no exception crate in the
workspace.

**Rejected.** Go (excellent for the tooling but a larger runtime and no `forbid
unsafe` equivalent; reuse of our Rust boundary lost). TypeScript/Python (runtime
dependency for users; weaker guarantees for the safety-critical core).

**Consequences.** tree-sitter's C core and grammars are a native dependency; ADR-004
addresses that.

---

## ADR-002: No daemon

**Status:** Accepted

**Context.** The sibling language-server MCP needs a shared long-running daemon
because language servers are heavy. Parsing with tree-sitter is cheap.

**Decision.** One stateless process per client over stdio. Shared state lives on disk
(plans, journals). No socket, no background process.

**Rejected.** A shared daemon for caching parse trees (a performance idea with a real
cost: a socket to authenticate, a long-lived process holding source from several
projects, and no native Windows).

**Consequences.** Slightly more re-parsing; no peer-credential problem; smaller
attack surface; native Windows follows naturally.

---

## ADR-003: Two-phase edits with persisted, content-addressed plans; apply never re-searches

**Status:** Accepted

**Context.** Agents change code; reviewers need to see exactly what will change; the
change must not differ from what was reviewed.

**Decision.** Edits are a *plan* (concrete byte-range replacements with pre/post
hashes) stored under an id derived from its canonical bytes. `apply` consumes only a
plan id and applies the recorded edits to files whose hash matches. Details in
[`EDIT-MODEL.md`](EDIT-MODEL.md).

**Rejected.**
- *Single-step "search and replace" tool* — nothing to review, no replay protection.
- *Re-running the pattern at apply time* — the result can differ from the preview
  (file changed, engine updated), defeating review.
- *Signed, self-contained tokens instead of a plan store* — more machinery for a
  local single-user trust model; the store plus content hash gives the needed
  integrity.

**Consequences.** A plan store with TTL and quotas to manage; previews must be
deterministic.

---

## ADR-004: Pure engine, privileged shell; isolated parse worker

**Status:** Accepted (design); implementation lands in M2 (engine) and M6 (worker)

**Context.** tree-sitter is a C library fed hostile files by design. A memory
corruption bug in a parser or generated grammar would otherwise run with the user's
authority, in a process that can write the workspace.

**Decision.**
1. The engine is a **pure function** from `(bytes, language, request, budget)` to a
   result. It has no filesystem, network or environment access, by construction of
   its API. The shell owns all side effects.
2. The engine can run **in-process** (tests, fast path, fallback) or in a **worker
   process** whose only channel is a pipe, with OS restrictions: resource limits
   everywhere; Job Objects on Windows; Landlock and a seccomp filter on Linux and
   AppContainer on Windows where available.
3. Budgets (size, depth, nodes, time) apply in both executors; the worker is killed
   and restarted on timeout or crash, with a restart cap.
4. The shell **validates engine output** (ranges, counts) rather than trusting it.
5. Default isolation is `auto`: the worker where the OS supports the restrictions,
   in-process with a watchdog otherwise, and `ast_info` says which.

**Rejected.**
- *In-process only* — leaves the C parser in the trusted base; unacceptable as the
  1.0 default where isolation is available.
- *Compile grammars to WebAssembly and run them in a Wasm runtime* — attractive
  because it makes memory corruption in a grammar non-exploitable without OS
  support. Deferred as a post-1.0 evaluation: it costs a runtime dependency and
  parse speed, and grammar tooling for it is still maturing. If the evaluation is
  favourable it supersedes the worker's OS-level restrictions, not the pure-engine
  split.
- *Serialise whole syntax trees across the pipe* — expensive and unnecessary: the
  worker returns results, not trees.

**Consequences.** A request/response protocol for the worker; a differential test
that both executors agree (PRS-07); an escape test per OS (PRS-06). Until M6 the
residual risk T-08r is real and is stated in release notes.

---

## ADR-005: Matching engine

**Status:** Accepted (2026-10-02) — purpose-built matcher, ast-grep-core as a differential oracle

**Context.** The pattern language ([`PATTERNS.md`](PATTERNS.md)) is ast-grep-like.
Two ways to implement it: depend on an existing Rust matcher library, or write a
purpose-built one over tree-sitter.

**Decision.** Build a purpose-built structural matcher over our pinned tree-sitter,
behind an internal trait. Use `ast-grep-core` only as a **test-time reference
oracle** for differential tests (PAT-05), never as a runtime dependency.

The criteria, in order (written before the spike), and how the evidence from the
[spike report](spikes/ADR-005-matching-engine.md) scored them:

1. **Licence** — ast-grep-core passes (MIT; the whole graph passes `cargo deny`).
2. **Budget control** — **fails.** `ast-grep-core` 0.45.3 has no match-step, node-visit
   or wall-clock budget inside its matcher; outer iterator caps do not bound
   backtracking in a single `$$$` match (SECURITY-MODEL T-09). The criterion says a
   matcher we cannot bound is rejected whatever its other merits.
3. **API stability** — 22 releases in 12 months across six minor lines, several of
   them tracking tree-sitter bumps.
4. **Determinism** — fine (document order), a stable post-sort is still ours to do.
5. **Diagnostics** — messages name the whole source, not a position; some broken
   fragments are accepted through parser recovery, so they do not fail closed.
6. **Dependency weight / coexistence** — works today (single tree-sitter 0.27), but
   one patch release earlier was irreconcilable: two tree-sitter versions cannot
   coexist in one build (`links = "tree-sitter"`), so a dependency's bump cadence
   would become our release constraint.

Criterion 2 alone decides it; criteria 3, 5 and 6 point the same way. A throwaway
prototype of the core matcher was about 170 lines and handled 20,000 matches under a
hard step budget in 0.7 s, so feasibility is not in question — productising (rule
combinators, per-language pattern preprocessing, exact rebinding semantics) is the
cost, and the differential tests are what pay for it.

**Oracle.** `ast-grep-core`, pinned to an exact version, may be a `dev-dependency` of
the crate that holds the differential tests while its tree-sitter requirement matches
ours; if a release stops matching, the differential suite moves to a separate,
non-workspace crate rather than blocking our builds.

**Consequences.** M3 includes the matcher itself, with budgets (visits, steps, wall
clock) and positioned `[invalid_pattern]` errors as first-class requirements. The
matcher sits behind a trait so a better engine could replace it later.

**Spike report:** [`docs/spikes/ADR-005-matching-engine.md`](spikes/ADR-005-matching-engine.md).

---

## ADR-006: Write capability is opt-in, absent from the tool list, and typed

**Status:** Accepted

**Decision.** Default read-only. `--allow-write` enables write mode unless a
user-level policy forbids it. In read-only mode the write tools are not listed and are
rejected as unknown. In code, write operations require a `WriteCapability` value that
can only be constructed when write mode is on, so a read-only code path cannot call
them by mistake.

**Rejected.** A runtime `if write_enabled` check inside each tool (one forgotten
check is a vulnerability); exposing the tools and refusing at call time (an agent
sees and plans around them; clients cannot tell that the server is read-only).

---

## ADR-007: No repository-local configuration

**Status:** Accepted

**Decision.** Configuration is read only from flags, environment variables and a
user-level file. A workspace cannot configure the tool. (Same decision as the
language-server sibling, for the same reason: an untrusted repository must not be
able to enable writing or loosen limits.)

**Consequences.** Per-project tuning needs the operator to set flags in the client's
server definition; `doctor` reports ignored workspace-local files.

---

## ADR-008: SHA-256 for content and plan hashes

**Status:** Accepted

**Decision.** `sha256` (RustCrypto `sha2`), written `sha256:<hex>`. Inputs are at
most a few megabytes, so speed is irrelevant; SHA-256 is universally available,
audited, and what users already use for checksums.

**Rejected.** BLAKE3 (faster, not needed, an extra dependency); a non-cryptographic
hash (unacceptable for a plan-integrity claim).

---

## ADR-009: Recovery rolls back, never forward

**Status:** Accepted

**Decision.** A half-applied plan is returned to the original state from the journal.
See [`EDIT-MODEL.md`](EDIT-MODEL.md#why-rollback-and-not-roll-forward).

---

## ADR-010: UTF-8 only

**Status:** Accepted

**Decision.** Files must be valid UTF-8 (with optional BOM). Other encodings are
refused with `[not_utf8]`. Editing bytes we cannot interpret is the kind of silent
corruption this project exists to prevent; support for other encodings is an explicit
future feature, not a fallback.

---

## ADR-011: Plan identifiers

**Status:** Accepted

**Decision.** `p-` + 26 characters of lowercase base32 (`a-z2-7`) from the first 128
bits of the SHA-256 of the canonical plan. Read-only tools accept an unambiguous
prefix of at least 10 characters; **every operation that writes requires the full
id.** (Amended by the design review, ADR-017: a 50-bit prefix can be ground offline
by an agent that can run code, so it must never authorise a write.)

---

## ADR-012: Companion, not a merge, with the language-server MCP

**Status:** Accepted

**Context.** `opencraylsp` promises that no tool ever writes a file. This project
writes.

**Decision.** Separate projects, no dependency in either direction. Their documents
link to each other. Verification after an edit is the *caller's* orchestration:
`ast_edit_apply` returns the changed files and suggests `lsp_diagnostics`.

**Rejected.** Merging (would break the read-only promise that makes the sibling easy
to trust); calling the language server from here (a socket client, a dependency, and
a failure mode inside the write path).

---

## ADR-013: Version scheme

**Status:** Accepted

**Decision.** `V0.YYYYMMDD.NNN` until 1.0, matching the sibling projects. Cargo does
not allow leading zeros, so the crate version is `0.YYYYMMDD.N`. Release tags are
`v0.YYYYMMDD.N` and the release workflow refuses a tag that differs from the in-code
version. At 1.0 the scheme becomes ordinary semantic versioning.

---

## ADR-014: Native Windows is a first-class target

**Status:** Accepted

**Decision.** Windows is supported natively from M0, not as a later port. This is
possible because ADR-002 removes the Unix-socket dependency; it requires explicit
handling of path aliasing (BND-11), junction escape (BND-06) and atomic replacement.
Windows CI runs the full suite, not a subset.

---

## ADR-015: Preview is available in read-only mode

**Status:** Accepted

**Decision.** `ast_edit_preview` writes only to the plan store in the state
directory, never to the workspace, so it is available without write mode. Together
with the CLI's `edit show` and `edit apply`, this gives a path where **an agent
proposes and a person applies**, and the agent's process never holds write
capability at all.

**Consequences.** `ast_edit_preview` is annotated `readOnlyHint = false` honestly
(it writes the plan store) with `destructiveHint = false` and `idempotentHint = true`.

---

## ADR-016: Publication and repository naming

**Status:** Accepted

**Context.** Design and early implementation happen in a private working
repository. The public project must not inherit private history, internal
references or working notes.

**Decision.** The product and the public repository are named **`opencrayast`**
(`github.com/amgio38/opencrayast`). Publication is a **clean export with a fresh
history**: the working tree is copied, scanned and committed as a new first commit
(author is the maintainer's public no-reply identity), after the pre-publication
checklist below. The working repository is never made public.

**Pre-publication checklist (reused from the sibling projects):**

Run `bash scripts/prepublish-scan.sh` then `bash scripts/clean-export.sh` before the
first push to `github.com/amgio38/opencrayast`. The working repository is never made
public.

| Item | Status (as of OSS-PUBLISH) | How checked |
|---|---|---|
| Secret scan of the export tree | **mechanised** | `scripts/prepublish-scan.sh` (pattern scan; optional `gitleaks` if installed) |
| No personal paths / internal refs / non-English in published sources | **mechanised** | `scripts/check-docs.sh` (export excludes `internal/`) |
| Authors / committer are the public identity | **on export** | `clean-export.sh` writes the initial commit as `amgio38 <amgio38@users.noreply.github.com>` |
| `LICENSE`, `deny.toml`, `THIRD-PARTY-LICENSES.md`, `cargo deny` | **mechanised** | `prepublish-scan.sh` + `cargo deny check` |
| Private vulnerability reporting enabled on GitHub | **post-push (human)** | `SECURITY.md` already points at GitHub private reporting; turn the repo setting on after the first public push |
| Repository URLs are the public ones | **in tree** | documents name `https://github.com/amgio38/opencrayast` and `opencraylsp` |
| CI green on three OS on the exported tree | **post-push (human / Actions)** | cannot be forged here; first proof is Actions on the public repo |

GitHub artifact attestation (`id-token`) is **deferred past 1.0** — see ADR-019. Absence
from `release.yml` is deliberate.

---

## ADR-019: Pre-publication product decisions (2026-10-04)

**Status:** Accepted

**Context.** Several open product questions were blocking an honest open-source
export. Leaving them as "ask the director" while the board was otherwise green made
the project look unfinished when the unfinished part was only decision debt.

**Decisions.**

1. **No GitHub artifact attestation before 1.0.** `release.yml` does not request
   `id-token`. Checksums (`SHA256SUMS`) remain the verified supply-chain gate for
   installers. Attestation may be revisited after a public repo exists.
2. **In-process fuzz-style suites are the delivered fuzz story.** A `cargo-fuzz`
   tree is not required for publication. Long libFuzzer campaigns stay a later
   hardening option (ROADMAP M6), not a lie in the current tree.
3. **Catalogue rows with Target `-` (deferred) do not fail CI** and do not block
   board closure. They mark work whose milestone has not landed. Policy text lives
   in [`TESTING.md`](TESTING.md#deferred-rows).
4. **`scripts/check-docs.sh` keeps refusing internal tracking codes** in published
   documents. Operator notes stay under `internal/`, which clean export deletes.
5. **The language-server companion is named publicly** as
   <https://github.com/amgio38/opencraylsp> in README and the integration docs.

**Consequences.** `internal/OPEN-DECISIONS.md` records the same five decisions in
Chinese for the private working tree; the public export does not ship that file.

---

## ADR-017: Outcome of the first adversarial design review

**Status:** Accepted

**Context.** Before any code, the whole design was reviewed by an independent model
instructed to attack it. It reported 12 ranked findings and a list of properties it
found sound.

**Decision.** All findings were accepted except as noted; each is now specified and
has named tests.

| # | Finding (severity) | Resolution |
|---|---|---|
| 1 | Undo not journaled; crash mid-undo unrecoverable (high) | `undoing` state; recovery completes undo; E-13, EDT-22 |
| 2 | Recovery/rollback can leave a mixture and overwrite others' work (high) | Classify-then-act; originals verified against `pre_hash`; failed rollback keeps journal open and blocks applies; paths re-resolved; E-14, EDT-23..25 |
| 3 | Human review path can show something other than what is applied (high) | Output sanitiser for every rendering, S-9, T-34, OUT-04..06 |
| 4 | Plan-store flooding plus prefix lets an agent swap a reviewed plan (high/med) | Full id for writes; never evict unexpired plans; per-process quota; CLI confirmation; E-15, S-10, T-30, EDT-26/27 |
| 5 | Hard links; properties lost by rename (med) | Refuse `nlink > 1`, read-only bit, ADS, uncopyable xattrs/ACLs, placeholders; property table; read-side limitation stated; BND-21, EDT-28 |
| 6 | Plan ids not reproducible as written (med) | Time and binary version moved to an unhashed envelope; note hashed; `engine_format`; EDT-30 |
| 7 | Ignore handling outside `Boundary`; FIFOs hang `open()` (med) | Ignore files read through `Boundary`; non-blocking open plus `fstat`; skips reported; T-31, BND-22/23, OUT-07 |
| 8 | Isolation weaker than claimed (med) | Per-executor, per-OS guarantees; fail closed after restart bound; memory limits; `execve` blocked; S-5/S-6 reworded; write mode needs a worker from M6; PRS-11/12 |
| 9 | Locks and workspace identity (med) | Identity-based 128-bit workspace id; overlapping roots refused; network FS detection; STA-07/08 |
| 10 | Rewrite-template claims wrong (med) | Parenthesise captures where needed; no re-indent inside strings/comments; dropped comments reported; location-based syntax gate; T-29, PAT-09..12 |
| 11 | Windows and filesystem details (med) | Handle-relative operations (`rustix`/`cap-std`); Windows closes the rename window; directory fsync / `F_FULLFSYNC`; bounded retry; non-cancellable write section; EDT-31 |
| 12 | Repository can steer config via client launch config (med) | `--allow-write` needs `policy.allow_write = true` in the user file (default false); read-root refusals; source of each setting reported; T-32, CFG-06/07 |
| 13 | Smaller inconsistencies (low) | Journal cap vs plan limits, hard maxima for every limit, base32 alphabet, read-root path labels, Windows ACL checks (STA-09), file-identity dedupe (EDT-29). Hook directories outside `.git` (`core.hooksPath`) are listed as a residual risk below |

**Notes on judgement.** Finding 5 says writing through rename is safe because the
directory entry is replaced; that is correct and the design keeps replace-by-rename,
but treats the broken link and lost properties as refusals rather than silent
changes. Finding 11's claim that Windows can close the rename race is adopted, which
narrows T-03r to Unix and macOS.

**Residual risk recorded:** a plan cannot be stopped from editing files that a Git
`core.hooksPath` points at when that directory is inside the workspace but outside
`.git`; reviewing the diff is the control (T-26).

---

## ADR-018: Adversarial review of the implementation, and what it changed

**Status:** Accepted

**Context.** ADR-017 records the review of the *design*, before code. This records
the review of the *implementation*, which happened afterwards and is the reason
several things in this repository look the way they do.

The baseline was `76da934`. The review ran in several passes by agents instructed to
attack the code rather than to agree with it, each reporting ranked findings with a
reproducible proof. What follows is not a summary of praise; it is the list of things
that were wrong, because that is the part a reader cannot reconstruct.

**Six findings, all now closed.** Each was closed by a test that runs by default —
the distinction matters here because this project's most repeated failure mode was a
finding marked "fixed" in prose while its proof-of-concept stayed `#[ignore]`d.

| # | What was wrong | Fixed by | What pins it |
|---|---|---|---|
| F-01 | `atomic_replace` worked from path strings for milliseconds before its rename, so a swapped directory let a write escape the workspace | `0a80c70` — the write path became handle-relative against a pinned root fd | `sec_audit_poc::f01_directory_swap_between_verification_and_rename_writes_outside` |
| F-01b | the same primitive was `pub`, so any dependent could bypass the boundary entirely | `0a80c70` — now `pub(crate)`, takes a `&Boundary` | `sec_audit_poc::secfix1_02_…` |
| F-02 | `write_enabled` was a plain `bool`, so nothing in the type system stopped a caller escalating to write | `c1598fb` — write mode is a `WriteCap` | `sec_audit_poc::secfix4_01_…`, `cli/tests/sec_audit2_r2_cli_poc.rs` |
| F-03 | `path_max_bytes` and `path_max_depth` were inert: the resolver and the walker each built their own `Limits::default()`, so an operator's documented `[limits]` bound nothing, silently | `7e4627e` — the operator's `Limits` travels in `BoundaryConfig` | `sec_audit_poc::f03_path_limits_ignore_the_configured_limits`, `core/tests/path_depth_tunable_spec.rs` |
| F-04 | a refusal reported the target's mode and hard-link count — a working metadata oracle for any path the process could name | SECFIX1 — refusals name the class and nothing else | `sec_audit_poc::secfix1_03_…`, `secfix1_04_…` |
| F-05 | `scripts/check-matrix.sh` compared identifiers between Markdown files and never looked at the test tree, while its own comment claimed a semantic check | `7e4627e` — every catalogue target is resolved on disk | `sec_audit_poc::f05_matrix_check_now_inspects_test_code` |

A second pass over the two *shells* found a gap the first had no way to see: the
guarantees above are core-level, and each shell renders a `ToolError` through its own
code. A guarantee does not survive three renderers on its own, so the MCP and CLI
renderers are now probed separately — see
[`security-audit-r2.md`](security-audit-r2.md), which carries the baseline sha and
the commands to re-run every row.

**Decision.** Two things, both about what a reviewer is owed.

1. **An audit report without a baseline sha is not a deliverable.** The round-1 report
   had none, which meant nothing in it could be re-run — it was a statement about a tree
   nobody could get back to. Every finding now names its fixing commit and its
   regression test, and the document names the sha it describes.
2. **`0 ignored` is a number worth watching.** The round-1 suite prints
   `17 passed; 0 ignored` at the current baseline. A proof-of-concept that stays
   ignored after its fix is a comment about the fix, not a test of it, and that was the
   most common way a finding here came to be marked closed.

**Consequences.** `docs/SECURITY-MODEL.md` rows T-18 and T-32 were rewritten to be
true rather than aspirational — including the admission that `--config` may point
inside the workspace, which is a deliberate choice paid for with visibility
(`ast_info` names the file in force, `doctor` warns) rather than with a refusal. The
release gate gained ART-01/ART-02 because the artefact was shipping statically linked
grammar code with no licence notices attached. And §3 of the audit document lists what
the second pass did **not** close, so "the review found nothing" cannot be read as
"the review covered everything".

---

## 1.0 checklist

Signed off by the maintainers before the version number becomes `1.0.0`:

- [ ] All milestones M0–M7 exit criteria met and recorded.
- [ ] Every threat in [`SECURITY-MODEL.md`](SECURITY-MODEL.md) has its tests passing on
      Linux, macOS and Windows; `scripts/check-matrix.sh` is green.
- [ ] Parse isolation is on by default wherever the OS supports it (ADR-004), and the
      escape tests (PRS-06) pass.
- [ ] An independent adversarial review of the whole project has no open high or
      medium findings; accepted risks are written down with reasons.
- [ ] Fuzz targets have run for the scheduled long campaign without findings.
- [ ] Tier 1 languages meet the Tier 1 definition on every platform.
- [ ] The tool catalogue and plan format are frozen and versioned; stored plans and
      journals from earlier versions have a defined migration or a clear refusal.
- [ ] Installers verified on clean machines, including upgrade; releases are
      all-platforms-or-nothing.
- [ ] The published token benchmark and agent task-suite results can be regenerated
      from the repository.
- [ ] Private vulnerability reporting is enabled and the response process has been
      rehearsed once.
- [ ] No CI job is permanently red or "informational".
