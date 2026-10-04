//! Spec for ISSUE-PATTERN-MATCH. Add cases; never weaken these.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
use opencrayast_core::ErrorCode;
use opencrayast_lang::{Language, ParseBudget, parse};
use opencrayast_query::pattern::{CaptureKind, Pattern, SearchBudget, search};
use std::time::{Duration, Instant};

fn pbudget() -> ParseBudget {
    ParseBudget {
        max_bytes: 1 << 24,
        timeout: Duration::from_secs(20),
        max_depth: 8192,
        max_nodes: 10_000_000,
    }
}

type Found = Vec<(String, Vec<(String, String)>)>;

fn find(lang: Language, pat: &str, src: &str) -> Found {
    let p = Pattern::compile(lang, pat).unwrap();
    let parsed = parse(lang, src, &pbudget()).unwrap();
    search(&parsed, src, &p, None, &SearchBudget::default())
        .unwrap()
        .matches
        .into_iter()
        .map(|m| {
            (
                m.text,
                m.captures.into_iter().map(|c| (c.name, c.text)).collect(),
            )
        })
        .collect()
}

fn f(text: &str, caps: &[(&str, &str)]) -> (String, Vec<(String, String)>) {
    (
        text.to_string(),
        caps.iter()
            .map(|(a, b)| (a.to_string(), b.to_string()))
            .collect(),
    )
}

use Language::{Go, JavaScript, Python, Rust, TypeScript};

#[test]
fn variadic_arguments_are_captured_with_their_separators() {
    let src = "console.log(\"start\", id);\nconsole.log();\nfoo(console.log(3));\nother.log(1);\n";
    assert_eq!(
        find(JavaScript, "console.log($$$ARGS)", src),
        vec![
            f("console.log(\"start\", id)", &[("ARGS", "\"start\", id")]),
            f("console.log()", &[("ARGS", "")]),
            f("console.log(3)", &[("ARGS", "3")]),
        ]
    );
}

#[test]
fn match_positions_are_one_based_lines_and_byte_columns() {
    let src = "console.log(\"start\", id);\n  console.log(x);\n";
    let p = Pattern::compile(JavaScript, "console.log($$$ARGS)").unwrap();
    let parsed = parse(JavaScript, src, &pbudget()).unwrap();
    let out = search(&parsed, src, &p, None, &SearchBudget::default()).unwrap();
    let m = &out.matches[0];
    assert_eq!(
        (m.start_line, m.start_col, m.end_line, m.end_col),
        (1, 1, 1, 25)
    );
    assert_eq!((m.start_byte, m.end_byte), (0, 24));
    let m = &out.matches[1];
    assert_eq!(
        (m.start_line, m.start_col, m.end_line, m.end_col),
        (2, 3, 2, 17)
    );
    let c = &m.captures[0];
    assert_eq!(
        (c.name.as_str(), c.kind, c.text.as_str()),
        ("ARGS", CaptureKind::List, "x")
    );
    assert_eq!(&src[c.start_byte..c.end_byte], "x");
}

#[test]
fn a_repeated_metavariable_requires_structurally_identical_nodes() {
    let src = "x == x;\ny == z;\n(a + b) == (a+b);\na == a;\nfoo(a == a);\n";
    assert_eq!(
        find(JavaScript, "$A == $A", src),
        vec![
            f("x == x", &[("A", "x")]),
            f("(a + b) == (a+b)", &[("A", "(a + b)")]),
            f("a == a", &[("A", "a")]),
            f("a == a", &[("A", "a")]),
        ]
    );
    assert_eq!(
        find(
            JavaScript,
            "foo($X, $X)",
            "foo(a, a); foo(a, b); foo(1+2, 1 + 2);"
        ),
        vec![
            f("foo(a, a)", &[("X", "a")]),
            f("foo(1+2, 1 + 2)", &[("X", "1+2")])
        ]
    );
}

#[test]
fn nested_matches_are_all_reported_outer_first() {
    assert_eq!(
        find(JavaScript, "f($X)", "f(f(1));"),
        vec![f("f(f(1))", &[("X", "f(1)")]), f("f(1)", &[("X", "1")])]
    );
}

#[test]
fn comments_and_whitespace_between_nodes_are_ignored_but_not_operators_or_literals() {
    assert_eq!(
        find(JavaScript, "foo(a, b)", "foo(a /* c */, b);\nfoo(a, c);"),
        vec![f("foo(a /* c */, b)", &[])]
    );
    assert_eq!(
        find(JavaScript, "$A + $B", "1 + 2;\n1 - 2;").len(),
        1,
        "operators are compared"
    );
    assert_eq!(
        find(
            JavaScript,
            "foo(\"a\")",
            "foo(\"a\"); foo(\"b\"); foo('a');"
        ),
        vec![f("foo(\"a\")", &[])],
        "string leaves compare exactly"
    );
}

