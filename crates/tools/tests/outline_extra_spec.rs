//! Extra cases for ISSUE-TOOLS-OUTLINE, in Python and Go.
//!
//! The golden cases in `read_tools_spec.rs` are Rust, because Rust is the reference language;
//! these run on the languages whose outline collectors are already merged, so the tools layer
//! itself can be verified without waiting for the Rust collector. The format is the one in the
//! `ast_outline` doc comment, character for character.
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
use opencrayast_tools::{Mode, OutlineArgs, ToolContext, ast_outline};
use std::fs;
use std::os::unix::fs::symlink;
use std::time::Instant;

const PY: &str = include_str!("../../query/tests/fixtures/sample.py");
const GO: &str = include_str!("../../query/tests/fixtures/sample.go");

fn ctx(ws: &tempfile::TempDir, limits: Limits) -> ToolContext {
    ToolContext {
        boundary: Boundary::new(BoundaryConfig {
            root: ws.path().to_path_buf(),
            limits: Limits::default(),
            read_roots: Vec::new(),
            state_dir: None,
            extra_protected: Vec::new(),
        })
        .unwrap(),
        limits,
        mode: Mode::ReadOnly,
        write: None,
        version: "0.20261002.1".to_string(),
        workspace_id: "w-00112233445566778899aabbccddeeff".to_string(),
        respect_gitignore: true,
        extra_ignore: vec![],
        config_source: Default::default(),
    }
}

fn ws() -> (tempfile::TempDir, ToolContext) {
    let d = tempfile::tempdir().unwrap();
    let c = ctx(&d, Limits::default());
    (d, c)
}

fn put(d: &tempfile::TempDir, rel: &str, body: &str) {
    let p = d.path().join(rel);
    fs::create_dir_all(p.parent().unwrap()).unwrap();
    fs::write(p, body).unwrap();
}

fn args(path: &str) -> OutlineArgs {
    OutlineArgs {
        path: path.to_string(),
        ..Default::default()
    }
}

/// The exact output for one file, docs included: header, two-space-per-depth indent, the doc
/// line under its symbol, and the singular noun for one syntax error.
#[test]
fn a_python_file_is_outlined_exactly() {
    let (d, c) = ws();
    put(&d, "s.py", PY);
    let out = ast_outline(
        &c,
        &OutlineArgs {
            include_docs: Some(true),
            ..args("s.py")
        },
    )
    .unwrap();
    assert_eq!(
        out,
        "s.py  python  24 lines\n\
         \x20 const MAX L3  MAX = 10\n\
         \x20 fn add L6-8  def add(a, b)\n\
         \x20   /// Adds.\n\
         \x20 class Config L11-19  class Config\n\
         \x20   /// A config.\n\
         \x20   method load L14-15  def load(self, path)\n\
         \x20   method make L17-19  def make()\n\
         \x20 fn decorated L22-24  def decorated()\n"
    );
}

/// The same for Go, whose `func (c *Config) Load` receiver must survive into the signature and
/// whose interface methods are `method`, not `fn`.
#[test]
fn a_go_file_is_outlined_exactly() {
    let (d, c) = ws();
    put(&d, "m.go", GO);
    assert_eq!(
        ast_outline(&c, &args("m.go")).unwrap(),
        "m.go  go  19 lines\n\
         \x20 struct Config L4-6  type Config struct\n\
         \x20 method Load L9-11  func (c *Config) Load(path string) error\n\
         \x20 interface Shape L13-15  type Shape interface\n\
         \x20   method Area L14  Area() float64\n\
         \x20 const Max L17  const Max = 10\n\
         \x20 fn Main L19  func Main()\n"
    );
}

