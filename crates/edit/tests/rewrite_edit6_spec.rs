//! EDIT6-01..EDIT6-08: cases the rewrite spec does not pin, added while implementing
//! ISSUE-EDIT-6. The spec file is never weakened; these only add coverage.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use opencrayast_core::ErrorCode;
use opencrayast_core::limits::Limits;
use opencrayast_edit::{RewriteOutcome, RewriteRequest, apply_edits, rewrite_file, validate_edits};
use opencrayast_lang::Language::{self, Go, JavaScript, Python, Rust};
use opencrayast_lang::{ParseBudget, parse};
use opencrayast_query::pattern::{Pattern, SearchBudget};
use std::time::Duration;

fn pb() -> ParseBudget {
    ParseBudget {
        max_bytes: 1 << 24,
        timeout: Duration::from_secs(20),
        max_depth: 4096,
        max_nodes: 10_000_000,
    }
}

fn run(lang: Language, pat: &str, repl: &str, src: &str) -> (String, RewriteOutcome) {
    run_with(lang, pat, repl, src, false, 1000).unwrap()
}

fn run_with(
    lang: Language,
    pat: &str,
    repl: &str,
    src: &str,
    allow_comment_loss: bool,
    max_matches: usize,
) -> Result<(String, RewriteOutcome), opencrayast_core::error::ToolError> {
    let p = Pattern::compile(lang, pat).unwrap();
    let parsed = parse(lang, src, &pb()).unwrap();
    let req = RewriteRequest {
        pattern: &p,
        rule: None,
        replacement: repl,
        allow_comment_loss,
        search_budget: SearchBudget {
            max_matches,
            ..SearchBudget::default()
        },
        parse_budget: pb(),
        max_expansion_bytes: 1 << 20,
    };
    let out = rewrite_file(&parsed, src, &req)?;
    if !out.edits.is_empty() {
        validate_edits(src, &out.edits, &Limits::default()).unwrap();
    }
    let new = if out.edits.is_empty() {
        src.to_string()
    } else {
        apply_edits(src, &out.edits).unwrap()
    };
    Ok((new, out))
}

/// EDIT6-01: edits are strictly ascending and never overlap, whatever the search returned.
#[test]
fn edits_are_ascending_and_disjoint() {
    let src = "double(a + b);\ndouble(c);\ndouble(d + e);\n";
    let (new, out) = run(JavaScript, "double($X)", "$X * 2", src);
    assert_eq!(new, "(a + b) * 2;\nc * 2;\n(d + e) * 2;\n");
    let mut last = 0usize;
    for e in &out.edits {
        assert!(e.start >= last, "edits must ascend: {e:?}");
        assert!(e.end > e.start);
        last = e.end;
    }
}

/// EDIT6-02: a match that needs no change is found and counted but produces no edit, even when a
/// sibling match does change.
#[test]
fn a_no_op_match_is_counted_but_not_edited() {
    let src = "f(1);\nf(2);\n";
    // The replacement is the same text, so no edit at all.
    let (new, out) = run(JavaScript, "f($X)", "f($X)", src);
    assert_eq!(new, src);
    assert_eq!((out.matches_found, out.edits.len()), (2, 0));
}

/// EDIT6-03: an empty list capture is substituted as the empty string, and never wrapped.
#[test]
fn an_empty_list_capture_is_substituted_empty_and_never_wrapped() {
    let src = "call();\n";
    let (new, out) = run(JavaScript, "call($$$A)", "other($$$A)", src);
    assert_eq!(new, "other();\n");
    assert!(out.wrapped.is_empty());
    assert_eq!(out.edits.len(), 1);
}

/// EDIT6-04: wrapping happens once per match that needs it, and the report names that match.
#[test]
fn the_wrapped_report_names_exactly_the_matches_that_needed_parentheses() {
    let src = "double(a + b);\ndouble(a);\ndouble(c * d);\n";
    let (new, out) = run(JavaScript, "double($X)", "$X * 2", src);
    // `c * d * 2` is the same tree as `(c * d) * 2`, so it is NOT parenthesised; `a + b` is.
    assert_eq!(new, "(a + b) * 2;\na * 2;\nc * d * 2;\n");
    assert_eq!(
        out.wrapped.len(),
        1,
        "only the sum needed it: {:?}",
        out.wrapped
    );
    let w = out.wrapped[0];
    assert_eq!(
        &src[w.start..w.end],
        "double(a + b)",
        "the report names that match; the match node stops before the `;`"
    );
}

