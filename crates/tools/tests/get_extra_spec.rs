//! Extra cases for ISSUE-TOOLS-GET: the deadline, the candidate list, and the line numbers
//! when the file is not a plain LF ASCII file.
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
use std::time::{Duration, Instant};

fn ctx_with(ws: &tempfile::TempDir, limits: Limits) -> ToolContext {
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
    let c = ctx_with(&d, Limits::default());
    (d, c)
}

fn put(d: &tempfile::TempDir, rel: &str, body: &str) {
    let p = d.path().join(rel);
    fs::create_dir_all(p.parent().unwrap()).unwrap();
    fs::write(p, body).unwrap();
}

fn gargs(symbol: &str) -> GetArgs {
    GetArgs {
        symbol: symbol.to_string(),
        ..Default::default()
    }
}

/// The ambiguity message names the TRUE number of matches and then lists at most twenty, so a
/// repository with hundreds of `new` functions still gets a readable answer instead of a wall.
#[test]
fn a_twenty_first_match_lists_twenty_and_says_one_more() {
    let (d, c) = ws();
    for i in 0..21 {
        put(&d, &format!("m{i:02}.py"), "def dup():\n    pass\n");
    }
    let e = ast_get(&c, &gargs("dup")).unwrap_err();
    assert_eq!(e.code, ErrorCode::Ambiguous);
    let lines: Vec<&str> = e.message.lines().collect();
    assert_eq!(lines[0], "21 symbols match `dup`:");
    assert_eq!(lines.len(), 1 + 20 + 1, "20 candidates then the tail");
    assert_eq!(lines[1], "1. m00.py:L1 fn dup");
    assert_eq!(lines[20], "20. m19.py:L1 fn dup");
    assert_eq!(lines[21], "... and 1 more");
    assert!(
        e.next.contains("`path`"),
        "the caller has to be told how to narrow: {}",
        e.next
    );

    // Narrowing with `path` is the documented way out, and it works.
    let mut a = gargs("dup");
    a.path = Some("m07.py".into());
    assert!(
        ast_get(&c, &a)
            .unwrap()
            .starts_with("m07.py:1-2  fn dup  (python)\n"),
        "{:?}",
        ast_get(&c, &a).unwrap()
    );
}

/// The whole call has a deadline, and it is checked BETWEEN files: one millisecond over a
/// directory of files must come back as `timeout` promptly, not hang and not return a partial
/// answer as if it were complete.
#[test]
fn a_one_millisecond_budget_over_many_files_returns_timeout() {
    let (d, base) = ws();
    for i in 0..400 {
        put(&d, &format!("f{i:03}.py"), "def target():\n    pass\n");
    }
    let mut limits = Limits::default();
    limits.call_timeout_ms = 1;
    let impatient = ctx_with(&d, limits);

    let t = Instant::now();
    let e = ast_get(&impatient, &gargs("target")).unwrap_err();
    let elapsed = t.elapsed();
    assert_eq!(e.code, ErrorCode::Timeout, "{e}");
    assert!(!e.message.contains(d.path().to_str().unwrap()), "{e}");
    assert!(e.next.contains("call_timeout_ms"), "{}", e.next);
    assert!(
        elapsed < Duration::from_secs(10),
        "the deadline must be noticed between files, not after the whole walk: {elapsed:?}"
    );

    // The same directory with the normal budget answers normally when the question has one
    // answer, so the timeout above was the budget and not a broken workspace. (Asking for
    // `target` across all 400 files is ambiguous - which is the point of the next test.)
    assert!(
        ast_get(
            &base,
            &GetArgs {
                path: Some("f000.py".into()),
                ..gargs("target")
            }
        )
        .unwrap()
        .starts_with("f000.py:1-2  fn target  (python)\n")
    );
}

/// A budget of zero is expired before the first file: still `timeout`, never a guess.
#[test]
fn a_zero_budget_expires_before_any_file() {
    let (d, base) = ws();
    put(&d, "a.py", "def a():\n    pass\n");
    let mut limits = Limits::default();
    limits.call_timeout_ms = 0;
    let impatient = ctx_with(&d, limits);
    assert_eq!(
        ast_get(&impatient, &gargs("a")).unwrap_err().code,
        ErrorCode::Timeout
    );
    assert!(ast_get(&base, &gargs("a")).is_ok());
}

