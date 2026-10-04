//! Spec for ISSUE-EDIT-6: the rewrite generator (docs/PATTERNS.md "Rewrite templates", "Overlap";
//! docs/EDIT-MODEL.md `rewrite`). Never weaken; add cases. If an expected string looks wrong to
//! you, block the ticket with a minimal reproduction: the expectations below were derived by
//! hand and the property tests are the arbiter of meaning.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use opencrayast_core::ErrorCode;
use opencrayast_core::error::ToolError;
use opencrayast_core::limits::Limits;
use opencrayast_edit::{
    RewriteOutcome, RewriteRequest, Span, apply_edits, rewrite_file, validate_edits,
};
use opencrayast_lang::Language::{self, Go, JavaScript, Python, Rust, Tsx, TypeScript};
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

fn run_with(
    lang: Language,
    pat: &str,
    repl: &str,
    src: &str,
    allow_comment_loss: bool,
    max_matches: usize,
) -> Result<(String, RewriteOutcome), ToolError> {
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
    // every outcome must be a valid edit set for the source (E-1), whatever the generator did
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

fn run(lang: Language, pat: &str, repl: &str, src: &str) -> (String, RewriteOutcome) {
    run_with(lang, pat, repl, src, false, 1000).unwrap()
}

#[test]
fn plain_rewrites_in_every_language() {
    let (new, out) = run(
        JavaScript,
        "console.log($$$ARGS)",
        "logger.debug($$$ARGS)",
        "console.log(\"a\", b);\nconsole.log(c);\n",
    );
    assert_eq!(new, "logger.debug(\"a\", b);\nlogger.debug(c);\n");
    assert_eq!((out.edits.len(), out.matches_found), (2, 2));
    assert!(
        out.overlaps_dropped.is_empty()
            && out.wrapped.is_empty()
            && out.comments_dropped.is_empty()
    );

    let (new, _) = run(
        Python,
        "connect($HOST, $PORT)",
        "connect($PORT, $HOST)",
        "connect(h, 80)\n",
    );
    assert_eq!(new, "connect(80, h)\n");

    let (new, _) = run(
        JavaScript,
        "var $NAME = $VALUE;",
        "let $NAME = $VALUE;",
        "var x = 1;\nvar y = f(2);\n",
    );
    assert_eq!(new, "let x = 1;\nlet y = f(2);\n");

    let (new, _) = run(
        Go,
        "fmt.Println($$$A)",
        "log.Println($$$A)",
        "package p\n\nfunc f() {\n\tfmt.Println(1, 2)\n}\n",
    );
    assert_eq!(new, "package p\n\nfunc f() {\n\tlog.Println(1, 2)\n}\n");

    let (new, _) = run(
        Rust,
        "println!($$$A)",
        "eprintln!($$$A)",
        "fn f() {\n    println!(\"x\");\n}\n",
    );
    assert_eq!(new, "fn f() {\n    eprintln!(\"x\");\n}\n");
}

#[test]
fn a_captured_expression_is_wrapped_only_when_substitution_would_change_its_meaning() {
    let (new, out) = run(
        JavaScript,
        "double($X)",
        "$X * 2",
        "double(a + b);\ndouble(a);\ndouble(f(x));\ndouble(-a);\ndouble(a ? b : c);\n",
    );
    assert_eq!(
        new,
        "(a + b) * 2;\na * 2;\nf(x) * 2;\n-a * 2;\n(a ? b : c) * 2;\n"
    );
    assert_eq!(
        out.wrapped.len(),
        2,
        "only the two that needed it: {:?}",
        out.wrapped
    );
    assert_eq!(out.wrapped[0], Span { start: 0, end: 13 });

    let (new, out) = run(
        JavaScript,
        "len($X)",
        "$X.length",
        "len(a + b);\nlen(xs);\nlen(f(1));\n",
    );
    assert_eq!(new, "(a + b).length;\nxs.length;\nf(1).length;\n");
    assert_eq!(out.wrapped.len(), 1);

    let (new, _) = run(
        Python,
        "twice($X)",
        "$X * 2",
        "y = twice(a + b)\nz = twice(a)\nw = twice(a if c else d)\nv = twice(not a)\n",
    );
    assert_eq!(
        new,
        "y = (a + b) * 2\nz = a * 2\nw = (a if c else d) * 2\nv = (not a) * 2\n"
    );

    let (new, _) = run(
        Go,
        "twice($X)",
        "$X * 2",
        "package p\n\nfunc f() {\n\t_ = twice(a + b)\n\t_ = twice(a)\n}\n",
    );
    assert_eq!(
        new,
        "package p\n\nfunc f() {\n\t_ = (a + b) * 2\n\t_ = a * 2\n}\n"
    );

    let (new, _) = run(
        Rust,
        "twice($X)",
        "$X * 2",
        "fn f() {\n    let _ = twice(a + b);\n    let _ = twice(a);\n}\n",
    );
    assert_eq!(
        new,
        "fn f() {\n    let _ = (a + b) * 2;\n    let _ = a * 2;\n}\n"
    );

    // PAT-09 is `golden per language`. The cases above cover Rust, JavaScript,
    // Python and Go, which is four of the six languages the catalogue knows - so
    // the row claimed more than the test could show. Rather than narrow the claim
    // to the four, here are TypeScript and TSX, which bind tighter than `+` the
    // same way: `a + b` substituted into `$X * 2` must be parenthesised.
    let (new, out) = run(
        TypeScript,
        "twice($X)",
        "$X * 2",
        "const r = twice(a + b);\nconst s = twice(a);\n",
    );
    assert_eq!(
        new, "const r = (a + b) * 2;\nconst s = a * 2;\n",
        "typescript: the looser-binding capture must be parenthesised",
    );
    assert_eq!(
        out.wrapped.len(),
        1,
        "typescript: only the capture that needs wrapping is reported: {:?}",
        out.wrapped,
    );

    let (new, out) = run(
        Tsx,
        "twice($X)",
        "$X * 2",
        "const r = twice(a + b);\nconst s = twice(a);\n",
    );
    assert_eq!(
        new, "const r = (a + b) * 2;\nconst s = a * 2;\n",
        "tsx: the looser-binding capture must be parenthesised",
    );
    assert_eq!(
        out.wrapped.len(),
        1,
        "tsx: only the capture that needs wrapping is reported: {:?}",
        out.wrapped,
    );
}

#[test]
fn list_captures_are_never_wrapped() {
    let (new, out) = run(JavaScript, "call($$$A)", "[$$$A]", "call(a + b, c);\n");
    assert_eq!(new, "[a + b, c];\n");
    assert!(out.wrapped.is_empty());
}

#[test]
fn nested_matches_rewrite_the_outermost_and_report_the_rest() {
    let (new, out) = run(JavaScript, "f($X)", "g($X)", "f(f(1));\n");
    assert_eq!(
        new, "g(f(1));\n",
        "captured text is copied verbatim, never rewritten again"
    );
    assert_eq!(out.matches_found, 2);
    assert_eq!(out.overlaps_dropped, vec![Span { start: 2, end: 6 }]);
    assert_eq!(out.edits.len(), 1);
    // siblings are all rewritten
    let (new, out) = run(JavaScript, "f($X)", "g($X)", "f(1);\nf(2);\n");
    assert_eq!(new, "g(1);\ng(2);\n");
    assert!(out.overlaps_dropped.is_empty());
}

#[test]
fn comments_inside_a_match_but_outside_every_capture_are_refused_unless_allowed() {
    let src = "foo(1, /* why */ 2);\n";
    let e = run_with(JavaScript, "foo($A, $B)", "bar($B, $A)", src, false, 1000).unwrap_err();
    assert_eq!(e.code, ErrorCode::CommentLoss);
    assert!(e.message.contains('1'), "gives the count: {}", e.message);
    assert!(
        !e.message.contains("why"),
        "never quotes the comment: {}",
        e.message
    );
    let (new, out) = run_with(JavaScript, "foo($A, $B)", "bar($B, $A)", src, true, 1000).unwrap();
    assert_eq!(new, "bar(2, 1);\n");
    assert_eq!(out.comments_dropped, vec![Span { start: 7, end: 16 }]);

    // a comment inside a capture travels with it
    let (new, out) = run(
        JavaScript,
        "foo($A, $B)",
        "bar($B, $A)",
        "foo(f(/*keep*/ 1), 2);\n",
    );
    assert_eq!(new, "bar(2, f(/*keep*/ 1));\n");
    assert!(out.comments_dropped.is_empty());
    // including inside a list capture
    let (new, _) = run(
        JavaScript,
        "foo($$$ARGS)",
        "bar($$$ARGS)",
        "foo(1, /*c*/ 2);\n",
    );
    assert_eq!(new, "bar(1, /*c*/ 2);\n");
    // comments outside the match are untouched
    let (new, _) = run(
        JavaScript,
        "foo($A, $B)",
        "bar($B, $A)",
        "// note\nfoo(1, 2); // after\n",
    );
    assert_eq!(new, "// note\nbar(2, 1); // after\n");

    let e = run_with(
        Python,
        "foo($A, $B)",
        "bar($B, $A)",
        "foo(1,  # why\n    2)\n",
        false,
        1000,
    )
    .unwrap_err();
    assert_eq!(e.code, ErrorCode::CommentLoss);
}

#[test]
fn multi_line_replacements_follow_the_indentation_and_line_ending_of_the_site() {
    let repl = "if ($C) {\n  $B;\n}";
    let (new, _) = run(
        JavaScript,
        "guard($C, $B);",
        repl,
        "function f() {\n    guard(x > 0, run());\n}\n",
    );
    assert_eq!(
        new,
        "function f() {\n    if (x > 0) {\n      run();\n    }\n}\n"
    );
    let (new, _) = run(
        JavaScript,
        "guard($C, $B);",
        repl,
        "function f() {\n\tguard(x > 0, run());\n}\n",
    );
    assert_eq!(new, "function f() {\n\tif (x > 0) {\n\t  run();\n\t}\n}\n");
    let (new, _) = run(
        JavaScript,
        "guard($C, $B);",
        repl,
        "function f() {\r\n    guard(x > 0, run());\r\n}\r\n",
    );
    assert_eq!(
        new,
        "function f() {\r\n    if (x > 0) {\r\n      run();\r\n    }\r\n}\r\n"
    );
}

#[test]
fn text_inside_template_strings_and_comments_of_the_replacement_is_not_reflowed() {
    let (new, _) = run(
        JavaScript,
        "show($X)",
        "log(`a\n b`,\n  $X)",
        "function f() {\r\n    show(1);\r\n}\r\n",
    );
    assert_eq!(
        new,
        "function f() {\r\n    log(`a\n b`,\r\n      1);\r\n}\r\n"
    );
}

/// PAT-10 is a PROPERTY: "re-indentation NEVER changes bytes inside strings,
/// template literals, docstrings or comments". The single case beside it covers
/// one construct (a JavaScript template literal) in one language, which is
/// weaker evidence than the claim: "never" is not something one example shows.
///
/// So this states it as a property over ALL FOUR constructs, in the languages
/// that have them, and - the part that makes it a property rather than four more
/// examples - it also varies the things a re-indenter could plausibly get wrong:
/// leading blank lines, blank lines inside the literal, an already-indented
/// literal, and a literal that contains the indent string itself.
#[test]
fn verbatim_regions_survive_reindentation_in_every_language_and_construct() {
    /// (label, language, pattern, replacement, source, expected)
    ///
    /// Every source has the literal carrying its OWN internal newlines and
    /// indentation, which is what must survive byte for byte. The `$X` capture is
    /// the only part allowed to be re-indented.
    const CASES: &[(&str, Language, &str, &str, &str, &str)] = &[
        // ---- JavaScript template literal: bare LF inside, CRLF file outside
        (
            "js template literal",
            JavaScript,
            "show($X)",
            "log(`a\n  b`,\n  $X)",
            "function f() {\r\n    show(1);\r\n}\r\n",
            "function f() {\r\n    log(`a\n  b`,\r\n      1);\r\n}\r\n",
        ),
        // ---- JavaScript ordinary string containing a newline escape
        (
            "js string escape",
            JavaScript,
            "show($X)",
            "log(\"a\\n  b\",\n  $X)",
            "function f() {\n    show(1);\n}\n",
            "function f() {\n    log(\"a\\n  b\",\n      1);\n}\n",
        ),
        // ---- JavaScript block comment
        (
            "js block comment",
            JavaScript,
            "show($X)",
            "log(/* keep\n   me */\n  $X)",
            "function f() {\n    show(1);\n}\n",
            "function f() {\n    log(/* keep\n   me */\n      1);\n}\n",
        ),
        // ---- Python triple-quoted docstring
        (
            "py docstring",
            Python,
            "show($X)",
            "log(\"\"\"a\n  b\"\"\",\n  $X)",
            "def f():\n    show(1)\n",
            "def f():\n    log(\"\"\"a\n  b\"\"\",\n      1)\n",
        ),
        // ---- Python `#` comment
        //
        // A `#` comment runs to END OF LINE, so its continuation lines are not
        // part of the comment and are re-indented as ordinary text. The relative
        // offset is preserved exactly (2 -> 6, 8 -> 12), which is what
        // "the comment's own bytes survive" means for a line comment: the `#`
        // text and its spacing are untouched, and a continuation line keeps its
        // alignment relative to the comment. The first version of this case
        // asserted the continuation was verbatim; the measurement showed 8 spaces
        // of source indentation becoming 12, which is correct behaviour and was a
        // wrong expectation here.
        (
            "py comment",
            Python,
            "show($X)",
            "log(1)  # keep\n  me\nshow($X)",
            "def f():\n    show(1)\n",
            "def f():\n    log(1)  # keep\n      me\n    show(1)\n",
        ),
        // ---- Rust raw string with an interior newline
        (
            "rust raw string",
            Rust,
            "show($X)",
            "log(r\"a\n  b\",\n  $X)",
            "fn f() {\n    show(1);\n}\n",
            "fn f() {\n    log(r\"a\n  b\",\n      1);\n}\n",
        ),
        // ---- Go interpreted string with an escaped newline
        (
            "go string escape",
            Go,
            "show($X)",
            "log(\"a\\n  b\",\n  $X)",
            "package p\n\nfunc f() {\n\tshow(1)\n}\n",
            "package p\n\nfunc f() {\n\tlog(\"a\\n  b\",\n\t  1)\n}\n",
        ),
        // ---- Go raw backtick string, which is Go's own docstring form
        (
            "go raw string",
            Go,
            "show($X)",
            "log(`a\n  b`,\n  $X)",
            "package p\n\nfunc f() {\n\tshow(1)\n}\n",
            "package p\n\nfunc f() {\n\tlog(`a\n  b`,\n\t  1)\n}\n",
        ),
        // ---- TypeScript template literal
        (
            "ts template literal",
            TypeScript,
            "show($X)",
            "log(`a\n  b`,\n  $X)",
            "function f(): void {\n    show(1);\n}\n",
            "function f(): void {\n    log(`a\n  b`,\n      1);\n}\n",
        ),
    ];

    for (label, lang, pat, repl, src, expected) in CASES {
        let (new, out) = run(*lang, pat, repl, src);
        assert_eq!(
            &new, expected,
            "{label}: the verbatim region must survive byte for byte while only $X is re-indented",
        );
        // And the guard that keeps this a property rather than a snapshot: each
        // verbatim region's interior must appear UNCHANGED in the output. If the
        // re-indenter reflowed, trimmed or re-spaced them, this fails even when
        // the surrounding indentation happened to come out right.
        //
        // `THE_VERBATIM` is what survives: for a string or template literal or
        // docstring the interior, for a comment the text after the marker. It is
        // listed per case rather than guessed, so adding a case without saying
        // what must survive is caught here.
        const VERBATIM: &[(&str, &str)] = &[
            ("js template literal", "a\n  b"),
            ("js string escape", "a\\n  b"),
            ("js block comment", "keep\n   me"),
            ("py docstring", "a\n  b"),
            ("py comment", "# keep"),
            ("rust raw string", "a\n  b"),
            ("go string escape", "a\\n  b"),
            ("go raw string", "a\n  b"),
            ("ts template literal", "a\n  b"),
        ];
        let (_, must_survive) = VERBATIM
            .iter()
            .find(|(l, _)| *l == *label)
            .unwrap_or_else(|| panic!("{label} is listed in CASES but not in VERBATIM"));
        assert!(
            new.contains(must_survive),
            "{label}: {must_survive:?} must appear unchanged in the output",
        );
        // Every edit must be a valid edit set for the source, whatever happened.
        assert!(
            out.edits.is_empty() || !new.is_empty(),
            "{label}: an empty result with edits is impossible",
        );
    }
}

#[test]
fn identical_expansions_make_no_edit_and_too_many_matches_refuse_everything() {
    let (new, out) = run(JavaScript, "foo($X)", "foo($X)", "foo(1);\n");
    assert_eq!(
        (new.as_str(), out.edits.len(), out.matches_found),
        ("foo(1);\n", 0, 1)
    );

    let src = "f(1);\nf(2);\nf(3);\nf(4);\nf(5);\n";
    let e = run_with(JavaScript, "f($X)", "g($X)", src, false, 3).unwrap_err();
    assert_eq!(
        e.code,
        ErrorCode::LimitExceeded,
        "a partial rewrite is never produced"
    );
    assert_eq!(
        run_with(JavaScript, "f($X)", "g($X)", src, false, 5)
            .unwrap()
            .1
            .edits
            .len(),
        5
    );
}

#[test]
fn template_variables_are_checked_even_when_nothing_matches() {
    let none = "nothing();\n";
    for (pat, repl) in [
        ("foo($X)", "bar($Y)"),
        ("foo($$$A)", "bar($A)"),
        ("foo($A)", "bar($$$A)"),
    ] {
        let e = run_with(JavaScript, pat, repl, none, false, 1000).unwrap_err();
        assert_eq!(e.code, ErrorCode::InvalidPattern, "{pat} -> {repl}");
    }
    // `$$` is a literal dollar and needs no binding
    let (new, _) = run(JavaScript, "foo($X)", "cost($X, '$$5')", "foo(1);\n");
    assert_eq!(new, "cost(1, '$5');\n");
}

fn mk<'a>(p: &'a Pattern) -> RewriteRequest<'a> {
    RewriteRequest {
        pattern: p,
        rule: None,
        replacement: "bar($X)",
        allow_comment_loss: false,
        search_budget: SearchBudget::default(),
        parse_budget: pb(),
        max_expansion_bytes: 1 << 20,
    }
}

