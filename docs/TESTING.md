# Testing

The rule of this project: **a guarantee written in the documentation without a test
that fails when the guarantee breaks is a bug in the documentation.** This document
defines the kinds of tests, lists every test that the security model and the edit
model depend on, and describes how token savings are measured honestly.

## Kinds of tests

| Kind | Tooling | What it is for |
|---|---|---|
| **Unit** | `cargo test` | Pure logic with fixed inputs |
| **Property** | `proptest` | "For all inputs…" claims: containment, round trips, determinism |
| **Golden** | stored outputs compared byte for byte | Outlines, diffs, MCP transcripts, plan ids |
| **Differential** | two implementations, same input | In-process vs worker; our matcher vs a reference matcher |
| **Fuzz (planned, not yet in the repo — there is no `fuzz/` tree)** | `cargo-fuzz` (libFuzzer) | Path resolver, pattern parser, parse wrapper, plan deserialiser, MCP message parser; the wrapping functions each in-process target names are specified in "Relationship to `cargo-fuzz`" below |
| **Fuzz-style (in-process)** | `tests/common/fuzz.rs` + `*_hostile_spec.rs` | The same code paths those future libFuzzer targets are specified to cover, deterministic and seeded, run by plain `cargo test` on every machine |
| **Fault injection** | a failpoint at every step of apply | All-or-nothing under interruption, I/O error, full disk |
| **Adversarial / hostile-input** | purpose-built corpora | Symlinks, junctions, reserved names, pathological source, hostile archives |
| **End to end** | real binaries, fake client | The two shipped programs, over real stdio, on a real filesystem |
| **CI check** | scripts | Layering, licences, docs, the matrix below |

### Conventions

- Every security-relevant check has a **negative test**: with the check removed, a
  named test goes red. We verify this by deliberately removing the check once per
  milestone (mutation by hand), and record it in the milestone's review notes.
- No test depends on the network, on the developer's home directory, or on a
  specific absolute path. Temporary directories are created per test.
- Tests run on **Linux, macOS and Windows** in CI. A test that cannot run on an OS
  says why in code and is listed in `docs/TESTING.md` with its replacement. There is
  no permanently red or "informational" job.
- Fuzz targets, once they exist, run for a short, fixed time on every pull request and for
  hours on a schedule; any crash input is committed to the regression corpus as a normal
  test. There is no `fuzz/` tree in the repository yet, so nothing in this row runs today.
- The fuzz-style suite below runs on every `cargo test`, needs no extra tooling, and
  covers the same code paths the future `cargo-fuzz` targets are specified to cover (the
  wrapping function for each is named in "Relationship to `cargo-fuzz`"), so a regression
  is caught by the ordinary test run rather than only by a scheduled fuzz job.
- Floating or time-based assertions use injected clocks; nothing sleeps for
  correctness.

## Test catalogue

The identifiers below are referenced from
[`SECURITY-MODEL.md`](SECURITY-MODEL.md) and [`EDIT-MODEL.md`](EDIT-MODEL.md).
"Kind" uses the names above; "M" is the milestone in which the test must exist.

**Target** names the thing that makes the row true, and
`scripts/check-matrix.sh` verifies it exists. A Target is one of:

| Form | What the script checks |
|---|---|
| `crates/<crate>/tests/foo_spec.rs` | that file exists **and** is a crate integration test (nothing under `src/`, no directory, no document) |
| `crates/<crate>/tests/foo_spec.rs::some_test` | as above, **and** that file has a function `some_test` that **carries `#[test]`** |
| `ci:.github/workflows/ci.yml::<text>` | that file is a workflow under `.github/workflows/` and contains `<text>` (for obligations that are a CI step rather than a `cargo test`) |
| `-` | deferred: no test yet |

### Deferred rows

A Target of `-` means **no test exists yet for a milestone that has not landed**
(typically M5/M6 hardening or later). `scripts/check-matrix.sh` treats those rows as
`deferred` and **does not fail CI** because of them. That is a product policy
(ADR-019), not a loophole: deferred rows are allowed so the catalogue can name future
obligations without pretending they already pass.

Rules that still apply:

- A deferred row must name a milestone. "Sometime" is not a milestone.
- When that milestone is claimed done, the `-` must be replaced by a real Target, or
  the row must be deleted with a reason in the commit message.
- Threats that are in force *today* must not use `-`.

A `::name` target must name a function that carries `#[test]`, not merely a function with
that name. An earlier version matched the attribute optionally, so replacing
`#[test] fn x` with `#[allow(dead_code)] fn x` — same function, no longer run by cargo,
still lint-clean — left this file green. A test that has quietly stopped running is worse
than a missing one, because the catalogue says it is there.

The script fails CI when a threat references an identifier that is missing here, when an
identifier here is referenced by nothing, when a threat row names no test at all, and
**when a Target does not exist on disk**. That last check is the one that matters: it is
what makes this catalogue falsifiable. Deleting or renaming a test file, or adding a row
whose test was never written, is a non-zero exit. (Before this check existed the script
only compared identifiers between Markdown files and never opened a `.rs` file, so the
whole catalogue could be deleted and CI stayed green — SEC-A1 finding F-05.)

`-` is legal only for a row whose milestone is **above** `LANDED_MILESTONE` in
`scripts/check-matrix.sh` (currently `M4`). At or below that milestone a row promises a
test and the script insists on finding it. The watermark is stated in exactly one place,
in the script, so this document and the script cannot disagree about what "already
landed" means.

### Rows whose milestone was corrected

Seven rows claimed a milestone that has already landed while no test existed for them.
Rather than leave the claim standing, their milestone was moved to the next open one and
the Target is `-`. This is a correction to what the model *claims*, not a description of
what the code does, and it is recorded here so the change is visible rather than silent:

| Obligation | Was | Now | Why |
|---|---|---|---|
| `BND-06` | M1 | M6 | Windows junction / reparse-point escape: no test exists on any platform |
| `BND-09` | M1 | M6 | case variants resolving to the canonical file: `resolve_read` does no case folding, so on a case-sensitive filesystem a variant is simply absent; the protected-list half of the claim is covered by `protected_extra_spec.rs::secret_matching_is_case_folded` |
| `BND-09` | M1 | M6 | case variants resolving to the canonical file: `resolve_read` does not case-fold, so the half that is testable anywhere is the protected-list fold, already anchored in the Anchors table; the other half needs a case-insensitive filesystem |
Five obligations deliberately resolve to a whole file rather than a single function,
because no one test covers the row on its own: `BND-11` (Windows alias spellings),
`LMT-04` (output caps under adversarial input), and `BND-17` / `PRS-08` / `PAT-04` (the
seeded in-process equivalents of the libFuzzer targets, which have no `fuzz/` tree yet).
Deleting any of those files is still a non-zero exit; deleting a single function inside
one of them is not. Every other row names a specific `#[test]` function.

### Rows whose Kind was corrected to match their evidence

A row audit found `Kind` cells claiming a dimension their cited test could not
demonstrate. `check-matrix.sh` now enforces three rules (`KIND-01..05` in
`scripts/tests/check_matrix_spec.sh`): a `per language` row must reach every language in
`Language::all()`; a `property` row must be quantified by at least one cited test; and a
cross-platform claim needs either a `cfg`-gated test or a `ci:` target naming a multi-platform
run. Four rows were made true rather than narrowed — `PAT-06`, `PAT-09`, `PAT-10` and `EDT-13`
each gained real cases — and six rows were corrected instead, because their evidence was a
fixed example and no test was going to be written for them here: `PAT-07`, `EDT-11`, `EDT-30`,
`OUT-01`, `OUT-02` and `OUT-04` changed `property` to `golden` and had "never"/"always"/"every"
removed from what they prove. A claim was not weakened to pass a gate; each was corrected to
say what its test actually shows.

`PAT-06` is worth spelling out, because "across operating systems" is the one dimension no
in-process test can supply. `query::pattern::search` takes `&str`, so nothing about the host
reaches the code under test, and no Linux-only test can vary the OS it runs on. The claim is
therefore split: repeated-run determinism and platform-convention invariance (line endings, BOM,
Unicode form, path separators) are shown in-process by
`crates/edit/tests/pattern_platform_invariance_spec.rs`, and the cross-platform half is the same
tests RUN ON THREE RUNNERS, cited as `ci:.github/workflows/ci.yml::cargo test --workspace`.

### Boundary (`BND`)