/// Depth, kinds and limit, each with its exact line count. A kind filter keeps a symbol's own
/// depth rather than re-indenting it: `method` is depth 2 wherever it is, so it stays indented.
#[test]
fn depth_kinds_and_limit_are_exact() {
    let (d, c) = ws();
    put(&d, "s.py", PY);
    for (depth, want) in [(1u64, 4usize), (2, 6), (3, 6)] {
        let out = ast_outline(
            &c,
            &OutlineArgs {
                depth: Some(depth),
                ..args("s.py")
            },
        )
        .unwrap();
        assert_eq!(out.lines().count(), 1 + want, "depth {depth}: {out}");
    }
    let out = ast_outline(
        &c,
        &OutlineArgs {
            kinds: Some(vec!["method".into()]),
            ..args("s.py")
        },
    )
    .unwrap();
    assert_eq!(out.lines().count(), 1 + 2, "{out}");
    // A filter that matches nothing is not a truncation: there is nothing to show and nothing
    // to explain.
    let out = ast_outline(
        &c,
        &OutlineArgs {
            kinds: Some(vec!["macro".into()]),
            ..args("s.py")
        },
    )
    .unwrap();
    assert_eq!(out, "s.py  python  24 lines\n", "{out}");

    let out = ast_outline(
        &c,
        &OutlineArgs {
            limit: Some(2),
            ..args("s.py")
        },
    )
    .unwrap();
    assert_eq!(
        out,
        "s.py  python  24 lines\n\
         \x20 const MAX L3  MAX = 10\n\
         \x20 fn add L6-8  def add(a, b)\n\
         [truncated: showing 2 of 6 symbols in 1 files; narrow `path`, lower `depth` or filter \
         `kinds`]\n"
    );
}

/// A directory: files in `rel` byte order, and every skipped thing counted in the documented
/// order - ignored, links, special, unsupported language, too large, not utf-8, unreadable.
#[test]
fn a_directory_sorts_files_and_counts_everything_it_skipped() {
    let (d, _c) = ws();
    put(&d, "b.py", "def b():\n    pass\n");
    put(&d, "a.go", "package main\n\nfunc main() {}\n");
    put(&d, "notes.txt", "hello\n");
    put(&d, ".gitignore", "ignored.py\n");
    put(&d, "ignored.py", "def i():\n    pass\n");
    put(&d, ".git/config", "x\n");
    put(&d, "bin.py", "");
    fs::write(d.path().join("bin.py"), [0xff, 0xfe]).unwrap();
    fs::write(
        d.path().join("big.py"),
        "def big():\n    pass\n".repeat(20).as_str(),
    )
    .unwrap();
    let out_dir = tempfile::tempdir().unwrap();
    fs::write(
        out_dir.path().join("secret.py"),
        "def secret():\n    pass\n",
    )
    .unwrap();
    symlink(out_dir.path().join("secret.py"), d.path().join("link.py")).unwrap();
    let fifo = d.path().join("pipe");
    let status = std::process::Command::new("mkfifo")
        .arg(&fifo)
        .status()
        .unwrap();
    assert!(status.success());

    let mut limits = Limits::default();
    limits.max_file_bytes = 40;
    let small = ctx(&d, limits);
    let out = ast_outline(&small, &args(".")).unwrap();
    assert_eq!(
        out,
        "a.go  go  3 lines\n\
         \x20 fn main L3  func main()\n\
         b.py  python  2 lines\n\
         \x20 fn b L1-2  def b()\n\
         [skipped: 2 ignored, 1 links, 1 special, 2 unsupported language, 1 too large, 1 not utf-8]\n"
    );
    assert!(!out.contains("secret"), "{out}");
    // Identical for identical input.
    assert_eq!(ast_outline(&small, &args(".")).unwrap(), out);
}

