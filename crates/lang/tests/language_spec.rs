//! Spec for ISSUE-LANG-REGISTRY. Add cases; never weaken these.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
use Language::*;
use opencrayast_lang::Language;

#[test]
fn ids_roundtrip_and_aliases() {
    for l in Language::all() {
        assert_eq!(Language::from_id(l.id()), Some(*l));
        assert_eq!(Language::from_id(&l.id().to_uppercase()), Some(*l));
    }
    let ids: Vec<&str> = Language::all().iter().map(|l| l.id()).collect();
    assert_eq!(
        ids,
        ["rust", "typescript", "tsx", "javascript", "python", "go"]
    );
    for (alias, l) in [
        ("ts", TypeScript),
        ("js", JavaScript),
        ("jsx", JavaScript),
        ("py", Python),
        ("rs", Rust),
        ("golang", Go),
    ] {
        assert_eq!(Language::from_id(alias), Some(l), "{alias}");
    }
    for bad in ["", "ruby", "c++", "rust ", "r\u{0}ust"] {
        assert_eq!(Language::from_id(bad), None, "{bad:?}");
    }
}

// Only meaningful when every grammar feature is on (the default); with a single feature enabled the
// other languages are, correctly, unavailable. The per-feature behaviour is tested separately.
#[cfg(all(
    feature = "lang-rust",
    feature = "lang-typescript",
    feature = "lang-javascript",
    feature = "lang-python",
    feature = "lang-go"
))]
#[test]
fn all_current_languages_are_tier_1_and_available_by_default() {
    for l in Language::all() {
        assert_eq!(l.tier(), 1);
        assert!(l.is_available(), "{l:?}");
        assert!(l.grammar().is_some(), "{l:?}");
    }
}

#[test]
fn extension_detection_table() {
    let table = [
        ("src/lib.rs", Rust),
        ("a.ts", TypeScript),
        ("a.mts", TypeScript),
        ("a.cts", TypeScript),
        ("types/index.d.ts", TypeScript),
        ("App.tsx", Tsx),
        ("a.js", JavaScript),
        ("a.jsx", JavaScript),
        ("a.mjs", JavaScript),
        ("a.cjs", JavaScript),
        ("tool.py", Python),
        ("tool.pyi", Python),
        ("main.go", Go),
        ("C:\\proj\\Main.RS", Rust),
        ("X.PY", Python),
        ("dir.with.dots/file.go", Go),
    ];
    for (name, want) in table {
        assert_eq!(Language::detect(name, None), Some(want), "{name}");
    }
}

#[test]
fn unknown_and_tricky_names_are_not_detected() {
    for name in [
        "",
        "Makefile",
        "README.md",
        "a.rs.bak",
        "rs",
        ".rs",
        "a.txt",
        "a.",
        "x.json",
        "dir.rs/",
    ] {
        assert_eq!(Language::detect(name, None), None, "{name:?}");
    }
}

#[test]
fn shebang_is_used_only_without_a_known_extension() {
    assert_eq!(
        Language::detect("script", Some("#!/usr/bin/env python3")),
        Some(Python)
    );
    assert_eq!(
        Language::detect("script", Some("#!/usr/bin/python")),
        Some(Python)
    );
    assert_eq!(
        Language::detect("run", Some("#!/usr/bin/env node")),
        Some(JavaScript)
    );
    assert_eq!(Language::detect("script", Some("#!/bin/sh")), None);
    assert_eq!(Language::detect("script", Some("print('x')")), None);
    assert_eq!(Language::detect("script", None), None);
    // a known extension wins over a shebang
    assert_eq!(
        Language::detect("x.go", Some("#!/usr/bin/env python3")),
        Some(Go)
    );
    // an unknown extension still gets the shebang
    assert_eq!(
        Language::detect("x.sh", Some("#!/usr/bin/env python3")),
        Some(Python)
    );
}

#[test]
fn detection_does_not_depend_on_features() {
    // Detection is pure naming logic: it must answer identically whether or not the grammar is
    // compiled in. `is_available` is what reports the grammar's presence.
    for l in Language::all() {
        assert_eq!(l.tier(), 1);
    }
    assert_eq!(Language::detect("a.rs", None), Some(Rust));
    assert_eq!(Language::from_id("RS"), Some(Rust));
}

// Each language reports its grammar only under its own feature. These assertions are compiled only
// when the feature is on, so the whole file stays green under `--no-default-features --features
// lang-<one>` as well.

#[cfg(feature = "lang-rust")]
#[test]
fn rust_grammar_is_present() {
    assert!(Rust.is_available());
    assert!(Rust.grammar().is_some());
}