| ID | What it proves | Kind | M | Target |
|---|---|---|---| --- |
| BND-01 | `..` traversal (plain, nested, mixed separators, encoded) is refused for read and write | unit+golden | M1 | `crates/core/tests/boundary_spec.rs::bnd01_traversal_refused` |
| BND-02 | Absolute paths outside the root (including `/`, another drive, UNC) are refused | unit | M1 | `crates/core/tests/boundary_spec.rs::bnd02_absolute_outside_refused_absolute_inside_ok` |
| BND-03 | A symlink to a file outside the root is refused for read and for write | adversarial | M1 | `crates/core/tests/boundary_spec.rs::bnd03_symlink_to_outside_file_refused` |
| BND-04 | A symlink to a directory outside the root, used as a path component, is refused | adversarial | M1 | `crates/core/tests/boundary_spec.rs::bnd04_symlinked_directory_component_refused` |
| BND-05 | Symlink loops and long chains terminate with a refusal, no hang | adversarial | M1 | `crates/core/tests/boundary_spec.rs::bnd05_symlink_loops_terminate` |
| BND-06 | Windows junctions and reparse points that leave the root are refused | adversarial (Windows) | M6 | `-` (no test yet; milestone corrected M1 -> M6, see below) |
| BND-07 | A path swapped for a symlink after the check but before use is caught by the identity check | adversarial/race | M1 | `crates/core/tests/boundary_spec.rs::bnd07_path_swapped_for_symlink_after_resolve_is_caught` |
| BND-08 | A file replaced between open and rename makes apply refuse | fault injection | M4 | `crates/core/tests/fsio_harden_spec.rs::target_replaced_with_a_new_inode_is_refused` |
| BND-09 | On case-insensitive filesystems, case variants resolve to the canonical file and cannot bypass the boundary or protected list | adversarial | M6 | `-` (no test yet; milestone corrected M1 -> M6: the protected-list half is covered by `crates/core/tests/protected_extra_spec.rs::secret_matching_is_case_folded`, but `resolve_read` does no case folding of its own - on a case-sensitive filesystem a variant is simply absent - so the "resolves to the canonical file" half needs a case-insensitive filesystem) |
| BND-10 | Unicode normalisation differences (NFC/NFD) cannot bypass the boundary or protected list | adversarial | M1 | `crates/core/tests/boundary_spec.rs::bnd10_normalisation_variants_cannot_escape` |
| BND-11 | Windows aliases are refused: reserved device names, trailing dots/spaces, alternate data streams, 8.3 short names, `\\?\` forms. The name rules are a pure string function unit-tested on **every** platform (so the table is provable on Linux) and *consulted* only under `cfg!(windows)`, because `con.go` and `aux.rs` are ordinary Unix files; on Windows each class answers `outside_workspace`, never `io_error` | adversarial (Windows) + unit (all platforms) | M1 | `crates/core/tests/core_windows_name_hazard_spec.rs::a_name_windows_cannot_name_answers_outside_workspace_and_never_io_error` |
| BND-12 | Empty paths, NUL and control characters are refused | unit | M1 | `crates/core/tests/boundary_spec.rs::bnd12_empty_nul_control_refused` |
| BND-13 | Protected targets (`.git/**`, VCS dirs) are refused, including via symlink, case variants and nested paths | adversarial | M1 | `crates/core/tests/protected_spec.rs::vcs_dirs_at_any_depth_and_case` |
| BND-14 | Secret-like file names are refused for write; configured extras apply; built-ins cannot be removed | unit | M1 | `crates/core/tests/protected_spec.rs::secret_like_names` |
| BND-15 | The state directory and the tool's configuration file are never writable targets | unit | M1 | `crates/core/tests/boundary_spec.rs::state_dir_is_never_a_write_target` |
| BND-16 | Property: any string resolves to a path inside the root or is refused, and never panics | property | M1 | `crates/core/tests/core_hostile_spec.rs::resolve_read_never_panics_and_never_answers_from_outside` |
| BND-17 | Fuzz target: the path resolver | fuzz | M1 | `crates/core/tests/boundary_prop.rs` |
| BND-18 | Refusals do not distinguish "does not exist" from "exists but outside" for outside paths | unit | M1 | `crates/core/tests/boundary_spec.rs::bnd18_outside_and_missing_look_the_same` |
| BND-19 | Read roots never grant write | unit | M1 | `crates/core/tests/boundary_spec.rs::bnd19_read_roots_never_grant_write` |
| BND-20 | Over-long paths and deep nesting are refused cleanly | unit | M1 | `crates/core/tests/boundary_spec.rs::bnd20_overlong_and_deep_refused` |
| BND-21 | A workspace hard link to a file outside the root is refused as a write target; the read-side limitation is documented and checked by `doctor` | adversarial | M1 | `crates/core/tests/boundary_spec.rs::bnd21_hard_linked_write_target_refused` |
| BND-22 | A FIFO, socket or device node in the workspace is skipped without blocking and reported | adversarial (Unix) | M1 | `crates/core/tests/boundary_spec.rs::bnd22_fifo_does_not_block` |
| BND-23 | A symlinked `.gitignore` pointing outside the root is not read; ignore files go through the Boundary (M2 scope: `.gitignore` only; `.ignore` and `.git/info/exclude` are backlog) | adversarial | M2 | `crates/core/tests/walk_spec.rs::bnd23_a_symlinked_gitignore_pointing_outside_is_not_read` |
| BND-24 | A network, device or drive-relative path is refused as a workspace / read root **before** any filesystem call, so canonicalisation never resolves a host or opens an SMB session; the refusal is the same one message for every spelling. The verbatim *disk* form `\\?\C:\x` is the exception and must be allowed, since it is a local volume and is what canonicalisation itself returns on Windows; `\\?\UNC\…`, `\\?\GLOBALROOT\…`, `\\?\Volume{…}\…` and `\\.\…` are not local disks and stay refused | unit + adversarial | M1 | `crates/core/tests/core_network_paths_spec.rs::a_network_root_is_refused_before_the_filesystem_is_consulted` |

### Limits (`LMT`)

| ID | What it proves | Kind | M | Target |
|---|---|---|---| --- |
| LMT-01 | A file over the size limit is refused without being read in full | unit | M1 | `crates/core/tests/text_props_spec.rs::size_is_checked_before_validity_and_boundary_is_inclusive` |
| LMT-02 | An oversized request message is refused before parsing | unit | M5 | `crates/core/tests/limits_spec.rs::defaults_match_the_documentation_and_validate` |
| LMT-03 | A plan over any plan limit is refused at preview with guidance | unit | M4 | `crates/edit/tests/edit1_extra_spec.rs::edit1_03_list_form_limit_exceeded` |
| LMT-04 | Output caps hold for adversarial inputs (huge outlines, many matches) and truncation is announced with counts | property | M2 | `crates/tools/tests/read_tools_spec.rs` |
| LMT-05 | Directory walks stop at `max_scan_files` and say so | unit | M2 | `crates/core/tests/walk_extra_spec.rs::k_max_files_cuts_the_sorted_front_exactly` |
| LMT-06 | Non-UTF-8 content is refused with `[not_utf8]` (read and edit) | unit | M1 | `crates/core/tests/text_spec.rs::decode_ok_and_errors` |
| LMT-07 | A binary file named like source is refused or reported, never mis-parsed into edits | unit | M2 | `crates/edit/src/spec/lmt07_binary_edit_spec.rs::lmt07_apply_refuses_a_binary_file_named_like_source` |
| LMT-08 | `path_max_depth` is operator-tunable but bounded by a compiled ceiling | unit | M1 | `crates/core/tests/path_depth_tunable_spec.rs::f03_b_an_above_ceiling_depth_is_clamped_to_the_compiled_ceiling` |
| LMT-09 | No resource ceiling (bytes, output, results, plan size) can be raised past its maximum by any configuration | unit | M1 | `crates/core/tests/path_depth_tunable_spec.rs::sec_a_resource_ceilings_above_their_maximum_are_refused` |

### Parsing and engine (`PRS`)

| ID | What it proves | Kind | M | Target |
|---|---|---|---| --- |
| PRS-01 | Pathologically deep nesting terminates within the depth budget | adversarial | M2 | `crates/lang/tests/sec2_parse_budget_hostile.rs::sec2_01_depth_budget_stops_pathological_nesting` |
| PRS-02 | A single huge token or very long line terminates within budget | adversarial | M2 | `crates/lang/tests/sec2_parse_budget_hostile.rs::sec2_02_size_budget_refuses_oversized_source` |
| PRS-03 | A file that yields millions of nodes is stopped by the node budget | adversarial | M2 | `crates/lang/tests/sec2_parse_budget_hostile.rs::sec2_03_node_budget_stops_many_small_nodes` |
| PRS-04 | The wall-clock parse timeout cancels a parse | unit | M2 | `crates/lang/tests/sec2_parse_budget_hostile.rs::sec2_04_timeout_cancels_via_zero_deadline` |
| PRS-05 | A worker that crashes or is killed does not take down the server; the request fails with a clear code | fault injection | M6 | `-` (no test yet) |
| PRS-06 | The worker cannot open a file, create a file, or make a network connection (attempted from inside the sandbox) | adversarial (per OS) | M6 | `-` (no test yet) |
| PRS-07 | In-process and worker executors return identical results for the same requests | differential | M6 | `-` (no test yet) |
| PRS-08 | Fuzz target: the parse wrapper and engine request decoder | fuzz | M2 | `crates/lang/tests/lang_hostile_spec.rs` |
| PRS-11 | After the restart bound is reached, requests fail with `[worker_unavailable]`; the server never falls back to in-process parsing | fault injection | M6 | `-` (no test yet) |
| PRS-12 | A parse that allocates without bound is killed by the memory limit (`RLIMIT_AS` / Job memory limit) and the server survives; the worker cannot `execve` | adversarial (per OS) | M6 | `-` (no test yet) |
| PRS-09 | Source with syntax errors is handled and the error count is reported honestly (golden per language) | golden | M2 | `crates/lang/tests/parse_golden_spec.rs` |
| PRS-10 | Repeated worker failures are bounded (restart limit, then a clear error) | fault injection | M6 | `-` (no test yet) |

### Patterns (`PAT`)

| ID | What it proves | Kind | M | Target |
|---|---|---|---| --- |
| PAT-01 | Pathological patterns terminate within the match budget | adversarial | M3 | `crates/query/tests/pattern_match_spec.rs::the_step_budget_stops_exponential_backtracking` |
| PAT-02 | Pathological source × pattern combinations terminate within budget | adversarial | M3 | `crates/query/tests/pattern_match_spec.rs::the_deadline_cancels_a_search` |
| PAT-03 | Regex constraints cannot backtrack catastrophically and respect the length cap | unit | M3 | `crates/query/tests/sec2_regex_budget_hostile.rs::sec2_05_regex_length_cap_refuses_overlong_pattern`; `crates/query/tests/sec2_regex_budget_hostile.rs::sec2_06_step_budget_stops_many_match_candidates` |
| PAT-04 | Fuzz target: the pattern and rule parser | fuzz | M3 | `crates/query/tests/query_hostile_spec.rs` |
| PAT-05 | Results agree with the reference matcher on a shared corpus | differential | M3 | `crates/query/tests/pattern_diff_spec.rs::corpus_expect_agree_cases_are_marked` |
| PAT-06 | Results and edit sets are identical across repeated runs of the same binary, and across the Windows/macOS/Linux conventions a source file can carry | golden/property | M3 | `crates/edit/tests/pattern_platform_invariance_spec.rs::results_are_identical_on_every_run`; `crates/edit/tests/pattern_platform_invariance_spec.rs::edit_sets_are_identical_on_every_run`; `ci:.github/workflows/ci.yml::cargo test --workspace` |
| PAT-07 | A verbatim range is copied exactly while text outside it is re-indented | golden | M3 | `crates/edit/tests/template_spec.rs::verbatim_ranges_are_copied_exactly` |
| PAT-08 | Invalid patterns return `[invalid_pattern]` with a position and a suggestion | golden | M3 | `crates/query/tests/pattern_compile_spec.rs::invalid_patterns_are_refused_with_a_suggestion` |
| PAT-09 | A captured expression substituted next to a tighter-binding operator is parenthesised and the preview says so (`$X * 2` with `a + b`) | golden per language | M3 | `crates/edit/tests/rewrite_spec.rs::a_captured_expression_is_wrapped_only_when_substitution_would_change_its_meaning`; `ci:.github/workflows/ci.yml::cargo test --workspace` |
| PAT-10 | Re-indentation never changes bytes inside strings, template literals, docstrings or comments | property | M3 | `crates/edit/tests/rewrite_spec.rs::verbatim_regions_survive_reindentation_in_every_language_and_construct`; `crates/edit/tests/rewrite_spec.rs::text_inside_template_strings_and_comments_of_the_replacement_is_not_reflowed` |
| PAT-11 | Comments dropped by a whole-match replacement are listed; the file is refused without `allow_comment_loss` | golden | M3 | `crates/edit/tests/rewrite_spec.rs::comments_inside_a_match_but_outside_every_capture_are_refused_unless_allowed` |
| PAT-12 | An edit that fixes one syntax error and introduces another is refused by the location-based gate | golden per language | M6 | `-` (no test yet; milestone corrected M4 -> M6, see below) |

### Edit engine (`EDT`)

| ID | What it proves | Kind | M | Target |
|---|---|---|---| --- |
| EDT-01 | Whatever edit set the engine returns (including a deliberately malicious mock), the shell rejects out-of-range, overlapping and mid-character edits | property | M4 | `crates/edit/tests/edit_hostile_spec.rs::a_hostile_edit_set_is_validated_and_never_applied_half` |
| EDT-02 | A tampered stored plan is rejected (`[plan_corrupt]`) | adversarial | M4 | `crates/edit/tests/plan_spec.rs::a_tampered_stored_plan_is_rejected_even_when_it_is_still_a_valid_plan` |
| EDT-03 | A forged or foreign-workspace plan is rejected | adversarial | M4 | `crates/edit/tests/plan_spec.rs::a_plan_is_bound_to_its_workspace` |
| EDT-04 | A file changed after preview makes apply fail with `[stale_plan]` and write nothing | e2e | M4 | `crates/edit/src/spec/apply_spec.rs::a_file_changed_after_preview_is_stale_and_nothing_is_written` |
| EDT-05 | Applying the same plan twice is refused | e2e | M4 | `crates/edit/tests/jstore_spec.rs::a_plan_can_be_journaled_only_once_in_any_state` |
| EDT-06 | An expired plan is refused | unit (injected clock) | M4 | `crates/edit/tests/store_spec.rs::an_expired_plan_is_refused_listed_nowhere_and_swept` |
| EDT-07 | Concurrent applies of one plan: exactly one succeeds | stress | M4 | `crates/edit/src/spec/apply_spec.rs::two_applies_of_one_plan_race_and_exactly_one_wins` |
| EDT-08 | Concurrent applies of different plans on overlapping files serialise; the loser gets `[stale_plan]` | stress | M4 | `crates/edit/src/spec/apply_spec.rs::two_different_plans_on_one_file_serialise_and_the_second_is_stale` |
| EDT-09 | A failure injected between every pair of apply steps ends fully original or fully applied, and is recoverable | fault injection | M4 | `crates/edit/src/spec/apply_spec.rs::a_failure_at_any_step_rolls_everything_back_and_leaves_nothing_behind` |
| EDT-10 | Recovery of a half-applied plan restores the original state and is idempotent | fault injection | M4 | `crates/edit/src/spec/apply_spec.rs::a_crash_at_any_step_is_repaired_by_recovery_to_the_original_state` |
| EDT-11 | Apply followed by undo restores every file of the plan byte for byte | golden | M4 | `crates/edit/src/spec/undo_spec.rs::an_applied_plan_is_undone_to_the_exact_original_bytes` |
| EDT-12 | Undo is refused with `[diverged]` if any file changed after apply, and writes nothing | e2e | M4 | `crates/edit/src/spec/undo_spec.rs::a_file_changed_after_apply_refuses_the_whole_undo_with_zero_writes` |
| EDT-13 | The syntax gate refuses edits that add syntax errors and allows edits that fix them | golden per language | M4 | `crates/edit/src/spec/apply_spec.rs::the_syntax_gate_behaves_the_same_in_every_language`; `crates/edit/src/spec/apply_spec.rs::the_syntax_gate_refuses_new_errors_and_allows_fixing_or_keeping_them`; `ci:.github/workflows/ci.yml::cargo test --workspace` |
| EDT-14 | Mode bits, BOM, CRLF/LF style, trailing newline and indentation are preserved | e2e per OS | M4 | `crates/edit/src/spec/apply_spec.rs::file_properties_are_preserved` |
| EDT-15 | If recorded edits do not yield `post_hash` (a nondeterministic or buggy engine), apply refuses | unit | M4 | `crates/edit/src/spec/apply_spec.rs::recorded_edits_that_do_not_produce_the_recorded_hash_are_refused` |
| EDT-16 | Plan-store TTL and quotas, and journal retention, evict correctly and never evict an in-use plan | unit | M4 | `crates/edit/tests/store_spec.rs::only_expired_plans_make_room_and_never_in_use_ones` |
| EDT-17 | Apply re-resolves every path; a plan that lists an outside or protected path cannot write it | adversarial | M4 | `crates/edit/src/spec/apply_spec.rs::the_write_policy_is_re_applied_to_every_stored_path` |
| EDT-18 | A symlink planted at a target just before rename is refused | adversarial/race | M4 | `crates/core/tests/fsio_harden_spec.rs::target_swapped_before_the_rename_is_refused` |
| EDT-19 | A full disk during temp-file writes fails cleanly with nothing changed | fault injection | M4 | `crates/core/tests/fsio_adversarial_spec.rs::rlimit_fsize_child_fails_cleanly` |
| EDT-20 | Fuzz target: the plan deserialiser | fuzz | M4 | `crates/edit/tests/edit_parse_hostile_spec.rs::a_mutated_plan_document_never_panics_and_is_canonical_when_accepted` |
| EDT-21 | Multi-file lock acquisition in sorted order does not deadlock under stress | stress | M4 | `crates/core/tests/workspace_lock_spec.rs::eight_threads_race_without_ever_overlapping` |
| EDT-22 | A crash injected between every pair of undo steps ends fully undone or fully applied, and is recoverable (undo is journaled) | fault injection | M4 | `crates/edit/src/spec/undo_spec.rs::a_crash_at_any_step_is_repaired_by_recovery_in_the_undo_direction` |
| EDT-23 | Recovery with one foreign-modified file among several changes nothing and reports the full per-file classification (no mixed tree) | adversarial | M4 | `crates/edit/src/spec/apply_spec.rs::a_foreign_edit_during_a_half_applied_state_is_reported_and_nothing_is_rewritten` |
| EDT-24 | A rollback that fails (injected disk-full or sharing violation) keeps the journal in `writing`, reports `[rollback_incomplete]` and blocks later applies until recovered | fault injection | M4 | `crates/edit/src/spec/apply_spec.rs::a_rollback_that_fails_keeps_the_journal_open_and_blocks_later_applies_until_recovered` |
| EDT-25 | A truncated or altered `orig/<n>` is detected by hash and never written back; recovery re-resolves manifest paths with the write policy | adversarial | M4 | `crates/edit/tests/jstore_spec.rs::an_original_is_re_verified_every_time_it_is_read` |
| EDT-26 | Apply, undo and recover refuse an abbreviated plan id; read-only tools accept an unambiguous prefix | e2e | M4 | `crates/edit/tests/store_spec.rs::write_paths_need_the_full_id_read_paths_accept_an_unambiguous_prefix` |
| EDT-27 | Filling the plan store with previews never evicts an unexpired or in-use plan; preview is refused instead; the per-process quota holds | stress | M4 | `crates/edit/tests/store_spec.rs::a_full_store_refuses_new_plans_and_never_evicts_an_unexpired_one` |
| EDT-28 | Targets with more than one hard link, a read-only bit, alternate data streams or unpreservable extended attributes are refused with `[unsupported_target]` | e2e per OS | M4 | `crates/core/tests/fsio_spec.rs::refuses_hard_linked_readonly_and_symlink_targets` |
| EDT-29 | One file reachable by two paths (case alias, second hard link) appears once in a plan | unit | M5 | `-` (no test yet; milestone corrected M4 -> M5, see below) |
| EDT-30 | A plan id is a function of the plan content alone: a change to the note, the summary or an edit changes it, and the same content gives the same id | golden | M4 | `crates/edit/tests/plan_spec.rs::the_id_is_a_function_of_the_content_only` |
| EDT-31 | Cancellation, stdin EOF and SIGTERM during the write section are deferred until apply completes or rolls back | fault injection | M5 | `-` (no test yet; milestone corrected M4 -> M5, see below) |

#### Preview

| ID | What it proves | Kind | M |
|---|---|---|---|
| EDIT9-01 | The same content and request give the same plan id, and the clock is not part of it (with EDT-30) | property | M4 |
| EDIT9-02 | One file reachable by two paths appears once in the plan, deduplicated by identity (with EDT-29) | unit | M4 |
| EDIT9-03 | A rewrite that adds syntax errors is refused at preview with `gate_failed`, naming the gate and the file, and stores nothing | unit | M4 |
| EDIT9-04 | `post_hash` is the hash of the content the recorded edits produce, so the stability gate is a proof and not a formality (E-4) | unit | M4 |
| EDIT9-05 | Files that cannot be edited during a scan are counted with a reason (too large, not UTF-8, protected), never silently dropped | unit | M4 |
| EDIT9-06 | A pattern that does not parse is `invalid_pattern` and points at `ast_explain_pattern`; no match anywhere is a success with zero matches | unit | M4 |
| EDIT9-07 | Each symbol operation edits exactly its range: `replace` keeps the doc comment, `delete` takes it, `insert_before` lands between the two | unit | M4 |
| EDIT9-08 | The diff data renders the worked example in `TOOLS.md`: one hunk per changed region, three lines of context, tagged lines, no trailing newline in a line's text | unit | M4 |
| EDIT9-09 | A plan this module builds always satisfies `Plan::check`: files strictly ascending by path whatever order the caller listed them, edits ascending and non-overlapping | property | M4 |

### MCP server (`MCP`)

| ID | What it proves | Kind | M | Target |
|---|---|---|---| --- |
| MCP-01 | In read-only mode, `tools/list` contains no write tool | e2e | M5 | `crates/mcp/tests/mcp1_stdio_spec.rs::mcp1_12_write_tools_are_listed_only_in_write_mode` |
| MCP-02 | In read-only mode, calling a write tool is an unknown-tool error | e2e | M5 | `crates/mcp/tests/mcp1_stdio_spec.rs::mcp1_06_write_tool_call_is_unknown_tool` |
| MCP-03 | Tool annotations match [`TOOLS.md`](TOOLS.md#modes-and-annotations); preview has no write capability (compile-time) | unit+compile-fail | M5 | `crates/mcp/tests/mcp1_stdio_spec.rs::mcp1_11_tools_list_matches_tools_md_modes_contract` |
| MCP-04 | Malformed JSON-RPC, wrong types and unknown methods yield errors, never panics | unit | M5 | `crates/mcp/tests/mcp1_stdio_spec.rs::mcp1_03_json_rpc_shape_errors_without_panic` |
| MCP-05 | Oversized messages are refused; request floods are bounded in concurrency (size cap covered; the request-flood concurrency bound has no test) | stress | M5 | `crates/mcp/tests/mcp1_stdio_spec.rs::mcp1_02c_cap_off_by_one_on_real_binary` |
| MCP-06 | Cancellation during parse and search stops the work promptly (`server.rs::handle_notification` ignores `notifications/cancelled`) | e2e | M5 | `-` (no test yet) |
| MCP-07 | Fuzz target: the MCP message parser (the in-process equivalents are `mcp1_03` / `mcp1_04`) | fuzz | M5 | `-` (no test yet) |
| MCP-08 | Golden transcripts for each tool, including every error code (35 transcripts in `crates/mcp/tests/golden/transcripts/`; format, coverage and the honest gaps in [`crates/mcp/tests/golden/COVERAGE.md`](../crates/mcp/tests/golden/COVERAGE.md)) | golden | M5 | `crates/mcp/tests/mcp8_golden.rs::every_transcript_replays_exactly` |

### Configuration (`CFG`)

| ID | What it proves | Kind | M | Target |
|---|---|---|---| --- |
| CFG-01 | A planted workspace-local config file has no effect (and `doctor` reports it) | e2e | M5 | `-` (no test yet) |
| CFG-02 | A user config owned by another user or writable by group/others is refused | adversarial (Unix) | M5 | `-` (no test yet) |
| CFG-03 | `policy.allow_write = false` cannot be overridden by a flag or variable | e2e | M5 | `-` (no test yet) |
| CFG-04 | Values above the hard maxima are rejected | unit | M1 | `crates/core/tests/limits_spec.rs::above_hard_max_is_rejected_naming_the_field` |
| CFG-05 | Invalid configuration stops startup with a clear message and never falls back looser | unit | M5 | `-` (no test yet) |
| CFG-06 | `--allow-write` without `policy.allow_write = true` in the user file leaves write mode off, and `ast_info` says why | e2e | M5 | `-` (no test yet) |
| CFG-07 | `--read-root` of `/`, a drive root, the home directory or a credential directory is refused | unit | M5 | `-` (no test yet) |

### State directory and logs (`STA`)

| ID | What it proves | Kind | M | Target |
|---|---|---|---| --- |
| STA-01 | The state directory is created `0700` and refused if owned by someone else, group-writable, or a symlink | adversarial | M1 | `crates/core/tests/statedir_spec.rs::creates_0700_and_is_idempotent` |
| STA-10 | The state directory resolves to the platform user-state base (`$XDG_STATE_HOME` / `%LOCALAPPDATA%`), never into the workspace, and is refused rather than guessed when the base cannot be determined | unit | M1 | `crates/core/tests/statedir_spec.rs::an_absolute_xdg_state_home_wins` |
| STA-11 | The apply lock and the stores agree on one directory policy: `ensure_state_dir` and `ApplyLock::acquire` refuse the same directories, and neither repairs one | adversarial | M1 | `crates/core/tests/workspace_lock_spec.rs::the_lock_path_and_ensure_state_dir_agree` |
| STA-12 | A directory the tool's own state used to occupy is never entered by a workspace walk, at any depth | adversarial | M1 | `crates/core/tests/walk_spec.rs::the_walk_never_enters_a_legacy_state_directory` |
| STA-02 | Plan and journal files are `0600` | unit | M4 | `crates/edit/tests/jstore_spec.rs::create_lays_out_the_journal_privately_and_returns_a_prepared_manifest` |
| STA-03 | A state directory pre-created by an attacker is refused | adversarial | M1 | `crates/core/tests/statedir_spec.rs::refuses_attacker_precreated_group_readable_dir` |
| STA-04 | Logs never contain source or replacement text (canary strings) | e2e | M5 | `-` (no test yet) |
| STA-05 | The log file is `0600` and rotates at start | unit | M5 | `-` (no test yet) |
| STA-06 | Quota and TTL eviction keep the state directory within its bounds | unit | M4 | `crates/edit/tests/store_spec.rs::the_store_size_cap_refuses_instead_of_evicting` |
| STA-13 | An unrelated `create` never expires another plan's journal; the age pass on that path is gated on the size cap, and retention is applied by the maintenance entry point | adversarial | M4 | `crates/edit/tests/jstore_spec.rs::an_unrelated_create_does_not_expire_another_plans_journal` |
| STA-14 | Retention is reachable: `plan gc` reclaims, and `doctor` reports what it would remove and removes nothing | e2e | M4 | `crates/cli/tests/sweep_spec.rs::doctor_reports_reclaimable_without_removing_it` |
| STA-07 | Two spellings of one root (bind mount, `subst`, UNC vs drive letter) get the same workspace id; overlapping workspaces are refused | adversarial | M1 | `crates/core/tests/workspace_spec.rs::two_spellings_of_one_root_share_an_id` |
| STA-08 | Write mode on a network filesystem with unreliable locking is refused unless explicitly allowed | unit | M6 | `-` (no test yet; milestone corrected M4 -> M6, see below) |
| STA-09 | On Windows the state directory and user config are verified by owner and ACL (no access for other principals), the equivalent of STA-01 and CFG-02 | adversarial (Windows) | M6 | `-` (no test yet; milestone corrected M1 -> M6, see below) |

### Output (`OUT`)

| ID | What it proves | Kind | M | Target |
|---|---|---|---| --- |
| OUT-01 | Returned source is fenced and delimited, and the fence is one backtick longer than the longest run in the code | golden | M2 | `crates/core/tests/render_spec.rs::fence_is_longer_than_any_backtick_run_in_the_code` |
| OUT-02 | Outputs contain only workspace-relative paths; a file under a read-only root is shown as `@root<N>/relative`, never as an absolute path | golden | M2 | `crates/core/tests/boundary_spec.rs::read_roots_are_labelled_and_never_absolute` |
| OUT-03 | Error messages contain no source text beyond a short bounded snippet and never a secret-like value | unit | M2 | `crates/edit/tests/editset_spec.rs::messages_name_the_edit_but_never_quote_the_source` |
| OUT-04 | An escape-sequence corpus (ANSI, OSC, `\r`, backspace) in source, names and notes is escaped rather than emitted raw | golden | M2 | `crates/core/tests/render_spec.rs::escape_sequences_and_cr_are_made_visible` |
| OUT-05 | Bidi override and zero-width characters are escaped and counted in the risk summary | golden | M2 | `crates/core/tests/render_spec.rs::bidi_and_invisible_are_escaped_and_counted_separately` |
| OUT-06 | The CLI colour diff of hostile content cannot hide, rewrite or recolour a line | e2e (pty) | M5 | `-` (no test yet) |
| OUT-07 | Files skipped (ignored, special, too large, not UTF-8) are counted and reported in every search and preview | unit | M2 | `crates/core/tests/walk_spec.rs::walk_is_sorted_skips_vcs_dirs_and_counts_links_and_specials` |

### Supply chain and release (`SUP`)

| ID | What it proves | Kind | M | Target |
|---|---|---|---| --- |
| SUP-01 | `cargo-deny` (licences, advisories, sources, bans) passes in CI | CI check | M0 | `ci:.github/workflows/ci.yml::cargo-deny` |
| SUP-02 | Every grammar crate in `Cargo.lock` is pinned to an exact version and recorded in `THIRD-PARTY-LICENSES.md` with its source, upstream commit and licence, and none is an unpinned git or branch dependency. **Runs in CI and again in the release workflow**, because a grammar arriving from a git source or a floating branch is a supply-chain change a release must not carry silently | CI check | M2 | `ci:.github/workflows/ci.yml::check-grammar-provenance.sh`; `ci:.github/workflows/ci.yml::test-check-grammar-provenance.sh`; `ci:.github/workflows/release.yml::Grammar provenance` |
| SUP-07 | Every release artefact carries the third-party licence notices for the code it statically links, and the check cannot report success on a tree it did not read | CI check | M2 | `ci:.github/workflows/release.yml::Third-party licence notices are present`; `ci:.github/workflows/ci.yml::verify-release.sh` |
| SUP-03 | Workflow actions are pinned and use minimal permissions; a release is created only if every platform builds | CI check | M7 | `ci:.github/workflows/release.yml::needs:` | |
| SUP-04 | Installer tests cover truncation, tampering, hostile archives, no `HOME`, unwritable targets. **`install.sh` is covered end to end; `install.ps1` has nine contract cases that run everywhere and three execution cases that need a PowerShell runner and have never run.** Run with `make test-rel2` | e2e + contract | M7 | `ci:.github/workflows/ci.yml::rel2_install_ps1_spec.sh` |
| SUP-05 | The lockfile is committed and CI builds with `--locked` | CI check | M0 | `ci:.github/workflows/ci.yml::--locked` |
| SUP-06 | Two builds of the same commit produce the same binary (best effort; documented exceptions) | CI check | M7 | `-` (no test yet) |
| SUP-07 | Release artifacts verify against their published checksums in a clean-machine test | e2e | M7 | `-` (no test yet) |
| SUP-08 | `cargo-audit` runs against the RustSec advisory database on a schedule, so a newly published advisory is caught without waiting for a code change | CI check | M0 | `ci:.github/workflows/audit.yml::cargo audit` |

## Supplementary suites (M1 / M2)

These suites support the catalogue above. They are **not** new catalogue IDs
(so `check-matrix.sh` stays green); each maps to existing obligations already
referenced from [`SECURITY-MODEL.md`](SECURITY-MODEL.md) or this catalogue.

| Suite (crate test file) | What it adds | Anchors |
|---|---|---|
| `core/tests/boundary_prop.rs`, `boundary_write_prop.rs` | Adversarial / property probes on resolve-read and resolve-write (tester-only; no lasting src edits) | BND-16, BND-18, T-20 |
| Mutation self-proof (local) | Once per milestone: disable one safety check, confirm a named test fails, revert; recorded in milestone review notes | Conventions above; CHANGELOG milestone notes |
| Cross-platform inventory | CI runs the same suite on Linux, macOS and Windows; OS-only gaps are named in code and listed with a replacement | Conventions (OS matrix) |
| `core/tests/render_spec.rs` | Character-class tables for control / bidi / invisible (incl. Tags U+E0000–E007F) escaping and fence adaptation | OUT-01, OUT-04, OUT-05 |
| `core/tests/ignore_spec.rs`, `walk_spec.rs`, `walk_extra_spec.rs` | gitignore subset matching, walk skip counts, depth ceiling, ignore-file size/UTF-8 refusal | BND-23, LMT-05, OUT-07, T-31 |
| `core/tests/listdir_spec.rs` | `read_dir` through the Boundary: kinds, entry ceiling, non-UTF-8 names counted | LMT-05, BND-22, OUT-07 |
| `query/tests/outline_spec.rs` (seeded random / broken inputs) | Fixed-seed hostile sources per language; no panic; no empty names; legal byte/line extents | PRS-09; outline empty-name guard |
| `core/tests/core_hostile_spec.rs` | Fuzz-style: resolve-read / resolve-write never answer from outside the root; `workspace_id` refused or well formed; hostile render escaping; limit validation total | BND-16, BND-18, OUT-01, OUT-04, LMT-01 |
| `core/tests/core_network_paths_spec.rs` | Windows-shaped paths refused by the string layer on every platform: drives (including drive-relative `C:foo`), UNC in every separator mixture, device and other `\\?\` namespaces (`\\?\UNC\…`, `\\?\GLOBALROOT\…`, `\\?\Volume{…}\…`, `\\.\…`), alternate data streams; a network / device / drive-relative **root** refused as `invalid_args` before canonicalisation, with one message for every spelling; and the verbatim **disk** root `\\?\C:\x` asserted *allowed*, on every platform | BND-02, BND-11, BND-18, BND-24 |
| `core/tests/core_windows_name_hazard_spec.rs` | The whole class of names Windows cannot spell — illegal characters, trailing dot/space, reserved device names in every spelling — refused as `outside_workspace` and never `io_error` on Windows, and resolving normally on Unix (which is what proves the `cfg!(windows)` gate is off there); ordinary near-miss names (`console`, `com10`, `com0`, `communicate`) resolve on both platforms | BND-11, BND-18 |
| `lang/tests/lang_hostile_spec.rs` | Fuzz-style: 3000 hostile sources per language, budgeted parse; oversized source refused by size; deep nesting refused by depth | PRS-01..04, PRS-09 |
| `query/tests/query_hostile_spec.rs` | Fuzz-style: pattern compile, compiled-pattern search on a fixed corpus, rules (incl. catastrophic regexes), outline | PAT-04, PRS-09 |
| `edit/tests/edit_hostile_spec.rs` | Fuzz-style: `validate_edits` / `apply_edits` consistency, overlap refusal, `expand_template`, `indent_of_line` clamping | EDT-01, EDT-02 |
| `edit/tests/edit_parse_hostile_spec.rs` | Fuzz-style, portable (no `cfg`): `Plan::parse` / `parse_named` canonicality and id binding, `Manifest::parse`, and a plan id refused before it can name a path | EDT-20, EDT-02 |
| `edit/tests/edit_state_hostile_spec.rs` (`#![cfg(unix)]`) | Fuzz-style: a `PlanStore` directory attacked with truncated / rewritten / symlinked plan and meta files, plus: nothing is written outside the store | EDT-06, EDT-16, EDT-26, EDT-27 |
| `lang/tests/parse_spec.rs` | Depth/node/timeout budgets, BOM/CRLF/`end_byte` length, honest error counts | PRS-01..04, PRS-09 |
| `lang/tests/sec2_parse_budget_hostile.rs` | SEC2 hostile inputs that cross depth / size / node / timeout budgets; mutation baselines inlined | PRS-01..04 |
| `query/tests/sec2_regex_budget_hostile.rs` | SEC2 hostile `where` regex length-cap + many-candidate step-budget binding | PAT-03 (T-09) |
| `edit/tests/preview_spec.rs` (`#![cfg(unix)]`) | Preview: determinism, identity deduplication, all five gates, the whole failure-semantics table (invalid pattern, zero matches, ambiguous, not found, unsupported language, limits, budget, outside/protected), the five symbol operations, the diff data, and the risk summary | EDT-29, EDT-30, EDT-01, E-1, E-4, E-5, E-12; EDIT9-01..09 |
| `edit/src/spec/edit11_encoding_gate_spec.rs` (in-crate, unix) | EDIT-11: apply re-runs shared `encoding_gate` on forged plans that flip trailing newline / CRLF→LF; workspace and journal untouched | EDIT11-01..03; EDT-14 |
| `tools/tests/read_tools_spec.rs` | Exact `ast_info` / `ast_outline` / `ast_get` format contracts (golden strings) | TOOLS.md; OUT-01, OUT-02, OUT-07 |

## Fuzz-style tests

`cargo-fuzz` needs a nightly toolchain and libFuzzer, which is the wrong thing to
require of every contributor and of CI. The targets that matter for safety are
covered instead by a deterministic, in-process suite that plain `cargo test`
runs everywhere.

### How it works

`crates/*/tests/common/fuzz.rs` is a small toolkit, copied into each crate's test
tree so a test binary needs no dependency the crate does not already have:

- **Deterministic PRNG.** xorshift64\*, seeded per target from a constant. The same
  seed always produces the same cases, in the same order, on every machine.
- **Seeded corpus.** Each target starts from a hand-written list of shapes
  (`SNIPPETS`): the seeds that make the parser work - deep nesting, long lines,
  unbalanced delimiters, bytes that are not text - plus the boundary values
  (`BOUNDARIES`): empty, one byte, one past the end, `usize::MAX`.
- **Mutation operators.** Bit flips, byte insertion / deletion / duplication,
  boundary-value replacement, and cuts at a character boundary. A cut is *how a valid
  string becomes bytes that are no longer valid UTF-8*: `cut_at_boundary` lands on a
  character boundary so the operator's own bookkeeping never splits a character, but the
  preceding splice can, so `mutate_bytes` can and does return invalid UTF-8.
  `case_at` recovers with `String::from_utf8_lossy`, which replaces each invalid sequence
  with U+FFFD, and `Case::is_binary` detects exactly this. The consequence is stated
  rather than wished away: the input the parsers actually see (`case.input`) is always
  valid UTF-8, so **this suite does not claim binary / invalid-UTF-8 coverage of the
  parsers.** The original bytes are kept on `Case::bytes` for reporting and for targets
  that want them.
- **Per-case time ceiling.** Every case runs on its own worker thread with a 2 s
  limit, so a single pathological input fails its case instead of hanging the run.
- **Reproducible failure.** A failure prints the case index, the seed and the
  mutated input as hex, plus a ready-to-paste replay line:
  `fuzz::case_at(<index>, <seed>, SEEDS)`. Paste it into the target and you have
  the failing case, with no corpus file to hunt for.

### Targets

Every name below is a real first argument to a `run_cases(...)` call in the named file;
these are the exact strings, not labels. A few `*_hostile_spec.rs` tests are hand-written
and pass no target name at all - they are called out as such below rather than given a
label they do not have.

| Target | File | What it asserts |
|---|---|---|
| `core.limits`, `core.resolve_read`, `core.resolve_write`, `core.traversal`, `core.workspace_id`, `core.render` | `core/tests/core_hostile_spec.rs` | No panic; no answer from outside the root; a well-formed or refused `workspace_id`; no unescaped control / bidi / invisible character in rendered output; limit validation is total |
| `lang.parse[<language>]` (one target per language, `format!("lang.parse[{}]", language.id())`) | `lang/tests/lang_hostile_spec.rs` | 3000 hostile sources per language parse inside a budget; every refusal is `budget_exceeded` or `timeout`; a parsed tree stays inside its source |
| *(hand-written `#[test]`s, no target label)* oversize and depth refusals | `lang/tests/lang_hostile_spec.rs` | An oversized source is refused by size and a deeply nested one by depth. These two are plain tests, not `run_cases` targets: they assert one specific refusal each rather than sweeping a mutated corpus |
| `query.pattern_compile[<language>]` (one per language, `format!("query.pattern_compile[{}]", language.id())`), `query.search`, `query.rules`, `query.outline` | `query/tests/query_hostile_spec.rs` | Compile refuses or yields a self-consistent pattern; a compiled pattern searches a fixed corpus inside its budget; matches are ordered and in range |
| *(hand-written `#[test]`, no target label)* catastrophic regex | `query/tests/query_hostile_spec.rs::a_catastrophic_regex_is_either_refused_or_linear` | For each classic catastrophic pattern, compile it with the linear-time engine (`regex::RegexBuilder::build`); if it compiles, assert `is_match` over a 4000-character input completes within the 2 s per-case ceiling, then run the same pattern through the real `search` path against a source built to span thousands of `a`s and assert that too, allowing only `budget_exceeded`. A compile refusal passes. An earlier version built the 4000-character input and discarded it (`let _ = long;`), so it measured the pattern against the 5-line corpus and proved nothing |
| `edit.editset`, `edit.overlap`, `edit.template`, `edit.indent_of_line` | `edit/tests/edit_hostile_spec.rs` | A validated set is ordered, in range and on character boundaries; a refused set is not appliable; **generated** overlapping edits are refused as `invalid_edit` in either order; an unbound capture never appears in expanded output. The overlap target exists because the general loop cannot cover it: `replace_by_hand` skips an overlapping edit (`if e.start < at { continue }`), so without it the mutation self-proof on the overlap check would stay green with the check deleted. All four edit targets run their full 3000 cases with nothing skipped, because `overlap_offsets` builds the pair **by index into the sorted boundary list** rather than by arithmetic on offsets: the source's boundaries are unevenly spaced (1, 2 or 3 bytes apart, since it has 2- and 3-byte characters), so `a_start + 2` is not reliably a boundary, and an earlier version that did that arithmetic produced a `b_start` inside a multi-byte character in about 40% of cases — refused for `invalid_edit` on a *boundary* check while still counting as a pass. See "The overlap sweep draws no two boundaries from arithmetic" below |
| `edit_state.plan_parse`, `edit_state.plan_parse_named`, `edit_state.manifest_parse` | `edit/tests/edit_parse_hostile_spec.rs` | An accepted plan or manifest re-serialises to exactly its input bytes and passes a fresh `check`; nothing else is accepted than `plan_corrupt`; bytes never verify under an id they do not hash to |
| `edit_state.plan_id` | `edit/tests/edit_parse_hostile_spec.rs` | A malformed id is refused before it can name a path. Needs a real store, so on a platform without state directory verification it prints `SKIPPED` with the reason and returns |
| `edit_state.plan_store[<shard>]` (six shards, `format!("edit_state.plan_store[{shard}]", …)`), `edit_state.journal_store` (written, off) | `edit/tests/edit_state_hostile_spec.rs` | A damaged store directory never yields a plan other than the one stored; every error code is one the decision table lists; nothing is written outside the store directory |

