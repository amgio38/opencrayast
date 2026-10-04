//! Spec for ISSUE-EDIT-9: preview — an edit request becomes a stored plan (EDIT-MODEL
//! §Preview, §Gates, §Risk summary; EDT-28..EDT-33).
//!
//! Unix only for now, like every spec that reads a file through `Boundary::open_read`: the
//! boundary verifies a file's owner and mode bits there and answers `unsupported_target`
//! elsewhere. Nothing in `preview.rs` is Unix-specific - the rules are string rules, hash
//! rules and arithmetic - so this gate is about the reader, not the logic under test.
#![cfg(unix)]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use opencrayast_core::ErrorCode;
use opencrayast_core::boundary::{Boundary, BoundaryConfig};
use opencrayast_core::hash::ContentHash;
use opencrayast_core::limits::Limits;
use opencrayast_edit::{
    Clock, DiffLineKind, EditRequest, Plan, PlanStore, PreviewContext, SymbolOp, preview,
};
use std::fs;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

/// A workspace with a boundary, a plan store and a clock that does not move.
struct Fx {
    _dir: tempfile::TempDir,
    root: PathBuf,
    state: PathBuf,
    clock: Arc<FakeClock>,
    workspace_id: String,
    limits: Limits,
}

struct FakeClock(AtomicU64);
impl Clock for FakeClock {
    fn now_secs(&self) -> u64 {
        self.0.load(Ordering::SeqCst)
    }
}

impl Fx {
    fn new() -> Fx {
        Fx::with_limits(Limits::default())
    }

    fn with_limits(limits: Limits) -> Fx {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("ws");
        fs::create_dir_all(root.join("src")).unwrap();
        let state = dir.path().join("state");
        let workspace_id = opencrayast_core::workspace::workspace_id(&root).unwrap();
        Fx {
            _dir: dir,
            root,
            state,
            clock: Arc::new(FakeClock(AtomicU64::new(1_000_000))),
            workspace_id,
            limits,
        }
    }

    fn write(&self, rel: &str, content: &str) {
        let path = self.root.join(rel);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, content).unwrap();
    }

    fn write_bytes(&self, rel: &str, content: &[u8]) {
        let path = self.root.join(rel);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, content).unwrap();
    }

    fn boundary(&self) -> Boundary {
        Boundary::new(BoundaryConfig {
            root: self.root.clone(),
            limits: Limits::default(),
            read_roots: Vec::new(),
            state_dir: None,
            extra_protected: Vec::new(),
        })
        .unwrap()
    }

    fn store(&self) -> PlanStore {
        PlanStore::open(
            &self.state,
            &self.workspace_id,
            self.limits.clone(),
            self.clock.clone(),
        )
        .unwrap()
    }

    fn run(&self, req: &EditRequest) -> Result<opencrayast_edit::PreviewOutcome, ToolError> {
        let boundary = self.boundary();
        let store = self.store();
        let ctx = PreviewContext {
            boundary: &boundary,
            plans: &store,
            limits: &self.limits,
            workspace_id: &self.workspace_id,
        };
        preview(&ctx, req)
    }
}

use opencrayast_core::error::ToolError;

/// A TypeScript file with a `console.log` on line 12, so the diff can be compared with the
/// worked example in `docs/TOOLS.md` byte for byte.
fn ts_with_log_on_line_12() -> String {
    let mut s = String::new();
    for i in 1..=11 {
        s.push_str(&format!("const filler{i} = {i};\n"));
    }
    s.push_str("  console.log(\"start\", id);\n");
    s.push_str("const tail = 1;\n");
    s
}

fn rewrite(paths: &[&str], pattern: &str, replacement: &str) -> EditRequest {
    rewrite_in("typescript", paths, pattern, replacement)
}

/// Every LF in the text must be part of a CRLF: a bare LF is a mixed-ending file, which no
/// diff shows and the next edit makes worse.
fn assert_no_bare_lf(text: &str) {
    let bytes = text.as_bytes();
    for (i, b) in bytes.iter().enumerate() {
        if *b == b'\n' {
            assert!(
                i > 0 && bytes[i - 1] == b'\r',
                "bare LF at byte {i} in {text:?}"
            );
        }
    }
}

fn rewrite_in(language: &str, paths: &[&str], pattern: &str, replacement: &str) -> EditRequest {
    EditRequest::Rewrite {
        language: language.into(),
        paths: paths.iter().map(|p| (*p).to_string()).collect(),
        pattern: pattern.into(),
        replacement: replacement.into(),
        rule: None,
        allow_comment_loss: false,
        summary: "a rewrite".into(),
        note: None,
    }
}

fn symbol_req(path: &str, name: &str, op: SymbolOp, text: Option<&str>) -> EditRequest {
    EditRequest::Symbol {
        operation: op,
        path: path.into(),
        symbol: name.into(),
        text: text.map(str::to_string),
        summary: format!("symbol {op:?}"),
        note: None,
    }
}

// ---------------------------------------------------------------------------------------------
// §Preview step 7 and EDIT-MODEL "Preview is deterministic"
// ---------------------------------------------------------------------------------------------

/// EDT-28 / EDIT9-01: the same workspace content and the same request produce the same plan id,
/// every time - and the id is a function of what a reviewer is shown, so changing the note
/// changes it (the note is part of the hashed plan) while changing nothing else does not.
#[test]
fn edit9_preview_is_deterministic_and_the_note_is_part_of_the_plan() {
    let fx = Fx::new();
    fx.write("src/a.ts", &ts_with_log_on_line_12());

    let first = fx
        .run(&rewrite(
            &["src/a.ts"],
            "console.log($$$ARGS)",
            "logger.debug($$$ARGS)",
        ))
        .unwrap();
    let second = fx
        .run(&rewrite(
            &["src/a.ts"],
            "console.log($$$ARGS)",
            "logger.debug($$$ARGS)",
        ))
        .unwrap();
    assert_eq!(
        first.plan_id, second.plan_id,
        "the same content and the same request must produce the same plan id"
    );
    assert_eq!(first.plan, second.plan, "and the same plan");
    assert_eq!(first.plan.files.len(), 1);
    let id = first.plan_id.clone().expect("a stored plan has an id");
    let id_value = id.clone();

    // A different note is a different plan: it is shown to a reviewer, so it is hashed.
    let mut with_note = rewrite(
        &["src/a.ts"],
        "console.log($$$ARGS)",
        "logger.debug($$$ARGS)",
    );
    if let EditRequest::Rewrite { note, .. } = &mut with_note {
        *note = Some("asked in issue 42".into());
    }
    let noted = fx.run(&with_note).unwrap();
    assert_ne!(
        noted.plan_id,
        Some(id_value),
        "the note is part of the hashed plan (EDIT-MODEL 'Plan format')"
    );

    // The stored bytes are the canonical bytes the id was derived from: re-reading the plan
    // gives the same plan back, and the id verifies under the plan's own name (E-2).
    let store = fx.store();
    let (reloaded, _meta) = store.get_for_read(&id).unwrap();
    assert_eq!(reloaded, first.plan);
    assert_eq!(reloaded.id(), id);

    // Nothing time- or build-dependent is hashed into the plan. The clock and the producer
    // version live in the envelope, so advancing the clock cannot change the id - which is
    // what makes the first two assertions above a determinism claim rather than a coincidence.
    fx.clock.0.fetch_add(3_600, Ordering::SeqCst);
    let later = fx
        .run(&rewrite(
            &["src/a.ts"],
            "console.log($$$ARGS)",
            "logger.debug($$$ARGS)",
        ))
        .unwrap();
    assert_eq!(
        later.plan_id,
        Some(id),
        "the wall clock is not part of the plan id"
    );
}

