# Security audit, round 2

**Status:** the round-1 findings are all closed; the two surfaces round 1 never looked at have
now been probed. This document is the record of *what was found, what fixed it, and how you can
tell the fix is real* — it exists because the round-1 report had no baseline sha, so nothing in it
could be re-run.

**Baseline:** every row below is a claim about the tree at `3eec376`
(`3eec376b6aef54708bc1f62b311b9fc91aa4999d`, 2026-10-04). Re-run the commands in
[§4](#4-how-to-re-run-this) at any later sha; a row that no longer holds is a regression, and
the command that shows it is named in the row.

**Scope:** the write path, the path policy, the configuration boundary, and error rendering, as
seen through the three surfaces a user can actually reach — the library (`opencrayast-core`,
`opencrayast-edit`), the MCP server (`opencrayast-mcp`) and the CLI (`opencrayast`).

---

## 1. What round 1 found, and what closed each finding

Five findings. All five are closed, and each is closed by a **test that runs by default**, not by
a comment — the distinction is the whole reason this table exists, because a finding "fixed" only
in prose is the failure mode this project keeps producing.

| # | Finding | Aspect | Status | Fixed by | Regression pin |
|---|---|---|---|---|---|
| **F-01** | `fsio::atomic_replace` worked from **path strings** for milliseconds (create temp, write, `fsync`, `set_permissions`, `listxattr`) before its rename. A directory swapped in between let the write land outside the workspace. | 3 — TOCTOU (S-1) | **closed** | `0a80c70` — the write path is handle-relative: the primitive takes a `&Boundary`, resolves against a pinned root fd, and `O_NOFOLLOW`/`BENEATH` cover every component | `sec_audit_poc::f01_directory_swap_between_verification_and_rename_writes_outside` (promoted from a PoC in `5ef7465`), plus `core/src` unit tests `secfix1_01`…`secfix1_04` |
| **F-01b** | `fsio::atomic_replace` was `pub`, so any dependent could call it with a bare path and bypass the boundary entirely. | 3 | **closed** | `0a80c70` — the primitive is `pub(crate)` and takes a `&Boundary`; there is no bare-path entry point to call instead | `sec_audit_poc::secfix1_02_the_primitive_is_not_part_of_the_public_surface` |
| **F-02** | `write_enabled` was a plain `bool` field, so any caller inside the process could set it. The type system offered no resistance to "this code should not be able to write". | 2 — write policy (S-2, T-17) | **closed** | `c1598fb` (SEC-FIX 4) — write mode is a `WriteCap` that cannot be minted outside the crate that parses the operator's file | `sec_audit_poc::secfix4_01_a_caller_outside_the_crate_cannot_escalate_to_write`; CLI surface in `crates/cli/tests/sec_audit2_r2_cli_poc.rs` (R2-A2-05, R2-A2-07) |
| **F-03** | `path_max_bytes` and `path_max_depth` were **inert**: the resolver and the walker each built their own `Limits::default()`, so an operator's `[limits]` block — which `CONFIGURATION.md` documented as binding — bound nothing, silently. | 4 — resource limits | **closed** | `7e4627e` (SEC-FIX 5) — the operator's `Limits` travels in `BoundaryConfig` and `Boundary::limits()` is the one place that answers "what is the path ceiling" | `sec_audit_poc::f03_path_limits_ignore_the_configured_limits` (promoted from a PoC; no longer `#[ignore]`d), plus `crates/core/tests/path_depth_tunable_spec.rs` F-03-a/b/c and `path_limits_spec.rs` |
| **F-04** | `atomic_replace` reported the **target's** metadata in the error text — mode, hard-link count, file type. A caller who could only name paths could read that information back out of a refusal: a working metadata oracle. | 5 — error leakage (T-19) | **closed** | `c1598fb` / SECFIX1 — a refusal names the class of problem and nothing else; `outside_error()` is a constant with no path in it | `sec_audit_poc::secfix1_03_a_refusal_names_the_class_and_not_the_targets_metadata` and `secfix1_04_one_refusal_class_is_byte_identical_for_present_and_absent_targets`; **shell surfaces added in this round**, see §2 |
| **F-05** | `scripts/check-matrix.sh` compared identifiers between Markdown files and **never looked at the test tree**. A catalogue row promising a test nobody wrote passed. The gate's own comment claimed a semantic check it did not perform. | 6 — and the credibility of the matrix itself | **closed** | `7e4627e` (SEC-FIX 3) — every catalogue row's target is resolved on disk, and a `::test_fn` target is checked for actually carrying `#[test]` | `sec_audit_poc::f05_matrix_check_now_inspects_test_code`; the script's own reverse verification is `scripts/tests/check_matrix_spec.sh` |

### The honest caveat on "closed"

`F-01`'s fix was followed by a build break on Windows (`7cf4bc9`). That is recorded here rather
than left out because it is the shape of the problem: a write path that no longer works from
path strings has to be re-established from handles, and the platform where that is hardest is
the one with the thinnest CI at the time of writing. **Windows support for F-01's fix is
verified by CI on the three platforms (milestone M1), not by a test in this tree.**

---

## 2. What this round added: the two surfaces round 1 never probed

Round 1 reviewed `core` and `edit`. The guarantees it established are **core-level**, and both
shells render a `ToolError` through their **own** code:

- the MCP shell through JSON-RPC `content[0].text`, with its own `escape_inline` path;
- the CLI through `Op::diag`, with its own palette and its own escaping;
- and `doctor`, which renders refusals into a table with a third shape.

A guarantee does not survive three renderers on its own. Round 2 probed two of them.

| Aspect | Surface | Test | Result |
|---|---|---|---|
| 2 — write policy | CLI | `crates/cli/tests/sec_audit2_r2_cli_poc.rs` (R2-A2-04…08) | **clean.** `--write` plus `[policy] allow_write = true` are both required; partial sign-off and the missing-config case are refused. Written after F-02 was fixed, so R2-A2-05/07 are the fix's regression pins. |
| 5 — error leakage | MCP | `crates/mcp/tests/sec_audit_r2_aspect5_spec.rs::r2a5_01`/`02` | **clean.** The refusal for `/etc/hostname` (exists) and `/etc/no-such-file-4f2a9c` (does not) is **byte-identical**, and no refusal carries the absolute path, the mode, the link count, or the uid. |
| 5 — error leakage | CLI | `crates/cli/tests/sec_audit_r2_aspect5_spec.rs::r2a5_03`/`04` | **clean.** Same byte-identity, and the refusal is still *useful*: it names the class and a next step. Those are not in tension — the class and the next step are constants, the target is not. |

**Why byte-identity and not "does not contain the path".** A weaker assertion ("the message has no
absolute path in it") passes if someone later adds a *different* discriminator — an inode, a
timing-shaped wording difference, a distinct error code for `EACCES` versus `ENOENT`. Comparing
the two refusals as bytes catches any discriminator, including one nobody thought to forbid.

### One thing this round did NOT close, and says so

**Aspect 5 on `doctor`** is not probed. `doctor`'s refusals are rendered into a table with a
different shape again, and this round did not write a probe for it. That is a real gap in this
document's coverage and is listed in §3 rather than left implied.

---

## 3. Open items

| Item | What it is | Why it is open |
|---|---|---|
| **A5-OPEN-01** | Aspect 5 is not probed on `doctor`'s renderer. | `doctor` renders a `ToolError` into a `{verdict, name, detail}` line rather than through `Op::diag` or the MCP text path. The core guarantee reaches it only by inspection, not by test. |
| **A5-OPEN-02** | Windows support for F-01's handle-relative fix is verified only in CI. | No test in this tree can verify it; M1 is the evidence. |
| **A5-OPEN-03** | Aspect 1 (input validation) and aspects 3/4 on the **shell** surfaces were not re-probed in this round. | Round 2's ruling was to cover aspects 2 and 5 on `mcp`/`cli`. Aspects 1, 3 and 4 on those surfaces are covered indirectly by `mcp1_stdio_spec.rs`, `mcp_edit_plan_spec.rs` and the CLI specs, but not by a probe written for the audit's purpose. |

---

## 4. How to re-run this

Every row above is a claim someone can check. The commands, at any sha:

```sh
# F-01, F-01b, F-02, F-03, F-04, F-05 — the round-1 PoCs, all promoted, none ignored
cargo test -p opencrayast-edit --test sec_audit_poc -- --include-ignored

# The two shell surfaces this round added
cargo test -p opencrayast-mcp --test sec_audit_r2_aspect5_spec
cargo test -p opencrayast     --test sec_audit_r2_aspect5_spec

# F-02 on the CLI
cargo test -p opencrayast     --test sec_audit2_r2_cli_poc

# F-05: the matrix gate, and its own reverse verification
bash scripts/check-matrix.sh
bash scripts/tests/check_matrix_spec.sh

# F-03 end to end, from the operator's configuration file
cargo test -p opencrayast-core --test path_depth_tunable_spec --test path_limits_spec
```

The round-1 suite prints **`17 passed; 0 ignored`** at the baseline sha. **`0 ignored` is the
number to watch**: this project's history is a series of PoCs that stayed `#[ignore]`d after the
fix, which is a comment about the fix rather than a test of it. If that count is ever above zero,
read why before trusting any row in §1.

Build with `CARGO_BUILD_JOBS=8 RUST_TEST_THREADS=4`; the round-1 suite spawns child processes and
races eight threads at one file.