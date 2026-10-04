//! Spec for ISSUE-TOOLS-SEARCH (`ast_search`, `ast_explain_pattern`). The expected strings are the
//! contract (see the doc comments of the two handlers). Add cases; never weaken these.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::field_reassign_with_default
)]
#![cfg(unix)]

use opencrayast_core::ErrorCode;
use opencrayast_core::boundary::{Boundary, BoundaryConfig};
use opencrayast_core::limits::Limits;
use opencrayast_query::pattern::{Rule, RuleOperand};
use opencrayast_tools::{
    ExplainArgs, Mode, SearchArgs, ToolContext, ast_explain_pattern, ast_search,
};
use std::fs;
use std::os::unix::fs::symlink;

fn ctx(d: &tempfile::TempDir, limits: Limits) -> ToolContext {
    ToolContext {
        boundary: Boundary::new(BoundaryConfig {
            root: d.path().to_path_buf(),
            limits: Limits::default(),
            read_roots: Vec::new(),
            state_dir: None,
            extra_protected: Vec::new(),
        })
        .unwrap(),
        limits,
        mode: Mode::ReadOnly,
        write: None,
        version: "0.20261002.1".into(),
        workspace_id: "w-00112233445566778899aabbccddeeff".into(),
        respect_gitignore: true,
        extra_ignore: vec![],
        config_source: Default::default(),
    }
}

fn ws() -> (tempfile::TempDir, ToolContext) {
    let d = tempfile::tempdir().unwrap();
    let c = ctx(&d, Limits::default());
    (d, c)
}

fn put(d: &tempfile::TempDir, rel: &str, body: &str) {
    let p = d.path().join(rel);
    fs::create_dir_all(p.parent().unwrap()).unwrap();
    fs::write(p, body).unwrap();
}

fn sargs(pattern: &str) -> SearchArgs {
    SearchArgs {
        pattern: pattern.into(),
        paths: vec![".".into()],
        ..Default::default()
    }
}

fn js_ws() -> (tempfile::TempDir, ToolContext) {
    let (d, c) = ws();
    put(
        &d,
        "src/a.js",
        "console.log(\"start\", id);\nfunction f() {\n  console.log(err);\n}\n",
    );
    put(&d, "src/b.js", "console.log(x);\n");
    (d, c)
}

#[test]
fn search_output_is_exact() {
    let (_d, c) = js_ws();
    let out = ast_search(&c, &sargs("console.log($$$ARGS)")).unwrap();
    assert_eq!(
        out,
        "Found 3 matches in 2 files for: console.log($$$ARGS)\n\
         src/a.js:1:1-1:25  console.log(\"start\", id)  $$$ARGS = \"start\", id\n\
         src/a.js:3:3-3:19  console.log(err)  $$$ARGS = err\n\
         src/b.js:1:1-1:15  console.log(x)  $$$ARGS = x\n"
    );
    // deterministic
    assert_eq!(ast_search(&c, &sargs("console.log($$$ARGS)")).unwrap(), out);
}

#[test]
fn zero_matches_say_what_was_searched() {
    let (_d, c) = js_ws();
    assert_eq!(
        ast_search(&c, &sargs("nothing($X)")).unwrap(),
        "0 matches in 2 files (javascript)\n"
    );
}

#[test]
fn context_lines_mark_matched_and_surrounding_lines() {
    let (_d, c) = js_ws();
    let mut a = sargs("console.log(err)");
    a.paths = vec!["src/a.js".into()];
    a.context_lines = Some(1);
    assert_eq!(
        ast_search(&c, &a).unwrap(),
        "Found 1 matches in 1 files for: console.log(err)\n\
         src/a.js:3:3-3:19  console.log(err)\n\
         \x20 2: function f() {\n\
         \x20 3>   console.log(err);\n\
         \x20 4: }\n"
    );
}