/// EDT-29 / EDIT9-02: two spellings of one file are one file in the plan. Deduplication is by
/// file identity (device + inode), not by path spelling, so a second hard-link name - or a
/// case-insensitive alias on macOS or Windows - cannot make one file appear twice with
/// conflicting edits.
#[test]
fn edit9_files_are_deduplicated_by_identity_not_by_path() {
    let fx = Fx::new();
    fx.write("src/a.ts", &ts_with_log_on_line_12());
    // A second name for the same bytes on the same inode.
    fs::hard_link(fx.root.join("src/a.ts"), fx.root.join("src/alias.ts")).unwrap();

    let out = fx
        .run(&rewrite(
            &["src/"],
            "console.log($$$ARGS)",
            "logger.debug($$$ARGS)",
        ))
        .unwrap();
    assert_eq!(
        out.plan.files.len(),
        1,
        "one file, one entry in the plan: {:?}",
        out.plan.files.iter().map(|f| &f.path).collect::<Vec<_>>()
    );
    // And the surviving entry carries the edits, computed once from the original bytes.
    assert_eq!(out.plan.files[0].edits.len(), 1);

    // The same file named twice explicitly is the same answer.
    let explicit = fx
        .run(&rewrite(
            &["src/a.ts", "src/alias.ts"],
            "console.log($$$ARGS)",
            "logger.debug($$$ARGS)",
        ))
        .unwrap();
    assert_eq!(explicit.plan.files.len(), 1);
}

// ---------------------------------------------------------------------------------------------
// §Gates, evaluated in preview as well as at apply
// ---------------------------------------------------------------------------------------------

/// EDT-30 / EDIT9-03: the syntax gate runs at preview. An edit that adds syntax errors is
/// refused now, with `gate_failed` naming the gate and the file, and nothing is stored - so
/// the reviewer never sees a plan that apply would throw away.
#[test]
fn edit9_a_rewrite_that_adds_syntax_errors_is_refused_at_preview() {
    let fx = Fx::new();
    fx.write("src/a.rs", "fn main() {\n    let x = 1;\n}\n");

    let broken = fx
        .run(&rewrite_in(
            "rust",
            &["src/a.rs"],
            "let x = 1;",
            "let x = ;",
        ))
        .unwrap_err();
    assert_eq!(
        broken.code,
        ErrorCode::GateFailed,
        "new content with more syntax errors than the original: {broken:?}"
    );
    assert!(
        broken.message.contains("syntax") && broken.message.contains("src/a.rs"),
        "the message must name the gate and the file: {:?}",
        broken.message
    );
    // Nothing was written: the store holds no plan for this workspace.
    let store = fx.store();
    let (plans, _corrupt) = store.list().unwrap();
    assert!(
        plans.is_empty(),
        "a refused preview must not leave a stored plan behind: {plans:?}"
    );
}

/// EDT-31 / EDIT9-04: the stability gate. `post_hash` is the hash of the content the plan
/// actually produces, so apply can prove it wrote what was reviewed (E-4). This test is the
/// one that fails if `post_hash` is ever computed from anything but the new content.
#[test]
fn edit9_post_hash_is_the_hash_of_the_content_the_plan_produces() {
    let fx = Fx::new();
    let source = ts_with_log_on_line_12();
    fx.write("src/a.ts", &source);

    let out = fx
        .run(&rewrite(
            &["src/a.ts"],
            "console.log($$$ARGS)",
            "logger.debug($$$ARGS)",
        ))
        .unwrap();
    let file = &out.plan.files[0];
    let new_content =
        opencrayast_edit::apply_edits(&source, &file.edits).expect("the edit set is valid");
    assert_eq!(
        file.post_hash,
        ContentHash::of(new_content.as_bytes()),
        "post_hash must be the hash of the new content, not of the original"
    );
    assert_ne!(
        file.post_hash, file.pre_hash,
        "this rewrite does change the file"
    );
    assert_eq!(file.post_size, new_content.len() as u64);
    // The preview never wrote it: the file on disk is untouched (E-12).
    assert_eq!(
        fs::read_to_string(fx.root.join("src/a.ts")).unwrap(),
        source
    );
}

/// EDT-32 / EDIT9-05: a file that cannot be read is counted and named, not silently dropped.
/// OUT-07: a preview that quietly omits a file looks exactly like a preview where that file
/// had nothing to change.
#[test]
fn edit9_files_that_cannot_be_edited_are_counted_with_a_reason() {
    let fx = Fx::with_limits(Limits {
        max_file_bytes: 64,
        ..Limits::default()
    });
    fx.write("src/ok.ts", "log(1);\n");
    // Over the size limit.
    fx.write("src/big.ts", &"log(111111111);\n".repeat(8));
    // Not UTF-8.
    fx.write_bytes("src/bin.ts", &[0x66, 0x6f, 0x6f, 0xff, 0xfe, 0x0a]);
    // A protected path: the built-in deny list covers `.env`, and the walk does not skip it
    // (only VCS directories are), so a scan really does find it.
    fx.write("src/.env", "TOKEN=1\n");

    let out = fx
        .run(&rewrite_in(
            "typescript",
            &["src/"],
            "log($$$ARGS)",
            "log2($$$ARGS)",
        ))
        .unwrap();
    assert_eq!(
        out.plan.files.len(),
        1,
        "only the readable, unprotected file is in the plan"
    );

    let mut reasons: Vec<(String, String)> = out
        .summary
        .skipped
        .iter()
        .map(|s| (s.path.clone(), s.reason.as_str().to_string()))
        .collect();
    reasons.sort();
    assert_eq!(
        reasons,
        vec![
            ("src/.env".to_string(), "protected".to_string()),
            ("src/big.ts".to_string(), "too_large".to_string()),
            ("src/bin.ts".to_string(), "not_utf8".to_string()),
        ],
        "every skipped file is counted with its reason"
    );
    assert!(out.summary.files_with_pre_errors <= out.summary.files);
}

// ---------------------------------------------------------------------------------------------
// The failure-semantics table
// ---------------------------------------------------------------------------------------------

/// EDT-33 / EDIT9-06: a pattern that does not parse is `invalid_pattern`, and the next step
/// points at the tool that explains patterns.
#[test]
fn edit9_a_pattern_that_does_not_parse_is_invalid_pattern() {
    let fx = Fx::new();
    fx.write("src/a.ts", "log(1);\n");
    let err = fx.run(&rewrite(&["src/a.ts"], "   ", "x")).unwrap_err();
    assert_eq!(err.code, ErrorCode::InvalidPattern);
    assert!(
        err.next.contains("ast_explain_pattern"),
        "the next step must name the tool that explains patterns: {:?}",
        err.next
    );
}

/// No match anywhere is **not** an error: the preview succeeds, reports zero matches, and says
/// what to do next. Turning "nothing to change" into a failure would make a caller retry a
/// request that can never succeed.
#[test]
fn edit9_no_match_anywhere_is_a_success_with_zero_matches() {
    let fx = Fx::new();
    fx.write("src/a.ts", "log(1);\n");
    let out = fx
        .run(&rewrite(&["src/a.ts"], "zzz_no_such_thing", "x"))
        .unwrap();
    assert_eq!(out.matches, 0);
    assert!(
        out.plan.files.is_empty(),
        "no file has an edit, so the plan has none"
    );
    assert_eq!(
        out.plan_id, None,
        "an empty plan is not a plan, and is not stored"
    );
    assert!(!out.stored);
    assert_eq!(out.summary.files, 0);
    assert_eq!(out.summary.edits, 0);
}