### The overlap sweep draws no two boundaries from arithmetic

`edit.overlap` has a stronger requirement than the other targets. The general edit-set loop can
assert on whatever `validate_edits` decided, but this one exists to prove a *specific* check
fired: the pair it builds must be wrong in exactly one way, so a refusal is attributable to
overlap and nothing else. Two things follow, and both are easy to get wrong silently.

**The pair is built by index, not by offset arithmetic.** `SOURCE` is 63 bytes with 59 character
boundaries, and they are not evenly spaced: the `é` and the two CJK characters mean neighbours can
be 1, 2 or 3 apart. So an expression like `b_start = a_start + 1` is not guaranteed to land on a
boundary — the earlier version used exactly that, and roughly 40% of its cases produced a `b_start`
inside a multi-byte character. Those cases were refused with `invalid_edit` for a *boundary* reason,
the reason code was still `invalid_edit`, the assertion still passed, and the case still counted as
executed. It looked like a working sweep while a large share of it measured the wrong check.
`overlap_offsets` therefore draws all four endpoints as indices into the sorted, de-duplicated
boundary list, with nested ranges (`b_start` in `1..=N-3`, `a_end` in `b_start+2..=N-1`, `b_end`
between them, `a_start` to the left) that are total for any list of three or more entries. The
result is `a_start < b_start < b_end < a_end`: ordered, in range, on boundaries, and genuinely
overlapping by construction.