#[test]
fn mismatched_inputs_are_invalid_args_not_panics() {
    let p = Pattern::compile(JavaScript, "foo($X)").unwrap();
    let parsed = parse(Python, "foo(1)\n", &pb()).unwrap();
    assert_eq!(
        rewrite_file(&parsed, "foo(1)\n", &mk(&p)).unwrap_err().code,
        ErrorCode::InvalidArgs,
        "pattern for another language"
    );
    let js = parse(JavaScript, "foo(1);\n", &pb()).unwrap();
    assert_eq!(
        rewrite_file(&js, "foo(1); // other text\n", &mk(&p))
            .unwrap_err()
            .code,
        ErrorCode::InvalidArgs,
        "source is not what was parsed"
    );
}

#[test]
fn a_huge_expansion_is_a_limit_error() {
    let p = Pattern::compile(JavaScript, "foo($X)").unwrap();
    let src = "foo(1);\n";
    let parsed = parse(JavaScript, src, &pb()).unwrap();
    let big = "x".repeat(5000);
    let repl = format!("{big} + $X");
    let req = RewriteRequest {
        pattern: &p,
        rule: None,
        replacement: &repl,
        allow_comment_loss: false,
        search_budget: SearchBudget::default(),
        parse_budget: pb(),
        max_expansion_bytes: 1000,
    };
    assert_eq!(
        rewrite_file(&parsed, src, &req).unwrap_err().code,
        ErrorCode::LimitExceeded
    );
}