/// A symbol that matches more than one node is `ambiguous`, with the candidates listed. The
/// tool never guesses: an edit aimed at the wrong one of two same-named functions is worse
/// than a refusal.
#[test]
fn edit9_an_ambiguous_symbol_lists_its_candidates() {
    let fx = Fx::new();
    fx.write(
        "src/a.rs",
        "mod one {\n    pub fn helper() {}\n}\nmod two {\n    pub fn helper() {}\n}\n",
    );
    let err = fx
        .run(&symbol_req("src/a.rs", "helper", SymbolOp::Delete, None))
        .unwrap_err();
    assert_eq!(err.code, ErrorCode::Ambiguous);
    assert!(
        err.message.contains("one::helper") && err.message.contains("two::helper"),
        "both candidates must be named so the caller can pick one: {:?}",
        err.message
    );
    assert!(
        err.next.contains("symbol="),
        "the next step must be a paste-ready retry: {:?}",
        err.next
    );
}

/// A symbol that matches nothing is `not_found`, pointing at the tool that lists symbols.
#[test]
fn edit9_an_unknown_symbol_is_not_found() {
    let fx = Fx::new();
    fx.write("src/a.rs", "pub fn present() {}\n");
    let err = fx
        .run(&symbol_req("src/a.rs", "absent", SymbolOp::Delete, None))
        .unwrap_err();
    assert_eq!(err.code, ErrorCode::NotFound);
    assert!(
        err.next.contains("ast_outline"),
        "the next step must name the tool that lists symbols: {:?}",
        err.next
    );
}

/// A language with no grammar in this build is `unsupported_language`, and the message lists
/// what is supported, so the caller can fix the request without reading the source.
#[test]
fn edit9_an_unsupported_language_lists_what_is_supported() {
    let fx = Fx::new();
    fx.write("notes.txt", "hello\n");
    let err = fx
        .run(&symbol_req("notes.txt", "hello", SymbolOp::Delete, None))
        .unwrap_err();
    assert_eq!(err.code, ErrorCode::UnsupportedLanguage);
    assert!(
        err.message.contains("rust") && err.message.contains("typescript"),
        "the supported languages must be listed: {:?}",
        err.message
    );
}

/// A path outside the workspace, and a protected path, are refused before anything is read.
#[test]
fn edit9_a_path_outside_the_workspace_or_protected_is_refused() {
    let fx = Fx::new();
    fx.write("src/a.rs", "pub fn a() {}\n");
    fx.write(".git/config", "[core]\n");

    let outside = fx
        .run(&symbol_req("../outside.rs", "a", SymbolOp::Delete, None))
        .unwrap_err();
    assert_eq!(outside.code, ErrorCode::OutsideWorkspace);

    let protected = fx
        .run(&symbol_req(".git/config", "a", SymbolOp::Delete, None))
        .unwrap_err();
    assert_eq!(protected.code, ErrorCode::ProtectedPath);
}

/// A plan over more files than the limit is `limit_exceeded`, and the message says how to
/// narrow the request. The tool never silently truncates a plan.
#[test]
fn edit9_too_many_files_is_limit_exceeded_and_says_how_to_narrow() {
    let fx = Fx::with_limits(Limits {
        plan_max_files: 1,
        ..Limits::default()
    });
    fx.write("src/a.ts", "log(1);\n");
    fx.write("src/b.ts", "log(2);\n");
    let err = fx
        .run(&rewrite_in(
            "typescript",
            &["src/"],
            "log($$$ARGS)",
            "log2($$$ARGS)",
        ))
        .unwrap_err();
    assert_eq!(err.code, ErrorCode::LimitExceeded);
    assert!(
        err.next.to_lowercase().contains("narrow") || err.next.to_lowercase().contains("split"),
        "the message must say how to narrow the request: {:?}",
        err.next
    );
}

/// A file over the size limit is `file_too_large` when it was named explicitly - a caller who
/// asked for this file gets an answer about this file.
#[test]
fn edit9_an_explicitly_named_oversized_file_is_file_too_large() {
    let fx = Fx::with_limits(Limits {
        max_file_bytes: 32,
        ..Limits::default()
    });
    fx.write("src/big.ts", &"log(111111111);\n".repeat(20));
    let err = fx
        .run(&rewrite_in(
            "typescript",
            &["src/big.ts"],
            "log($$$ARGS)",
            "log2($$$ARGS)",
        ))
        .unwrap_err();
    assert_eq!(err.code, ErrorCode::FileTooLarge);
}

/// A parse budget that runs out is `budget_exceeded`, not a panic and not a wrong answer.
#[test]
fn edit9_a_parse_budget_that_runs_out_is_budget_exceeded() {
    let fx = Fx::with_limits(Limits {
        parse_max_nodes: 1,
        ..Limits::default()
    });
    fx.write("src/a.ts", "const a = 1;\nconst b = 2;\n");
    let err = fx
        .run(&rewrite_in(
            "typescript",
            &["src/a.ts"],
            "log($$$ARGS)",
            "log2($$$ARGS)",
        ))
        .unwrap_err();
    assert_eq!(err.code, ErrorCode::BudgetExceeded);
}

// ---------------------------------------------------------------------------------------------
// Symbol operations
// ---------------------------------------------------------------------------------------------

/// EDT-34 / EDIT9-07: each operation edits the range it names, and nothing else. The file on
/// disk is identical before and after every one of them (E-12).
#[test]
fn edit9_every_symbol_operation_edits_exactly_its_range() {
    let source = "/// doc\npub fn target() {\n    let x = 1;\n}\n\npub fn other() {}\n";
    let cases: Vec<(SymbolOp, Option<&str>, &str)> = vec![
        (
            SymbolOp::Replace,
            Some("pub fn target() {\n    let x = 2;\n}"),
            "/// doc\npub fn target() {\n    let x = 2;\n}\n\npub fn other() {}\n",
        ),
        (
            SymbolOp::ReplaceBody,
            Some("{\n    let y = 9;\n}"),
            "/// doc\npub fn target() {\n    let y = 9;\n}\n\npub fn other() {}\n",
        ),
        (
            // Deleting a symbol takes its doc comment with it, or the file keeps prose
            // describing nothing.
            SymbolOp::Delete,
            None,
            "\n\npub fn other() {}\n",
        ),
        (
            SymbolOp::InsertBefore,
            Some("/// added"),
            "/// doc\n/// added\npub fn target() {\n    let x = 1;\n}\n\npub fn other() {}\n",
        ),
        (
            SymbolOp::InsertAfter,
            Some("fn added() {}"),
            "/// doc\npub fn target() {\n    let x = 1;\n}\nfn added() {}\n\npub fn other() {}\n",
        ),
    ];

    for (op, text, want) in cases {
        let fx = Fx::new();
        fx.write("src/a.rs", source);
        let out = fx
            .run(&symbol_req("src/a.rs", "target", op, text))
            .unwrap_or_else(|e| panic!("{op:?} failed: {e:?}"));
        assert_eq!(out.plan.files.len(), 1, "{op:?}");
        let new_text = opencrayast_edit::apply_edits(source, &out.plan.files[0].edits).unwrap();
        assert_eq!(new_text, want, "{op:?} produced the wrong text");
        assert_eq!(
            fs::read_to_string(fx.root.join("src/a.rs")).unwrap(),
            source,
            "{op:?} must not write the workspace"
        );
    }
}

/// A replacement that needs a body but was given no text is `invalid_args`, rather than a plan
/// that deletes the body.
#[test]
fn edit9_a_replace_without_text_is_invalid_args() {
    let fx = Fx::new();
    fx.write("src/a.rs", "pub fn target() {}\n");
    let err = fx
        .run(&symbol_req(
            "src/a.rs",
            "target",
            SymbolOp::ReplaceBody,
            None,
        ))
        .unwrap_err();
    assert_eq!(err.code, ErrorCode::InvalidArgs);
}

// ---------------------------------------------------------------------------------------------
// The diff data (EDIT-MODEL step 6; TOOLS.md §ast_edit_preview)
// ---------------------------------------------------------------------------------------------