**A decline must not be a way to pass.** Picking `a_start` from the whole candidate list and then
needing an offset at least 2 past it made the trailing boundaries of the source unusable, because
the source ends in a single-byte newline. That skipped 100 of 3000 cases — which the 0.95 floor
tolerated, and which the log reported by name, but which was still 100 cases in which the overlap
check was never exercised. The picker above has no such dead end: the ranges are nested so the
`b_start` index is bounded away from the end, so no case can fall out. The `debug_assert`s in the
sweep are there to make a future regression loud rather than quiet — a silent fallback there is
precisely how the skips would come back.

### The executed-fraction floor is per target, and it is 1.0

`RunReport::assert_executed_fraction(minimum)` takes its threshold as an argument, and every target
that calls it passes **1.0**: every case the generator produced was run. There is no crate-wide
`MIN_EXECUTED_FRACTION` any more, and no default.

That is not aspirational. Measured on this tree, all 19 live targets execute every case they ask for
— 3000/3000 for each of the `core.*`, `edit.*`, `edit_state.plan_*`, `lang.parse[<language>]`,
`query.*` targets, and 500/500 on each of the six `edit_state.plan_store[<shard>]` shards, which is
3000 in total. 1.0 is a statement about those measurements, not a target the suite is being asked to
climb to.

