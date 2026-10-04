//! Spec for ISSUE-LANG-PARSE (PRS-01..04, PRS-09). Add cases; never weaken these.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
use opencrayast_core::ErrorCode;
use opencrayast_lang::{Language, ParseBudget, parse};
use std::time::Duration;

fn budget() -> ParseBudget {
    ParseBudget {
        max_bytes: 4 * 1024 * 1024,
        timeout: Duration::from_secs(5),
        max_depth: 512,
        max_nodes: 2_000_000,
    }
}

const GOOD: [(Language, &str); 6] = [
    (
        Language::Rust,
        "pub fn add(a: i32, b: i32) -> i32 {\n    a + b\n}\n",
    ),
    (
        Language::TypeScript,
        "export function add(a: number, b: number): number {\n  return a + b;\n}\n",
    ),
    (
        Language::Tsx,
        "export const App = () => <div className=\"x\">hi</div>;\n",
    ),
    (
        Language::JavaScript,
        "function add(a, b) {\n  return a + b;\n}\nconst el = <b>x</b>;\n",
    ),
    (Language::Python, "def add(a, b):\n    return a + b\n"),
    (
        Language::Go,
        "package main\n\nfunc add(a, b int) int {\n\treturn a + b\n}\n",
    ),
];

#[test]
fn valid_source_parses_with_zero_errors_in_every_language() {
    for (lang, src) in GOOD {
        let p = parse(lang, src, &budget()).unwrap();
        assert_eq!(p.error_count, 0, "{lang:?}");
        assert_eq!(p.language, lang);
        assert!(
            p.node_count > 5 && p.max_depth >= 3,
            "{lang:?} {} {}",
            p.node_count,
            p.max_depth
        );
        assert_eq!(p.tree.root_node().end_byte(), src.len());
    }
}

/// PRS-09: broken source is still parsed and the errors are counted honestly.
#[test]
fn broken_source_has_a_positive_error_count_and_fixing_it_lowers_it() {
    let broken = [
        (Language::Rust, "fn f( {\n    let x = ;\n}\n"),
        (Language::TypeScript, "function f( { return ; \n"),
        (Language::JavaScript, "function f( { return ; \n"),
        (Language::Python, "def f(:\n    return\n"),
        (Language::Go, "package main\nfunc f( {\n"),
    ];
    for (lang, src) in broken {
        let p = parse(lang, src, &budget()).unwrap();
        assert!(p.error_count > 0, "{lang:?} must report errors");
    }
    // one missing closing brace is exactly the kind of thing MISSING nodes are for
    let one = parse(Language::Rust, "fn f() {\n", &budget()).unwrap();
    assert!(one.error_count >= 1);
    let fixed = parse(Language::Rust, "fn f() {\n}\n", &budget()).unwrap();
    assert_eq!(fixed.error_count, 0);
}

#[test]
fn empty_source_parses() {
    for l in Language::all() {
        let p = parse(*l, "", &budget()).unwrap();
        assert_eq!(p.error_count, 0, "{l:?}");
    }
}

#[test]
fn over_the_size_budget_is_file_too_large_and_is_checked_first() {
    let mut b = budget();
    b.max_bytes = 10;
    let e = parse(Language::Rust, "fn main() { /* long enough */ }", &b)
        .err()
        .unwrap();
    assert_eq!(e.code, ErrorCode::FileTooLarge);
    // checked before the timeout/other budgets
    b.timeout = Duration::from_nanos(1);
    assert_eq!(
        parse(Language::Rust, "fn main() { /* long enough */ }", &b)
            .err()
            .unwrap()
            .code,
        ErrorCode::FileTooLarge
    );
}

/// PRS-01
#[test]
fn pathological_nesting_hits_the_depth_budget_and_does_not_overflow_the_stack() {
    let n = 20_000;
    let src = format!(
        "fn f() {{ let x = {}1{}; }}\n",
        "(".repeat(n),
        ")".repeat(n)
    );
    let mut b = budget();
    b.max_depth = 256;
    let e = parse(Language::Rust, &src, &b).err().unwrap();
    assert_eq!(e.code, ErrorCode::BudgetExceeded);
    assert!(e.message.to_lowercase().contains("depth"), "{}", e.message);
    // the same shape in a bracket-heavy JS expression
    let js = format!("var a = {}0{};\n", "[".repeat(n), "]".repeat(n));
    let e = parse(Language::JavaScript, &js, &b).err().unwrap();
    assert_eq!(e.code, ErrorCode::BudgetExceeded);
}