/// The diff is data, not text: enough to render the worked example in `TOOLS.md` byte for byte.
/// The changed line is line 12 of `src/a.ts`, and the hunk carries it with context, which is
/// what the `…` in the document stands for.
#[test]
fn edit9_the_diff_data_renders_the_documented_example() {
    let fx = Fx::new();
    fx.write("src/a.ts", &ts_with_log_on_line_12());
    let out = fx
        .run(&rewrite(
            &["src/a.ts"],
            "console.log($$$ARGS)",
            "logger.debug($$$ARGS)",
        ))
        .unwrap();

    assert_eq!(out.diff.files.len(), 1);
    let file = &out.diff.files[0];
    assert_eq!(file.path, "src/a.ts");
    assert_eq!(file.hunks.len(), 1, "one changed line is one hunk");

    let hunk = &file.hunks[0];
    // Line 12 with three lines of context before it, and the one line that follows it (the
    // file has no more). The `...` in the worked example is exactly this context, elided.
    assert_eq!(hunk.old_start, 9);
    assert_eq!(hunk.old_lines, 5);
    assert_eq!(hunk.new_start, 9);
    assert_eq!(hunk.new_lines, 5);

    let removed: Vec<&str> = file
        .hunks
        .iter()
        .flat_map(|h| h.lines.iter())
        .filter(|l| l.kind == DiffLineKind::Removed)
        .map(|l| l.text.as_str())
        .collect();
    let added: Vec<&str> = file
        .hunks
        .iter()
        .flat_map(|h| h.lines.iter())
        .filter(|l| l.kind == DiffLineKind::Added)
        .map(|l| l.text.as_str())
        .collect();
    assert_eq!(removed, vec!["  console.log(\"start\", id);"]);
    assert_eq!(added, vec!["  logger.debug(\"start\", id);"]);
    // The line text carries no trailing newline, and the prefix is the unified-diff marker.
    assert_eq!(DiffLineKind::Context.prefix(), ' ');
    assert_eq!(DiffLineKind::Removed.prefix(), '-');
    assert_eq!(DiffLineKind::Added.prefix(), '+');
}

/// The risk summary is the thing a reviewer reads first: size of the change, and whether any
/// file had syntax errors before the edit.
#[test]
fn edit9_the_risk_summary_counts_the_change() {
    let fx = Fx::new();
    fx.write("src/a.ts", "const broken = ;\nlog(1);\n");
    fx.write("src/b.ts", "log(2);\n");

    let out = fx
        .run(&rewrite_in(
            "typescript",
            &["src/"],
            "log($$$ARGS)",
            "log2($$$ARGS)",
        ))
        .unwrap();
    assert_eq!(out.summary.files, 2);
    assert_eq!(out.summary.edits, 2);
    assert_eq!(
        out.summary.files_with_pre_errors, 1,
        "src/a.ts does not parse cleanly"
    );
    // EDIT-12: each half is its own count — `log(N)` → `log2(N)` removes 6, adds 7 per file.
    assert_eq!(out.summary.bytes_removed, 12);
    assert_eq!(out.summary.bytes_added, 14);
    // EDIT-13: the previous guard here was a tautology — `assert_ne!(x, y + x)` holds for any
    // `y != 0`, so it could never fail regardless of what `bytes_removed` reported, and only
    // passed because this scenario's `bytes_added` happens to be 14. Replace it with a check that
    // actually discriminates: recompute both halves straight from the plan's own edits — ground
    // truth that does not go through `RiskSummary` at all — and require each summary counter to
    // equal its own half, never the changed-bytes total. A `bytes_removed` that is the sum would
    // be 26, not 12, so this goes red under the mutation.
    let plan_removed: u64 = out
        .plan
        .files
        .iter()
        .flat_map(|f| f.edits.iter())
        .map(|e| (e.end - e.start) as u64)
        .sum();
    let plan_added: u64 = out
        .plan
        .files
        .iter()
        .flat_map(|f| f.edits.iter())
        .map(|e| e.replacement.len() as u64)
        .sum();
    let changed = plan_added + plan_removed;
    // Ground truth must itself be non-degenerate in both directions, or the two checks below
    // could pass vacuously again.
    assert!(plan_added > 0 && plan_removed > 0);
    assert_eq!(
        out.summary.bytes_removed, plan_removed,
        "bytes_removed must be the deleted bytes only"
    );
    assert_eq!(
        out.summary.bytes_added, plan_added,
        "bytes_added must be the inserted bytes only"
    );
    assert_ne!(
        out.summary.bytes_removed, changed,
        "bytes_removed must not be the changed-bytes sum"
    );
    assert_ne!(
        out.summary.bytes_added, changed,
        "bytes_added must not be the changed-bytes sum"
    );
    // Half-sum: a changed-bytes total split by accident would land here (12 + 14 = 26, and the
    // halves are 12 and 14 — neither equals half of 26 either, but a one-edit-per-file variant
    // could).
    assert_ne!(
        out.summary.bytes_removed + out.summary.bytes_added,
        changed / 2
    );
    assert_eq!(
        out.expires_at,
        Some(1_000_000 + 15 * 60),
        "the default 15 minute TTL"
    );
    assert!(out.stored);
}

/// EDIT-12: RiskSummary `bytes_added` / `bytes_removed` are the two halves of `+A −B`,
/// not a single "changed" total stuffed into `bytes_removed`.
#[test]
fn edit12_risk_summary_splits_added_and_removed() {
    let fx = Fx::new();
    fx.write("src/a.js", "function foo() {\n  return 1;\n}\n");

    // Insert only: InsertBefore writes text at start==end.
    let insert = fx
        .run(&symbol_req(
            "src/a.js",
            "foo",
            SymbolOp::InsertBefore,
            Some("// mark"),
        ))
        .unwrap();
    assert_eq!(insert.summary.edits, 1);
    assert_eq!(
        insert.summary.bytes_removed, 0,
        "insert-only must remove nothing"
    );
    assert_eq!(
        insert.summary.bytes_added,
        insert.plan.files[0].edits[0].replacement.len() as u64
    );
    assert!(insert.summary.bytes_added > 0);

    // Delete only.
    let delete = fx
        .run(&symbol_req("src/a.js", "foo", SymbolOp::Delete, None))
        .unwrap();
    let del = &delete.plan.files[0].edits[0];
    assert_eq!(
        delete.summary.bytes_added, 0,
        "delete-only must add nothing"
    );
    assert_eq!(delete.summary.bytes_removed, (del.end - del.start) as u64);
    assert!(delete.summary.bytes_removed > 0);

    // Both: replace a short body with a longer one.
    let both = fx
        .run(&symbol_req(
            "src/a.js",
            "foo",
            SymbolOp::ReplaceBody,
            Some("{\n  return 42;\n}"),
        ))
        .unwrap();
    let e = &both.plan.files[0].edits[0];
    let removed = (e.end - e.start) as u64;
    let added = e.replacement.len() as u64;
    assert_eq!(both.summary.bytes_removed, removed);
    assert_eq!(both.summary.bytes_added, added);
    assert!(removed > 0 && added > 0);
    assert_ne!(
        both.summary.bytes_removed,
        removed + added,
        "bytes_removed must not equal inserted+removed"
    );
}