/// CRLF line endings and a UTF-8 BOM change the bytes, not the lines: the header still points
/// at the lines a person would count.
#[test]
fn line_numbers_survive_crlf_and_a_bom() {
    let (d, c) = ws();
    put(
        &d,
        "crlf.py",
        "\u{feff}def first():\r\n    return 1\r\n\r\ndef second():\r\n    return 2\r\n",
    );
    assert_eq!(
        ast_get(&c, &gargs("second")).unwrap(),
        // The lines are re-joined with `\n`: the header points at the right line numbers, and
        // `symbol_text` is documented to return the text of whole lines joined that way.
        "crlf.py:4-5  fn second  (python)\n```python\ndef second():\n    return 2\n```\n"
    );
    // Same answer through ast_outline, so the two tools cannot disagree about where a line is.
    let out = ast_outline(
        &c,
        &OutlineArgs {
            path: "crlf.py".into(),
            ..Default::default()
        },
    )
    .unwrap();
    assert!(out.contains("fn second L4-5"), "{out}");
    assert!(out.contains("crlf.py  python  5 lines"), "{out}");
}

/// A decorated Python definition starts at its decorator: the decorator belongs to the symbol,
/// and a caller who asks for it gets the whole thing.
#[test]
fn a_python_decorator_line_is_part_of_the_symbol() {
    let (d, c) = ws();
    put(
        &d,
        "deco.py",
        "import functools\n\n@functools.cache\ndef slow():\n    return 1\n",
    );
    let out = ast_get(&c, &gargs("slow")).unwrap();
    assert!(
        out.starts_with(
            "deco.py:3-5  fn slow  (python)\n```python\n@functools.cache\ndef slow():\n"
        ),
        "{out}"
    );
    // And the outline agrees about the first line.
    let outline = ast_outline(
        &c,
        &OutlineArgs {
            path: "deco.py".into(),
            ..Default::default()
        },
    )
    .unwrap();
    assert!(outline.contains("fn slow L3-5"), "{outline}");
}

/// The file count in `not_found` is the number of files actually parsed, so "no such symbol" is
/// never confused with "nothing was searched". A file with no grammar was not parsed.
#[test]
fn not_found_counts_only_the_files_it_parsed() {
    let (d, c) = ws();
    put(&d, "a.py", "def a():\n    pass\n");
    put(&d, "b.py", "def b():\n    pass\n");
    put(&d, "notes.txt", "hello\n");
    put(&d, "data.bin", "");
    fs::write(d.path().join("bin.py"), [0xff, 0xfe]).unwrap();

    let e = ast_get(&c, &gargs("nope")).unwrap_err();
    assert_eq!(e.code, ErrorCode::NotFound);
    assert_eq!(
        e.message, "No symbol named `nope` found in 2 files.",
        "the .txt and the non-UTF-8 file were never parsed"
    );
    assert!(e.next.contains("ast_outline"), "{}", e.next);
}

/// Argument validation, including the exact boundaries of both ranges.
#[test]
fn arguments_are_validated_at_their_boundaries() {
    let (d, c) = ws();
    put(&d, "a.py", "def a():\n    pass\n");
    let code = |a: &GetArgs| ast_get(&c, a).unwrap_err().code;

    // 1..=256 bytes. A 256-byte name is accepted (and then simply not found); 257 is not.
    assert_eq!(code(&gargs(&"x".repeat(256))), ErrorCode::NotFound);
    assert_eq!(code(&gargs(&"x".repeat(257))), ErrorCode::InvalidArgs);
    assert_eq!(code(&gargs("")), ErrorCode::InvalidArgs);
    // Multi-byte characters count as BYTES, so 200 of them is over the limit.
    assert_eq!(
        code(&gargs(&"\u{4e2d}".repeat(200))),
        ErrorCode::InvalidArgs
    );

    // 0..=20 context lines.
    for lines in [0u64, 1, 20] {
        let mut a = gargs("a");
        a.context_lines = Some(lines);
        assert!(ast_get(&c, &a).is_ok(), "context_lines {lines}");
    }
    assert_eq!(
        code(&GetArgs {
            context_lines: Some(21),
            ..gargs("a")
        }),
        ErrorCode::InvalidArgs
    );

    // Control characters in the symbol, at both ends and in the middle.
    for bad in ["\u{1b}", "a\u{7}b", "a\nb", "\u{0}"] {
        assert_eq!(code(&gargs(bad)), ErrorCode::InvalidArgs, "{bad:?}");
    }
    // Path errors are the boundary's, unchanged.
    assert_eq!(
        code(&GetArgs {
            path: Some("missing.py".into()),
            ..gargs("a")
        }),
        ErrorCode::NotFound
    );
    assert_eq!(
        code(&GetArgs {
            path: Some("../outside".into()),
            ..gargs("a")
        }),
        ErrorCode::OutsideWorkspace
    );
}

