//! Spec for ISSUE-TOOLS-INFO, ISSUE-TOOLS-OUTLINE and ISSUE-TOOLS-GET. The expected strings are the
//! contract (docs/TOOLS.md plus the exact formats in the doc comments of `ast_outline`/`ast_get`).
//! Add cases; never weaken these.
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
use opencrayast_tools::{GetArgs, Mode, OutlineArgs, ToolContext, ast_get, ast_info, ast_outline};
use std::fs;
use std::os::unix::fs::symlink;

const RS: &str = include_str!("../../query/tests/fixtures/sample.rs");
const ID: &str = "w-00112233445566778899aabbccddeeff";

fn ctx_with(ws: &tempfile::TempDir, limits: Limits, mode: Mode) -> ToolContext {
    let boundary = Boundary::new(BoundaryConfig {
        root: ws.path().to_path_buf(),
        limits: Limits::default(),
        read_roots: Vec::new(),
        state_dir: None,
        extra_protected: Vec::new(),
    })
    .unwrap();
    ToolContext {
        boundary,
        limits,
        mode,
        write: None,
        version: "0.20261002.1".to_string(),
        workspace_id: ID.to_string(),
        respect_gitignore: true,
        extra_ignore: vec![],
        config_source: Default::default(),
    }
}

fn ws() -> (tempfile::TempDir, ToolContext) {
    let d = tempfile::tempdir().unwrap();
    let c = ctx_with(&d, Limits::default(), Mode::ReadOnly);
    (d, c)
}

fn put(d: &tempfile::TempDir, rel: &str, body: &str) {
    let p = d.path().join(rel);
    fs::create_dir_all(p.parent().unwrap()).unwrap();
    fs::write(p, body).unwrap();
}

fn oargs(path: &str) -> OutlineArgs {
    OutlineArgs {
        path: path.to_string(),
        ..Default::default()
    }
}

fn gargs(symbol: &str) -> GetArgs {
    GetArgs {
        symbol: symbol.to_string(),
        ..Default::default()
    }
}

// ---------------------------------------------------------------- ast_info

#[test]
fn info_read_only_exact() {
    let (_d, c) = ws();
    assert_eq!(
        ast_info(&c),
        "opencrayast 0.20261002.1 (mode: read-only)\n\
         workspace: . (id w-00112233445566778899aabbccddeeff)\n\
         languages: rust (tier 1), typescript (tier 1), tsx (tier 1), javascript (tier 1), python (tier 1), go (tier 1)\n\
         limits: file 4 MiB \u{b7} output 64 KiB \u{b7} results 200 \u{b7} plan 50 files / 500 edits\n\
         config: defaults (no user configuration file found)\n\
         write: disabled (needs BOTH --allow-write AND policy.allow_write = true in the server config; together they expose ast_edit_apply, ast_undo, ast_recover)\n"
    );
}

#[test]
fn info_write_mode_and_odd_sizes() {
    let d = tempfile::tempdir().unwrap();
    let mut l = Limits::default();
    l.max_file_bytes = 1000;
    l.max_output_bytes = 3 * 1024;
    let c = ctx_with(&d, l, Mode::Write);
    let s = ast_info(&c);
    assert!(
        s.starts_with("opencrayast 0.20261002.1 (mode: write)\n"),
        "{s}"
    );
    assert!(
        s.contains("limits: file 1000 B \u{b7} output 3 KiB \u{b7}"),
        "{s}"
    );
    assert!(
        s.ends_with("write: enabled (ast_edit_apply, ast_undo, ast_recover)\n"),
        "{s}"
    );
    assert_eq!(s.lines().count(), 6);
}

// ------------------------------------------------------------- ast_outline

#[test]
fn outline_of_a_rust_file_exact() {
    let (d, c) = ws();
    put(&d, "src/config.rs", RS);
    let out = ast_outline(&c, &oargs("src/config.rs")).unwrap();
    assert_eq!(
        out,
        "src/config.rs  rust  34 lines\n\
         \x20 struct Config L4-6  pub struct Config\n\
         \x20 impl Config L8-17  impl Config\n\
         \x20   method load L10-12  pub fn load(path: &str) -> Result<Config, Error>\n\
         \x20   method validate L14-16  fn validate(&self) -> bool\n\
         \x20 enum Error L19-22  pub enum Error\n\
         \x20 trait Shape L24-26  pub trait Shape\n\
         \x20   method area L25  fn area(&self) -> f64\n\
         \x20 const MAX L28  const MAX: usize = 10\n\
         \x20 module inner L30-32  mod inner\n\
         \x20   fn helper L31  pub fn helper()\n\
         \x20 fn main L34  pub fn main()\n"
    );
}