/// PRS-03
#[test]
fn a_file_with_millions_of_nodes_is_stopped_by_the_node_budget() {
    let src = "a;\n".repeat(200_000); // ~400k+ nodes in JS
    let mut b = budget();
    b.max_nodes = 10_000;
    let e = parse(Language::JavaScript, &src, &b).err().unwrap();
    assert_eq!(e.code, ErrorCode::BudgetExceeded);
    assert!(e.message.to_lowercase().contains("node"), "{}", e.message);
}

/// PRS-02
#[test]
fn a_huge_single_token_or_line_terminates() {
    let src = format!("let s = \"{}\";\n", "x".repeat(3_000_000));
    let t = std::time::Instant::now();
    let r = parse(Language::Rust, &src, &budget());
    assert!(r.is_ok() || r.err().unwrap().code != ErrorCode::Internal);
    assert!(t.elapsed().as_secs() < 20, "must terminate promptly");
    let one_line = "1,".repeat(500_000);
    let _ = parse(Language::JavaScript, &format!("f({one_line});"), &budget());
}

/// PRS-04
#[test]
fn the_wall_clock_timeout_cancels_a_parse() {
    let src = "fn f() { let a = 1; }\n".repeat(150_000); // ~3 MiB
    let mut b = budget();
    b.max_bytes = 16 * 1024 * 1024;
    b.timeout = Duration::from_millis(1);
    let t = std::time::Instant::now();
    let e = parse(Language::Rust, &src, &b)
        .err()
        .expect("1 ms cannot parse 3 MiB");
    assert_eq!(e.code, ErrorCode::Timeout);
    assert!(
        t.elapsed() < Duration::from_secs(5),
        "cancel must be prompt, took {:?}",
        t.elapsed()
    );
}

#[test]
fn error_messages_never_contain_source_text() {
    let secret = "SUPER_SECRET_TOKEN_12345";
    let src = format!(
        "fn f() {{ let {secret} = ({}1{}); }}",
        "(".repeat(5000),
        ")".repeat(5000)
    );
    let mut b = budget();
    b.max_depth = 50;
    let e = parse(Language::Rust, &src, &b).err().unwrap();
    assert!(!e.to_string().contains(secret));
}

#[test]
fn budget_from_limits_maps_the_documented_fields() {
    let l = opencrayast_core::limits::Limits::default();
    let b = ParseBudget::from(&l);
    assert_eq!(b.max_bytes, l.max_file_bytes);
    assert_eq!(b.timeout, Duration::from_millis(l.parse_timeout_ms));
    assert_eq!(b.max_depth, l.parse_max_depth);
    assert_eq!(b.max_nodes, l.parse_max_nodes);
}

#[test]
fn arbitrary_bytes_as_text_never_panic() {
    let mut x: u64 = 0xdead_beef_cafe_f00d;
    for _ in 0..300 {
        let mut s = String::new();
        for _ in 0..200 {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            s.push(char::from_u32((x % 0x2fff) as u32).unwrap_or('?'));
        }
        for l in Language::all() {
            let _ = parse(*l, &s, &budget());
        }
    }
}