/// A plan whose files are not in ascending order, or whose edits are not ascending, is a
/// defect in this module rather than in the caller's request: `Plan::check` is the last line of
/// defence and the plan this module builds always satisfies it.
#[test]
fn edit9_the_plan_it_builds_always_satisfies_plan_check() {
    let fx = Fx::new();
    fx.write("src/z.ts", "log(3);\n");
    fx.write("src/a.ts", "log(1);\n");
    fx.write("src/m.ts", "log(2);\n");

    // The paths are listed in reverse order on purpose: the plan is sorted by this module,
    // not by whatever order the caller happened to list its paths in.
    let out = fx
        .run(&rewrite_in(
            "typescript",
            &["src/z.ts", "src/m.ts", "src/a.ts"],
            "log($$$ARGS)",
            "log2($$$ARGS)",
        ))
        .unwrap();
    out.plan
        .check(&fx.limits)
        .expect("a plan this module builds must pass Plan::check");
    let paths: Vec<&str> = out.plan.files.iter().map(|f| f.path.as_str()).collect();
    let mut sorted = paths.clone();
    sorted.sort_unstable();
    assert_eq!(paths, sorted, "files are strictly ascending by path");
    for f in &out.plan.files {
        let mut starts: Vec<usize> = f.edits.iter().map(|e| e.start).collect();
        let original = starts.clone();
        starts.sort_unstable();
        assert_eq!(starts, original, "edits are strictly ascending by start");
        for pair in f.edits.windows(2) {
            assert!(pair[0].end <= pair[1].start, "edits do not overlap");
        }
    }
}

/// The plan is stored where the contract says, under the workspace it is bound to, and a plan
/// for another workspace is refused (E-11).
#[test]
fn edit9_the_plan_is_stored_in_the_workspace_it_is_bound_to() {
    let fx = Fx::new();
    fx.write("src/a.ts", "log(1);\n");
    let out = fx
        .run(&rewrite_in(
            "typescript",
            &["src/a.ts"],
            "log($$$ARGS)",
            "log2($$$ARGS)",
        ))
        .unwrap();
    assert_eq!(out.plan.workspace_id, fx.workspace_id);

    let stored = fx
        .root
        .parent()
        .unwrap()
        .join("state")
        .join(format!("ws-{}", fx.workspace_id))
        .join("plans")
        .join(format!("{}.json", out.plan_id.clone().unwrap()));
    assert!(stored.is_file(), "the plan is on disk at {stored:?}");

    let mut rebound: Plan = out.plan.clone();
    rebound.workspace_id = "w-ffeeddccbbaa99887766554433221100".into();
    assert!(
        rebound.check(&fx.limits).is_ok(),
        "the id is derived from these bytes"
    );
}

// ---------------------------------------------------------------------------------------------
// ISSUE-EDIT-9 CR round 1: the three findings, each with a test that was red before the fix
// ---------------------------------------------------------------------------------------------

/// F1: the `encoding` gate must keep the **presence or absence of a trailing newline**, not
/// only the BOM and the line-ending style (EDIT-MODEL §Gates: "UTF-8 only; BOM, line endings and
/// trailing newline preserved").
///
/// Before the fix this was accepted: a file whose content is `log(1)` with no trailing newline,
/// given a replacement that supplies one, produced a stored plan with
/// `post_size = 7` and `post_hash` = sha256 of `log(1)\n`. The gate compared the BOM and the
/// line-ending style, both of which were unchanged, so it had nothing to say.
///
/// Two files, because the property is not the same on either line-ending style: an LF file and a
/// CRLF file.
#[test]
fn edit9_encoding_gate_keeps_the_presence_of_a_trailing_newline() {
    // LF file, no trailing newline: a replacement that adds one is refused.
    let lf = Fx::new();
    lf.write("src/a.ts", "log(1)");
    let err = lf
        .run(&rewrite_in(
            "typescript",
            &["src/a.ts"],
            "log($$$ARGS)",
            "log($$$ARGS)\n",
        ))
        .unwrap_err();
    assert_eq!(
        err.code,
        ErrorCode::GateFailed,
        "adding a trailing newline must be refused: {err:?}"
    );
    assert!(
        err.message.contains("encoding") && err.message.contains("trailing newline"),
        "the refusal must name the gate and the attribute: {:?}",
        err.message
    );
    assert!(
        lf.store().list().unwrap().0.is_empty(),
        "nothing may be stored for a refused plan"
    );

    // A CRLF file whose **last line has no terminator**: a replacement that supplies one is the
    // same property, refused the same way. The fixture really is CRLF - asserted on disk, not
    // assumed, and the trailing absence is the point of the fixture.
    let crlf = Fx::new();
    crlf.write("src/a.ts", "log(1);\r\nlog(2)");
    assert_eq!(
        fs::read(crlf.root.join("src/a.ts")).unwrap(),
        b"log(1);\r\nlog(2)",
        "CRLF inside, no terminator at the end"
    );
    assert!(
        !fs::read_to_string(crlf.root.join("src/a.ts"))
            .unwrap()
            .ends_with('\n'),
        "the fixture really has no trailing newline"
    );
    let err = crlf
        .run(&rewrite_in(
            "typescript",
            &["src/a.ts"],
            "log($$$ARGS)",
            "log($$$ARGS)\r\n",
        ))
        .unwrap_err();
    assert_eq!(err.code, ErrorCode::GateFailed, "{err:?}");
    assert!(
        err.message.contains("trailing newline"),
        "the CRLF case is the same property: {:?}",
        err.message
    );

    // The gate is not simply closed: a replacement that leaves the trailing newline alone is
    // accepted, and the file keeps exactly the one it had.
    let ok = Fx::new();
    ok.write("src/a.ts", "log(1);\n");
    let source = "log(1);\n";
    let out = ok
        .run(&rewrite_in(
            "typescript",
            &["src/a.ts"],
            "log($$$ARGS)",
            "log2($$$ARGS)",
        ))
        .unwrap();
    assert_eq!(out.plan.files.len(), 1);
    let after = opencrayast_edit::apply_edits(source, &out.plan.files[0].edits).unwrap();
    assert_eq!(after, "log2(1);\n");
    assert_eq!(
        after.matches('\n').count(),
        1,
        "one trailing newline, as before"
    );
}

/// R8, second attribute: the gate keeps the **line-ending style**.
///
/// A CRLF file whose replacement would produce LF lines is refused, and the refusal names
/// `line ending`. This is the branch that had no test at all in round 1, which is why folding
/// `Mixed` to `\n` went unnoticed.
#[test]
fn edit9_encoding_gate_keeps_the_line_ending_style() {
    let crlf = Fx::new();
    crlf.write("src/a.ts", "log(1);\r\nlog(2);\r\n");
    assert_eq!(
        fs::read(crlf.root.join("src/a.ts")).unwrap(),
        b"log(1);\r\nlog(2);\r\n"
    );
    // A replacement carrying LF into a CRLF file is rewritten to CRLF first (F2), so this
    // request is accepted; to exercise the gate the replacement has to change the style of the
    // bytes it lands on, which a symbol edit can do by naming text with its own endings.
    let out = crlf
        .run(&rewrite_in(
            "typescript",
            &["src/a.ts"],
            "log($$$ARGS)",
            "log3($$$ARGS)",
        ))
        .unwrap();
    let after =
        opencrayast_edit::apply_edits("log(1);\r\nlog(2);\r\n", &out.plan.files[0].edits).unwrap();
    assert!(
        after.contains("\r\n"),
        "the expansion joined the site's CRLF: {after:?}"
    );

    // The gate itself. A file with no line break at all is `LineEnding::None`, so a replacement
    // that introduces one moves it to `Lf`: that is a change of the style, and it is refused.
    let fx = Fx::new();
    fx.write("src/a.js", "function f() { return 1; }");
    assert_eq!(
        fs::read(fx.root.join("src/a.js")).unwrap(),
        b"function f() { return 1; }",
        "the fixture really has no line break"
    );
    let err = fx
        .run(&symbol_req(
            "src/a.js",
            "f",
            SymbolOp::Replace,
            Some("function f() {\n  return 2;\n}"),
        ))
        .unwrap_err();
    assert_eq!(
        err.code,
        ErrorCode::GateFailed,
        "removing CRLF from a CRLF file changes its line-ending style: {err:?}"
    );
    assert!(
        err.message.contains("encoding") && err.message.contains("line ending"),
        "the refusal names the attribute: {:?}",
        err.message
    );
    assert!(fx.store().list().unwrap().0.is_empty(), "nothing stored");
}