#[test]
fn list_metavariables_are_lazy_and_may_be_empty() {
    assert_eq!(
        find(JavaScript, "[$A, $$$REST]", "[1, 2, 3];"),
        vec![f("[1, 2, 3]", &[("A", "1"), ("REST", "2, 3")])]
    );
    assert_eq!(
        find(JavaScript, "[$$$X, $A]", "[1, 2, 3];"),
        vec![f("[1, 2, 3]", &[("X", "1, 2"), ("A", "3")])]
    );
    assert_eq!(
        find(JavaScript, "[$$$ALL]", "[];"),
        vec![f("[]", &[("ALL", "")])]
    );
    assert_eq!(
        find(JavaScript, "foo($$$ARGS)", "foo();"),
        vec![f("foo()", &[("ARGS", "")])]
    );
    // the comma after `$A` is part of the pattern, so a one-element array does not match
    assert!(find(JavaScript, "[$A, $$$REST]", "[1];").is_empty());
}

#[test]
fn anonymous_metavariables_match_without_capturing() {
    assert_eq!(
        find(JavaScript, "foo($_, $_)", "foo(1, 2); foo(1); foo(1,2,3);"),
        vec![f("foo(1, 2)", &[])]
    );
    assert_eq!(
        find(JavaScript, "foo($$$)", "foo(1, 2); foo(1); foo(1,2,3);"),
        vec![f("foo(1, 2)", &[]), f("foo(1)", &[]), f("foo(1,2,3)", &[])]
    );
}

#[test]
fn a_statement_list_metavariable_inside_a_block() {
    assert_eq!(
        find(
            JavaScript,
            "if ($C) { $$$B }",
            "if (a) { x(); y(); }\nif (b) {}\n"
        ),
        vec![
            f("if (a) { x(); y(); }", &[("C", "a"), ("B", "x(); y();")]),
            f("if (b) {}", &[("C", "b"), ("B", "")])
        ]
    );
}

#[test]
fn rust_patterns() {
    let src = "fn f() {\n    a.unwrap();\n    b.c().unwrap();\n    d.expect(\"x\");\n}\n";
    assert_eq!(
        find(Rust, "$X.unwrap()", src),
        vec![
            f("a.unwrap()", &[("X", "a")]),
            f("b.c().unwrap()", &[("X", "b.c()")])
        ]
    );
    let fns = "fn add(a: i32, b: i32) -> i32 { a + b }\nfn noret() {}\nfn id(x: u8) -> u8 { x }\n";
    assert_eq!(
        find(Rust, "fn $NAME($$$PARAMS) -> $RET { $$$BODY }", fns),
        vec![
            f(
                "fn add(a: i32, b: i32) -> i32 { a + b }",
                &[
                    ("NAME", "add"),
                    ("PARAMS", "a: i32, b: i32"),
                    ("RET", "i32"),
                    ("BODY", "a + b")
                ]
            ),
            f(
                "fn id(x: u8) -> u8 { x }",
                &[
                    ("NAME", "id"),
                    ("PARAMS", "x: u8"),
                    ("RET", "u8"),
                    ("BODY", "x")
                ]
            ),
        ]
    );
}

#[test]
fn python_patterns() {
    assert_eq!(
        find(Python, "print($X)", "print(1)\nprint(a, b)\nprint('hi')\n"),
        vec![
            f("print(1)", &[("X", "1")]),
            f("print('hi')", &[("X", "'hi'")])
        ]
    );
    assert_eq!(
        find(
            Python,
            "def $F($$$P):\n    return $X",
            "def add(a, b):\n    return a + b\n\ndef noop():\n    pass\n"
        ),
        vec![f(
            "def add(a, b):\n    return a + b",
            &[("F", "add"), ("P", "a, b"), ("X", "a + b")]
        )]
    );
    assert_eq!(
        find(
            Python,
            "$X == None",
            "if x == None:\n    pass\ny = (z == None)\n"
        ),
        vec![f("x == None", &[("X", "x")]), f("z == None", &[("X", "z")])]
    );
}

#[test]
fn go_patterns() {
    let src = "package main\n\nfunc main() {\n\tfmt.Println(\"a\", 1)\n\tfmt.Println()\n\tfmt.Printf(\"x\")\n}\n";
    assert_eq!(
        find(Go, "fmt.Println($$$A)", src),
        vec![
            f("fmt.Println(\"a\", 1)", &[("A", "\"a\", 1")]),
            f("fmt.Println()", &[("A", "")])
        ]
    );
}

#[test]
fn syntax_errors_in_the_source_do_not_abort_a_search() {
    assert_eq!(
        find(JavaScript, "foo($X)", "foo(1);\nfoo(;\nfoo(2);\n")
            .iter()
            .map(|m| m.0.as_str())
            .collect::<Vec<_>>(),
        ["foo(1)", "foo(2)"]
    );
}

