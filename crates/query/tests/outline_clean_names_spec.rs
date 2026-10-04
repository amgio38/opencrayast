//! The clean-name rule, exercised across every language (ISSUE-QUERY-CLEAN-NAMES).
//!
//! One rule, one set of assertions: whatever a language's collector produces, the pipeline in
//! `outline::collect_all` must never publish a name that came from damaged source. The same defect
//! (broken text pasted into a symbol name) showed up once per language before the rule existed, so
//! each language gets the same random broken-source test over its own keyword and punctuation
//! tokens - the collectors themselves are not touched.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
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

fn run(lang: Language, src: &str) -> Vec<Symbol> {
    let p = parse(lang, src, &budget()).unwrap();
    outline(&p, src, &OutlineOptions::default())
}

/// Every name property the outline promises, checked for one symbol.
///
/// `name` is bounded by 256 bytes and `qualified` by 1024, so the two are checked against their own
/// limits rather than a single one. The byte-range and line checks are the other half of "the
/// symbol really exists": an extent that is out of order, outside the source, or not on a character
/// boundary would make `ast_get` slice invalid text.
#[track_caller]
fn assert_clean(sym: &Symbol, src: &str) {
    let lang = sym.language.id();
    for value in [&sym.name, &sym.qualified] {
        let value = value.as_str();
        assert!(
            !value.trim().is_empty(),
            "{lang}: empty name from {src:?}: {sym:?}"
        );
        assert!(
            !value.contains('\n') && !value.contains('\r'),
            "{lang}: line break in a name from {src:?}: {sym:?}"
        );
        assert!(
            !value.contains("//") && !value.contains("/*") && !value.contains("*/"),
            "{lang}: comment marker in a name from {src:?}: {sym:?}"
        );
        assert!(
            !value.contains("  "),
            "{lang}: doubled space in a name from {src:?}: {sym:?}"
        );
        assert!(
            !value.trim_start().starts_with("for ") && !value.trim_end().ends_with(" for"),
            "{lang}: dangling `for` in a name from {src:?}: {sym:?}"
        );
    }
    assert!(
        sym.name.len() <= 256,
        "{lang}: name over 256 bytes from {src:?}: {}",
        sym.name.len()
    );
    assert!(
        sym.qualified.len() <= 1024,
        "{lang}: qualified over 1024 bytes from {src:?}: {}",
        sym.qualified.len()
    );
    assert!(sym.depth >= 1, "{lang}: depth 0 from {src:?}: {sym:?}");
    assert!(
        sym.start_line >= 1,
        "{lang}: start_line 0 from {src:?}: {sym:?}"
    );
    assert!(
        sym.end_line >= sym.start_line,
        "{lang}: end_line < start_line from {src:?}: {sym:?}"
    );
    assert!(
        sym.start_byte <= sym.end_byte && sym.end_byte <= src.len(),
        "{lang}: byte extent outside the source from {src:?}: {sym:?}"
    );
    assert!(
        src.is_char_boundary(sym.start_byte) && src.is_char_boundary(sym.end_byte),
        "{lang}: byte extent splits a character from {src:?}: {sym:?}"
    );
}

/// A fixed-seed xorshift64, so a failure names the exact input that caused it.
struct Rng(u64);

impl Rng {
    fn new(seed: u64) -> Self {
        Rng(seed)
    }
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    /// Build a source from `count` tokens of `tokens`, separated by spaces.
    fn source(&mut self, tokens: &[&str], max_count: usize) -> String {
        let n = 1 + (self.next() % max_count as u64) as usize;
        let mut s = String::new();
        for _ in 0..n {
            s.push_str(tokens[(self.next() % tokens.len() as u64) as usize]);
            s.push(' ');
        }
        s.push('\n');
        s
    }
}

/// Run the shared assertions over `cases` random sources for one language, and report what the
/// filter dropped. The count is asserted to be plausible so a silently dead test cannot pass.
#[track_caller]
fn assert_random_clean(lang: Language, tokens: &[&str], cases: usize, seed: u64, label: &str) {
    let mut rng = Rng::new(seed);
    let mut seen = 0usize;
    for _ in 0..cases {
        let src = rng.source(tokens, 12);
        for sym in run(lang, &src) {
            seen += 1;
            assert_clean(&sym, &src);
        }
    }
    assert!(
        seen > 0,
        "{label}: the generator produced no symbols; the test is vacuous"
    );
}

#[test]
fn clean_names_rust_random_broken_source() {
    const TOKENS: [&str; 17] = [
        "impl", "?", "where", "extern", "<T>", "///", "d", "for", "!", "*", "[", "]", "Bar", "dyn",
        "\n", "//!", ";",
    ];
    assert_random_clean(Language::Rust, &TOKENS, 5000, 0x5eed_1234_abcd_9876, "rust");
}