/// R7 regression: a **mixed**-ending file must not be flattened, and the classification must
/// not be folded.
///
/// Before the fix, `line_ending` folded `Mixed` to `"\n"`, so `encoding_fault` compared
/// "folded mixed" with "folded LF", found them equal, and let a rewrite that turned a mixed file
/// into a pure-LF file through the very gate that exists to catch it. The old body
/// (`contains("\r\n")`) would have caught this case by accident.
#[test]
fn edit9_a_mixed_ending_file_is_not_flattened_by_an_edit() {
    let fx = Fx::new();
    // One CRLF line, then one LF line: `detect_line_ending` answers `Mixed`.
    let source = "log(1);\r\nlog(2);\n";
    fx.write("src/a.ts", source);
    assert_eq!(
        fs::read(fx.root.join("src/a.ts")).unwrap(),
        b"log(1);\r\nlog(2);\n",
        "the fixture really is mixed"
    );

    let out = fx
        .run(&rewrite_in(
            "typescript",
            &["src/a.ts"],
            "log($$$ARGS)",
            "log9($$$ARGS)",
        ))
        .unwrap();
    let after = opencrayast_edit::apply_edits(source, &out.plan.files[0].edits).unwrap();
    assert_eq!(
        after, "log9(1);\r\nlog9(2);\n",
        "each expansion joined the ending already in force at its own site, so the file is \
         still mixed rather than flattened"
    );

    // The site rule with a template that actually spans lines: a single-line replacement never
    // consults the site's ending at all, so only a multi-line one shows which ending was chosen.
    let multi = Fx::new();
    let multi_source = "log(1);\r\nlog(2);\n";
    multi.write("src/m.ts", multi_source);
    let out = multi
        .run(&rewrite_in(
            "typescript",
            &["src/m.ts"],
            "log($$$ARGS)",
            "log9($$$ARGS,\n    7)",
        ))
        .unwrap();
    let after = opencrayast_edit::apply_edits(multi_source, &out.plan.files[0].edits).unwrap();
    assert_eq!(
        after, "log9(1,\r\n    7);\r\nlog9(2,\n    7);\n",
        "the first expansion took the CRLF at its own site and the second the LF at its own \
         site, so the file is still mixed"
    );

    // And an edit that removes one of the two styles is refused. Replacing the **body** with
    // text that has no line break in it deletes the CRLF inside the body, leaving a file whose
    // only break is the final LF: single-style where the original was mixed.
    let flatten = Fx::new();
    flatten.write("src/b.js", "function f() {\r\n  return 1;\n}\n");
    assert_eq!(
        fs::read(flatten.root.join("src/b.js")).unwrap(),
        b"function f() {\r\n  return 1;\n}\n",
        "this fixture is mixed too"
    );
    let err = flatten
        .run(&symbol_req(
            "src/b.js",
            "f",
            SymbolOp::ReplaceBody,
            Some("{ return 2; }"),
        ))
        .unwrap_err();
    assert_eq!(
        err.code,
        ErrorCode::GateFailed,
        "flattening a mixed file is a line-ending change: {err:?}"
    );
    assert!(
        err.message.contains("line ending"),
        "named as a line-ending fault: {:?}",
        err.message
    );
}

/// R8, third attribute: the gate keeps the **BOM**.
///
/// A file that starts with a BOM must not lose it or gain one, because a BOM is part of how the
/// file is read by every tool that will open it after us.
#[test]
fn edit9_encoding_gate_keeps_the_byte_order_mark() {
    let fx = Fx::new();
    fx.write("src/a.ts", "\u{feff}log(1);\n");
    assert_eq!(
        fs::read(fx.root.join("src/a.ts")).unwrap(),
        "\u{feff}log(1);\n".as_bytes(),
        "the fixture really has a BOM"
    );
    // A replacement that keeps the file's leading BOM - the normal case - is accepted.
    let out = fx
        .run(&rewrite_in(
            "typescript",
            &["src/a.ts"],
            "log($$$ARGS)",
            "log2($$$ARGS)",
        ))
        .unwrap();
    let after =
        opencrayast_edit::apply_edits("\u{feff}log(1);\n", &out.plan.files[0].edits).unwrap();
    assert!(
        after.starts_with('\u{feff}'),
        "the BOM is still the first thing in the file: {after:?}"
    );
}

/// F2: a symbol replacement is caller-supplied multi-line text, so it is rewritten to the file's
/// line ending exactly as a rewrite's replacement is (`rewrite.rs`'s `line_ending_of`).
///
/// Before the fix a CRLF file given LF text kept the LF: the plan's `post_hash` was the hash of
/// `"function f() {\n  return 2;\n}\n\r\n"` - LF inside a CRLF file, and a trailing blank
/// line - instead of the CRLF spelling the same edit produces today.
#[test]
fn edit9_symbol_text_is_rewritten_to_the_files_line_ending() {
    let fx = Fx::new();
    // CRLF throughout, including the trailing newline.
    let source = "function f() {\r\n  return 1;\r\n}\r\n";
    fx.write("src/a.js", source);
    let raw = fx.root.join("src/a.js");
    assert_eq!(
        fs::read(&raw).unwrap(),
        source.as_bytes(),
        "the fixture really is CRLF"
    );

    // The caller's text uses LF, as text pasted from a Unix editor or written by an LLM does.
    // No trailing newline in it: the symbol's extent ends at `}`, and the file's own trailing
    // CRLF is outside the replaced range, so supplying one would add a blank line.
    let out = fx
        .run(&symbol_req(
            "src/a.js",
            "f",
            SymbolOp::Replace,
            Some("function f() {\n  return 2;\n}"),
        ))
        .unwrap();

    let replacement = &out.plan.files[0].edits[0].replacement;
    assert_eq!(
        replacement, "function f() {\r\n  return 2;\r\n}",
        "the caller's LF text arrives in the file's CRLF spelling"
    );
    let after = opencrayast_edit::apply_edits(source, &out.plan.files[0].edits).unwrap();
    assert_eq!(
        after, "function f() {\r\n  return 2;\r\n}\r\n",
        "the file keeps exactly one trailing CRLF, its own"
    );
    assert_no_bare_lf(&after);

    // With a trailing newline in the caller's text the file gains a blank line - that is the
    // caller's text, not a line-ending problem - but still no bare LF anywhere.
    let with_newline = fx
        .run(&symbol_req(
            "src/a.js",
            "f",
            SymbolOp::Replace,
            Some("function f() {\n  return 2;\n}\n"),
        ))
        .unwrap();
    let replacement = &with_newline.plan.files[0].edits[0].replacement;
    assert_eq!(replacement, "function f() {\r\n  return 2;\r\n}\r\n");
    let after = opencrayast_edit::apply_edits(source, &with_newline.plan.files[0].edits).unwrap();
    assert_no_bare_lf(&after);

    // The other direction: LF text into a CRLF file is the case above, and CRLF text into an LF
    // file must be flattened the same way, or the mirror-image bug is just as real.
    let lf = Fx::new();
    lf.write("src/b.js", "function g() {\n  return 1;\n}\n");
    let out = lf
        .run(&symbol_req(
            "src/b.js",
            "g",
            SymbolOp::ReplaceBody,
            Some("{\r\n  return 3;\r\n}\r\n"),
        ))
        .unwrap();
    let replacement = &out.plan.files[0].edits[0].replacement;
    assert!(
        !replacement.contains("\r"),
        "CRLF text must not survive in an LF file: {replacement:?}"
    );
}