**Why per target rather than one shared constant.** The floors were once a single
`MIN_EXECUTED_FRACTION = 0.95` that every target in a crate imported. Against this tree that value
had no force: every target runs at 100%, so 0.95 admitted a 3.3% decline (2900/3000), a 10% decline
and a 30% decline without complaint — and a 30% decline means a third of the suite never ran the
property it names. It was not hypothetical either: the old `edit.overlap` generator really did
produce 2900/3000 with the suite green, and an earlier one produced 952/3000. A shared 0.95 was the
"green but nothing was tested" failure this harness exists to prevent, wearing the costume of the fix
for it.

Declaring the value per target keeps the exception where it can be seen. A target that genuinely must
decline some cases declares a **lower value in its own file, next to the target, with a reason
attached** — never by editing a shared constant, which would silently weaken every other target in
the crate at the same time. Today no target needs one: every body asserts on whatever the call
decided and returns `Ran::checked()` unconditionally, so there is nothing for any sweep to decline.

Two apparent exceptions, both of which measure 100% and are not exceptions:

- `query.outline` has a `Ran::skip` branch for "none of the four languages parsed the mutated text".
  It never fires; the target measures 3000/3000.
- `edit_state.plan_id` prints `SKIPPED` and returns early on a platform without state-directory
  verification — but that happens **before** `run_cases` is called, so the sweep never starts. It is
  a skipped test, not a skipped case.