#[test]
fn the_limit_truncates_and_says_more_exist() {
    let (_d, c) = js_ws();
    let mut a = sargs("console.log($$$ARGS)");
    a.limit = Some(2);
    let out = ast_search(&c, &a).unwrap();
    assert!(out.starts_with("Found 2 matches in "), "{out}");
    assert!(out.ends_with("[truncated: showing 2 matches, more exist; narrow `paths`, add a `rule` or raise `limit`]\n"), "{out}");
    assert_eq!(out.lines().count(), 1 + 2 + 1, "{out}");
}

#[test]
fn rules_narrow_the_result() {
    let (_d, c) = js_ws();
    let mut a = sargs("console.log($$$ARGS)");
    a.rule = Some(Rule {
        inside: Some(Box::new(RuleOperand::Rule(Box::new(Rule {
            kind: Some("function_declaration".into()),
            ..Default::default()
        })))),
        ..Default::default()
    });
    let out = ast_search(&c, &a).unwrap();
    assert!(out.starts_with("Found 1 matches in 2 files for: "), "{out}");
    assert!(out.contains("src/a.js:3:3-3:19"), "{out}");
}

#[test]
fn mixed_languages_need_a_language_argument_and_other_languages_are_counted() {
    let (d, c) = js_ws();
    put(&d, "tool.py", "print(1)\n");
    let e = ast_search(&c, &sargs("console.log($$$ARGS)")).unwrap_err();
    assert_eq!(e.code, ErrorCode::InvalidArgs);
    assert_eq!(e.message, "mixed languages: javascript, python");
    assert!(e.next.contains("language"), "{}", e.next);
    let mut a = sargs("console.log($$$ARGS)");
    a.language = Some("javascript".into());
    let out = ast_search(&c, &a).unwrap();
    assert!(out.starts_with("Found 3 matches in 2 files for: "), "{out}");
    assert!(out.ends_with("[skipped: 1 other language]\n"), "{out}");
    let mut b = sargs("print($X)");
    b.language = Some("python".into());
    assert!(ast_search(&c, &b).unwrap().starts_with(
        "Found 1 matches in 1 files for: print($X)\ntool.py:1:1-1:9  print(1)  $X = 1\n"
    ));
}

#[test]
fn argument_and_pattern_errors() {
    let (d, c) = js_ws();
    put(&d, "notes.txt", "x\n");
    let code = |a: &SearchArgs| ast_search(&c, a).unwrap_err().code;
    assert_eq!(code(&sargs("")), ErrorCode::InvalidArgs);
    assert_eq!(
        code(&SearchArgs {
            pattern: "x".repeat(20_000),
            paths: vec![".".into()],
            ..Default::default()
        }),
        ErrorCode::InvalidArgs
    );
    let mut a = sargs("foo($X)");
    a.paths = vec![];
    assert_eq!(code(&a), ErrorCode::InvalidArgs);
    for cl in [6u64, 100] {
        let mut a = sargs("foo($X)");
        a.context_lines = Some(cl);
        assert_eq!(code(&a), ErrorCode::InvalidArgs, "context_lines {cl}");
    }
    for l in [0u64, 100_000] {
        let mut a = sargs("foo($X)");
        a.limit = Some(l);
        assert_eq!(code(&a), ErrorCode::InvalidArgs, "limit {l}");
    }
    let mut a = sargs("foo($X)");
    a.language = Some("cobol".into());
    assert_eq!(code(&a), ErrorCode::InvalidArgs);
    let mut a = sargs("foo($X)");
    a.paths = vec!["notes.txt".into()];
    assert_eq!(
        code(&a),
        ErrorCode::InvalidArgs,
        "no supported file and no language"
    );
    let mut a = sargs("function (");
    a.language = Some("javascript".into());
    let e = ast_search(&c, &a).unwrap_err();
    assert_eq!(e.code, ErrorCode::InvalidPattern);
    assert!(e.message.contains("at byte"), "{}", e.message);
    let mut a = sargs("foo($X)");
    a.paths = vec!["../x".into()];
    assert_eq!(code(&a), ErrorCode::OutsideWorkspace);
    let mut a = sargs("foo($X)");
    a.rule = Some(Rule {
        kind: Some("no_such_kind".into()),
        ..Default::default()
    });
    assert_eq!(code(&a), ErrorCode::InvalidPattern);
}