// ---- the oracle: grouping preserves meaning, checked against an independent reference -----------
//
// For random expressions X, rewrite `double(X)` to `$X * 2`. The reference is the text
// `(X) * 2`, which has the intended meaning by construction. The rewritten text and the
// reference must have the same syntax tree once redundant parentheses are ignored. No
// precedence table is involved on either side, so a wrong decision to wrap (or not to wrap)
// shows up as a different tree.

struct Lcg(u64);
impl Lcg {
    fn below(&mut self, n: usize) -> usize {
        self.0 = self
            .0
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        ((self.0 >> 33) as usize) % n.max(1)
    }
}

fn js_expr(r: &mut Lcg, depth: u32) -> String {
    let atoms = ["a", "b", "1", "\"s\"", "x.y", "f(a)", "xs[1]", "this"];
    if depth == 0 || r.below(4) == 0 {
        return atoms[r.below(atoms.len())].to_string();
    }
    let l = js_expr(r, depth - 1);
    let rr = js_expr(r, depth - 1);
    match r.below(9) {
        0 => format!("{l} + {rr}"),
        1 => format!("{l} - {rr}"),
        2 => format!("{l} * {rr}"),
        3 => format!("{l} / {rr}"),
        4 => format!("{l} && {rr}"),
        5 => format!("{l} == {rr}"),
        6 => format!("{l} ? {rr} : {l}"),
        7 => format!("!{rr}"),
        _ => format!("({l}) + {rr}"),
    }
}

