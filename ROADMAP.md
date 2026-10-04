# Roadmap

This is a production roadmap, not an MVP ladder. Every milestone ends with
**exit criteria that are checked, not asserted**, and no milestone is allowed to
ship a security-relevant component without its tests (see
[`docs/TESTING.md`](docs/TESTING.md)). Versions follow `V0.YYYYMMDD.NNN` until the
1.0 criteria below are met; Cargo cannot carry leading zeros, so the crate version
is written `0.YYYYMMDD.N`.

The order is chosen so that the riskiest and most load-bearing parts — the
boundary, the parser isolation, the edit engine — exist, are fuzzed and are
reviewed *before* the surface that exposes them to an agent.

## M0 — Foundation

Everything later is born under the full set of quality gates.

- Cargo workspace (edition 2024, MSRV pinned), `rust-toolchain.toml`, `rustfmt`.
- `clippy -D warnings`; `unsafe_code = forbid` workspace-wide today (see
  `[workspace.lints.rust]` in the root `Cargo.toml`). No crate is exempt. If an
  exception is ever required, it would be a separate, small, individually reviewed
  crate after an ADR — that crate does not exist yet (planned with the M6 worker).
- `cargo-deny` (licenses, advisories, sources, bans) on every push; `cargo-audit`
  on a weekly schedule, with the schedule's limits stated in the workflow itself.
- Layering check: the dependency direction in
  [`docs/ARCHITECTURE.md`](docs/ARCHITECTURE.md) is enforced by a script, not by
  convention.
- Docs check: links resolve, no personal paths, English only, no internal ticket
  references; the threat-to-test matrix has no orphan rows.
- CI on Linux, macOS and Windows; Dependabot; issue forms and PR template.

**Exit:** an empty workspace is green on three operating systems, and deliberately
breaking layering, licensing or formatting turns CI red (each demonstrated once).

## M1 — Boundary and core

The smallest, most-tested component in the project.

- Path policy: lexical normalisation, canonicalisation, symlink and junction escape
  checks, Windows aliasing rules, case-insensitive filesystems, protected targets.
- `Limits`, error taxonomy, content hashing, line index, UTF-8 policy.
- Filesystem primitives: write-through-temp-and-rename, preservation of
  mode/BOM/line endings, cross-platform advisory locks, identity checks that defeat
  swap races.
- Fuzz targets: path resolver.

**Exit:** the hostile-path matrix in `docs/TESTING.md` passes on all three
operating systems; the property "every input resolves inside the boundary or is
refused, and never panics" holds under a fuzzing campaign of at least one hour per
target in CI plus a long scheduled run.

## M2 — Language layer and read tools

- Language registry and Tier definitions; grammar pinning and supply-chain checks.
- Budgeted parsing (size, time, depth, nodes) behind a **pure engine interface**:
  bytes in, results out, no filesystem access (see ADR-004).
- Rust first, then TypeScript/JavaScript, Python, Go.
- `ast_outline`, `ast_get`, `ast_info`; deterministic, size-capped output.
- Token benchmark harness and the first published, reproducible measurement.

**Exit:** each Tier 1 language has golden outlines, syntax-error cases and
pathological-input cases; output caps hold for adversarial files; the benchmark
report can be regenerated from the repository.

**Backlog (not M2 exit):** honour `.ignore` and `.git/info/exclude` the same way as
`.gitignore` (BND-23 is `.gitignore`-only for M2).

## M3 — Pattern engine and search

- ADR-005 spike: reuse an existing matcher versus a purpose-built one, decided on
  the written criteria (licence, API stability, DoS control, diagnostics, upkeep).
- Pattern and rule language as specified in [`docs/PATTERNS.md`](docs/PATTERNS.md).
- Matching budgets (node visits, backtracking, wall clock) with clear errors.
- `ast_search`, `ast_explain_pattern`.

**Exit:** differential tests against a reference matcher agree; pathological
patterns and inputs terminate inside their budgets; the pattern parser is fuzzed.

## M4 — Edit engine

The core differentiator. Specified in [`docs/EDIT-MODEL.md`](docs/EDIT-MODEL.md).

- Plan model, canonical serialisation, content-addressed plan ids, plan store with
  TTL and quotas.
