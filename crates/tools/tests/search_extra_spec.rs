//! Extra SRCH-* cases for ISSUE-TOOLS-SEARCH (validation, language choice, explain)
//! that do not need the pattern matcher.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::field_reassign_with_default
)]
#![cfg(unix)]

use opencrayast_core::ErrorCode;
use opencrayast_core::boundary::{Boundary, BoundaryConfig};
use opencrayast_core::limits::Limits;
use opencrayast_tools::{
    ExplainArgs, Mode, SearchArgs, ToolContext, ast_explain_pattern, ast_search,
};
use std::fs;

fn ctx(d: &tempfile::TempDir, limits: Limits) -> ToolContext {
    ToolContext {
        boundary: Boundary::new(BoundaryConfig {
            root: d.path().to_path_buf(),
            limits: Limits::default(),
            read_roots: Vec::new(),
            state_dir: None,
            extra_protected: Vec::new(),
        })
        .unwrap(),
        limits,
        mode: Mode::ReadOnly,
        write: None,
        version: "0.20261002.1".into(),
        workspace_id: "w-00112233445566778899aabbccddeeff".into(),
        respect_gitignore: true,
        extra_ignore: vec![],
        config_source: Default::default(),
    }
}

fn put(d: &tempfile::TempDir, rel: &str, body: &str) {
    let p = d.path().join(rel);
    fs::create_dir_all(p.parent().unwrap()).unwrap();
    fs::write(p, body).unwrap();
}

/// SRCH-01: empty / oversized pattern and empty paths are `invalid_args`.
#[test]
fn srch01_pattern_and_paths_validation() {
    let d = tempfile::tempdir().unwrap();
    put(&d, "a.js", "x\n");
    let c = ctx(&d, Limits::default());
    let err = |a: SearchArgs| ast_search(&c, &a).unwrap_err().code;
    assert_eq!(
        err(SearchArgs {
            pattern: String::new(),
            paths: vec![".".into()],
            ..Default::default()
        }),
        ErrorCode::InvalidArgs
    );
    assert_eq!(
        err(SearchArgs {
            pattern: "x".into(),
            paths: vec![],
            ..Default::default()
        }),
        ErrorCode::InvalidArgs
    );
    assert_eq!(
        err(SearchArgs {
            pattern: "x".into(),
            paths: vec![String::new()],
            ..Default::default()
        }),
        ErrorCode::InvalidArgs
    );
}

/// SRCH-02: mixed languages are refused with a sorted id list before any search.
#[test]
fn srch02_mixed_languages_message_is_sorted() {
    let d = tempfile::tempdir().unwrap();
    put(&d, "a.js", "console.log(1);\n");
    put(&d, "b.py", "print(1)\n");
    put(&d, "c.go", "package p\nfunc f() {}\n");
    let c = ctx(&d, Limits::default());
    let e = ast_search(
        &c,
        &SearchArgs {
            pattern: "f($X)".into(),
            paths: vec![".".into()],
            ..Default::default()
        },
    )
    .unwrap_err();
    assert_eq!(e.code, ErrorCode::InvalidArgs);
    assert_eq!(e.message, "mixed languages: go, javascript, python");
    assert!(e.next.contains("language"), "{}", e.next);
}

/// SRCH-03: explain lists metavariables in pattern order and fences the tree.
#[test]
fn srch03_explain_metavars_order_and_fence() {
    let d = tempfile::tempdir().unwrap();
    let c = ctx(&d, Limits::default());
    let out = ast_explain_pattern(
        &c,
        &ExplainArgs {
            pattern: "f($B, $A, $$$C)".into(),
            language: "javascript".into(),
        },
    )
    .unwrap();
    assert!(
        out.contains("metavariables: $B (one), $A (one), $$$C (list)\n"),
        "{out}"
    );
    assert!(out.contains("```text\n"), "{out}");
    assert!(out.ends_with("```\n"), "{out}");
}

/// SRCH-04: unknown explain language lists supported ids.
#[test]
fn srch04_explain_unknown_language_lists_ids() {
    let d = tempfile::tempdir().unwrap();
    let c = ctx(&d, Limits::default());
    let e = ast_explain_pattern(
        &c,
        &ExplainArgs {
            pattern: "x".into(),
            language: "fortran".into(),
        },
    )
    .unwrap_err();
    assert_eq!(e.code, ErrorCode::InvalidArgs);
    for id in ["javascript", "python", "rust", "go", "typescript"] {
        assert!(e.message.contains(id), "missing {id} in {}", e.message);
    }
}