fn py_expr(r: &mut Lcg, depth: u32) -> String {
    let atoms = ["a", "b", "1", "\"s\"", "x.y", "f(a)", "xs[1]", "None"];
    if depth == 0 || r.below(4) == 0 {
        return atoms[r.below(atoms.len())].to_string();
    }
    let l = py_expr(r, depth - 1);
    let rr = py_expr(r, depth - 1);
    match r.below(10) {
        0 => format!("{l} + {rr}"),
        1 => format!("{l} - {rr}"),
        2 => format!("{l} * {rr}"),
        3 => format!("{l} // {rr}"),
        4 => format!("{l} and {rr}"),
        5 => format!("{l} or {rr}"),
        6 => format!("{l} == {rr}"),
        7 => format!("{l} if {rr} else {l}"),
        8 => format!("not {rr}"),
        _ => format!("({l}) + {rr}"),
    }
}

/// The s-expression of the named nodes with `parenthesized_expression` wrappers removed.
fn shape(node: tree_sitter::Node<'_>, out: &mut String) {
    if node.kind() == "parenthesized_expression" {
        let mut c = node.walk();
        for ch in node.named_children(&mut c) {
            shape(ch, out);
        }
        return;
    }
    out.push('(');
    out.push_str(node.kind());
    let mut c = node.walk();
    for ch in node.named_children(&mut c) {
        out.push(' ');
        shape(ch, out);
    }
    out.push(')');
}

