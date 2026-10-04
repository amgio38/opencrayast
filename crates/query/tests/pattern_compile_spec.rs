//! Spec for ISSUE-PATTERN-COMPILE. Add cases; never weaken these.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
use opencrayast_core::ErrorCode;
use opencrayast_core::error::ToolError;
use opencrayast_lang::{Language, ParseBudget, parse};
use opencrayast_query::pattern::{CaptureKind, MetaVar, Pattern, SearchBudget, search};
use std::time::Duration;

fn budget() -> ParseBudget {
    ParseBudget {
        max_bytes: 1 << 24,
        timeout: Duration::from_secs(10),
        max_depth: 8192,
        max_nodes: 10_000_000,
    }
}

fn texts(lang: Language, pat: &str, src: &str) -> Vec<String> {
    let p = Pattern::compile(lang, pat).unwrap();
    let parsed = parse(lang, src, &budget()).unwrap();
    search(&parsed, src, &p, None, &SearchBudget::default())
        .unwrap()
        .matches
        .into_iter()
        .map(|m| m.text)
        .collect()
}

#[test]
fn metavariables_are_listed_in_order_of_first_appearance_without_duplicates() {
    let p = Pattern::compile(Language::JavaScript, "foo($A, $$$B, $A, $_, $$$, $C)").unwrap();
    assert_eq!(
        p.metavars(),
        vec![
            MetaVar {
                name: "A".into(),
                kind: CaptureKind::One
            },
            MetaVar {
                name: "B".into(),
                kind: CaptureKind::List
            },
            MetaVar {
                name: "C".into(),
                kind: CaptureKind::One
            },
        ]
    );
    assert_eq!(p.language(), Language::JavaScript);
    assert_eq!(p.source(), "foo($A, $$$B, $A, $_, $$$, $C)");
}

#[test]
fn double_dollar_is_a_literal_dollar() {
    let p = Pattern::compile(Language::JavaScript, "foo($$X)").unwrap();
    assert!(p.metavars().is_empty());
    assert_eq!(
        texts(Language::JavaScript, "foo($$X)", "foo($X); foo(y);"),
        ["foo($X)"]
    );
}

#[test]
fn a_dollar_not_followed_by_a_valid_name_is_just_code() {
    // `$x` (lower case) is not a metavariable; in JavaScript it is an ordinary identifier.
    let p = Pattern::compile(Language::JavaScript, "foo($x)").unwrap();
    assert!(p.metavars().is_empty());
    assert_eq!(
        texts(Language::JavaScript, "foo($x)", "foo($x); foo(y);"),
        ["foo($x)"]
    );
}

#[test]
fn explain_shows_the_parsed_tree_exactly() {
    let p = Pattern::compile(Language::JavaScript, "foo($A, $$$B)").unwrap();
    assert_eq!(
        p.explain(),
        "call_expression\n  identifier \"foo\"\n  arguments\n    \"(\"\n    $A (one)\n    \",\"\n    $$$B (list)\n    \")\"\n"
    );
    assert_eq!(p.explain(), p.explain(), "deterministic");
}

#[test]
fn explain_marks_anonymous_metavariables_and_warns_about_contexts() {
    let p = Pattern::compile(Language::JavaScript, "foo($_, $$$)").unwrap();
    let e = p.explain();
    assert!(
        e.contains("    $_ (one)\n") && e.contains("    $$$ (list)\n"),
        "{e}"
    );
    // Go only parses an expression inside a function body: the explain output says so.
    let g = Pattern::compile(Language::Go, "fmt.Println($$$A)").unwrap();
    assert!(g.explain().starts_with("warning:"), "{}", g.explain());
    assert!(g.explain().contains("call_expression"), "{}", g.explain());
    assert!(!p.explain().starts_with("warning:"));
}

#[test]
fn the_root_is_the_innermost_node_covering_exactly_the_pattern_text() {
    // Without `;` the root is the call expression, so it matches calls anywhere ...
    assert_eq!(
        texts(Language::JavaScript, "foo($X)", "foo(1);\nbar(foo(2));"),
        ["foo(1)", "foo(2)"]
    );
    // ... with `;` it is the whole statement, so it only matches statements.
    assert_eq!(
        texts(Language::JavaScript, "foo($X);", "foo(1);\nbar(foo(2));"),
        ["foo(1);"]
    );
    assert_eq!(
        texts(
            Language::JavaScript,
            "return $X;",
            "function f() { return 1; }\nfunction g() { return; }"
        ),
        ["return 1;"]
    );
}