#[test]
fn outline_depth_kinds_docs_and_limit() {
    let (d, c) = ws();
    put(&d, "src/config.rs", RS);
    let mut a = oargs("src/config.rs");
    a.depth = Some(1);
    let out = ast_outline(&c, &a).unwrap();
    assert_eq!(out.lines().count(), 1 + 7, "{out}");
    let mut a = oargs("src/config.rs");
    a.kinds = Some(vec!["method".into()]);
    let out = ast_outline(&c, &a).unwrap();
    assert_eq!(out.lines().count(), 1 + 3, "{out}");
    let mut a = oargs("src/config.rs");
    a.include_docs = Some(true);
    a.depth = Some(1);
    let out = ast_outline(&c, &a).unwrap();
    assert!(
        out.contains("  struct Config L4-6  pub struct Config\n    /// A configuration.\n"),
        "{out}"
    );
    let mut a = oargs("src/config.rs");
    a.limit = Some(3);
    let out = ast_outline(&c, &a).unwrap();
    assert!(out.ends_with("[truncated: showing 3 of 11 symbols in 1 files; narrow `path`, lower `depth` or filter `kinds`]\n"), "{out}");
    assert_eq!(out.lines().count(), 1 + 3 + 1, "{out}");
}

#[test]
fn outline_of_a_directory_sorts_skips_and_counts() {
    let (d, c) = ws();
    put(&d, "b.py", "def b():\n    pass\n");
    put(&d, "a.rs", "pub fn a() {}\n");
    put(&d, "notes.txt", "hello\n");
    put(&d, ".gitignore", "ignored.rs\n");
    put(&d, "ignored.rs", "fn x() {}\n");
    let out_dir = tempfile::tempdir().unwrap();
    fs::write(out_dir.path().join("secret.rs"), "fn secret() {}\n").unwrap();
    symlink(out_dir.path().join("secret.rs"), d.path().join("link.rs")).unwrap();
    let out = ast_outline(&c, &oargs(".")).unwrap();
    assert_eq!(
        out,
        "a.rs  rust  1 lines\n\
         \x20 fn a L1  pub fn a()\n\
         b.py  python  2 lines\n\
         \x20 fn b L1-2  def b()\n\
         [skipped: 1 ignored, 1 links, 2 unsupported language]\n"
    );
    assert!(!out.contains("secret"));
    // deterministic
    assert_eq!(ast_outline(&c, &oargs(".")).unwrap(), out);
}

#[test]
fn a_file_with_syntax_errors_says_so_in_its_header() {
    let (d, c) = ws();
    put(&d, "bad.rs", "pub fn good() {}\nfn broken( {\n");
    let out = ast_outline(&c, &oargs("bad.rs")).unwrap();
    let first = out.lines().next().unwrap();
    assert!(first.starts_with("bad.rs  rust  2 lines ("), "{first}");
    assert!(
        first.ends_with("syntax errors)") || first.ends_with("syntax error)"),
        "{first}"
    );
    assert!(out.contains("fn good L1  pub fn good()"), "{out}");
}

#[test]
fn outline_argument_and_file_errors() {
    let (d, c) = ws();
    put(&d, "a.rs", "fn a() {}\n");
    put(&d, "notes.txt", "x\n");
    fs::write(d.path().join("bin.rs"), [0xff, 0xfe, 0x00]).unwrap();
    let code = |a: &OutlineArgs| ast_outline(&c, a).unwrap_err().code;
    let mut a = oargs("a.rs");
    a.kinds = Some(vec!["function".into()]);
    assert_eq!(code(&a), ErrorCode::InvalidArgs);
    for depth in [0u64, 7] {
        let mut a = oargs("a.rs");
        a.depth = Some(depth);
        assert_eq!(code(&a), ErrorCode::InvalidArgs, "depth {depth}");
    }
    for limit in [0u64, 201 + 800] {
        let mut a = oargs("a.rs");
        a.limit = Some(limit);
        assert_eq!(code(&a), ErrorCode::InvalidArgs, "limit {limit}");
    }
    assert_eq!(code(&oargs("")), ErrorCode::InvalidArgs);
    assert_eq!(code(&oargs("../x")), ErrorCode::OutsideWorkspace);
    assert_eq!(code(&oargs("/etc/passwd")), ErrorCode::OutsideWorkspace);
    assert_eq!(code(&oargs("missing.rs")), ErrorCode::NotFound);
    assert_eq!(code(&oargs("notes.txt")), ErrorCode::UnsupportedLanguage);
    assert_eq!(code(&oargs("bin.rs")), ErrorCode::NotUtf8);
    let big = "fn a() {}\n".repeat(10);
    put(&d, "big.rs", &big);
    let mut l = Limits::default();
    l.max_file_bytes = 20;
    let small = ctx_with(&d, l, Mode::ReadOnly);
    assert_eq!(
        ast_outline(&small, &oargs("big.rs")).unwrap_err().code,
        ErrorCode::FileTooLarge
    );
    // inside a directory walk the same file is counted, not fatal
    let out = ast_outline(&small, &oargs(".")).unwrap();
    assert!(out.contains("too large"), "{out}");
}