- `edit_state.journal_store` is `#[cfg(any())]` until EDIT-5 lands, so it does not run at all. Its
  floor is declared anyway, at 1.0, so that enabling the target does not inherit a number that was
  chosen for something nobody had measured.


`edit/tests/edit_parse_hostile_spec.rs`: their seed corpus is the canonical bytes of real
plans and manifests, and the invariant under attack is that **an accepted document
re-serialises to exactly its input bytes**. `parse_named` adds the id binding.

That file carries no `cfg`, because those targets are pure functions over byte strings and
hold on every platform - except `edit_state.plan_id`, which opens a real `PlanStore` in a
`tempfile::tempdir()` and so does need a platform that verifies a state directory; where that
is missing it prints `SKIPPED` with the reason. The `PlanStore` and `JournalStore` targets
live in `edit/tests/edit_state_hostile_spec.rs`, which is `#![cfg(unix)]` because the stores
verify owner and mode bits. The store target attacks the store directory itself - truncated,
rewritten and symlinked plan and meta files - asserting that nothing but the plan that was
put ever comes back, and that no operation writes outside the store directory (checked
against a sentinel directory beside it).

`JournalStore` is written but switched off with `#[cfg(any())]`: the store is landing
under EDIT-5, and enabling a target against an API that is still moving would mean
rewriting it. Turning it on is a one-token edit (`#[cfg(any())]` to `#[cfg(unix)]`); the
test asserts that an original read back out of a journal hashes to the manifest's
`pre_hash` (E-6).

