//! Rust outline suite (ISSUE-QUERY-OUTLINE-RUST), split from outline_spec.rs. Add cases; never weaken these.
//! Line numbers refer to the files under tests/fixtures/.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
use opencrayast_lang::{Language, ParseBudget, parse};
use opencrayast_query::SymbolKind::*;
use opencrayast_query::{OutlineOptions, Symbol, SymbolKind, find_symbols, outline, symbol_text};
use std::time::Duration;

fn budget() -> ParseBudget {
    ParseBudget {
        max_bytes: 4 << 20,
        timeout: Duration::from_secs(5),
        max_depth: 512,
        max_nodes: 2_000_000,
    }
}

fn run(lang: Language, src: &str, opts: &OutlineOptions) -> Vec<Symbol> {
    let p = parse(lang, src, &budget()).unwrap();
    outline(&p, src, opts)
}

type Row = (
    usize,
    SymbolKind,
    &'static str,
    &'static str,
    usize,
    usize,
    Option<&'static str>,
);

fn check(lang: Language, src: &str, want: &[Row]) {
    let got = run(lang, src, &OutlineOptions::default());
    let g: Vec<(usize, SymbolKind, &str, &str, usize, usize)> = got
        .iter()
        .map(|s| {
            (
                s.depth,
                s.kind,
                s.name.as_str(),
                s.qualified.as_str(),
                s.start_line,
                s.end_line,
            )
        })
        .collect();
    let w: Vec<(usize, SymbolKind, &str, &str, usize, usize)> = want
        .iter()
        .map(|r| (r.0, r.1, r.2, r.3, r.4, r.5))
        .collect();
    assert_eq!(g, w, "{lang:?} symbols");
    for (s, r) in got.iter().zip(want) {
        if let Some(sig) = r.6 {
            assert_eq!(s.signature, sig, "{lang:?} signature of {}", s.qualified);
        }
        assert_eq!(
            &src[s.start_byte..s.end_byte].lines().count(),
            &(s.end_line - s.start_line + 1),
            "{lang:?} byte/line extent of {}",
            s.qualified
        );
    }
}

#[test]
fn rust_trait_impl_and_inherent_impl_names() {
    let src = "\
struct Config;
trait Display {}
impl Config {
    pub fn new() -> Self {}
}
impl Display for Config {
    fn fmt(&self) {}
}
";
    check(
        Language::Rust,
        src,
        &[
            (1, Struct, "Config", "Config", 1, 1, Some("struct Config")),
            (1, Trait, "Display", "Display", 2, 2, Some("trait Display")),
            (1, Impl, "Config", "Config", 3, 5, Some("impl Config")),
            (
                2,
                Method,
                "new",
                "Config::new",
                4,
                4,
                Some("pub fn new() -> Self"),
            ),
            (
                1,
                Impl,
                "Display for Config",
                "Display for Config",
                6,
                8,
                Some("impl Display for Config"),
            ),
            (
                2,
                Method,
                "fmt",
                "Display for Config::fmt",
                7,
                7,
                Some("fn fmt(&self)"),
            ),
        ],
    );
}

#[test]
fn rust_nested_modules_qualify_through_every_level() {
    let src = "\
mod a {
    pub mod b {
        pub fn deep() {}
    }
    pub fn shallow() {}
}
";
    check(
        Language::Rust,
        src,
        &[
            (1, Module, "a", "a", 1, 6, Some("mod a")),
            (2, Module, "b", "a::b", 2, 4, Some("pub mod b")),
            (3, Fn, "deep", "a::b::deep", 3, 3, Some("pub fn deep()")),
            (
                2,
                Fn,
                "shallow",
                "a::shallow",
                5,
                5,
                Some("pub fn shallow()"),
            ),
        ],
    );
}

#[test]
fn rust_attributes_belong_to_the_symbol_but_docs_do_not() {
    let src = "\
/// A config.
#[derive(Debug, Clone)]
pub struct Config {
    pub name: String,
}

#[test]
fn checks() {
    true
}
";
    let s = run(Language::Rust, src, &OutlineOptions::default());
    let config = s.iter().find(|x| x.name == "Config").unwrap();
    // The doc comment is not part of the extent; the attribute is.
    assert_eq!((config.start_line, config.end_line), (2, 5));
    assert_eq!(
        src.lines().nth(1).unwrap().trim(),
        "#[derive(Debug, Clone)]"
    );
    let checks = s.iter().find(|x| x.name == "checks").unwrap();
    assert_eq!(checks.start_line, 7, "the attribute line is included");
    assert_eq!(checks.end_line, 10);
    // The signature never carries the attributes.
    assert_eq!(config.signature, "pub struct Config");
    assert_eq!(checks.signature, "fn checks()");
    // With docs on, the doc line is reported but still not inside the extent.
    let o = OutlineOptions {
        include_docs: true,
        ..Default::default()
    };
    let s2 = run(Language::Rust, src, &o);
    let config2 = s2.iter().find(|x| x.name == "Config").unwrap();
    assert_eq!(config2.doc_first_line.as_deref(), Some("A config."));
    assert_eq!(config2.start_line, 2, "doc line is not part of start_line");
}

