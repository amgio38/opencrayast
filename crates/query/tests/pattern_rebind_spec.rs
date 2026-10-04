//! CR probes for repeated-metavariable equality: it is syntax-tree equality (kinds, children,
//! leaf text), not text equality. Whitespace and comments do not matter; literals do.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use opencrayast_lang::{Language, ParseBudget, parse};
use opencrayast_query::pattern::{Pattern, SearchBudget, search};
use std::time::Duration;

fn count(lang: Language, pat: &str, src: &str) -> usize {
    let p = Pattern::compile(lang, pat).unwrap();
    let b = ParseBudget {
        max_bytes: 1 << 20,
        timeout: Duration::from_secs(10),
        max_depth: 1024,
        max_nodes: 1_000_000,
    };
    let parsed = parse(lang, src, &b).unwrap();
    search(&parsed, src, &p, None, &SearchBudget::default())
        .unwrap()
        .matches
        .len()
}

#[test]
fn whitespace_between_tokens_does_not_matter_in_js() {
    assert_eq!(
        count(Language::JavaScript, "$A == $A", "(a + b) == (a+b);\n"),
        1
    );
    assert_eq!(
        count(Language::JavaScript, "$A == $A", "f(1,2) == f( 1 , 2 );\n"),
        1
    );
    assert_eq!(
        count(Language::JavaScript, "$A == $A", "x.y == x . y;\n"),
        1
    );
}

#[test]
fn comments_do_not_matter() {
    assert_eq!(
        count(
            Language::JavaScript,
            "$A == $A",
            "f(1, /* c */ 2) == f(1, 2);\n"
        ),
        1
    );
    assert_eq!(
        count(
            Language::JavaScript,
            "$A == $A",
            "f(1, // c\n 2) == f(1, 2);\n"
        ),
        1
    );
}

#[test]
fn literals_are_compared_exactly() {
    assert_eq!(
        count(Language::JavaScript, "$A == $A", "/a b/ == /a  b/;\n"),
        0
    );
    assert_eq!(
        count(Language::JavaScript, "$A == $A", "'a b' == 'a  b';\n"),
        0
    );
    assert_eq!(
        count(Language::JavaScript, "$A == $A", "`a ${x}` == `a ${y}`;\n"),
        0
    );
    assert_eq!(
        count(Language::JavaScript, "$A == $A", "/a b/ == /a b/;\n"),
        1
    );
}

#[test]
fn different_structure_with_the_same_text_is_not_equal() {
    // same characters once whitespace is dropped, different trees
    assert_eq!(
        count(Language::JavaScript, "$A == $A", "(a - -b) == (a - - b);\n"),
        1
    );
    assert_eq!(
        count(Language::JavaScript, "$A == $A", "(a - -b) == (a--b);\n"),
        0
    );
    assert_eq!(
        count(
            Language::JavaScript,
            "$A == $A",
            "a + b * c == (a + b) * c;\n"
        ),
        0
    );
}

#[test]
fn python_and_go_equality() {
    assert_eq!(
        count(Language::Python, "$A == $A", "f(1,2) == f( 1 , 2 )  # c\n"),
        1
    );
    assert_eq!(
        count(Language::Python, "$A == $A", "f('a b') == f('a  b')\n"),
        0
    );
    assert_eq!(
        count(
            Language::Go,
            "func f() { _ = $A == $A }",
            "package p\nfunc f() { _ = g(1,2) == g( 1, 2 ) }\n"
        ),
        1
    );
    assert_eq!(
        count(
            Language::Go,
            "func f() { _ = $A == $A }",
            "package p\nfunc f() { _ = \"a b\" == \"a  b\" }\n"
        ),
        0
    );
}

#[test]
fn deep_rebound_nodes_neither_overflow_the_stack_nor_escape_the_step_budget() {
    let n = 3000;
    let side = format!("{}1{}", "(".repeat(n), ")".repeat(n));
    let src = format!("{side} == {side};\n");
    let p = Pattern::compile(Language::JavaScript, "$A == $A").unwrap();
    let b = ParseBudget {
        max_bytes: 1 << 24,
        timeout: Duration::from_secs(20),
        max_depth: 8192,
        max_nodes: 10_000_000,
    };
    let parsed = parse(Language::JavaScript, &src, &b).unwrap();
    let ok = search(&parsed, &src, &p, None, &SearchBudget::default()).unwrap();
    assert_eq!(ok.matches.len(), 1);
    let tiny = SearchBudget {
        max_steps: 100,
        ..Default::default()
    };
    let e = search(&parsed, &src, &p, None, &tiny).unwrap_err();
    assert_eq!(e.code, opencrayast_core::ErrorCode::BudgetExceeded);
}

#[test]
fn a_repeated_metavariable_is_token_equal_across_grammar_roles() {
    let ts = Language::TypeScript;
    // `T` is a type_parameter in `<T>` and a type_identifier in `: T`
    assert_eq!(
        count(
            ts,
            "function $F<$T>($A: $T) { $$$B }",
            "function f<T>(a: T) { return a; }\n"
        ),
        1
    );
    assert_eq!(
        count(
            ts,
            "function $F<$T>($A: $T) { $$$B }",
            "function f<T>(a: U) { return a; }\n"
        ),
        0
    );
    assert_eq!(
        count(
            ts,
            "function $F<$T>($A: $T) { $$$B }",
            "function f<T>(a: T[]) { return a; }\n"
        ),
        0
    );
}

#[test]
fn go_declarations_compile_at_the_top_level_and_statements_still_work() {
    let go = Language::Go;
    let src = "package p\nfunc f(a int) { _ = a }\nfunc g() {}\nfunc (r T) m() {}\nfunc h() { fmt.Println(1) }\n";
    assert_eq!(
        count(go, "func $F($$$A) { $$$B }", src),
        3,
        "f, g and h ($$$A may absorb zero parameters); the method is a method_declaration"
    );
    assert_eq!(count(go, "func $F() { $$$B }", src), 2, "g and h");
    assert_eq!(count(go, "func ($R $T) $M() { $$$B }", src), 1);
    assert_eq!(
        count(go, "fmt.Println($$$A)", src),
        1,
        "expression patterns fall through to the body context"
    );
    assert_eq!(count(go, "_ = $X", src), 1, "statement patterns too");
    assert_eq!(
        count(
            go,
            "type $N struct { $$$F }",
            "package p\ntype S struct { a int }\n"
        ),
        1
    );
}