### Determinism and cost

- Seeded, so three consecutive `cargo test --workspace` runs are identical. No
  clock, no threads shared between cases; the filesystem work a target does is confined
  to a fresh tempdir with no content carried between cases.
- Each `*_hostile_spec.rs` completes in well under 30 s - on one local run, 0.8 s for core,
  7.0 s for lang, 8.3 s for query, 0.4 s for edit, 3-11 s for the edit state suite - and
  they together added about 20 s to the workspace run. **These figures are indicative
  measurements from a single local machine, not a CI guarantee:** they move with the
  runner, and CI has no wall-clock budget on this suite.
- The store target is the only one that touches the filesystem, and each of its cases does
  real `fsync`-backed writes, so it is split into six seeded shards that the test runner
  runs on separate threads. 3000 cases in total, each shard with its own seed: the set is
  still deterministic and a printed seed replays exactly. This is not only a budget
  measure. `run_cases` deliberately runs cases one at a time, because that is the only way
  to catch a case that never *returns* as well as one that returns slowly; a target made of
  3000 `fsync` writes therefore cannot be sped up from inside the test, and measured 13-40
  seconds on a shared machine. Sharding moves the parallelism to the runner, where the
  per-case ceiling still holds.
- The mutation stream is pure: it depends on the seed and the case index only, so a
  failure on one machine reproduces on all of them.

### Relationship to `cargo-fuzz`

The in-process suite is the floor, not the ceiling. Each target here names the
function a future `cargo-fuzz` target should wrap:

| In-process target | Future libFuzzer target | Wrapper |
|---|---|---|
| `core.resolve_read`, `core.resolve_write` | `fuzz_resolve` | `Boundary::resolve_read` / `resolve_write` |
| `lang.parse` | `fuzz_parse` | `lang::parse(lang, bytes, &budget)` |
| `query.pattern_compile`, `query.search` | `fuzz_pattern` | `Pattern::compile` + `search` |
| `query.rules` | `fuzz_rule` | `CompiledRule::compile` |
| `edit.editset` | `fuzz_editset` | `validate_edits` + `apply_edits` |
| `edit.template` | `fuzz_template` | `expand_template` |
| `edit_state.plan_parse`, `edit_state.plan_parse_named`, `edit_state.manifest_parse` | `fuzz_plan` | `Plan::parse` / `parse_named`, `Manifest::parse` |
| `edit_state.plan_store` | `fuzz_plan_store` | `PlanStore::put` / `get_for_write` with a hostile store directory |
| (written, off) | `fuzz_journal` | `JournalStore` - enabling the `#[cfg(any())]` target when EDIT-5 lands |
| (not yet) | `fuzz_mcp` | the MCP message parser - with M5 |

A crash found by libFuzzer gets a case appended to the target's `SNIPPETS` here, so
it is covered on every machine from then on. Adding a libFuzzer target without
adding its cases here is not "done".

### Cross-platform

The mutated paths are pure functions over byte strings. The specs are portable because
they use only `tempfile::tempdir()` and portable `std` filesystem APIs: they need no root,
no privileged operation, no fixed path and no OS-specific branch, so they behave the same
on every platform that has a writable temporary directory. They do use the filesystem -
`core_hostile_spec.rs` writes files in a tempdir in four cases, and `edit_parse_hostile_spec.rs::a_hostile_plan_id_never_becomes_a_path`
opens a real `PlanStore` - and none of those is `#![cfg(unix)]`. The one spec that is gated
is `edit/tests/edit_state_hostile_spec.rs`, which is `#![cfg(unix)]` because the stores verify
owner and mode bits; that matches the treatment in the hazards table below.

## Checking other platforms locally (crates with C grammars)

`cargo clippy --target <triple>` normally fails for the crates that bundle C grammars
(`lang`, `query`, `edit`, `tools`) because there is no cross C compiler. Clippy does not link, so
the C build scripts only have to *succeed*: point the `cc` crate at the `true` command and the
whole workspace, tests included, type-checks and lints for another platform in seconds:

```sh
t=x86_64-pc-windows-gnu; u=${t//-/_}
env CC_$u=true AR_$u=true CXX_$u=true \
  cargo clippy --workspace --all-targets --target $t -- -D warnings
```

Run it for `x86_64-pc-windows-gnu`, `aarch64-apple-darwin`, `x86_64-apple-darwin` and
`aarch64-unknown-linux-gnu` before pushing. It catches unix-only imports, dead code that only
exists on one platform and tests that do not compile elsewhere. It cannot catch *runtime*
differences (file-system semantics); those are what the hazard list below is for.

## Cross-platform test hazards

A test that assumes the filesystem it runs on is not a portable test. Linux
(ext4) is the reference, and three of its habits are false on macOS: it stores
file names as arbitrary non-NUL bytes, it keeps names that differ only by
letter case apart, and it stores exactly the bytes you wrote. macOS (APFS by
default) requires valid UTF-8 names, folds case, and normalises names to NFD.
A fourth habit is not about macOS at all: Windows never runs these suites,
because every filesystem test file is `#![cfg(unix)]` and `rustix` is a
`cfg(unix)` dependency.

The six classes below are the ones that have actually broken a run. Each has a
standard treatment, and the treatment is part of the contract, not a matter of
taste:

| # | Hazard | Linux does | macOS does | Standard treatment |
|---|---|---|---|---| --- | --- |
| A | Two names in one directory differing only by letter case | two files | one file | probe the filesystem (`x` then `X`, count the entries) and branch; keep each branch's names and expectations in a pure function so both are checked on every machine |
| B | A name that is not valid UTF-8 | stores it | refuses with `EILSEQ` (or `EINVAL`) | print `SKIPPED: this filesystem cannot hold non-UTF-8 names (...)` and return, **for those two errnos only** - any other errno still fails the test |
| C | NFC vs NFD spellings of one name | keeps both | normalises to NFD | never assert that a name round-trips; assert the property that holds under both (BND-10 does this); write non-decomposable names where the spelling itself is under test |
| D | Inode numbers | rarely reused | reused aggressively | never assert "a different inode" unless the code allocates the new one before freeing the old; when a test really needs a fresh inode, detect the reuse and skip |
| E | `/tmp` being a real directory | yes | a symlink to `/private/tmp` | use `tempfile::tempdir()`, never a hard-coded path; the boundary canonicalises and pins its roots, so the two spellings are equivalent anyway |
| F | A backslash in a name | an ordinary byte | an ordinary byte | legal in a unix name, so `#![cfg(unix)]` is the guard; if Windows is ever in scope this becomes a separator and the test must branch |

Two more are not filesystem facts but fail the same way, on a slow shared
runner rather than on another operating system:

- **Tight timings.** A 20 ms lock timeout that a thread must observe is a
  claim about the machine. Give every such budget at least ten times the margin
  the test needs, and let the holder's budget be far longer than the racers'.
  **Margin is not synchronisation.** Widening a budget only makes a race *less
  likely*; it does not make it impossible, and a test that leans on a margin is
  a test whose green depends on the runner. This used to be the only advice
  offered here, and it is what produced the 300 ms sleep in
  `workspace_lock_spec::eight_threads_race_without_ever_overlapping`, whose own
  comment defended a 10x increase as "safe". It was not safe; it was just
  usually right. What the margin is legitimately for is a *ceiling* - a bound
  on how long a test may hang - and never for arranging an ordering. To order two
  threads, use a handoff the test can observe: see the table below.