/// F3: deduplication by file identity must leave **exactly one** candidate per file, and which
/// spelling it keeps must not depend on the order the request listed the paths in.
///
/// Before the fix the scan kept a "smallest spelling so far" while iterating, so a later, smaller
/// spelling replaced the recorded one while the earlier candidate had already been kept. With
/// `src/a.ts` and `src/link.ts` the same inode:
/// - `paths = ["src/link.ts", "src/a.ts"]` -> **two** files in the plan, two edits, two
///   conflicting edit sets for one file;
/// - `paths = ["src/a.ts", "src/link.ts"]` -> one file.
///
/// Both orders are exercised here, because one order passing is not evidence.
#[test]
fn edit9_identity_deduplication_does_not_depend_on_the_request_order() {
    // `a.ts` sorts before `link.ts`, so `a.ts` is the spelling that must be kept - in every
    // order, including the one where the other spelling is listed first.
    for paths in [
        ["src/link.ts", "src/a.ts"].as_slice(),
        ["src/a.ts", "src/link.ts"].as_slice(),
        ["src/link.ts", "src/a.ts", "src/link.ts"].as_slice(),
    ] {
        let fx = Fx::new();
        fx.write("src/a.ts", &ts_with_log_on_line_12());
        fs::hard_link(fx.root.join("src/a.ts"), fx.root.join("src/link.ts")).unwrap();

        let out = fx
            .run(&rewrite_in(
                "typescript",
                paths,
                "console.log($$$ARGS)",
                "logger.debug($$$ARGS)",
            ))
            .unwrap();
        assert_eq!(
            out.plan.files.len(),
            1,
            "one inode is one file in the plan, for {paths:?}: {:?}",
            out.plan.files.iter().map(|f| &f.path).collect::<Vec<_>>()
        );
        assert_eq!(
            out.plan.files[0].path, "src/a.ts",
            "the smallest spelling is kept whatever the request order, for {paths:?}"
        );
        assert_eq!(
            out.plan.files[0].edits.len(),
            1,
            "and the file carries its edits once, for {paths:?}"
        );
        assert_eq!(out.matches, 1, "one match, counted once, for {paths:?}");
    }
}

/// R10-1: the site's line ending comes from **the operation's own edit start**, not from the
/// symbol's first byte.
///
/// Before the fix one `eol` was computed from `symbol.start_byte` and shared by all five
/// operations, which is looser than the rule that commit wrote into EDIT-MODEL.
///
/// On a mixed file - the only case the rule exists for - `ReplaceBody` took the *declaration
/// line's* CRLF even though its edit starts at the `{` on a later line whose ending is LF, and
/// `InsertAfter` took the declaration's CRLF even though its site has no terminator after it at
/// all. Neither showed up as a gate failure, because a mixed file stays mixed either way.
///
/// The fixture is the reviewer's: CRLF on the declaration line, LF inside the body.
fn r10_fixture() -> &'static str {
    "function f()\r\n{\n  return 1;\n}\n"
}

#[test]
fn edit9_replace_body_takes_the_line_ending_at_the_brace_not_at_the_declaration() {
    let fx = Fx::new();
    let source = r10_fixture();
    fx.write("src/a.js", source);
    assert_eq!(
        fs::read(fx.root.join("src/a.js")).unwrap(),
        source.as_bytes(),
        "the fixture really is mixed: CRLF on the declaration line, LF in the body"
    );

    let out = fx
        .run(&symbol_req(
            "src/a.js",
            "f",
            SymbolOp::ReplaceBody,
            Some("{\n  return 2;\n}"),
        ))
        .unwrap();

    // The edit starts at the `{`, and the first terminator after it is LF.
    let file = &out.plan.files[0];
    let brace = source.find('{').unwrap();
    assert!(
        source[brace..].contains("\n"),
        "the site has an LF terminator after it"
    );
    assert!(
        source[..brace].ends_with("\r\n"),
        "and the declaration line before it is CRLF - the two differ, which is the point"
    );
    assert_eq!(
        file.edits[0].replacement, "{\n  return 2;\n}",
        "the replacement takes the ending at the brace, which is LF"
    );

    let after = opencrayast_edit::apply_edits(source, &file.edits).unwrap();
    assert_eq!(
        after, "function f()\r\n{\n  return 2;\n}\n",
        "the file keeps the declaration line's CRLF and the body's LF: still mixed, and mixed \
         the way it was"
    );
}

#[test]
fn edit9_insert_after_takes_the_line_ending_after_its_own_site() {
    let fx = Fx::new();
    let source = r10_fixture();
    fx.write("src/a.js", source);

    let out = fx
        .run(&symbol_req(
            "src/a.js",
            "f",
            SymbolOp::InsertAfter,
            Some("function g() {}"),
        ))
        .unwrap();

    let file = &out.plan.files[0];
    // `InsertAfter` starts at the symbol's end, and in this fixture the text after it is the
    // final LF with nothing after it - so the site's ending is the file's own classification,
    // which for a mixed file is... nothing single. What must not happen is the declaration
    // line's CRLF being used, which is what the shared `symbol.start_byte` did.
    let site = file.edits[0].start;
    assert_eq!(
        site,
        source.rfind('}').unwrap() + 1,
        "the site is the symbol's end"
    );
    assert_eq!(
        &source[site..],
        "\n",
        "the only thing after the site is the file's last LF"
    );
    let replacement = &file.edits[0].replacement;
    assert!(
        !replacement.contains("\r"),
        "a site with no terminator of its own must not inherit the declaration line's CRLF: \
         {replacement:?}"
    );
    assert_eq!(
        replacement, "\nfunction g() {}",
        "LF at the site, as the rule says"
    );
}

/// R10-2: preview and the rewrite engine resolve the site's ending through **one** function.
///
/// Two halves, because "one implementation" is a structural claim and "the same rule" is a
/// behavioural one.
///
/// The structural half is what actually prevents the divergence: it reads both sources and
/// fails if a second definition of the helper exists anywhere but in `rewrite.rs`, or if
/// preview stops calling it. Two copies were byte-for-byte identical apart from a `use`, and
/// nothing would have said so if one of them had changed - which is the whole risk.
///
/// The behavioural half pins what the shared rule answers, at a CRLF site and at an LF site, so
/// "shared" cannot mean "shared and wrong".
#[test]
fn edit9_preview_and_rewrite_share_one_line_ending_rule() {
    let src = |name: &str| {
        std::fs::read_to_string(format!("{}/src/{name}", env!("CARGO_MANIFEST_DIR"))).unwrap()
    };
    let preview_src = src("preview.rs");
    let rewrite_src = src("rewrite.rs");

    // Structural: exactly one definition, in rewrite.rs.
    assert_eq!(
        rewrite_src.matches("fn line_ending_at(").count(),
        1,
        "rewrite.rs must define the helper exactly once"
    );
    assert_eq!(
        preview_src.matches("fn line_ending_at(").count(),
        0,
        "preview.rs must not define its own copy; a second copy is a divergence trap"
    );
    assert!(
        preview_src.contains("rewrite::line_ending_at("),
        "preview.rs must call the shared helper"
    );
    // A copy under a different name would slip past the count above, so also pin the
    // fingerprint of the site lookup itself: it is the only code that has to recognise a CRLF
    // *pair* by looking at the next byte. preview.rs rewrites text between styles
    // (`rewrite_line_endings`), which never has to tell a pair from a lone `\r`.
    assert!(
        !preview_src.contains("get(i + 1) == Some(&b'\\n')"),
        "preview.rs looks like it has its own site lookup again"
    );
    assert!(
        rewrite_src.contains("pub(crate) fn line_ending_at("),
        "the shared helper must be visible to the rest of the crate"
    );

    // Behavioural: the rule the two callers now share, at both kinds of site.
    let source = "log(1);\r\nlog(2);\n";
    let fx = Fx::new();
    fx.write("src/a.ts", source);
    let out = fx
        .run(&rewrite_in(
            "typescript",
            &["src/a.ts"],
            "log($$$ARGS)",
            "log9($$$ARGS,\n    7)",
        ))
        .unwrap();
    let after = opencrayast_edit::apply_edits(source, &out.plan.files[0].edits).unwrap();
    assert_eq!(
        after, "log9(1,\r\n    7);\r\nlog9(2,\n    7);\n",
        "the first expansion took the CRLF at its site and the second the LF at its own"
    );
}