#[test]
fn syntax_errors_are_reported_but_do_not_stop_the_search() {
    let (d, c) = ws();
    put(
        &d,
        "bad.js",
        "console.log(1);\nconsole.log(;\nconsole.log(2);\n",
    );
    let out = ast_search(&c, &sargs("console.log($X)")).unwrap();
    assert!(
        out.starts_with("Found 2 matches in 1 files for: console.log($X)\n"),
        "{out}"
    );
    assert!(out.contains("[syntax errors: bad.js ("), "{out}");
}

#[test]
fn skipped_things_are_counted() {
    let (d, c) = js_ws();
    put(&d, ".gitignore", "ignored.js\n");
    put(&d, "ignored.js", "console.log(1);\n");
    put(&d, "notes.txt", "x\n");
    let outside = tempfile::tempdir().unwrap();
    fs::write(outside.path().join("secret.js"), "console.log(1);\n").unwrap();
    symlink(outside.path().join("secret.js"), d.path().join("link.js")).unwrap();
    fs::write(d.path().join("bin.js"), [0xff, 0xfe, 0x00]).unwrap();
    let out = ast_search(&c, &sargs("console.log($$$ARGS)")).unwrap();
    assert!(
        out.contains("[skipped: 1 ignored, 1 links, 2 unsupported language, 1 not utf-8]\n"),
        "{out}"
    );
    assert!(!out.contains("secret"), "{out}");
}

#[test]
fn hostile_text_is_escaped_and_counted() {
    let (d, c) = ws();
    put(
        &d,
        "h.js",
        "foo(\"\u{202e}x\\u{1b}\");\nfoo(\"\u{1b}[31m\");\n",
    );
    let out = ast_search(&c, &sargs("foo($X)")).unwrap();
    assert!(
        !out.contains('\u{202e}') && !out.contains('\u{1b}'),
        "{out:?}"
    );
    assert!(out.contains("[escaped: "), "{out:?}");
}

#[test]
fn the_shared_step_budget_fails_the_call_instead_of_returning_partial_results() {
    let (d, c) = ws();
    let items: Vec<String> = (0..2000).map(|i| i.to_string()).collect();
    put(&d, "big.js", &format!("[{}];\n", items.join(", ")));
    let e = ast_search(&c, &sargs("[$$$A, $$$B, $$$C, x]")).unwrap_err();
    assert_eq!(e.code, ErrorCode::BudgetExceeded);
    assert!(e.message.to_lowercase().contains("step"), "{}", e.message);
}

#[test]
fn the_call_deadline_is_enforced() {
    let (d, _c) = ws();
    for i in 0..300 {
        put(&d, &format!("p/f{i}.js"), &format!("foo({i});\n"));
    }
    let mut l = Limits::default();
    l.call_timeout_ms = 1;
    let c = ctx(&d, l);
    let t = std::time::Instant::now();
    let e = ast_search(&c, &sargs("foo($X)")).unwrap_err();
    assert_eq!(e.code, ErrorCode::Timeout);
    assert!(
        t.elapsed() < std::time::Duration::from_secs(10),
        "{:?}",
        t.elapsed()
    );
}