#[test]
fn the_step_budget_stops_exponential_backtracking() {
    let items: Vec<String> = (0..200).map(|i| i.to_string()).collect();
    let src = format!("[{}];", items.join(", "));
    let p = Pattern::compile(JavaScript, "[$$$A, $$$B, $$$C, x]").unwrap();
    let parsed = parse(JavaScript, &src, &pbudget()).unwrap();
    let small = SearchBudget {
        max_steps: 100_000,
        ..Default::default()
    };
    let e = search(&parsed, &src, &p, None, &small).unwrap_err();
    assert_eq!(e.code, ErrorCode::BudgetExceeded);
    assert!(e.message.to_lowercase().contains("step"), "{}", e.message);
    // with the default budget the same search finishes (no `x` anywhere: zero matches)
    let t = Instant::now();
    let out = search(&parsed, &src, &p, None, &SearchBudget::default()).unwrap();
    assert!(out.matches.is_empty() && out.steps_used > 100_000);
    assert!(
        t.elapsed() < Duration::from_secs(30),
        "took {:?}",
        t.elapsed()
    );
}

#[test]
fn the_deadline_cancels_a_search() {
    let items: Vec<String> = (0..2000).map(|i| i.to_string()).collect();
    let src = format!("[{}];", items.join(", "));
    let p = Pattern::compile(JavaScript, "[$$$A, $$$B, $$$C, x]").unwrap();
    let parsed = parse(JavaScript, &src, &pbudget()).unwrap();
    let b = SearchBudget {
        deadline: Some(Instant::now()),
        ..Default::default()
    };
    let t = Instant::now();
    let e = search(&parsed, &src, &p, None, &b).unwrap_err();
    assert_eq!(e.code, ErrorCode::Timeout);
    assert!(
        t.elapsed() < Duration::from_secs(5),
        "cancel must be prompt, took {:?}",
        t.elapsed()
    );
}