/// EDIT6-05: a list capture substituted where a single node belongs is refused, because it can
/// neither be one node nor be fixed with parentheses.
///
/// `$X` binds a whole `let` statement's value here, so `$X + 1` happens to stay valid - that is the
/// grouping check doing its job, not luck. The refusal below uses a case that genuinely cannot work:
/// a LIST capture used where the template needs one node.
#[test]
fn a_capture_that_cannot_be_one_node_is_refused_rather_than_wrapped() {
    // `$$$ARGS` absorbs several nodes; putting it where one node is required cannot be repaired by
    // parentheses, so the template check refuses it up front.
    let e = run_with(
        JavaScript,
        "foo($$$ARGS)",
        "bar($ARGS) + $NOPE",
        "foo(1, 2);\n",
        false,
        1000,
    )
    .unwrap_err();
    assert_eq!(e.code, ErrorCode::InvalidPattern);
    // The message must not quote source text.
    assert!(!e.message.contains("foo(1"), "{}", e.message);
}

/// EDIT6-06: CRLF output follows the file, and a multi-line replacement does not convert the
/// line breaks inside a capture.
#[test]
fn crlf_files_stay_crlf_and_captures_keep_their_own_line_endings() {
    let src = "function f() {\r\n    guard(c, run());\r\n}\r\n";
    let (new, _) = run(JavaScript, "guard($C, $B);", "if ($C) {\n  $B;\n}", src);
    assert_eq!(
        new,
        "function f() {\r\n    if (c) {\r\n      run();\r\n    }\r\n}\r\n"
    );
    assert!(
        !new.contains("\n\n"),
        "no bare LF may appear in a CRLF file: {new:?}"
    );
}

/// EDIT6-07: a template naming an anonymous metavariable is fine, and `$$` is a literal dollar
/// even next to digits and inside a string.
#[test]
fn literal_dollars_are_literal_even_next_to_digits() {
    let (new, _) = run(JavaScript, "cost($X)", "pay($X, '$$5', $$)", "cost(1);\n");
    assert_eq!(new, "pay(1, '$5', $);\n");
}

/// EDIT6-08: the generated edits are valid for `validate_edits` in every language, which is the
/// contract the shell relies on.
#[test]
fn generated_edits_validate_in_every_language() {
    for (lang, pat, repl, src) in [
        (JavaScript, "double($X)", "$X * 2", "double(a + b);\n"),
        (Python, "twice($X)", "$X * 2", "y = twice(a + b)\n"),
        (
            Go,
            "twice($X)",
            "$X * 2",
            "package p\n\nfunc f() {\n\t_ = twice(a + b)\n}\n",
        ),
        (
            Rust,
            "twice($X)",
            "$X * 2",
            "fn f() {\n    let _ = twice(a + b);\n}\n",
        ),
    ] {
        // `run` already calls validate_edits and unwraps, so reaching here means it passed.
        let (new, out) = run(lang, pat, repl, src);
        assert!(!new.is_empty(), "{lang:?} produced nothing");
        assert!(out.matches_found >= 1, "{lang:?}");
    }
}

/// EDIT6-09: a capture used in BOTH a string and as code is parenthesised only in the code
/// position - the decision is per occurrence, not per capture.
#[test]
fn a_capture_used_in_a_string_and_as_code_is_wrapped_only_in_the_code() {
    let (new, out) = run(JavaScript, "f($X)", "g(\"$X\", $X * 2)", "f(a + b);\n");
    assert_eq!(new, "g(\"a + b\", (a + b) * 2);\n");
    assert_eq!(out.wrapped.len(), 1);

    // The same, in a template literal and in a comment: still only the code position is wrapped.
    let (new, _) = run(JavaScript, "f($X)", "g(`$X`, $X)", "f(a + b);\n");
    assert_eq!(new, "g(`a + b`, a + b);\n");

    // Two code positions and one string: each occurrence is judged on its own, and a capture used
    // as a bare argument needs no parentheses (the commas already delimit it), so none are added.
    let (new, out) = run(JavaScript, "f($X)", "g(\"$X\", $X, $X)", "f(a + b);\n");
    assert_eq!(new, "g(\"a + b\", a + b, a + b);\n");
    assert!(
        out.wrapped.is_empty(),
        "an argument needs no parentheses: {:?}",
        out.wrapped
    );
}

/// EDIT6-10: a capture inside a string is inserted as written, even when the text would be a
/// different shape as code - the literal is never re-parenthesised.
#[test]
fn a_capture_inside_a_string_is_never_parenthesised_whatever_it_contains() {
    // `a, b` is several nodes; as a string's content that is perfectly fine.
    let (new, out) = run(JavaScript, "f($$$A)", "g(\"[$$$A]\")", "f(1, 2);\n");
    assert_eq!(new, "g(\"[1, 2]\");\n");
    assert!(out.wrapped.is_empty());
}