/// A link inside the workspace pointing at another file inside the workspace is still only a
/// link: it is never followed, so its target is outlined once, under its real name.
#[test]
fn a_symlink_inside_the_workspace_is_counted_not_followed() {
    let (d, c) = ws();
    put(&d, "real.py", "def real():\n    pass\n");
    put(&d, "realdir/x.py", "def x():\n    pass\n");
    symlink(d.path().join("real.py"), d.path().join("alias.py")).unwrap();
    symlink(d.path().join("realdir"), d.path().join("dirlink")).unwrap();
    assert_eq!(
        ast_outline(&c, &args(".")).unwrap(),
        "real.py  python  2 lines\n\
         \x20 fn real L1-2  def real()\n\
         realdir/x.py  python  2 lines\n\
         \x20 fn x L1-2  def x()\n\
         [skipped: 2 links]\n"
    );
}

/// A file with syntax errors is still outlined: the symbols that parsed are real, and the
/// header says how much to trust them. Exactly one error is singular.
#[test]
fn a_file_with_syntax_errors_says_so_in_its_header() {
    let (d, c) = ws();
    put(&d, "bad.py", "def good():\n    pass\ndef broken(\n");
    let out = ast_outline(&c, &args("bad.py")).unwrap();
    assert_eq!(
        out,
        "bad.py  python  3 lines (1 syntax error)\n\
         \x20 fn good L1-2  def good()\n"
    );
}

/// A UTF-8 BOM and CRLF line endings change neither the line count nor the line numbers: the
/// text is decoded as it is, and the collector works on the parse tree.
#[test]
fn a_bom_and_crlf_file_is_outlined_by_its_lines() {
    let (d, c) = ws();
    put(
        &d,
        "crlf.py",
        "\u{feff}def f():\r\n    return 1\r\n\r\ndef g():\r\n    return 2\r\n",
    );
    assert_eq!(
        ast_outline(&c, &args("crlf.py")).unwrap(),
        "crlf.py  python  5 lines\n\
         \x20 fn f L1-2  def f()\n\
         \x20 fn g L4-5  def g()\n"
    );
}

/// Deep nesting: the reported path is the whole workspace-relative path, and the walk stops at
/// the boundary's depth ceiling instead of recursing until the stack runs out.
#[test]
fn deeply_nested_directories_are_walked_to_the_depth_ceiling() {
    let (d, c) = ws();
    let deep: String = (0..30).map(|i| format!("d{i}/")).collect();
    put(&d, &format!("{deep}x.py"), "def deep():\n    pass\n");
    put(&d, "top.py", "def top():\n    pass\n");
    let out = ast_outline(&c, &args(".")).unwrap();
    assert_eq!(
        out,
        format!(
            "{deep}x.py  python  2 lines\n\
             \x20 fn deep L1-2  def deep()\n\
             top.py  python  2 lines\n\
             \x20 fn top L1-2  def top()\n"
        )
    );
}

/// The byte cap is a hard limit and the output says it was hit. 600 bytes cannot hold 120
/// files, so the truncation line has to fit inside what is left.
#[test]
fn the_output_is_capped_in_bytes_and_says_so() {
    let (d, mut c) = ws();
    for i in 0..120 {
        put(&d, &format!("f{i:03}.py"), "def f():\n    pass\n");
    }
    c.limits.max_output_bytes = 600;
    let out = ast_outline(&c, &args(".")).unwrap();
    assert!(out.len() <= 600, "{} bytes:\n{out}", out.len());
    assert!(
        out.contains("[truncated: showing 10 of 120 symbols"),
        "{out}"
    );
    assert_eq!(
        out.lines().last().unwrap(),
        "[truncated: showing 10 of 120 symbols in 10 files; narrow `path`, lower `depth` or filter `kinds`]"
    );

    // The escape counts describe what survived the cap, not what was built and thrown away.
    c.limits.max_output_bytes = 64 * 1024;
    let out = ast_outline(&c, &args(".")).unwrap();
    assert!(!out.contains("[truncated:"), "{out}");
}