/// SRCH-05: the tool-layer guard in the **load loop** is what stops a call whose deadline runs
/// out while files are being read and parsed, and nothing else can stop that call.
///
/// `the_call_deadline_is_enforced` above cannot tell this guard from the matcher's: both report
/// `Timeout`, and that shape reaches the matcher too. So this case is built so the matcher is
/// never even consulted:
///
/// - every candidate is an unsupported `.txt`, so no file ever lands in `loaded`;
/// - `language` is therefore still unset, so even if the load loop finished in time the call
///   could not reach the search loop - it would fail `invalid_args` instead.
///
/// That leaves the load-loop guard as the single possible source of the `timeout`, which is what
/// makes this case go red when the guard is removed. 3000 files put roughly 8 ms of real
/// open/read/close work in front of a 1 ms deadline - an eightfold margin, so the case does not
/// hinge on a tight race. Nothing here sleeps or polls: the deadline is exceeded by construction,
/// so the outcome is deterministic rather than timing-dependent.
#[test]
fn srch05_the_deadline_guard_in_the_load_loop_is_the_only_thing_that_can_stop_it() {
    let (d, _c) = ws();
    for i in 0..3000 {
        put(&d, &format!("p/f{i}.txt"), "x\n");
    }
    let mut l = Limits::default();
    l.call_timeout_ms = 1;
    let c = ctx(&d, l);
    let t = std::time::Instant::now();
    let e = ast_search(&c, &sargs("foo($X)")).unwrap_err();
    assert_eq!(e.code, ErrorCode::Timeout, "{e:?}");
    // The tool layer's own wording, never the matcher's ("pattern matching exceeded its
    // wall-clock deadline"). Asserting it too documents which layer answered.
    assert_eq!(
        e.message,
        "the call deadline passed before the search finished"
    );
    assert!(
        t.elapsed() < std::time::Duration::from_secs(10),
        "{:?}",
        t.elapsed()
    );
}

/// SRCH-06: the tool-layer guard in the **search loop** is reachable in its own right, in the
/// window between the end of the load loop and the first `search()` call.
///
/// SRCH-05 pins the load-loop guard by starving the matcher. This pins the search-loop guard by
/// never letting the matcher run: one `.py` file whose parse overruns the deadline, paired with
/// `language: javascript`, which that file is not. The file is counted as `other language` and
/// skipped, so `search()` is never called - with the guard removed the call succeeds with a normal
/// `0 matches ... [skipped: 1 other language]` result instead of timing out.
///
/// Without the language mismatch the matcher would answer first (it checks the same deadline), so
/// the mismatch is what makes this guard observable rather than redundant.
#[test]
fn srch06_the_deadline_guard_in_the_search_loop_is_reachable_without_the_matcher() {
    let (d, _c) = ws();
    // ~240 KB: parsing this overruns a 1 ms deadline by two orders of magnitude.
    put(
        &d,
        "one.py",
        &format!("{}foo(1);\n", "x = 1\n".repeat(40_000)),
    );
    let mut l = Limits::default();
    l.call_timeout_ms = 1;
    let c = ctx(&d, l);
    let mut a = sargs("foo($X)");
    a.language = Some("javascript".into());
    let t = std::time::Instant::now();
    let e = ast_search(&c, &a).unwrap_err();
    assert_eq!(e.code, ErrorCode::Timeout, "{e:?}");
    assert_eq!(
        e.message,
        "the call deadline passed before the search finished"
    );
    assert!(
        t.elapsed() < std::time::Duration::from_secs(10),
        "{:?}",
        t.elapsed()
    );
}

/// SRCH-07: `respect_gitignore` is read from the context, and `false` really does surface the
/// files the ignore filter would otherwise hide.
///
/// Both halves are pinned, because "the flag is plumbed through" and "the flag does something"
/// are different claims. With `true` the ignored file must be **absent** from the output and
/// counted in `[skipped: ... ignored]`; with `false` the same file must **appear**, counted as a
/// searched file instead. A build that ignored `ctx.respect_gitignore` and always filtered fails
/// the second half; a build that never filtered at all fails the first.
#[test]
fn srch07_respect_gitignore_false_surfaces_files_the_filter_would_hide() {
    let (d, _base) = ws();
    put(&d, ".gitignore", "hidden.py\n");
    put(&d, "hidden.py", "def hidden():\n    return 1\n");
    put(&d, "shown.py", "def shown():\n    return 2\n");

    // Filtered (the default): the ignored file never reaches the search.
    let filtered = ast_search(
        &ctx(&d, Limits::default()),
        &sargs("def $NAME():\n    $$$BODY"),
    )
    .unwrap();
    assert!(
        filtered.contains("$NAME = shown"),
        "the non-ignored file was not searched: {filtered}"
    );
    assert!(
        !filtered.contains("$NAME = hidden"),
        "a match from the gitignored file was searched: {filtered}"
    );
    // `.gitignore` itself is unsupported, so the footer carries more than one counter;
    // what matters is that exactly one entry was ignored.
    assert!(
        filtered.contains("[skipped: 1 ignored,"),
        "the ignored file was not counted as ignored: {filtered}"
    );
    assert!(
        filtered.starts_with("Found 1 matches in 1 files for: def $NAME():\n"),
        "{filtered}"
    );

    // Unfiltered: the same file becomes visible, and is no longer counted as ignored.
    let unfiltered = ToolContext {
        respect_gitignore: false,
        ..ctx(&d, Limits::default())
    };
    let all = ast_search(&unfiltered, &sargs("def $NAME():\n    $$$BODY")).unwrap();
    assert!(
        all.contains("$NAME = hidden"),
        "respect_gitignore: false did not surface the ignored file: {all}"
    );
    assert!(
        all.contains("$NAME = shown"),
        "the non-ignored file went missing: {all}"
    );
    assert!(
        !all.contains("ignored"),
        "something was still counted as ignored with the filter off: {all}"
    );
    assert!(
        all.ends_with("[skipped: 1 unsupported language]\n"),
        "{all}"
    );
    assert!(
        all.starts_with("Found 2 matches in 2 files for: def $NAME():\n"),
        "{all}"
    );

    // Deterministic: the same answer on a repeat call, and it differs from the filtered run.
    assert_eq!(
        ast_search(&unfiltered, &sargs("def $NAME():\n    $$$BODY")).unwrap(),
        all
    );
    assert_ne!(all, filtered);
}