- Preview (rewrite and symbol edits), diff rendering, risk summary.
- Apply (locks, re-verification, gates, atomic multi-file write, journal), undo,
  crash recovery.

**Exit:** failure-injection tests interrupt apply between every pair of steps and
the workspace always ends fully applied or fully original; `apply` then `undo`
restores bytes exactly (property test); stale, replayed, forged and concurrent
plans are all refused (named tests).

## M5 — MCP server and CLI

- stdio MCP with strict input handling; tool catalogue per
  [`docs/TOOLS.md`](docs/TOOLS.md); correct tool annotations.
- Read-only versus write mode; write tools absent from `tools/list` when disabled.
- Human CLI sharing the same core: plan review with coloured diff, apply, undo,
  `doctor`.
- User-level configuration only; no repository-local configuration.

**Exit:** protocol conformance and golden transcripts pass; calling a write tool in
read-only mode is an *unknown tool* error (tested); malformed and oversized
requests never crash the server; CLI and MCP produce identical plans for identical
input.

## M6 — Hardening

- Process-isolated parse worker with OS-level restrictions (rlimits, Job Objects;
  Landlock/seccomp or AppContainer where available) — ADR-004.
- Long fuzzing campaigns on every fuzz target; corpus kept under version control or
  in a documented location.
- Cross-platform fidelity pass: Windows path rules, macOS case/normalisation.
- Performance budgets with regression guards.
- **Independent adversarial security review** of the whole design and code
  (not only the diff of the last milestone). Findings are fixed with regression
  tests, or recorded as accepted risk with a reason in `docs/DECISIONS.md`.

**Exit:** no open high or medium findings; budgets met; the worker cannot be made
to write a file in an automated escape test.

## M7 — Distribution and public preview

- Release pipeline: all platforms or no release; tag must equal the in-code
  version; minimal Actions permissions; pinned actions.
- Prebuilt binaries: Linux x86-64 and aarch64 (static), macOS x86-64 and arm64,
  Windows x86-64; checksums; evaluation of build provenance attestations.
- Installers (`install.sh`, `install.ps1`) following the hardened design already
  proven in the sibling projects: whole script inside `main()`, input validation,
  checksum verification, archive allow-list, UTF-8 BOM for Windows PowerShell 5.1,
  no `sudo`.
- Agent guide finished; measured agent task suite published; configuration
  reference complete.
- Publication: the public repository is `github.com/amgio38/opencrayast`, created
  from a clean export with a fresh history (ADR-016); the working repository stays
  private.
- First public pre-release.

**Exit:** a clean-machine install-and-use run on each platform, including upgrade;
installer tests cover truncation, tampering and hostile archives.

## M8 — 1.0

- API and wire-format freeze for the tool catalogue and the plan format
  (versioned; migrations for stored plans/journals defined).
- Stability and deprecation policy published.
- Security process rehearsed: private reporting enabled, response targets stated,
  one dry-run advisory.
- All Tier 1 languages meet their Tier 1 definition on all three operating systems.
- No "informational" CI jobs that are red.

**Exit:** the 1.0 checklist in `docs/DECISIONS.md` is complete and signed off by the
maintainers.

## Non-goals

These are deliberate and will not be added to 1.0:

- **Semantic analysis.** No type inference, no cross-file reference resolution, no
  semantic rename. A language server does that; use one alongside.
- **Executing anything.** No shell, no build, no test run, no code evaluation, no
  network access — including "verify by compiling".
- **Deciding what to change.** The tool executes a change an agent or a person has
  already decided on, safely. It does not generate changes.
- **Guaranteeing that edited code compiles.** It guarantees the result still parses
  and that the edit is exactly the reviewed one; compilation is verified outside.
- **A daemon.** Parsing is cheap and a daemon is an attack surface with no payoff
  here.

## Ideas after 1.0 (not commitments)

- Sandboxed grammars (running tree-sitter grammars as WebAssembly) to remove the C
  parser from the trusted computing base — evaluated in ADR-004.
- More languages (Tier 2 candidates: Java, C, C++, C#, PHP, Ruby, Kotlin).
- Language-aware helpers: add/remove import, move a symbol between files.
- Optional pluggable verifier hooks configured by the operator (never by a
  repository).
- Signed releases and SLSA-style provenance.