/// PRS-09, one golden pair per language: the same construct broken reports errors, and the
/// repaired source reports none. This is the comparison the syntax gate makes before an edit.
#[test]
fn broken_then_fixed_is_zero_for_every_language() {
    let pairs: [(Language, &str, &str); 6] = [
        (
            Language::Rust,
            "pub fn add(a: i32, b: i32 -> i32 {\n    a + \n",
            "pub fn add(a: i32, b: i32) -> i32 {\n    a + b\n}\n",
        ),
        (
            Language::TypeScript,
            "export function add(a: number, b: number: number {\n  return a + \n",
            "export function add(a: number, b: number): number {\n  return a + b;\n}\n",
        ),
        (
            Language::Tsx,
            "export const App = () => <div>hi</div\n",
            "export const App = () => <div>hi</div>;\n",
        ),
        (
            Language::JavaScript,
            "function add(a, b {\n  return a + \n}\n",
            "function add(a, b) {\n  return a + b;\n}\n",
        ),
        (
            Language::Python,
            "def add(a, b)\n    return a + b\n",
            "def add(a, b):\n    return a + b\n",
        ),
        (
            Language::Go,
            "package main\n\nfunc add(a, b int) int {\n\treturn a + \n",
            "package main\n\nfunc add(a, b int) int {\n\treturn a + b\n}\n",
        ),
    ];
    for (lang, broken, fixed) in pairs {
        let b = parse(lang, broken, &budget()).unwrap();
        assert!(
            b.error_count > 0,
            "{lang:?} broken source must report errors"
        );
        let f = parse(lang, fixed, &budget()).unwrap();
        assert_eq!(f.error_count, 0, "{lang:?} fixed source: {fixed:?}");
        assert!(f.error_count < b.error_count, "{lang:?}");
    }
}

/// Byte offsets are byte offsets, not character counts: a multibyte source still spans exactly
/// its own length, so tools that slice on `end_byte` stay correct (S-6).
#[test]
fn multibyte_source_spans_its_own_length_and_reports_no_errors() {
    let cases: [(Language, &str); 6] = [
        (
            Language::Rust,
            "pub fn f() -> &'static str { \"héllo 世界 🦞\" }\n",
        ),
        (
            Language::TypeScript,
            "export const 名: string = \"世界\";\n",
        ),
        (
            Language::Tsx,
            "export const 名 = () => <div>世界 🦞</div>;\n",
        ),
        (Language::JavaScript, "const 名 = \"héllo 世界\";\n"),
        (
            Language::Python,
            "def f() -> str:\n    return \"héllo 世界 🦞\"\n",
        ),
        (
            Language::Go,
            "package main\n\nfunc F() string { return \"héllo 世界\" }\n",
        ),
    ];
    for (lang, src) in cases {
        let p = parse(lang, src, &budget()).unwrap();
        assert_eq!(p.error_count, 0, "{lang:?}");
        assert_eq!(p.tree.root_node().end_byte(), src.len(), "{lang:?}");
    }
}

/// A leading BOM is not a syntax error in any of these grammars, and it is part of the source
/// length: a tool must not treat it as invisible when slicing.
#[test]
fn a_leading_bom_parses_cleanly() {
    for l in Language::all() {
        let src = format!("\u{feff}{}", tail(*l));
        let p = parse(*l, &src, &budget()).unwrap();
        assert_eq!(p.error_count, 0, "{l:?} BOM source");
        assert_eq!(p.tree.root_node().end_byte(), src.len(), "{l:?}");
    }
}

/// CRLF sources must not become errors and must not change the reported depth behaviour.
#[test]
fn crlf_source_parses_cleanly() {
    let cases: [(Language, &str); 6] = [
        (Language::Rust, "fn f() {\r\n    let x = 1;\r\n}\r\n"),
        (
            Language::TypeScript,
            "export function f(a: number): number {\r\n  return a;\r\n}\r\n",
        ),
        (Language::Tsx, "export const A = () => <div>hi</div>;\r\n"),
        (
            Language::JavaScript,
            "function f(a) {\r\n  return a;\r\n}\r\n",
        ),
        (Language::Python, "def f():\r\n    return 1\r\n"),
        (
            Language::Go,
            "package main\r\n\r\nfunc f() int {\r\n\treturn 1\r\n}\r\n",
        ),
    ];
    for (lang, src) in cases {
        let p = parse(lang, src, &budget()).unwrap();
        assert_eq!(p.error_count, 0, "{lang:?} CRLF source");
        assert_eq!(p.tree.root_node().end_byte(), src.len(), "{lang:?}");
    }
}