/// A qualified name may be spelled with either separator, in any language; a bare name matches
/// at any depth. This is the difference between "not found" and "found three times".
#[test]
fn qualified_names_accept_either_separator() {
    let (d, c) = ws();
    put(
        &d,
        "q.py",
        "class Config:\n    def load(self):\n        return 1\n",
    );
    put(
        &d,
        "m.go",
        "package main\n\ntype Config struct{ A int }\n\nfunc Helper() {}\n",
    );
    for query in ["Config.load", "Config::load"] {
        let out = ast_get(&c, &gargs(query)).unwrap();
        assert!(
            out.starts_with("q.py:2-3  method Config.load  (python)\n"),
            "{query}: {out}"
        );
    }
    // Across the whole workspace the query still has exactly one answer: the Go `struct` is
    // named `Config`, not `Config.load`, and a qualified name is matched exactly. A guesser
    // that matched on the bare name here would have offered two files.
    let out = ast_get(&c, &gargs("Config.load")).unwrap();
    assert!(
        out.starts_with("q.py:2-3  method Config.load  (python)\n"),
        "{out}"
    );
    // A second file with the same qualified name IS ambiguous, and both are listed.
    put(
        &d,
        "q2.py",
        "class Config:\n    def load(self):\n        return 2\n",
    );
    let e = ast_get(&c, &gargs("Config::load")).unwrap_err();
    assert_eq!(e.code, ErrorCode::Ambiguous, "{e}");
    assert!(e.message.contains("1. q.py:L2 method Config.load"), "{e}");
    assert!(e.message.contains("2. q2.py:L2 method Config.load"), "{e}");
    // A bare name matches at any depth.
    assert!(
        ast_get(
            &c,
            &GetArgs {
                path: Some("m.go".into()),
                ..gargs("Helper")
            }
        )
        .unwrap()
        .starts_with("m.go:5  fn Helper  (go)\n")
    );
}

/// Anything that had to be escaped to print is counted, including in the header: a file whose
/// NAME carries a bidi character must not put that character on the screen.
#[test]
fn a_hostile_file_name_is_escaped_in_the_header_and_counted() {
    let (d, c) = ws();
    put(&d, "a\u{202e}b.py", "def f():\n    return 1\n");
    let out = ast_get(&c, &gargs("f")).unwrap();
    assert!(!out.contains('\u{202e}'), "{out:?}");
    assert!(out.contains("a\\u{202e}b.py:1-2"), "{out:?}");
    assert!(
        out.ends_with("[escaped: 1 bidi characters in names or paths]\n"),
        "{out:?}"
    );
}

/// `ast_info` reports the languages this build actually has, and the size formatting never
/// rounds: a limit that is not a whole KiB or MiB is printed exactly.
#[test]
fn info_prints_exact_sizes_and_only_available_languages() {
    let d = tempfile::tempdir().unwrap();
    let mut limits = Limits::default();
    limits.max_file_bytes = 1536; // 1.5 KiB: not a multiple of either unit
    limits.max_output_bytes = 1024 * 1024 * 3;
    limits.max_results = 7;
    let c = ctx_with(&d, limits);
    let s = ast_info(&c);
    assert!(
        s.contains("limits: file 1536 B · output 3 MiB · results 7 · plan 50 files / 500 edits"),
        "{s}"
    );
    assert_eq!(s.lines().count(), 6, "{s}");
    // Every listed language is one this build can actually parse.
    let listed = s
        .lines()
        .nth(2)
        .unwrap()
        .strip_prefix("languages: ")
        .unwrap();
    assert!(!listed.is_empty(), "{s}");
    for entry in listed.split(", ") {
        let id = entry.split(" (").next().unwrap();
        assert!(
            opencrayast_lang::Language::from_id(id).is_some_and(|l| l.is_available()),
            "{entry} is listed but not available"
        );
    }
}
