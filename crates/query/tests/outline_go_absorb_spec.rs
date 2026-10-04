//! Regression spec for ISSUE-PRS-09-GO: a broken Go declaration must not silently swallow
//! its later siblings from the outline.
//!
//! `docs/LANGUAGES.md` warned that a broken `function_declaration` absorbs later sibling
//! symbols. It does, and it was real: before this fix the collector only walked
//! `source_file`'s direct children, so everything tree-sitter-go folded into the broken
//! declaration simply vanished from the outline — a symbol that is plainly in the source
//! text, with nothing in the response to say it went missing.
//!
//! These tests pin the *recovery*, and equally the two things recovery must not do: invent
//! top-level symbols for genuine function locals, or disturb a clean file.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic, missing_docs)]
use opencrayast_lang::{Language, ParseBudget, parse};
use opencrayast_query::{OutlineOptions, Symbol, outline};
use std::time::Duration;

fn budget() -> ParseBudget {
    ParseBudget {
        max_bytes: 4 << 20,
        timeout: Duration::from_secs(5),
        max_depth: 512,
        max_nodes: 2_000_000,
    }
}

fn go(src: &str) -> Vec<Symbol> {
    let parsed = parse(Language::Go, src, &budget()).unwrap();
    outline(&parsed, src, &OutlineOptions::default())
}

fn names(src: &str) -> Vec<String> {
    go(src).into_iter().map(|s| s.name).collect()
}

/// Every declaration after the break, of each kind the docs call out, survives.
///
/// The concrete symbols are the ones `docs/LANGUAGES.md` warns about: `const`, `var`,
/// `type` and a further `func`. Recovery nests them at two different depths — the
/// `var`/`const`/`type` land as statements of the broken function's body, the `func` is
/// demoted to an expression statement — so both paths are covered by one source.
#[test]
fn broken_function_does_not_swallow_later_siblings() {
    let src = r#"package main

func Before() {}

func Broken( {

const AfterConst = 1

var AfterVar = 2

type AfterType struct{}

func AfterFunc() {}
"#;
    let got = go(src);
    let got_names: Vec<&str> = got.iter().map(|s| s.name.as_str()).collect();

    for expected in [
        "Before",
        "Broken",
        "AfterConst",
        "AfterVar",
        "AfterType",
        "AfterFunc",
    ] {
        assert!(
            got_names.contains(&expected),
            "{expected} vanished from the outline of a damaged Go file: {got_names:?}"
        );
    }

    // Recovered in source order, and each with the kind its text declares.
    let qualified: Vec<&str> = got.iter().map(|s| s.qualified.as_str()).collect();
    assert_eq!(
        qualified,
        [
            "Before",
            "Broken",
            "AfterConst",
            "AfterVar",
            "AfterType",
            "AfterFunc"
        ],
        "recovered symbols must keep source order"
    );

    let by_name = |n: &str| got.iter().find(|s| s.name == n).unwrap();
    assert_eq!(
        by_name("AfterConst").kind,
        opencrayast_query::SymbolKind::Const
    );
    assert_eq!(
        by_name("AfterVar").kind,
        opencrayast_query::SymbolKind::Variable
    );
    assert_eq!(
        by_name("AfterType").kind,
        opencrayast_query::SymbolKind::Struct
    );
    assert_eq!(by_name("AfterFunc").kind, opencrayast_query::SymbolKind::Fn);

    // Each recovered symbol points at the text it came from, not at the broken declaration.
    assert_eq!(
        &src[by_name("AfterConst").start_byte..by_name("AfterConst").end_byte],
        "AfterConst = 1"
    );
    assert_eq!(by_name("AfterConst").signature, "const AfterConst = 1");
    assert_eq!(by_name("AfterVar").signature, "var AfterVar = 2");
    assert_eq!(by_name("AfterFunc").signature, "func AfterFunc()");
}

/// The parameter-list shape: here recovery swallows the next `func` as a *parameter*, so
/// the name survives but the receiver/body do not and the signature is marked `?`.
#[test]
fn broken_function_does_not_swallow_a_later_func_absorbed_as_a_parameter() {
    let src = r#"
package main

func good() {}

func broken( {

func also_good() {}
"#;
    let got = go(src);
    let got_names: Vec<&str> = got.iter().map(|s| s.name.as_str()).collect();
    assert_eq!(got_names, ["good", "broken", "also_good"]);
    let also_good = &got[2];
    assert_eq!(also_good.kind, opencrayast_query::SymbolKind::Fn);
    // The parameter-list shape loses the trailing `()` to an unnamed position, so the
    // recovered signature is explicitly marked as uncertain rather than presented as clean.
    assert!(
        also_good.signature.contains("/*?*/"),
        "absorbed func must be marked as recovered: {:?}",
        also_good.signature
    );
}

/// Recovery must not invent top-level symbols out of a broken function's real locals.
///
/// `:=` and the typed declaration forms are legal statements in any function body, so a
/// bare `var x = …` is the distinguishing signal; this pins that the guard is a MISSING
/// token, not merely "the declaration has a statement_list".
#[test]
fn a_healthy_function_with_locals_outlines_only_itself() {
    let src = r#"package main

func healthy() {
	var localVar = 1
	const localConst = 2
	type localType struct{}
	var localTyped int = 3
	localShort := 4
}
"#;
    let parsed = parse(Language::Go, src, &budget()).unwrap();
    assert_eq!(parsed.error_count, 0, "fixture must be valid Go");
    assert_eq!(names(src), ["healthy"]);
}

/// A damaged function that also has genuine locals recovers only the sibling.
#[test]
fn recovery_keeps_genuine_locals_out_of_the_outline() {
    let src = r#"package main

func Broken( {

	genuineLocal := 5
	var typedLocal int = 6

var AbsorbedVar = 2
"#;
    assert_eq!(names(src), ["Broken", "AbsorbedVar"]);
}

/// A file that parses cleanly must be outlined exactly as before — recovery only engages
/// when the declaration is actually unterminated.
#[test]
fn a_clean_file_is_unaffected() {
    let src = include_str!("../../lang/tests/fixtures/skeleton.go");
    let parsed = parse(Language::Go, src, &budget()).unwrap();
    assert_eq!(parsed.error_count, 0, "skeleton.go must be valid Go");
    assert_eq!(
        names(src),
        ["Config", "Load", "Max", "add", "main"],
        "a clean Go file must outline exactly as before"
    );
    // A method is qualified by its receiver; only recovered functions carry a bare name,
    // because ERROR recovery leaves no receiver behind to read.
    assert_eq!(go(src)[1].qualified, "Config.Load");
}

/// Every recovered symbol must still satisfy the invariants the outline promises, in the
/// pathological case: real name, in-range byte extent on a character boundary, sane lines.
#[test]
fn recovered_symbols_are_well_formed() {
    let src = r#"package main

func Broken( {

const AfterConst = 1

var AfterVar = 2

type AfterType struct{}

func AfterFunc() {}
"#;
    for symbol in go(src) {
        assert!(!symbol.name.trim().is_empty(), "empty name: {symbol:?}");
        assert!(symbol.start_byte <= symbol.end_byte, "{symbol:?}");
        assert!(symbol.end_byte <= src.len(), "{symbol:?}");
        assert!(
            src.is_char_boundary(symbol.start_byte) && src.is_char_boundary(symbol.end_byte),
            "byte range off a character boundary: {}",
            symbol.qualified
        );
        assert!(symbol.start_line >= 1, "{symbol:?}");
        assert!(symbol.end_line >= symbol.start_line, "{symbol:?}");
    }
}