#[cfg(feature = "lang-typescript")]
#[test]
fn typescript_feature_provides_both_typescript_and_tsx() {
    assert!(TypeScript.is_available());
    assert!(Tsx.is_available());
    assert!(TypeScript.grammar().is_some());
    assert!(Tsx.grammar().is_some());
    // Two distinct grammars, not one reused.
    assert_ne!(TypeScript.grammar(), Tsx.grammar());
}

#[cfg(feature = "lang-javascript")]
#[test]
fn javascript_grammar_is_present() {
    assert!(JavaScript.is_available());
    assert!(JavaScript.grammar().is_some());
}

#[cfg(feature = "lang-python")]
#[test]
fn python_grammar_is_present() {
    assert!(Python.is_available());
    assert!(Python.grammar().is_some());
}

#[cfg(feature = "lang-go")]
#[test]
fn go_grammar_is_present() {
    assert!(Go.is_available());
    assert!(Go.grammar().is_some());
}

// With exactly one grammar feature enabled, the other languages must report "no grammar" rather
// than fail to compile. `cfg(any(...))` is true in the default build too, so guard it: these only
// add information when the default set is off.
#[cfg(not(feature = "lang-rust"))]
#[test]
fn rust_is_absent_without_its_feature() {
    assert!(!Rust.is_available());
    assert!(Rust.grammar().is_none());
}

#[cfg(not(feature = "lang-typescript"))]
#[test]
fn typescript_is_absent_without_its_feature() {
    assert!(!TypeScript.is_available());
    assert!(TypeScript.grammar().is_none());
    assert!(!Tsx.is_available());
    assert!(Tsx.grammar().is_none());
}

#[cfg(not(feature = "lang-javascript"))]
#[test]
fn javascript_is_absent_without_its_feature() {
    assert!(!JavaScript.is_available());
    assert!(JavaScript.grammar().is_none());
}

#[cfg(not(feature = "lang-python"))]
#[test]
fn python_is_absent_without_its_feature() {
    assert!(!Python.is_available());
    assert!(Python.grammar().is_none());
}

#[cfg(not(feature = "lang-go"))]
#[test]
fn go_is_absent_without_its_feature() {
    assert!(!Go.is_available());
    assert!(Go.grammar().is_none());
}

#[test]
fn detection_edge_cases() {
    // Only the last component matters.
    assert_eq!(Language::detect("a.py/b.rs", None), Some(Rust));
    assert_eq!(Language::detect("dir.rs/", None), None);
    assert_eq!(
        Language::detect("dir.rs/", Some("#!/usr/bin/env python3")),
        Some(Python)
    );
    assert_eq!(Language::detect("C:\\dir.rs\\x.go", None), Some(Go));
    // A dotfile has no extension.
    assert_eq!(Language::detect(".rs", None), None);
    assert_eq!(
        Language::detect(".rs", Some("#!/usr/bin/env node")),
        Some(JavaScript)
    );
    // Trailing dot: unknown extension, so the shebang still applies.
    assert_eq!(
        Language::detect("a.", Some("#!/usr/bin/python3")),
        Some(Python)
    );
    assert_eq!(Language::detect("a.", None), None);
    // Interpreter forms.
    assert_eq!(
        Language::detect("s", Some("#!/usr/bin/python3.11")),
        Some(Python)
    );
    assert_eq!(
        Language::detect("s", Some("#!  /usr/bin/env   python3  ")),
        Some(Python)
    );
    assert_eq!(
        Language::detect("s", Some("#!/usr/bin/env -S python3 -u")),
        Some(Python)
    );
    assert_eq!(
        Language::detect("s", Some("#!/usr/bin/node")),
        Some(JavaScript)
    );
    assert_eq!(
        Language::detect("s", Some("#!/usr/bin/env node --experimental-modules")),
        Some(JavaScript)
    );
    assert_eq!(
        Language::detect("s", Some("#! /usr/bin/python\r")),
        Some(Python)
    );
    // Near-misses that must not match.
    assert_eq!(Language::detect("s", Some("#!/usr/bin/pythonista")), None);
    assert_eq!(Language::detect("s", Some("#!/usr/bin/pypy3")), None);
    assert_eq!(Language::detect("s", Some("#!")), None);
    assert_eq!(Language::detect("s", Some("#usr/bin/env python3")), None);
    // A known extension wins even against a shebang naming another language.
    assert_eq!(
        Language::detect("x.py", Some("#!/usr/bin/env node")),
        Some(Python)
    );
}