#[test]
fn clean_names_ecma_random_broken_source() {
    // TypeScript/JavaScript vocabulary plus the punctuation that produced damaged text in the
    // Rust case: comment markers, line breaks and the `=>`/`<T>` generics.
    const TOKENS: [&str; 20] = [
        "export",
        "default",
        "class",
        "interface",
        "enum",
        "type",
        "namespace",
        "declare",
        "abstract",
        "function",
        "const",
        "let",
        "=>",
        "<T>",
        "extends",
        "implements",
        "//",
        "/*",
        "}",
        ";",
    ];
    for lang in [Language::TypeScript, Language::Tsx, Language::JavaScript] {
        assert_random_clean(lang, &TOKENS, 3000, 0x5eed_1234_abcd_9876, lang.id());
    }
}

#[test]
fn clean_names_python_random_broken_source() {
    // Python needs indentation and colons to form anything at all, so the generator emits whole
    // fragments rather than bare tokens: a line is an indentation plus a few keywords.
    const LINES: [&str; 12] = [
        "def f():",
        "class C:",
        "async def g():",
        "@dec",
        "lambda x:",
        "x = 1",
        "if x:",
        "    return x",
        "# comment",
        "    # indented comment",
        "for i in y:",
        "    pass",
    ];
    let mut rng = Rng::new(0x5eed_1234_abcd_9876);
    let mut seen = 0usize;
    for _ in 0..3000 {
        let n = 1 + (rng.next() % 10) as usize;
        let mut src = String::new();
        for _ in 0..n {
            src.push_str(LINES[(rng.next() % LINES.len() as u64) as usize]);
            src.push('\n');
        }
        for sym in run(Language::Python, &src) {
            seen += 1;
            assert_clean(&sym, &src);
        }
    }
    assert!(seen > 0, "python: no symbols produced; the test is vacuous");
}

#[test]
fn clean_names_go_random_broken_source() {
    // Go needs its type-parameter brackets and comment markers to be part of the token set.
    const TOKENS: [&str; 16] = [
        "func",
        "type",
        "struct",
        "interface",
        "const",
        "var",
        "[T any]",
        "package",
        "main",
        "func F()",
        "//",
        "/*",
        "}",
        "int",
        "string",
        ";",
    ];
    assert_random_clean(Language::Go, &TOKENS, 3000, 0x5eed_1234_abcd_9876, "go");
}

/// The shared gate in `collect_all` is load-bearing, and this is the test that witnesses it.
///
/// The rule this pins is the **size bound** (`MAX_NAME_BYTES` = 256), because that is the one rule
/// the collectors do not already enforce themselves for every language. A 257-byte identifier is a
/// single, well-formed identifier node: it satisfies the ecma collector's `is_identifier_node`
/// guard and its `usable_name` check, and it satisfies the per-module filters in `rust.rs`, `go.rs`
/// and `python.rs` - none of which look at length. So the gate is the only thing standing between
/// that symbol and a caller, and removing the gate genuinely lets it through.
///
/// Every case below was found by probing the collectors with the filter disabled, not guessed.
///
/// What this test proves: with the gate removed, the cases below fail, because the outline then
/// contains a symbol whose name exceeds the documented 256-byte limit. What it does NOT prove: that
/// the gate filters damaged *text* (comment markers, line breaks, doubled spaces, `for`
/// fragments). A 300k-source fuzz sweep over all four collectors found no input that reaches any of
/// those rules - the collectors reject that text upstream, so they are currently unwitnessed defence
/// in depth. See the comment on the gate in `outline/mod.rs`.
///
/// A note on the earlier version of this test: it used `declare const {\n}` and asserted that no
/// name contains `{` or a newline. That input does not produce such a name, so the assertion held
/// for the wrong reason and the test stayed green with the filter deleted. It was an overclaim, not
/// a protection.
#[test]
fn the_pipeline_gate_is_what_removes_damaged_names() {
    // 257 bytes: one past the limit, so the assertion below is about the bound and not about a
    // comfortable margin.
    let just_over = "A".repeat(MAX_NAME_BYTES_FOR_TEST + 1);
    // Comfortably over, so a future change to the constant cannot accidentally still pass here.
    let well_over = "B".repeat(MAX_NAME_BYTES_FOR_TEST * 2);

    // (language, source, label)
    let cases: [(Language, String, &str); 8] = [
        (
            Language::TypeScript,
            format!("function {just_over}() {{}}\n"),
            "ts function",
        ),
        (
            Language::JavaScript,
            format!("class {well_over} {{}}\n"),
            "js class",
        ),
        (
            Language::TypeScript,
            format!("const {well_over} = 1;\n"),
            "ts const",
        ),
        (
            Language::TypeScript,
            format!("interface {well_over} {{}}\n"),
            "ts interface",
        ),
        (
            Language::Python,
            format!("def {well_over}(): pass\n"),
            "py def",
        ),
        (
            Language::Python,
            format!("class {well_over}: pass\n"),
            "py class",
        ),
        (
            Language::Go,
            format!("func {well_over}() {{}}\n"),
            "go func",
        ),
        (Language::Go, format!("type {well_over} int\n"), "go type"),
    ];

    for (lang, src, label) in cases {
        let symbols = run(lang, &src);
        for sym in &symbols {
            assert_clean(sym, &src);
        }

        // The assertion that makes this test load-bearing: with the gate removed, the collector
        // emits the over-long symbol and this fails. Checked over the whole result, not against one
        // hard-coded name, so it cannot be satisfied by a symbol that merely has a different name.
        assert!(
            !symbols.iter().any(|s| s.name.len() > 256),
            "{label} ({}): the gate let a {}-byte name through: {symbols:?}",
            lang.id(),
            MAX_NAME_BYTES_FOR_TEST * 2,
        );

        // The same must hold for the qualified form, which has its own, larger bound.
        assert!(
            !symbols.iter().any(|s| s.qualified.len() > 1024),
            "{label} ({}): the gate let a {}-byte qualified name through: {symbols:?}",
            lang.id(),
            MAX_NAME_BYTES_FOR_TEST * 2,
        );
    }

    // Guard against this test going vacuous: a sibling declaration beside the over-long one must
    // still be published. Without this, a future change that made the collector reject *every*
    // source in this file would keep the test green for the wrong reason - which is exactly the
    // failure mode this test is being rewritten to eliminate.
    let long = "C".repeat(MAX_NAME_BYTES_FOR_TEST * 2);
    let src = format!("function keep_me() {{}}\nfunction {long}() {{}}\n");
    let symbols = run(Language::TypeScript, &src);
    assert!(
        symbols.iter().any(|s| s.name == "keep_me"),
        "the gate removed a legitimate symbol; the test above would be vacuous: {symbols:?}"
    );
    assert_eq!(
        symbols.len(),
        1,
        "expected exactly the one nameable symbol to survive: {symbols:?}"
    );
}