/// The size budget counts bytes, so a multibyte source crosses a byte budget sooner than its
/// character count suggests.
#[test]
fn the_size_budget_counts_bytes_not_characters() {
    let src = "\"世界\"".repeat(10); // 80 bytes, 40 characters
    assert_eq!(src.chars().count(), 40);
    assert_eq!(src.len(), 80);
    let mut b = budget();
    b.max_bytes = 40; // the character count: not enough
    let e = parse(Language::Rust, &src, &b).err().unwrap();
    assert_eq!(e.code, ErrorCode::FileTooLarge);
    b.max_bytes = 80; // the byte count: exactly enough
    assert!(parse(Language::Rust, &src, &b).is_ok());
}

/// A generous budget must not turn a small file into a failure: the smallest possible breach is
/// still a breach, so check that limits just above the real numbers pass.
#[test]
fn limits_just_above_the_real_numbers_pass_and_one_below_fails() {
    let src = "fn f() { let a = 1; }\n";
    let ok = parse(Language::Rust, src, &budget()).unwrap();
    assert!(ok.max_depth >= 1 && ok.node_count >= 1);
    // The smallest budget that still accepts this file is the real count itself.
    let mut b = budget();
    b.max_depth = ok.max_depth as u64;
    assert!(parse(Language::Rust, src, &b).is_ok());
    let mut b = budget();
    b.max_depth = ok.max_depth as u64 - 1;
    let e = parse(Language::Rust, src, &b).err().unwrap();
    assert_eq!(e.code, ErrorCode::BudgetExceeded);
    assert!(e.message.to_lowercase().contains("depth"));

    let mut b = budget();
    b.max_nodes = ok.node_count as u64;
    assert!(parse(Language::Rust, src, &b).is_ok());
    let mut b = budget();
    b.max_nodes = ok.node_count as u64 - 1;
    let e = parse(Language::Rust, src, &b).err().unwrap();
    assert_eq!(e.code, ErrorCode::BudgetExceeded);
    assert!(e.message.to_lowercase().contains("node"));
}

/// The walk must stop at the first breach rather than counting the whole tree: a source that would
/// blow the node budget by a wide margin still returns promptly.
#[test]
fn the_walk_stops_at_the_first_breach() {
    let src = "a;\n".repeat(200_000);
    let mut b = budget();
    b.max_nodes = 1;
    let t = std::time::Instant::now();
    let e = parse(Language::JavaScript, &src, &b).err().unwrap();
    assert_eq!(e.code, ErrorCode::BudgetExceeded);
    assert!(
        t.elapsed() < Duration::from_secs(5),
        "walk should stop early, took {:?}",
        t.elapsed()
    );
}

/// A zero budget is a valid (if useless) budget: every parse is refused, not a panic.
#[test]
fn a_zero_budget_is_refused_not_panicked_on() {
    let b = ParseBudget {
        max_bytes: 0,
        timeout: Duration::ZERO,
        max_depth: 0,
        max_nodes: 0,
    };
    for l in Language::all() {
        let e = parse(*l, "fn f() {}\n", &b).err().unwrap();
        assert_eq!(e.code, ErrorCode::FileTooLarge, "{l:?}");
    }
}

/// Depth counting counts the root as 1, as documented.
#[test]
fn max_depth_counts_the_root_as_one() {
    // The walk compares `cursor.depth() + 1`, so the root counts as 1 and a `source_file` with a
    // function in it is deeper than 1. Any grammar of this shape reaches at least 4.
    let p = parse(Language::Rust, "fn f() {}\n", &budget()).unwrap();
    assert!(
        p.max_depth >= 2,
        "root counts as 1, so depth > 1: {}",
        p.max_depth
    );
    let deeper = parse(Language::Rust, "fn f() { let x = g(1); }\n", &budget()).unwrap();
    assert!(deeper.max_depth > p.max_depth);
    assert!(deeper.node_count > p.node_count);
}