/// Ten thousand files: bounded in time and in memory, and the scan limit is reported rather
/// than silently obeyed. The default `max_scan_files` is 5000, so this also proves the walk
/// limit is what stops it.
#[test]
fn ten_thousand_files_stay_fast_and_report_the_scan_limit() {
    let (d, mut c) = ws();
    for i in 0..10_000 {
        let dir = d.path().join(format!("d{}", i % 50));
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join(format!("f{i:05}.py")), "def f():\n    pass\n").unwrap();
    }
    let t = Instant::now();
    let out = ast_outline(&c, &args(".")).unwrap();
    let elapsed = t.elapsed();
    assert!(
        out.len() <= c.limits.max_output_bytes as usize,
        "{} bytes",
        out.len()
    );
    assert!(
        out.contains("[walk truncated at 5000 files; narrow `path`]"),
        "{out}"
    );
    assert!(elapsed.as_secs() < 5, "10k files took {elapsed:?}");

    // Raising the scan limit walks all of them; the symbol limit is what caps the output.
    c.limits.max_scan_files = 50_000;
    let out = ast_outline(&c, &args(".")).unwrap();
    assert!(!out.contains("[walk truncated"), "{out}");
    assert!(out.contains("of 10000 symbols"), "{out}");
    assert!(
        out.len() <= c.limits.max_output_bytes as usize,
        "{}",
        out.len()
    );
}

/// A directory where nothing can be outlined returns the footer lines and nothing else: an
/// empty body is not an error, and the counts are what explain it.
#[test]
fn a_directory_with_nothing_outlinable_returns_only_the_footer() {
    let (d, c) = ws();
    put(&d, "notes.txt", "hello\n");
    assert_eq!(
        ast_outline(&c, &args(".")).unwrap(),
        "[skipped: 1 unsupported language]\n"
    );
}

/// Every hostile character class is escaped on the way out, counted in the footer, and never
/// present raw - in a signature, in a doc line or in a file name.
#[test]
fn hostile_characters_are_escaped_and_counted() {
    let (d, c) = ws();
    put(
        &d,
        "h.py",
        "def f(x=\"\u{202e}\", y=\"\u{1b}\"):\n    \"\"\"Doc \u{200b}here.\"\"\"\n    pass\n",
    );
    put(&d, "sub/a\u{202e}b.py", "def n():\n    pass\n");
    let out = ast_outline(
        &c,
        &OutlineArgs {
            path: "h.py".into(),
            include_docs: Some(true),
            ..Default::default()
        },
    )
    .unwrap();
    assert!(
        !out.contains('\u{202e}') && !out.contains('\u{1b}'),
        "{out:?}"
    );
    assert!(
        out.contains("def f(x=\"\\u{202e}\", y=\"\\u{1b}\")"),
        "{out:?}"
    );
    assert_eq!(
        out,
        "h.py  python  3 lines\n\
         \x20 fn f L1-3  def f(x=\"\\u{202e}\", y=\"\\u{1b}\")\n\
         \x20   /// Doc \\u{200b}here.\n\
         [escaped: 1 control, 1 bidi, 1 invisible characters in names or paths]\n"
    );

    // A file NAME with a bidi character: whatever the walk decides, it is never raw.
    let out = ast_outline(&c, &args("sub")).unwrap();
    assert!(!out.contains('\u{202e}'), "{out:?}");
    assert!(out.contains("\\u{202e}"), "{out:?}");
    assert!(
        out.ends_with("[escaped: 1 bidi characters in names or paths]\n"),
        "{out:?}"
    );
}