#[test]
fn rust_visibility_modifiers_and_async_unsafe_stay_in_the_signature() {
    let src = "\
pub(crate) fn restricted() {}
pub async fn slow() {}
pub unsafe fn danger() {}
pub(super) fn to_parent() {}
pub(in crate::a) fn to_path() {}
";
    check(
        Language::Rust,
        src,
        &[
            (
                1,
                Fn,
                "restricted",
                "restricted",
                1,
                1,
                Some("pub(crate) fn restricted()"),
            ),
            (1, Fn, "slow", "slow", 2, 2, Some("pub async fn slow()")),
            (
                1,
                Fn,
                "danger",
                "danger",
                3,
                3,
                Some("pub unsafe fn danger()"),
            ),
            (
                1,
                Fn,
                "to_parent",
                "to_parent",
                4,
                4,
                Some("pub(super) fn to_parent()"),
            ),
            (
                1,
                Fn,
                "to_path",
                "to_path",
                5,
                5,
                Some("pub(in crate::a) fn to_path()"),
            ),
        ],
    );
}

#[test]
fn rust_generic_impl_name_drops_parameters_and_signature_keeps_them() {
    let src = "\
pub struct Foo<T>(pub T);

impl<T: Clone> Foo<T> {
    pub fn get<U>(&self, u: U) -> &T {}
}

impl<T> Default for Foo<T> {
    fn default() -> Self {}
}
";
    let s = run(Language::Rust, src, &OutlineOptions::default());
    let rows: Vec<(String, String, String)> = s
        .iter()
        .map(|x| {
            (
                x.kind.as_str().to_string(),
                x.name.clone(),
                x.signature.clone(),
            )
        })
        .collect();
    assert_eq!(
        rows,
        [
            ("struct".into(), "Foo".into(), "pub struct Foo<T>".into()),
            // name drops the generics...
            ("impl".into(), "Foo".into(), "impl<T: Clone> Foo<T>".into()),
            (
                "method".into(),
                "get".into(),
                "pub fn get<U>(&self, u: U) -> &T".into()
            ),
            (
                "impl".into(),
                "Default for Foo".into(),
                "impl<T> Default for Foo<T>".into()
            ),
            (
                "method".into(),
                "default".into(),
                "fn default() -> Self".into()
            ),
        ]
    );
    assert_eq!(s[2].qualified, "Foo::get");
    assert_eq!(s[4].qualified, "Default for Foo::default");
}

#[test]
fn rust_unsafe_impl_is_an_impl_block() {
    // `unsafe impl Send for X {}` is the legal form and must still be an Impl symbol.
    let s = run(
        Language::Rust,
        "unsafe impl Send for X {}\n",
        &OutlineOptions::default(),
    );
    assert_eq!(s.len(), 1, "{s:?}");
    assert_eq!(s[0].kind, Impl);
    assert_eq!(s[0].name, "Send for X");
    assert_eq!(s[0].qualified, "Send for X");
    assert_eq!(s[0].signature, "unsafe impl Send for X");
    assert_eq!(s[0].depth, 1);
    // The grammar does not accept `pub unsafe impl`: the `pub` and `unsafe` are split off as an
    // ERROR node and the impl itself parses separately. The outline then reports the impl (the
    // real symbol) and skips the damaged `pub unsafe` prefix, rather than losing the whole block.
    let src = "\
pub struct X;

pub unsafe impl Send for X {}
";
    let s = run(Language::Rust, src, &OutlineOptions::default());
    let names: Vec<&str> = s.iter().map(|x| x.name.as_str()).collect();
    assert_eq!(names, ["X", "Send for X"], "{names:?}");
    let imp = s.iter().find(|x| x.kind == Impl).unwrap();
    assert_eq!(imp.qualified, "Send for X");
    assert_eq!(imp.depth, 1);
}