/// A language with no grammar in this build must say so rather than parse with something else.
/// Under the default feature set every language has a grammar, so this asserts the shape of the
/// error the caller would get.
#[cfg(not(all(
    feature = "lang-rust",
    feature = "lang-typescript",
    feature = "lang-javascript",
    feature = "lang-python",
    feature = "lang-go"
)))]
#[test]
fn a_language_without_a_grammar_is_unsupported() {
    for l in Language::all() {
        if l.grammar().is_none() {
            let e = parse(*l, "fn f() {}\n", &budget()).err().unwrap();
            assert_eq!(e.code, ErrorCode::UnsupportedLanguage, "{l:?}");
            // unsupported_language is decided before the size budget, even for a huge source.
            let mut b = budget();
            b.max_bytes = 0;
            assert_eq!(
                parse(*l, "fn f() {}\n", &b).err().unwrap().code,
                ErrorCode::UnsupportedLanguage,
                "{l:?}"
            );
        }
    }
}

/// The smallest parsable source, and one that is only a comment, must not error.
#[test]
fn trivial_sources_parse_cleanly() {
    for (lang, src) in [
        (Language::Rust, "// just a comment\n"),
        (Language::Rust, "\n\n\n"),
        (Language::TypeScript, "// ts\n"),
        (Language::JavaScript, "// js\n"),
        (Language::Python, "# py\n"),
        (Language::Go, "package main\n"),
    ] {
        let p = parse(lang, src, &budget()).unwrap();
        assert_eq!(p.error_count, 0, "{lang:?} {src:?}");
    }
}

/// A shebang is not a syntax error for the grammars that skip it; make sure the parse count we
/// report is still honest about the rest of the file.
#[test]
fn a_shebang_line_is_not_a_syntax_error() {
    let p = parse(
        Language::Python,
        "#!/usr/bin/env python3\ndef f():\n    pass\n",
        &budget(),
    )
    .unwrap();
    assert_eq!(p.error_count, 0);
}

/// `tail` is the smallest source each language parses cleanly; used by the BOM test.
fn tail(l: Language) -> &'static str {
    match l {
        Language::Rust => "fn f() {}\n",
        Language::TypeScript => "export const a: number = 1;\n",
        Language::Tsx => "export const A = () => <div>hi</div>;\n",
        Language::JavaScript => "const a = 1;\n",
        Language::Python => "a = 1\n",
        Language::Go => "package main\n\nfunc f() {}\n",
    }
}

/// The parse must consume the whole input. If the reader callback ever stops early - for example
/// by handing tree-sitter an empty slice while bytes remain - the root node's extent would be
/// short and the syntax-error count would understate the damage, which is exactly what the syntax
/// gate compares. So assert `root.end_byte() == source.len()` on inputs chosen to break a reader:
/// multibyte-dense, BOM-prefixed, CRLF and outright broken.
#[test]
fn the_root_always_spans_the_whole_source() {
    let cases: [&str; 7] = [
        "",
        "fn f() {}\n",
        // Multibyte at the very start and in every position.
        "fn 世界() -> &'static str { \"🦞héllo\" }\n",
        "const 名: &str = \"é́\u{0301}世界\";\n",
        // Multibyte inside every kind of token, including where an error will be inserted.
        "fn f(世界: 世界) -> 世界 { 世界 + \"世界🦞\" }\n",
        // A broken construct surrounded by multibyte text.
        "fn f() { let 名 = \"世界\" * ; }\n",
        // Byte-order mark, then CRLF, then a broken item.
        "\u{feff}fn f() {\r\n    let 名 = \"世界\";\r\n",
    ];
    for l in Language::all() {
        for src in cases {
            let p = parse(*l, src, &budget()).unwrap();
            assert_eq!(
                p.tree.root_node().end_byte(),
                src.len(),
                "{l:?} root extent on {:?}",
                src
            );
        }
    }
}

/// Same property on generated multibyte-heavy junk: a reader that rounds an offset to the next
/// character boundary would silently truncate these before they are fully read.
#[test]
fn the_root_spans_multibyte_junk() {
    const PIECES: [char; 6] = ['世', '🦞', 'é', '�', 'a', '"'];
    let mut x: u64 = 0x1234_5678_9abc_def0;
    for _ in 0..200 {
        let mut s = String::new();
        for _ in 0..64 {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            s.push(PIECES[(x % PIECES.len() as u64) as usize]);
        }
        for l in Language::all() {
            let p = parse(*l, &s, &budget()).unwrap();
            assert_eq!(p.tree.root_node().end_byte(), s.len(), "{l:?} on {s:?}");
        }
    }
}