/// Arguments are refused before anything is read, and the refusals are the documented ones.
#[test]
fn argument_errors_are_invalid_args() {
    let (d, c) = ws();
    put(&d, "a.py", "def a():\n    pass\n");
    let code = |a: &OutlineArgs| ast_outline(&c, a).unwrap_err().code;
    assert_eq!(code(&args("")), ErrorCode::InvalidArgs);
    for depth in [0u64, 7, 100] {
        assert_eq!(
            code(&OutlineArgs {
                depth: Some(depth),
                ..args("a.py")
            }),
            ErrorCode::InvalidArgs,
            "depth {depth}"
        );
    }
    for limit in [0u64, 201, 100_000] {
        assert_eq!(
            code(&OutlineArgs {
                limit: Some(limit),
                ..args("a.py")
            }),
            ErrorCode::InvalidArgs,
            "limit {limit}"
        );
    }
    for kinds in [
        vec!["function".into()],
        vec!["fn".into(), "nope".into()],
        vec![String::new()],
    ] {
        assert_eq!(
            code(&OutlineArgs {
                kinds: Some(kinds.clone()),
                ..args("a.py")
            }),
            ErrorCode::InvalidArgs,
            "{kinds:?}"
        );
    }
    // Path refusals are the boundary's, unchanged.
    assert_eq!(code(&args("../x")), ErrorCode::OutsideWorkspace);
    assert_eq!(code(&args("/etc/passwd")), ErrorCode::OutsideWorkspace);
    assert_eq!(code(&args("missing.py")), ErrorCode::NotFound);
}

/// A single file that cannot be outlined is an ERROR; the same file inside a directory is a
/// count. This is the whole difference between the two paths.
#[test]
fn a_single_bad_file_is_an_error_and_in_a_walk_it_is_a_count() {
    let (d, c) = ws();
    put(&d, "ok.py", "def ok():\n    pass\n");
    put(&d, "notes.txt", "hello\n");
    fs::write(d.path().join("bin.py"), [0xff, 0xfe]).unwrap();
    let big = "def big():\n    pass\n".repeat(20);
    put(&d, "big.py", &big);

    assert_eq!(
        ast_outline(&c, &args("notes.txt")).unwrap_err().code,
        ErrorCode::UnsupportedLanguage
    );
    assert_eq!(
        ast_outline(&c, &args("bin.py")).unwrap_err().code,
        ErrorCode::NotUtf8
    );
    // Not UTF-8 is decided before any grammar is needed, so the file name's language is
    // irrelevant: this is the decode step refusing, not a missing grammar.
    fs::write(d.path().join("bin.rs"), [0xff, 0xfe]).unwrap();
    assert_eq!(
        ast_outline(&c, &args("bin.rs")).unwrap_err().code,
        ErrorCode::NotUtf8
    );

    let mut limits = Limits::default();
    limits.max_file_bytes = 40;
    let small = ctx(&d, limits);
    assert_eq!(
        ast_outline(&small, &args("big.py")).unwrap_err().code,
        ErrorCode::FileTooLarge
    );

    // In a walk the same three files are counted, and the good one is still outlined.
    let out = ast_outline(&small, &args(".")).unwrap();
    assert_eq!(
        out,
        "ok.py  python  2 lines\n\
         \x20 fn ok L1-2  def ok()\n\
         [skipped: 1 unsupported language, 1 too large, 2 not utf-8]\n"
    );
}

/// `respect_gitignore` and `extra_ignore` come from the context, not from the arguments: a
/// caller cannot widen what the operator configured.
#[test]
fn ignore_settings_come_from_the_context() {
    let (d, _base) = ws();
    put(&d, ".gitignore", "hidden.py\n");
    put(&d, "hidden.py", "def hidden():\n    pass\n");
    put(&d, "vendor/v.py", "def v():\n    pass\n");
    put(&d, "shown.py", "def shown():\n    pass\n");

    let off = ToolContext {
        respect_gitignore: false,
        ..ctx(&d, Limits::default())
    };
    assert!(ast_outline(&off, &args(".")).unwrap().contains("hidden.py"));

    let filtered = ToolContext {
        extra_ignore: vec!["vendor/".into()],
        ..ctx(&d, Limits::default())
    };
    let out = ast_outline(&filtered, &args(".")).unwrap();
    assert!(!out.contains("vendor/v.py"), "{out}");
    assert!(out.contains("shown.py"), "{out}");
    // Two ignored entries: the one from `.gitignore` and the whole `vendor/` directory.
    assert!(out.contains("2 ignored"), "{out}");
}