#[test]
fn rust_extern_blocks_are_skipped_entirely() {
    // Milestone choice: an `extern "C" { .. }` block is not a symbol, and the functions it
    // declares are not emitted either - they have no body, live in a foreign ABI, and are
    // declarations of another language's symbols. They are deliberately absent, not a gap.
    let src = "\
extern \"C\" {
    pub fn c_fn(x: i32) -> i32;
    static C_VAR: i32;
}
pub fn real() {}
";
    let s = run(Language::Rust, src, &OutlineOptions::default());
    let names: Vec<&str> = s.iter().map(|x| x.name.as_str()).collect();
    assert_eq!(names, ["real"], "{names:?}");
}

#[test]
fn rust_traits_const_static_type_and_macro_kinds() {
    let src = "\
pub trait Shape {
    fn area(&self) -> f64;
    const SIDES: u32;
    type Output;
}
const MAX: usize = 10;
static NAME: &str = \"x\";
pub type Id = u64;
macro_rules! shout {
    () => {};
}
";
    let s = run(Language::Rust, src, &OutlineOptions::default());
    let rows: Vec<(&str, &str, &str, usize)> = s
        .iter()
        .map(|x| {
            (
                x.kind.as_str(),
                x.name.as_str(),
                x.signature.as_str(),
                x.depth,
            )
        })
        .collect();
    assert_eq!(
        rows,
        [
            ("trait", "Shape", "pub trait Shape", 1),
            // A body-less declaration inside a trait is still a method, and its signature has no
            // trailing `;`.
            ("method", "area", "fn area(&self) -> f64", 2),
            ("const", "SIDES", "const SIDES: u32", 2),
            ("type", "Output", "type Output", 2),
            ("const", "MAX", "const MAX: usize = 10", 1),
            ("static", "NAME", "static NAME: &str = \"x\"", 1),
            ("type", "Id", "pub type Id = u64", 1),
            // A macro definition has no body field, so its signature is the whole definition.
            ("macro", "shout", "macro_rules! shout { () => {}; }", 1,),
        ]
    );
}

#[test]
fn rust_struct_fields_are_not_symbols_in_this_milestone() {
    let src = "\
pub struct Config {
    pub name: String,
    count: usize,
    nested: Inner,
}
struct Inner;
";
    let s = run(Language::Rust, src, &OutlineOptions::default());
    let names: Vec<&str> = s.iter().map(|x| x.name.as_str()).collect();
    assert_eq!(
        names,
        ["Config", "Inner"],
        "fields are not emitted: {names:?}"
    );
}