#[test]
fn expression_and_statement_patterns_compile_in_rust_and_go_through_the_body_context() {
    let r = Pattern::compile(Language::Rust, "$X.unwrap()").unwrap();
    assert_eq!(r.metavars().len(), 1);
    assert!(r.explain().starts_with("warning:"), "{}", r.explain());
    assert!(Pattern::compile(Language::Rust, "let $X = $Y;").is_ok());
    assert!(Pattern::compile(Language::Rust, "fn $NAME($$$PARAMS) -> $RET { $$$BODY }").is_ok());
    assert!(Pattern::compile(Language::Go, "fmt.Println($$$A)").is_ok());
    assert!(Pattern::compile(Language::Go, "func $F($$$P) { $$$B }").is_ok());
    assert!(Pattern::compile(Language::Python, "def $F($$$P):\n    return $X").is_ok());
    assert!(Pattern::compile(Language::Python, "$X == None").is_ok());
    assert!(Pattern::compile(Language::TypeScript, "const $X: $T = $V").is_ok());
}

fn invalid(lang: Language, pat: &str) -> opencrayast_query::pattern::PatternError {
    Pattern::compile(lang, pat)
        .err()
        .unwrap_or_else(|| panic!("{lang:?} {pat:?} must be invalid"))
}

#[test]
fn invalid_patterns_are_refused_with_a_suggestion() {
    for (lang, pat) in [
        (Language::JavaScript, ""),
        (Language::JavaScript, "   \n "),
        (Language::JavaScript, "a; b;"),
        (Language::JavaScript, "$$$ARGS"),
        (Language::JavaScript, "$$$"),
        (Language::JavaScript, "function ("),
        (Language::JavaScript, "{ let x = "),
        (Language::Rust, "fn ("),
        (Language::Python, "def (:"),
        (Language::Go, "func ("),
        (Language::TypeScript, "interface {"),
    ] {
        let e = invalid(lang, pat);
        assert!(
            !e.message.is_empty() && !e.suggestion.is_empty(),
            "{lang:?} {pat:?}: {e:?}"
        );
        if let Some(p) = e.position {
            assert!(
                p <= pat.len(),
                "{lang:?} {pat:?}: position {p} past the end"
            );
        }
    }
    // syntax errors report a position, the empty pattern does not need one
    assert!(
        invalid(Language::JavaScript, "function (")
            .position
            .is_some()
    );
}

#[test]
fn pattern_error_becomes_invalid_pattern_with_the_position_in_the_message() {
    let e: ToolError = invalid(Language::JavaScript, "function (").into();
    assert_eq!(e.code, ErrorCode::InvalidPattern);
    assert!(e.message.contains("at byte"), "{}", e.message);
    assert!(!e.next.is_empty());
}

#[test]
fn size_limits() {
    assert!(
        Pattern::compile(
            Language::JavaScript,
            &format!("foo({})", "1,".repeat(20_000))
        )
        .is_err(),
        "over 16 KiB"
    );
    let many: String = (0..70)
        .map(|i| format!("$V{i}"))
        .collect::<Vec<_>>()
        .join(", ");
    assert!(
        Pattern::compile(Language::JavaScript, &format!("foo({many})")).is_err(),
        "more than 64 metavariables"
    );
    let ok: String = (0..64)
        .map(|i| format!("$V{i}"))
        .collect::<Vec<_>>()
        .join(", ");
    assert!(
        Pattern::compile(Language::JavaScript, &format!("foo({ok})")).is_ok(),
        "exactly 64 is allowed"
    );
    let deep = format!("{}1{}", "(".repeat(100), ")".repeat(100));
    assert!(
        Pattern::compile(Language::JavaScript, &deep).is_err(),
        "pattern depth over 64"
    );
}

#[test]
fn compilation_never_panics_on_hostile_text() {
    let mut x: u64 = 0x1357_9bdf_2468_ace0;
    let toks = [
        "$", "$$", "$$$", "$A", "$$$B", "(", ")", "{", "}", ";", "foo", "\n", "µ", "µµµX", "\u{0}",
        "é", "//", "/*", "'", "\"", "$_", "fn", "def", "func", "=>",
    ];
    for lang in Language::all() {
        for _ in 0..800 {
            let mut s = String::new();
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            for _ in 0..(x % 25) {
                x ^= x << 13;
                x ^= x >> 7;
                x ^= x << 17;
                s.push_str(toks[(x as usize) % toks.len()]);
                s.push(' ');
            }
            let _ = Pattern::compile(*lang, &s);
        }
    }
}