#[test]
fn hostile_characters_never_reach_the_output_raw() {
    let (d, c) = ws();
    put(&d, "h.py", "def f(x=\"\u{202e}\"):\n    pass\n");
    let out = ast_outline(&c, &oargs("h.py")).unwrap();
    assert!(!out.contains('\u{202e}'), "{out:?}");
    assert!(out.contains("\\u{202e}"), "{out:?}");
    assert!(
        out.ends_with("[escaped: 1 bidi characters in names or paths]\n"),
        "{out:?}"
    );
    // a file NAME with a bidi character: whatever the walk decides, the character is never raw
    put(&d, "sub/a\u{202e}b.rs", "fn n() {}\n");
    let out = ast_outline(&c, &oargs("sub")).unwrap();
    assert!(!out.contains('\u{202e}'), "{out:?}");
}

#[test]
fn output_is_capped_and_says_so() {
    let (d, mut c) = ws();
    for i in 0..120 {
        put(&d, &format!("f{i:03}.rs"), "pub fn f() {}\n");
    }
    c.limits.max_output_bytes = 600;
    let out = ast_outline(&c, &oargs(".")).unwrap();
    assert!(out.len() <= 600, "{} bytes", out.len());
    assert!(out.contains("[truncated:"), "{out}");
}

#[test]
fn the_scan_limit_is_reported() {
    let (d, mut c) = ws();
    for i in 0..30 {
        put(&d, &format!("g{i:02}.rs"), "fn g() {}\n");
    }
    c.limits.max_scan_files = 10;
    let out = ast_outline(&c, &oargs(".")).unwrap();
    assert!(
        out.contains("[walk truncated at 10 files; narrow `path`]\n"),
        "{out}"
    );
}

// ----------------------------------------------------------------- ast_get

#[test]
fn get_one_symbol_exact_with_doc_and_context() {
    let (d, c) = ws();
    put(&d, "src/config.rs", RS);
    let mut a = gargs("Config::load");
    a.path = Some("src/config.rs".into());
    let out = ast_get(&c, &a).unwrap();
    assert_eq!(
        out,
        "src/config.rs:9-12  method Config::load  (rust)\n\
         ```rust\n\
         \x20   /// Load it.\n\
         \x20   pub fn load(path: &str) -> Result<Config, Error> {\n\
         \x20       todo!()\n\
         \x20   }\n\
         ```\n"
    );
    let mut a = gargs("Config.load");
    a.include_doc = Some(false);
    let out = ast_get(&c, &a).unwrap();
    assert!(
        out.starts_with(
            "src/config.rs:10-12  method Config::load  (rust)\n```rust\n    pub fn load("
        ),
        "{out}"
    );
    let mut a = gargs("load");
    a.context_lines = Some(1);
    let out = ast_get(&c, &a).unwrap();
    assert!(out.starts_with("src/config.rs:8-13  "), "{out}");
}

#[test]
fn get_ambiguous_not_found_and_invalid_symbol() {
    let (d, c) = ws();
    put(&d, "a.rs", "fn dup() {}\n");
    put(&d, "b.rs", "fn dup() {}\n");
    let e = ast_get(&c, &gargs("dup")).unwrap_err();
    assert_eq!(e.code, ErrorCode::Ambiguous);
    assert_eq!(
        e.message,
        "2 symbols match `dup`:\n1. a.rs:L1 fn dup\n2. b.rs:L1 fn dup"
    );
    assert!(e.next.contains("path"), "{}", e.next);
    let mut a = gargs("dup");
    a.path = Some("a.rs".into());
    assert!(
        ast_get(&c, &a)
            .unwrap()
            .starts_with("a.rs:1  fn dup  (rust)\n")
    );
    let e = ast_get(&c, &gargs("nope")).unwrap_err();
    assert_eq!(e.code, ErrorCode::NotFound);
    assert_eq!(e.message, "No symbol named `nope` found in 2 files.");
    assert!(e.next.contains("ast_outline"), "{}", e.next);
    for bad in [
        String::new(),
        "x".repeat(300),
        "a\nb".to_string(),
        "a\u{1b}b".to_string(),
    ] {
        assert_eq!(
            ast_get(&c, &gargs(&bad)).unwrap_err().code,
            ErrorCode::InvalidArgs,
            "{bad:?}"
        );
    }
}

#[test]
fn get_fence_grows_when_the_code_contains_backticks() {
    let (d, c) = ws();
    put(&d, "t.rs", "fn t() {\n    let s = \"```\";\n}\n");
    let out = ast_get(&c, &gargs("t")).unwrap();
    assert!(out.contains("\n````rust\n"), "{out}");
    assert!(out.ends_with("\n````\n"), "{out}");
}

#[test]
fn get_never_prints_raw_hostile_characters() {
    let (d, c) = ws();
    put(
        &d,
        "h.py",
        "def f():\n    x = '\u{1b}[31m'  # \u{e0041}\u{e0042}\n",
    );
    let out = ast_get(&c, &gargs("f")).unwrap();
    assert!(
        !out.contains('\u{1b}') && !out.contains('\u{e0041}'),
        "{out:?}"
    );
}