#[test]
fn explain_output_is_exact() {
    let (_d, c) = ws();
    let out = ast_explain_pattern(
        &c,
        &ExplainArgs {
            pattern: "foo($A, $$$B)".into(),
            language: "javascript".into(),
        },
    )
    .unwrap();
    assert_eq!(
        out,
        "pattern: foo($A, $$$B)\n\
         language: javascript\n\
         metavariables: $A (one), $$$B (list)\n\
         ```text\n\
         call_expression\n\
         \x20 identifier \"foo\"\n\
         \x20 arguments\n\
         \x20   \"(\"\n\
         \x20   $A (one)\n\
         \x20   \",\"\n\
         \x20   $$$B (list)\n\
         \x20   \")\"\n\
         \n\
         ```\n"
    );
    let none = ast_explain_pattern(
        &c,
        &ExplainArgs {
            pattern: "foo(1)".into(),
            language: "javascript".into(),
        },
    )
    .unwrap();
    assert!(none.contains("metavariables: none\n"), "{none}");
}

#[test]
fn explain_errors() {
    let (_d, c) = ws();
    let e = ast_explain_pattern(
        &c,
        &ExplainArgs {
            pattern: "foo".into(),
            language: "cobol".into(),
        },
    )
    .unwrap_err();
    assert_eq!(e.code, ErrorCode::InvalidArgs);
    assert!(
        e.message.contains("javascript"),
        "supported ids are listed: {}",
        e.message
    );
    let e = ast_explain_pattern(
        &c,
        &ExplainArgs {
            pattern: "function (".into(),
            language: "javascript".into(),
        },
    )
    .unwrap_err();
    assert_eq!(e.code, ErrorCode::InvalidPattern);
    assert!(e.message.contains("at byte"), "{}", e.message);
    assert_eq!(
        ast_explain_pattern(
            &c,
            &ExplainArgs {
                pattern: String::new(),
                language: "javascript".into()
            }
        )
        .unwrap_err()
        .code,
        ErrorCode::InvalidArgs
    );
}

#[test]
fn explain_marks_context_warnings_for_go_and_hostile_patterns_stay_safe() {
    let (_d, c) = ws();
    let out = ast_explain_pattern(
        &c,
        &ExplainArgs {
            pattern: "fmt.Println($$$A)".into(),
            language: "go".into(),
        },
    )
    .unwrap();
    assert!(out.contains("warning:"), "{out}");
    let out = ast_explain_pattern(
        &c,
        &ExplainArgs {
            pattern: "foo(\"\u{202e}\")".into(),
            language: "javascript".into(),
        },
    )
    .unwrap();
    assert!(!out.contains('\u{202e}'), "{out:?}");
}