/// Search-dependent cases from this file stay red until ISSUE-PATTERN-MATCH lands.
/// These stubs check that compilation alone already produces the trees those tests need.
#[test]
fn stub_search_cases_have_sensible_compiled_trees() {
    // double_dollar → literal `$`, no metavars; leaf text includes `$X`.
    let p = Pattern::compile(Language::JavaScript, "foo($$X)").unwrap();
    assert!(p.metavars().is_empty());
    let e = p.explain();
    assert!(e.contains("call_expression"), "{e}");
    assert!(e.contains("$X") || e.contains("\"$X\""), "{e}");

    // `$x` is not a metavar (lower case).
    let p = Pattern::compile(Language::JavaScript, "foo($x)").unwrap();
    assert!(p.metavars().is_empty());
    assert!(
        p.explain().contains("$x") || p.explain().contains("\"$x\""),
        "{}",
        p.explain()
    );

    // Root without `;` is the call; with `;` is the statement (explain kinds differ).
    let call = Pattern::compile(Language::JavaScript, "foo($X)").unwrap();
    assert!(
        call.explain().starts_with("call_expression\n"),
        "{}",
        call.explain()
    );
    let stmt = Pattern::compile(Language::JavaScript, "foo($X);").unwrap();
    assert!(
        stmt.explain().starts_with("expression_statement\n"),
        "{}",
        stmt.explain()
    );
    let ret = Pattern::compile(Language::JavaScript, "return $X;").unwrap();
    assert!(
        ret.explain().starts_with("return_statement\n"),
        "{}",
        ret.explain()
    );
}

#[test]
fn metavariable_boundaries() {
    // Valid names
    assert!(
        Pattern::compile(Language::JavaScript, "foo($A1)")
            .unwrap()
            .metavars()
            .len()
            == 1
    );
    assert!(
        Pattern::compile(Language::JavaScript, "foo($A_B)")
            .unwrap()
            .metavars()[0]
            .name
            == "A_B"
    );
    // Not metavars
    assert!(
        Pattern::compile(Language::JavaScript, "foo($1)")
            .unwrap()
            .metavars()
            .is_empty()
    );
    assert!(
        Pattern::compile(Language::JavaScript, "foo($a)")
            .unwrap()
            .metavars()
            .is_empty()
    );
    assert!(
        Pattern::compile(Language::JavaScript, "foo($$$a)")
            .unwrap()
            .metavars()
            .is_empty()
    );
    // `$ A` — dollar then space: not a metavar name (may also fail to parse).
    if let Ok(p) = Pattern::compile(Language::JavaScript, "foo($ A)") {
        assert!(p.metavars().is_empty(), "{:?}", p.metavars());
    }
}

#[test]
fn one_and_list_same_name_conflicts() {
    let e = invalid(Language::JavaScript, "foo($A, $$$A)");
    assert!(
        e.message.contains("both") || e.message.contains("list"),
        "{e:?}"
    );
}

#[test]
fn unicode_and_crlf_patterns_compile() {
    assert!(Pattern::compile(Language::JavaScript, "foo(\"世界\")").is_ok());
    assert!(Pattern::compile(Language::JavaScript, "foo($X);\r\n").is_ok());
    assert!(Pattern::compile(Language::Python, "x = \"héllo\"").is_ok());
}

#[test]
fn explain_golden_rust_go_python() {
    let r = Pattern::compile(Language::Rust, "$X.unwrap()").unwrap();
    let er = r.explain();
    assert!(er.starts_with("warning:"), "{er}");
    assert!(
        er.contains("unwrap") || er.contains("call_expression") || er.contains("field"),
        "{er}"
    );

    let g = Pattern::compile(Language::Go, "fmt.Println($$$A)").unwrap();
    let eg = g.explain();
    assert!(eg.starts_with("warning:"), "{eg}");
    assert!(eg.contains("call_expression"), "{eg}");
    assert!(eg.contains("$$$A (list)"), "{eg}");

    let p = Pattern::compile(Language::Python, "$X == None").unwrap();
    let ep = p.explain();
    assert!(!ep.starts_with("warning:"), "{ep}");
    assert!(
        ep.contains("comparison") || ep.contains("None") || ep.contains("$X (one)"),
        "{ep}"
    );
}