#[test]
fn max_matches_truncates_and_says_so() {
    let src = "foo(1);\n".repeat(1500);
    let p = Pattern::compile(JavaScript, "foo($X)").unwrap();
    let parsed = parse(JavaScript, &src, &pbudget()).unwrap();
    let out = search(
        &parsed,
        &src,
        &p,
        None,
        &SearchBudget {
            max_matches: 10,
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(out.matches.len(), 10);
    assert!(out.truncated);
    let all = search(
        &parsed,
        &src,
        &p,
        None,
        &SearchBudget {
            max_matches: 5000,
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(all.matches.len(), 1500);
    assert!(!all.truncated);
}

#[test]
fn very_deep_sources_do_not_overflow_the_stack() {
    let n = 3000;
    let src = format!("{}1{};", "(".repeat(n), ")".repeat(n));
    let out = find(JavaScript, "1", &src);
    assert_eq!(out.len(), 1);
    let p = Pattern::compile(JavaScript, "($X)").unwrap();
    let parsed = parse(JavaScript, &src, &pbudget()).unwrap();
    let all = search(
        &parsed,
        &src,
        &p,
        None,
        &SearchBudget {
            max_matches: 10_000,
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(all.matches.len(), n);
}

#[test]
fn a_pattern_compiled_for_another_language_is_refused() {
    let p = Pattern::compile(Rust, "$X.unwrap()").unwrap();
    let parsed = parse(JavaScript, "a.unwrap();", &pbudget()).unwrap();
    assert_eq!(
        search(&parsed, "a.unwrap();", &p, None, &SearchBudget::default())
            .unwrap_err()
            .code,
        ErrorCode::InvalidArgs
    );
}

#[test]
fn random_sources_never_panic_and_matches_are_valid() {
    let toks = [
        "foo", "(", ")", "1", ",", ";", "{", "}", "[", "]", "+", "==", "x", "\n", "if", "return",
        "/*", "*/", "é", "=>", "'", "\"",
    ];
    let pats = [
        "foo($X)",
        "foo($$$A)",
        "$A + $B",
        "[$$$X, $Y]",
        "if ($C) { $$$B }",
        "$A == $A",
    ];
    let mut x: u64 = 0x2468_ace0_1357_9bdf;
    for _ in 0..1500 {
        let mut s = String::new();
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        for _ in 0..(x % 60) {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            s.push_str(toks[(x as usize) % toks.len()]);
            s.push(' ');
        }
        let Ok(parsed) = parse(JavaScript, &s, &pbudget()) else {
            continue;
        };
        for pat in pats {
            let p = Pattern::compile(JavaScript, pat).unwrap();
            let out = search(&parsed, &s, &p, None, &SearchBudget::default()).unwrap();
            for m in &out.matches {
                assert!(m.start_byte <= m.end_byte && m.end_byte <= s.len());
                assert!(s.is_char_boundary(m.start_byte) && s.is_char_boundary(m.end_byte));
                assert_eq!(m.text, &s[m.start_byte..m.end_byte]);
                assert!(m.start_line >= 1 && m.start_col >= 1 && m.end_line >= m.start_line);
            }
        }
    }
}

// ---- FIX-1 regressions ----

#[test]
fn rust_declarations_and_statements_still_match() {
    // FIX-1 changed Go's context order; Rust's order is unchanged, and these pin that `fn`,
    // `struct`, `impl` and `trait` still match as declarations while statements and expressions
    // still go through the function-body context.
    let src = "\
struct S;
impl S { fn m(&self) {} }
trait T { fn t(&self); }
fn f(a: i32) -> i32 { a }
fn g() {}
fn main() { let v = 1; v.unwrap(); }
";
    assert_eq!(find(Rust, "fn $F($$$P) -> $R { $$$B }", src).len(), 1);
    assert_eq!(find(Rust, "fn $F() { $$$B }", src).len(), 2, "g and main");
    assert_eq!(find(Rust, "struct $N;", src).len(), 1);
    assert_eq!(find(Rust, "impl $T { $$$B }", src).len(), 1);
    assert_eq!(find(Rust, "trait $N { $$$B }", src).len(), 1);
    // Statements and expressions use the body context.
    assert_eq!(find(Rust, "let $X = 1;", src).len(), 1);
    assert_eq!(find(Rust, "$X.unwrap()", src).len(), 1);
}

#[test]
fn rust_generic_functions_and_type_positions_match() {
    // The shape that was a TypeScript bug: a repeated variable whose two occurrences sit in
    // different grammar roles. With token equality this matches in Rust too.
    assert_eq!(
        find(
            Rust,
            "fn $F<T>(a: T) { $$$B }",
            "fn f<T>(a: T) { let _ = a; }\n"
        )
        .len(),
        1
    );
    assert_eq!(
        find(
            Rust,
            "fn $F<T>(a: T) -> T { $$$B }",
            "fn f<T>(a: T) -> T { a }\n"
        )
        .len(),
        1,
        "the same `T` in a parameter and in the return type is token-equal"
    );
    // A type position bound by one variable, matched against different types.
    assert_eq!(
        find(
            Rust,
            "fn f(x: $T) { $$$B }",
            "fn f(x: i32) { let _ = x; }\n"
        )
        .len(),
        1
    );
    assert_eq!(
        find(
            Rust,
            "fn f(x: $T) { $$$B }",
            "fn f(x: bool) { let _ = x; }\n"
        )
        .len(),
        1
    );
}

#[test]
fn go_declarations_methods_and_expressions() {
    // FIX-1: Go declarations parse at the top level, expressions still use the body context.
    let src = "\
package p
func f(a int) { _ = a }
func g() {}
func (r T) m() {}
func h() { fmt.Println(1) }
";
    assert_eq!(
        find(Go, "func $F($$$A) { $$$B }", src).len(),
        3,
        "f, g and h: $$$A absorbs zero or more, so a parameterless function matches"
    );
    assert_eq!(find(Go, "func $F() { $$$B }", src).len(), 2, "g and h");
    assert_eq!(
        find(Go, "func ($R $T) $M() { $$$B }", src).len(),
        1,
        "a method declaration with a receiver"
    );
    assert_eq!(
        find(Go, "func $F(a int) { $$$B }", src).len(),
        1,
        "an exact parameter"
    );
    assert_eq!(
        find(Go, "fmt.Println($$$A)", src).len(),
        1,
        "an expression still matches as a call, not a type conversion"
    );
    assert_eq!(
        find(
            Go,
            "type $N struct { $$$F }",
            "package p\ntype S struct { a int }\n"
        )
        .len(),
        1
    );
}

#[test]
fn typescript_generic_functions_are_token_equal() {
    // The divergence this ticket closed: `$T` appears as a type parameter and as an annotation.
    assert_eq!(
        find(
            TypeScript,
            "function $F<$T>($A: $T) { $$$B }",
            "function f<T>(a: T) { return a; }\n"
        )
        .len(),
        1,
        "the two `T`s have the same leaf text even though their node kinds differ"
    );
    assert_eq!(
        find(
            TypeScript,
            "function $F<$T>(a: $X) { $$$B }",
            "function f<T>(a: T) { return a; }\n"
        )
        .len(),
        1,
        "the type parameter binds, the annotation is a free variable"
    );
    // Different texts are still different.
    assert_eq!(
        find(
            TypeScript,
            "function $F<$T>($A: $T) { $$$B }",
            "function f<T>(a: U) { return a; }\n"
        )
        .len(),
        0
    );
}