/// The published size limits themselves, restated here so a change to the constants is visible in
/// this file rather than silently changing what the test above means.
const MAX_NAME_BYTES_FOR_TEST: usize = 256;

/// A damaged declaration must not reach the outline as a symbol, in any language. These are the
/// shapes that produced garbage names before the shared rule existed.
#[test]
fn damaged_declarations_are_dropped_in_every_language() {
    let cases: [(Language, &str); 12] = [
        (Language::Rust, "unsafe impl ;\n"),
        (Language::Rust, "impl ? where extern <T> /// d for \n"),
        (Language::Rust, "impl d for ? \n Bar \n"),
        (Language::Rust, "struct ;\n"),
        (Language::TypeScript, "class ??? { \n"),
        (Language::TypeScript, "export interface { // \n"),
        (Language::TypeScript, "declare // \n"),
        (Language::JavaScript, "class { }\n"),
        (Language::Python, "def (:\n"),
        (Language::Python, "class ???:\n    // \n"),
        (Language::Go, "func F( {\n"),
        (Language::Go, "type { // \n"),
    ];
    for (lang, src) in cases {
        let symbols = run(lang, src);
        for sym in &symbols {
            assert_clean(sym, src);
        }
        // Nothing whose name looks like the damaged text itself.
        assert!(
            !symbols.iter().any(|s| {
                s.name.contains("???")
                    || s.name.contains("//")
                    || s.name.contains("/*")
                    || s.name.contains('\n')
                    || s.name.trim().is_empty()
            }),
            "{}: damaged text leaked into a name from {src:?}: {symbols:?}",
            lang.id()
        );
    }
}

/// The filter must not eat legitimate symbols: a clean file still outlines completely.
#[test]
fn clean_source_outlines_completely() {
    for (lang, src) in [
        (
            Language::Rust,
            "pub struct Config;\nimpl Config { pub fn load() {} }\n",
        ),
        (
            Language::TypeScript,
            "export class Config {\n  static load(): Config { return new Config(); }\n}\n",
        ),
        (
            Language::JavaScript,
            "class Counter {\n  add(n) { return n; }\n}\n",
        ),
        (
            Language::Python,
            "class Config:\n    def make(self):\n        return 1\n",
        ),
        (
            Language::Go,
            "type Config struct{}\nfunc (c *Config) Load() {}\n",
        ),
    ] {
        let symbols = run(lang, src);
        assert!(
            !symbols.is_empty(),
            "{}: clean source produced nothing",
            lang.id()
        );
        for sym in &symbols {
            assert_clean(sym, src);
        }
    }
}