fn shape_of(lang: Language, src: &str) -> Option<String> {
    let p = parse(lang, src, &pb()).unwrap();
    if p.error_count != 0 {
        return None;
    }
    let mut s = String::new();
    shape(p.tree.root_node(), &mut s);
    Some(s)
}

fn property(
    lang: Language,
    gen_expr: fn(&mut Lcg, u32) -> String,
    wrap_src: fn(&str) -> String,
    wrap_ref: fn(&str) -> String,
    seed: u64,
) {
    let mut r = Lcg(seed);
    let mut checked = 0;
    let mut wrapped_some = 0;
    for _ in 0..600 {
        let x = gen_expr(&mut r, 3);
        let src = wrap_src(&x);
        let reference = wrap_ref(&x);
        let (Some(_), Some(want)) = (shape_of(lang, &src), shape_of(lang, &reference)) else {
            continue; // the generator produced something this grammar rejects
        };
        let (new, out) = run(lang, "double($X)", "$X * 2", &src);
        let got = shape_of(lang, &new)
            .unwrap_or_else(|| panic!("the rewrite of {src:?} does not parse: {new:?}"));
        assert_eq!(got, want, "meaning changed: {src:?} became {new:?}");
        checked += 1;
        wrapped_some += usize::from(!out.wrapped.is_empty());
    }
    assert!(checked > 300, "{lang:?}: only {checked} usable cases");
    assert!(
        wrapped_some > 30,
        "{lang:?}: the generator must exercise the wrapping path ({wrapped_some})"
    );
}