- **Fixed-size kernel interfaces.** `sockaddr_un.sun_path` is 108 bytes on
  Linux and 104 on macOS, and macOS temporary directories are much longer than
  Linux's, so a socket path that fits here need not fit there. Check the length
  before binding and skip with it printed.

### Timing dependence in the `core` tests

A `sleep`, an `Instant::now` or a `Duration` in a test is not automatically a
problem - what matters is whether the *ordering* it establishes is a fact about
the system under test or a guess about the runner. This is the judgement for
every timing construct in `crates/core/tests`, re-checked on 2026-10-02. **Synchronising** means it establishes an ordering
the test can rely on; **waiting** means the test is only hoping the machine gets
round to it.

| Site | Construct | Verdict | Reasoning |
|---|---|---|---|
| `workspace_lock_spec.rs` `eight_threads_race_without_ever_overlapping` (was 300 ms) | `sleep(300ms)` inside the critical section | **WAITING - changed** | The hold existed to make the other seven threads *probably* contend. Longer meant only *less likely* to overlap, and the comment defending a 10x increase is the pattern this section used to recommend. Replaced by a handoff: the holder takes the lock, seven racers are each guaranteed to be refused (`busy` is their only possible outcome, because the lock is provably held) and each reports back over a channel; the holder releases only after all seven reports land. The critical section itself is now one `yield_now`, since a correct lock already guarantees exclusivity and there is nothing left to wait for. |
| `workspace_lock_spec.rs` `a_waiter_acquires_as_soon_as_the_holder_drops` (was 50 ms) | `sleep(50ms)` before `drop(held)` | **WAITING - changed** | A bet that the waiter had entered its retry loop before the lock came free. Had the waiter not started yet, the sleep would expire, the lock would drop, and the waiter would take a *free* lock on its first try: green, with the retry loop never exercised. Replaced by the waiter proving it is blocked - a bounded acquire that can only come back `busy` - and announcing that before the release. |
| `listdir_spec.rs:356,359` | `sleep(hold)`, 250 us, inside the flipper | **SYNCHRONISING - left alone** | It paces the flipper against the lister, giving the directory time to exist in each state. Load only reduces the flip count, and the test asserts lower bounds (`listed > 10`, `refused > 10`), so a starved race is red rather than green. |
| `listdir_spec.rs:277,279,365,368` | deadline-bounded poll loop | **SYNCHRONISING - left alone** | Bounded by both a deadline and a count, and the assertions are lower bounds on the work actually done. Spinning cannot manufacture a pass. |
| `boundary_harden_spec.rs:326,330` | deadline-bounded poll loop | **SYNCHRONISING - left alone** | Same shape, with lower bounds `attempts > 1000`, `opened > 100`, `flips > 100`. |
| `fsio_adversarial_spec.rs:456` | `sleep(delay_ms)` before `SIGKILL` | **SYNCHRONISING - left alone** | Jittering the kill point across the child's write is the point of the test; the delay is the random variable, not an ordering assumption. |
| `fsio_adversarial_spec.rs:513` | `sleep(600s)` in a helper child | **SYNCHRONISING - left alone** | Keeps the child alive to be killed. It is bounded by the parent killing it, not by wall-clock luck. |
| `boundary_spec.rs:268`, `boundary_write_prop.rs:624,640` | `Instant::now` + upper-bound assert | **SYNCHRONISING - left alone** | Measures that a call does not block (FIFO open, call timeouts). The assertion is a ceiling on a real property; being slow is red, never green. |
| `ignore_spec.rs:70`, `protected_extra_spec.rs:177,205,213`, `render_spec.rs:129`, `walk_extra_spec.rs:122,195`, `workspace_spec.rs:51`, `fsio_props_spec.rs:300,422`, `common/fuzz.rs:307` | `Instant::now` + budget assert | **SYNCHRONISING - left alone** | Performance and budget measurement. The property under test *is* "this finishes within N"; the clock is the instrument, and there is no ordering being faked. |

The general rule the two changed sites point at: **a sleep may bound how long
a test waits, never stand in for a handoff.** And a timeout that is only there
to make a collision happen is the more dangerous kind, because "nothing
contended" is indistinguishable from a pass unless the test says so out loud.
Both changed tests now treat "the collision never happened" as a *failure* -
`CONTENTION_DEADLINE` panics rather than passing quietly - so the invariant
cannot be reported as proven on a run where it was never exercised.


The rule behind all of it: **a skip must announce itself, name the reason, and
be decided by a pure function that is itself tested.** A skip that can also be
reached by an unrelated error turns a real defect into a green run.

## Coverage and quality gates

- **The line-coverage floor is enforced, not merely stated.**
  [`scripts/check-coverage.sh`](../scripts/check-coverage.sh) runs on Linux in CI
  and on demand via `make coverage`. It measures region-based **line** coverage
  from `cargo llvm-cov --workspace --all-targets` over **first-party code only**:
  `tests/`, `benches/`, `examples/` and the cargo registry are excluded twice —
  by `--ignore-filename-regex` and again inside the script, so the gate does not
  silently depend on one flag being right.
- **The floor is 80% for each of `core`, `edit` and `query`.** One number, in one
  place (`COV_FLOOR_*` at the top of the script); CI never hard-codes a figure.
  As of this writing the measured values are `core` **87.82%**, `edit` **91.10%**,
  `query` **93.48%**.
- **Why 80 and not 90.** The long-standing aspiration is 90%; 90 stays the
  *direction*. A floor set *at* today's measurement is theatre: the next PR that
  legitimately adds code is red for adding a feature, and the cheapest response
  is to write tests that move a number rather than to test the code. 80 is below
  every crate as measured, so the gate has headroom and can be obeyed. `core` has
  the least margin and is where the floor does the most work today.
- **How to raise it.** Run `make coverage`, then raise the `COV_FLOOR_*` constant
  in the script and the number quoted here, in the same commit. Never raise a
  floor above what the crates currently achieve — a floor nobody can pass is the
  same as no floor. Never lower one to make a PR green; that is a policy change
  and deserves its own commit.
- **The gate is proven able to fail.**
  [`scripts/test-check-coverage.sh`](../scripts/test-check-coverage.sh) runs the
  real script against synthetic reports and requires a non-zero exit for: one
  crate below its floor; an **empty** report; a report containing **only** `tests/`
  files; and input that is not an llvm-cov report at all. It also pins the
  boundary — a crate sitting *exactly* at the floor passes. The two empty cases
  are the important ones: a broken toolchain must never be read as 100%.
- The boundary module is expected to be effectively fully covered, and a
  coverage *drop* in it is covered by the per-test named suites rather than by
  this percentage gate; a percentage cannot distinguish "the refusal is still
  there" from "the refusal is gone".
- **What the gate does NOT prove.** Line coverage is not branch coverage and not
  mutation testing. A crate can sit at 93% line coverage while the one `if` that
  guards a path-traversal refusal has no test at all — every line of the `if` is
  executed on the pass-through path. The standing manual mutation check below is
  what covers that class of gap, and the matrix check is what ties each threat to
  a named test. This gate is a floor against large-scale erosion, not evidence of
  test *quality*.
- `clippy -D warnings`, `rustfmt --check`, `unsafe_code = forbid` workspace-wide
  with **no exception today** (root `Cargo.toml`). A future safety-boundary crate,
  if an ADR ever allows one (M6), would be the only place for `unsafe` and would be
  reviewed line by line; that crate does not exist yet.
- The standing manual check: once per milestone, remove one safety check and confirm
  the named test fails. Results are recorded in `CHANGELOG.md`'s milestone notes.

## Token-savings benchmark

The project claims large token savings for exploration. That claim is only
acceptable with a method anyone can rerun.

**What is measured.** For a fixed set of realistic tasks on fixed, pinned
repositories (the exact commits are recorded), compare two ways of obtaining the
information an agent needs:

1. **Baseline:** read the whole files the agent would otherwise open (the files
   containing the relevant symbols).
2. **opencrayast:** the outlines, `ast_get` calls and searches an agent would make.

Tasks are written down before measuring (for example "find the definition and all
callers of `X`", "list the public API of module `Y`", "what does `Z` do").

**Metrics.** Bytes returned; token estimates under at least two published
tokenisers; number of tool calls; and — separately — *task success*, because saving
tokens by returning too little is not a saving. The report states whether the
information needed was actually present in the smaller output.

**Reporting rules.** Report the distribution (median, p10, p90), not the best case;
publish the task list, repository commits and raw measurements in the repository;
state what is **not** measured (model behaviour, caching). No figure appears in the
README unless the harness that produced it is in the repository and CI can run it.

**Agent task suite (M7).** A smaller suite runs real agents on fixed tasks and records
first-call success, error recovery and the most common misuse, to feed back into
tool descriptions ([`AGENT-GUIDE.md`](AGENT-GUIDE.md)). Results are comparable
between releases.

**What exists today.** The **deterministic half** of that suite is built and green:
`crates/tools/tests/ux_taskset_spec.rs` is 25 scored tasks with known-correct expected
outcomes, run by `cargo test` with no LLM involved, scoring **25/25** on this tree. Its
method, task list, baseline and self-proof are in
[`AGENT-TASKSET.md`](AGENT-TASKSET.md).

The **agent half is not done.** First-call success is *not* measured, because nothing here
runs a model, and a number for it would be invented. Anyone quoting this suite for agent
success rate is quoting something it does not contain; the document says so in as many words.