#[test]
fn rust_a_broken_region_does_not_hide_the_rest_of_the_file() {
    let src = "\
pub fn before() {}

fn broken( {

pub fn after() {}

pub struct AlsoFine {
    a: u8,
}
";
    let p = parse(Language::Rust, src, &budget()).unwrap();
    assert!(p.error_count > 0, "the fixture must really be broken");
    let s = outline(&p, src, &OutlineOptions::default());
    let names: Vec<&str> = s.iter().map(|x| x.name.as_str()).collect();
    assert!(
        names.contains(&"before") && names.contains(&"after") && names.contains(&"AlsoFine"),
        "{names:?}"
    );
}

#[test]
fn rust_doc_blocks_line_and_block_forms() {
    let src = "\
// not a doc comment
/// First line.
/// Second line.
pub fn a() {}

/**
 * A block doc.
 */
pub fn b() {}

/** Single-line block. */
pub fn c() {}

/// Not contiguous: a blank line follows.
pub fn d() {}
";
    let o = OutlineOptions {
        include_docs: true,
        ..Default::default()
    };
    let s = run(Language::Rust, src, &o);
    let doc = |q: &str| {
        s.iter()
            .find(|x| x.qualified == q)
            .unwrap_or_else(|| {
                panic!(
                    "{q} missing: {:?}",
                    s.iter().map(|x| x.qualified.as_str()).collect::<Vec<_>>()
                )
            })
            .doc_first_line
            .clone()
    };
    assert_eq!(doc("a").as_deref(), Some("First line."));
    assert_eq!(doc("b").as_deref(), Some("A block doc."));
    assert_eq!(doc("c").as_deref(), Some("Single-line block."));
    assert_eq!(
        doc("d").as_deref(),
        Some("Not contiguous: a blank line follows.")
    );

    // The plain `//` above `a` is not a doc and does not break the `///` block.
    assert_eq!(doc("a").as_deref(), Some("First line."));
    // `ast_get` uses doc_start_line to pull the whole block in.
    let p = parse(Language::Rust, src, &budget()).unwrap();
    let a = &find_symbols(&p, src, "a")[0];
    let t = symbol_text(src, a, true, 0).unwrap();
    assert!(t.text.contains("/// First line."), "{:?}", t.text);
    assert!(t.text.contains("/// Second line."), "{:?}", t.text);
    let b = &find_symbols(&p, src, "b")[0];
    let tb = symbol_text(src, b, true, 0).unwrap();
    assert!(tb.text.contains("/**"), "{:?}", tb.text);
    assert!(tb.text.contains("A block doc."), "{:?}", tb.text);
}

#[test]
fn rust_inner_doc_comments_do_not_attach_to_the_next_symbol() {
    // `//!` documents the module; it must not become the doc of the struct below it.
    let src = "//! Module doc.\n\npub struct S;\n";
    let o = OutlineOptions {
        include_docs: true,
        ..Default::default()
    };
    let s = run(Language::Rust, src, &o);
    let st = s.iter().find(|x| x.name == "S").unwrap();
    assert_eq!(st.doc_first_line, None);
    assert_eq!(st.start_line, 3, "the struct starts on its own line");
}

#[test]
fn rust_deeply_nested_modules_do_not_overflow_the_stack() {
    // The parse budget caps tree depth first, so the depth budget is raised for this case: the
    // point of the test is that `collect` walks the scopes iteratively. Every nesting level costs
    // about two tree levels, so this stays well inside the hard maximum.
    //
    // 340 is the deepest nesting whose innermost qualified name (`m::` * n + "deep" = 3n + 4 bytes)
    // still fits the 1024-byte qualified cap: 3*340 + 4 = 1024. One more level is 1027 bytes and the
    // symbol would be dropped by the cap, so this is the boundary the test can probe - it tests the
    // iterative walk, not the cap.
    let n = 340;
    let mut src = String::new();
    for _ in 0..n {
        src.push_str("mod m {\n");
    }
    // `deep` goes inside all n modules, so it must come before the closing braces.
    src.push_str("pub fn deep() {}\n");
    for _ in 0..n {
        src.push_str("}\n");
    }
    let p = parse(
        Language::Rust,
        &src,
        &ParseBudget {
            max_bytes: 4 << 20,
            timeout: Duration::from_secs(10),
            max_depth: 4096,
            max_nodes: 2_000_000,
        },
    )
    .unwrap();
    let s = outline(
        &p,
        &src,
        &OutlineOptions {
            // The outline's own depth cap, not the parse budget.
            max_depth: n + 1,
            ..Default::default()
        },
    );
    let deep = s
        .iter()
        .find(|x| x.name == "deep")
        .expect("the innermost fn must be reached at the deepest nesting the name cap allows");
    assert_eq!(deep.depth, n + 1);
    let expected = (0..n).map(|_| "m").collect::<Vec<_>>().join("::");
    assert_eq!(deep.qualified, format!("{expected}::deep"));
    assert_clean(deep, &src);
}

/// A damaged declaration can still produce a node whose name is empty (an `impl_item` whose type
/// is a zero-width `type_identifier`). Such a symbol is unreachable: `find_symbols` can never match
/// an empty name and `ast_get` could never return it, so it must never be emitted.
#[test]
fn rust_no_symbol_is_ever_emitted_with_an_empty_name() {
    for src in [
        // Reported by the reviewer: a bare `unsafe impl ;` left a nameless Impl symbol.
        "unsafe impl ;\n",
        "impl ;\n",
        "impl for {}\n",
        "impl <T> ;\n",
        // `impl Foo for ;` leaves an ERROR node where the `for` was: it must not be mistaken for
        // the implemented type (which produced the nonsense name "Foo for for").
        "impl Foo for ;\n",
        "impl Send for ;\n",
        "unsafe trait ;\n",
        "struct ;\n",
        "fn () {}\n",
        "trait {}\n",
        "pub struct ;\n",
    ] {
        let s = run(Language::Rust, src, &OutlineOptions::default());
        for sym in &s {
            assert!(
                !sym.name.trim().is_empty(),
                "empty name in {src:?}: {sym:?}"
            );
            assert!(
                !sym.qualified.trim().is_empty(),
                "empty qualified name in {src:?}: {sym:?}"
            );
            // An empty name would also make the qualified path degenerate.
            assert!(!sym.qualified.ends_with("::"), "dangling path in {src:?}");
        }
    }
}

/// The reviewer's three inputs, verbatim. Before the fix these produced an `Impl` symbol whose
/// name was the damaged source itself - `"? where extern <T> for /// d for"`, and in two cases a
/// name containing a newline and `//!`.
#[test]
fn rust_damaged_impl_text_never_becomes_a_name() {
    let cases = [
        "; impl ? where extern <T> /// d for ",
        // The same shape with a line break and a module doc comment after it.
        "; impl ? where extern <T> /// d for \n//! x\n",
        // No leading `;`: the damaged impl on its own.
        "impl ? where extern <T> /// d for ",
        // The two other shapes the 20000-case probe surfaced.
        "impl d for ? \n Bar ",
        "impl ] [ extern \n ? d ] ",
        "; impl Bar /// //! [ ",
    ];
    for src in cases {
        let p = parse(Language::Rust, src, &budget()).unwrap();
        let s = outline(&p, src, &OutlineOptions::default());
        for sym in &s {
            assert_clean(sym, src);
        }
        // Specifically: no impl named from damaged text.
        assert!(
            !s.iter().any(|x| x.kind == Impl && x.name.contains("where")),
            "damaged impl text leaked into a name from {src:?}: {s:?}"
        );
    }
}

/// The name of every emitted symbol must be clean: no line breaks, no comment markers, no leading
/// `for `, no trailing ` for`, and bounded in length. Damaged input used to publish a name like
/// `"? where extern <T> for /// d for"` or one carrying a newline and a `//!` comment.
///
/// The token set is the reviewer's, including the pieces that produce a damaged `impl` child:
/// `dyn`, `?`, `!`, `*`, `[`, `]`, `Bar`, `<T>` and the comment forms. The seed is fixed, so a
/// failure names the exact input.
#[test]
fn rust_names_are_always_clean_identifiers() {
    const TOKENS: [&str; 17] = [
        "impl", "?", "where", "extern", "<T>", "///", "d", "for", "!", "*", "[", "]", "Bar", "dyn",
        "\n", "//!", ";",
    ];
    let mut x: u64 = 0x5eed_1234_abcd_9876;
    let mut next = move || {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        x
    };
    let mut seen_symbols = 0usize;
    let mut seen_impls = 0usize;
    for _ in 0..5000 {
        let n = 1 + (next() % 12) as usize;
        let mut src = String::new();
        for _ in 0..n {
            src.push_str(TOKENS[(next() % TOKENS.len() as u64) as usize]);
            src.push(' ');
        }
        let s = run(Language::Rust, &src, &OutlineOptions::default());
        seen_symbols += s.len();
        seen_impls += s.iter().filter(|x| x.kind == Impl).count();
        for sym in &s {
            assert_clean(sym, &src);
        }
    }
    // Guard the guard: if the generator stopped producing symbols the loop would prove nothing.
    assert!(seen_symbols > 0, "the generator produced no symbols at all");
    // The token set must actually reach the impl path this test exists to protect.
    assert!(
        seen_impls > 0,
        "no impl symbols were produced; the test would be vacuous"
    );
}

/// Every name property the outline promises, checked for one symbol.
///
/// `name` is bounded by 256 bytes and `qualified` by 1024, so the two are checked against their
/// own limits rather than a single one.
#[track_caller]
fn assert_clean(sym: &Symbol, src: &str) {
    assert!(
        !sym.name.trim().is_empty(),
        "empty name from {src:?}: {sym:?}"
    );
    assert!(
        sym.name.len() <= 256,
        "name longer than 256 bytes from {src:?}: {sym:?}"
    );
    for value in [&sym.name, &sym.qualified] {
        let value = value.as_str();
        assert!(
            !value.contains('\n') && !value.contains('\r'),
            "name contains a line break from {src:?}: {sym:?}"
        );
        assert!(
            !value.contains("//") && !value.contains("/*") && !value.contains("*/"),
            "name contains a comment marker from {src:?}: {sym:?}"
        );
        assert!(
            !value.contains("  "),
            "name contains a doubled space from {src:?}: {sym:?}"
        );
        assert!(
            !value.trim_start().starts_with("for ") && !value.trim_end().ends_with(" for"),
            "name has a dangling `for` from {src:?}: {sym:?}"
        );
    }
    assert!(
        sym.qualified.len() <= 1024,
        "qualified name longer than 1024 bytes from {src:?}: {}",
        sym.qualified.len()
    );
}

/// The same invariant with `find_symbols`, so a nameless symbol could not be smuggled in through
/// the other entry point either.
#[test]
fn rust_find_symbols_never_returns_an_empty_match() {
    let src = "unsafe impl ;\npub fn real() {}\nimpl Foo for ;\n";
    let p = parse(Language::Rust, src, &budget()).unwrap();
    assert!(p.error_count > 0, "the fixture must really be broken");
    let found = find_symbols(&p, src, "");
    for sym in &found {
        assert!(!sym.qualified.is_empty(), "{sym:?}");
    }
    // The real symbol is still findable.
    assert_eq!(find_symbols(&p, src, "real").len(), 1);
}