#[test]
fn bad_patterns_twenty_per_language_report_positions() {
    let deep = format!("{}1{}", "(".repeat(100), ")".repeat(100));
    let huge = "x".repeat(20_000);
    let many_vars = format!(
        "foo({})",
        (0..65)
            .map(|i| format!("$V{i}"))
            .collect::<Vec<_>>()
            .join(",")
    );

    let langs = [
        Language::JavaScript,
        Language::TypeScript,
        Language::Rust,
        Language::Python,
        Language::Go,
    ];
    for lang in langs {
        let mut pats: Vec<String> = vec![
            "".into(),
            "   \n ".into(),
            "a; b;".into(),
            "$$$".into(),
            "$$$ONLY".into(),
            "function (".into(),
            "{ let x = ".into(),
            "((((".into(),
            "foo($$$".into(),
            "};{".into(),
            "???".into(),
            "/*".into(),
            "foo($A, $$$A)".into(),
            huge.clone(),
            many_vars.clone(),
            deep.clone(),
        ];
        match lang {
            Language::JavaScript => {
                pats.extend([
                    "if (".into(),
                    "class {".into(),
                    "=>".into(),
                    "'unterminated".into(),
                ]);
            }
            Language::TypeScript => {
                pats.extend([
                    "interface {".into(),
                    "type =".into(),
                    "enum {".into(),
                    "implements {".into(),
                ]);
            }
            Language::Rust => {
                pats.extend([
                    "fn (".into(),
                    "struct {".into(),
                    "impl {".into(),
                    "let =".into(),
                ]);
            }
            Language::Python => {
                pats.extend([
                    "def (:".into(),
                    "class :".into(),
                    "if :".into(),
                    "lambda :".into(),
                ]);
            }
            Language::Go => {
                pats.extend([
                    "func (".into(),
                    "type {".into(),
                    "import (".into(),
                    "var =".into(),
                ]);
            }
            _ => {}
        }
        assert_eq!(pats.len(), 20, "{lang:?}");
        for pat in &pats {
            let e = Pattern::compile(lang, pat);
            assert!(
                e.is_err(),
                "{lang:?} should reject {:?}",
                &pat[..pat.len().min(80)]
            );
            let err = e.unwrap_err();
            if let Some(p) = err.position {
                assert!(p <= pat.len(), "{lang:?} pos {p} len {}", pat.len());
            }
            assert!(!err.message.is_empty() && !err.suggestion.is_empty());
        }
    }
}

#[test]
fn bare_metavariables_in_item_bodies_compile() {
    // the grammars wrap a bare identifier in an ERROR node here; it is the metavariable
    for (lang, pat) in [
        (Language::Rust, "impl $T { $$$B }"),
        (Language::Rust, "struct $S { $$$F }"),
        (Language::Rust, "trait $T { $$$B }"),
        (Language::Rust, "enum $E { $$$V }"),
        (Language::Go, "type $T struct { $$$F }"),
        (Language::TypeScript, "class $C { $$$B }"),
        (Language::TypeScript, "interface $I { $$$M }"),
    ] {
        let p = Pattern::compile(lang, pat).unwrap_or_else(|e| panic!("{lang:?} {pat:?}: {e:?}"));
        // each pattern has two named metavars (type/name + body/fields)
        assert_eq!(p.metavars().len(), 2, "{lang:?} {pat:?}");
    }
    // ... but an ERROR that is more than one metavariable token is still an error
    assert!(Pattern::compile(Language::Rust, "impl $T { $A $B }").is_err());
    assert!(Pattern::compile(Language::Rust, "impl $T { fn ( }").is_err());
}

#[test]
fn error_positions_point_at_the_problem_not_at_the_start_of_the_pattern() {
    for (lang, pat, needle) in [
        (
            Language::JavaScript,
            "foo($$$ARGS, $$$MORE, function (",
            "function",
        ),
        (Language::JavaScript, "$A + $B + (", "("),
        (Language::Python, "def $F($$$P):\n    return (", "return"),
    ] {
        let e = Pattern::compile(lang, pat).err().unwrap();
        let start = pat.find(needle).unwrap();
        let pos = e.position.expect("a syntax error has a position");
        assert!(
            pos >= start && pos <= pat.len(),
            "{lang:?} {pat:?}: position {pos}, the problem starts at {start}"
        );
        assert!(pat.is_char_boundary(pos));
    }
}