#[test]
fn grouping_preserves_the_meaning_of_random_javascript_expressions() {
    property(
        JavaScript,
        js_expr,
        |x| format!("double({x});\n"),
        |x| format!("({x}) * 2;\n"),
        20261002,
    );
}

#[test]
fn grouping_preserves_the_meaning_of_random_python_expressions() {
    property(
        Python,
        py_expr,
        |x| format!("r = double({x})\n"),
        |x| format!("r = ({x}) * 2\n"),
        7,
    );
}

// ---- CR finding: `$` handling is the same everywhere; `verbatim` only affects layout -----------

/// A metavariable inside a string, template literal or comment of the replacement is substituted
/// like anywhere else (the captured SOURCE TEXT is inserted, never wrapped in parentheses there,
/// because it is not an expression in that position), and `$$` still becomes `$`. Only line breaks
/// and indentation are left alone inside those regions. If the inserted text breaks the literal
/// (a quote inside a string), the syntax gate at preview/apply is what refuses it.
#[test]
fn metavariables_inside_strings_templates_and_comments_are_substituted_without_parentheses() {
    let (new, out) = run(JavaScript, "f($X)", "g(\"v=$X\")", "f(a + b);\n");
    assert_eq!(new, "g(\"v=a + b\");\n");
    assert!(
        out.wrapped.is_empty(),
        "no parentheses inside a string: {:?}",
        out.wrapped
    );
    let (new, _) = run(JavaScript, "f($X)", "g(`v=$X`)", "f(a + b);\n");
    assert_eq!(new, "g(`v=a + b`);\n");
    let (new, _) = run(JavaScript, "f($X)", "g(1) /* was $X */", "f(a + b);\n");
    assert_eq!(new, "g(1) /* was a + b */;\n");
    let (new, _) = run(Python, "f($X)", "g(\"v=$X\")", "y = f(a + b)\n");
    assert_eq!(new, "y = g(\"v=a + b\")\n");
    // `$$` and `$X` side by side in one string
    let (new, _) = run(JavaScript, "f($X)", "g(\"cost $$5 $X\")", "f(a + b);\n");
    assert_eq!(new, "g(\"cost $5 a + b\");\n");
    // the same variable inside a string AND as an expression: only the expression is wrapped
    let (new, out) = run(JavaScript, "f($X)", "g(\"$X\", $X * 2)", "f(a + b);\n");
    assert_eq!(new, "g(\"a + b\", (a + b) * 2);\n");
    assert_eq!(out.wrapped.len(), 1);
    // a list capture inside a string is its text, separators included
    let (new, _) = run(JavaScript, "f($$$A)", "g(\"args: $$$A\")", "f(1, 2);\n");
    assert_eq!(new, "g(\"args: 1, 2\");\n");
}