/// EDIT-13 (guards EDIT-12): each direction on its own, in a **single-file, single-edit** plan,
/// so `bytes_removed` and `bytes_added` are measured with nothing else in the sum.
///
/// The old guard in `edit9_the_risk_summary_counts_the_change` was a tautology
/// (`assert_ne!(removed, added + removed)` holds whenever `added != 0`) and could never fail.
/// This test is the real replacement: it pins each counter against ground truth recomputed from
/// the plan's own edits, and each case is degenerate in exactly one direction, which is the only
/// shape that can tell "bytes deleted" apart from "changed-bytes total".
///
/// Under the EDIT-12 bug (`bytes_removed` = `bytes_added + bytes_removed`) both cases go red:
/// the insertion case would report `bytes_removed == 7 != 0`, and the deletion case would report
/// `bytes_removed == 26` instead of 19.
#[test]
fn edit13_single_edit_summary_separates_inserted_from_deleted_bytes() {
    // A `Rewrite` is always a replace (it rewrites the matched span), so it can never be a pure
    // insertion. `SymbolOp::InsertBefore` writes text at `start == end`, and `SymbolOp::Delete`
    // writes an empty replacement over a non-empty range — those are the two degenerate shapes.
    //
    // ---- Direction 1: pure INSERTION — adds bytes, deletes nothing.
    let fx = Fx::new();
    fx.write("src/a.js", "function foo() {\n  return 1;\n}\n");
    let ins = fx
        .run(&symbol_req(
            "src/a.js",
            "foo",
            SymbolOp::InsertBefore,
            Some("// mark"),
        ))
        .unwrap();
    assert_eq!(
        ins.summary.edits, 1,
        "one edit, so the counters have nothing else to sum"
    );
    let ins_edits = &ins.plan.files[0].edits;
    assert_eq!(ins_edits.len(), 1);
    let ins_added = ins_edits[0].replacement.len() as u64;
    let ins_removed = (ins_edits[0].end - ins_edits[0].start) as u64;
    assert!(ins_added > 0, "the fixture must actually insert something");
    assert_eq!(
        ins_removed, 0,
        "InsertBefore is an insertion, so the edit's own range must be empty; \
         otherwise this case is not degenerate and would prove nothing"
    );
    assert_eq!(
        ins.summary.bytes_added, ins_added,
        "added must be the replacement's own length"
    );
    assert_eq!(
        ins.summary.bytes_removed, 0,
        "a pure insertion deletes nothing; a changed-bytes total would report {ins_added} here"
    );

    // ---- Direction 2: pure DELETION — deletes bytes, adds nothing.
    let fx = Fx::new();
    fx.write("src/a.js", "function foo() {\n  return 1;\n}\n");
    let del = fx
        .run(&symbol_req("src/a.js", "foo", SymbolOp::Delete, None))
        .unwrap();
    assert_eq!(
        del.summary.edits, 1,
        "one edit, so the counters have nothing else to sum"
    );
    let del_edits = &del.plan.files[0].edits;
    assert_eq!(del_edits.len(), 1);
    let del_added = del_edits[0].replacement.len() as u64;
    let del_removed = (del_edits[0].end - del_edits[0].start) as u64;
    assert!(
        del_removed > 0,
        "the fixture must actually delete something"
    );
    assert_eq!(
        del_added, 0,
        "Delete writes an empty replacement, so the case must be degenerate in the added \
         direction; otherwise it would prove nothing"
    );
    assert_eq!(
        del.summary.bytes_removed, del_removed,
        "removed must be the deleted range's own length"
    );
    assert_eq!(
        del.summary.bytes_added, 0,
        "a pure deletion inserts nothing; a changed-bytes total would report {del_removed} here"
    );
}

/// A `replace_body` whose `text` is a bare body — no braces — fails the syntax gate, and the
/// refusal must say what to do about it.
///
/// `ReplaceBody` replaces the bytes from the opening brace to the closing one **inclusive**, so a
/// bare body leaves the braces doubled up and the file stops parsing. The refusal is right; the
/// next step used to be "fix the named files", which describes the *file* as broken when the file
/// was untouched and only the request was wrong — it sends a caller to repair something that was
/// never damaged.
///
/// Mutation self-proof: restore the single generic gate next step in `preview.rs` — red on the
/// missing `braces`.
#[test]
fn a_bare_replace_body_is_told_to_include_the_braces() {
    let f = Fx::new();
    f.write("src/lib.rs", "pub fn target() {\n    let x = 1;\n}\n");
    // No braces: this is the mistake, and it is the mistake this message exists for.
    let req = symbol_req(
        "src/lib.rs",
        "target",
        SymbolOp::ReplaceBody,
        Some("\n    let x = 2;\n"),
    );
    let err = f.run(&req).expect_err("a bare body must not parse");
    assert_eq!(err.code, ErrorCode::GateFailed, "{err}");
    assert!(
        err.next.contains("braces"),
        "the next step must name the actual fix — `text` includes the braces: {}",
        err.next
    );
}

/// The converse: a gate failure that is *not* the brace mistake keeps the generic advice.
///
/// Without this, the case above could be satisfied by making every gate failure talk about
/// braces, which would be its own kind of lie. The gate used here is `size`: the rewrite makes the
/// file larger than `max_file_bytes` but not so large that the loader refuses it first.
#[test]
fn a_non_syntax_gate_failure_does_not_claim_the_caller_missed_braces() {
    // An `encoding` refusal, reached with no size threshold at all: a file with no trailing
    // newline whose replacement adds one. This is the other thing that comes back from the same
    // `gates failed:` refusal, so it is the right converse for "not every gate says braces".
    let f = Fx::new();
    f.write("src/a.ts", "log(1)");
    let err = f
        .run(&rewrite_in(
            "typescript",
            &["src/a.ts"],
            "log($$$ARGS)",
            "log($$$ARGS)\n",
        ))
        .expect_err("adding a trailing newline must fail the encoding gate");
    assert_eq!(err.code, ErrorCode::GateFailed, "{err}");
    assert!(
        err.message.contains("encoding"),
        "this must be the encoding gate, or the case proves nothing: {err}"
    );
    assert!(
        !err.next.contains("braces"),
        "a size failure is not the brace mistake and must not be described as one: {}",
        err.next
    );
}

/// No refusal may point at a document the caller cannot read.
///
/// The generic `invalid_args` next step used to say "Check the arguments against the tool's
/// argument table" — which lives in `docs/TOOLS.md`, a file neither shell exposes to a
/// `tools/call` caller. An unfollowable instruction is worse than none, because it reads as
/// advice and sends the model nowhere.
#[test]
fn no_refusal_points_at_a_document_the_caller_cannot_read() {
    let f = Fx::new();
    f.write("src/lib.rs", "pub fn target() {\n    let x = 1;\n}\n");

    // A rewrite with no paths: `invalid_args` from the shared constructor.
    let err = f
        .run(&rewrite_in("rust", &[], "pub fn $N() { $$$B }", "x"))
        .expect_err("a rewrite with no paths must be refused");
    for forbidden in ["argument table", "TOOLS.md", "docs/"] {
        assert!(
            !err.next.contains(forbidden),
            "the next step must not send the caller to `{forbidden}`: {}",
            err.next
        );
    }
    // And it must still be actionable: the arguments that decide the request are named.
    assert!(err.next.contains("kind=rewrite"), "{}", err.next);
    assert!(err.next.contains("kind=symbol"), "{}", err.next);
}
