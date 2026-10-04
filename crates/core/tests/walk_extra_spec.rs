//! Extra cases for ISSUE-CORE-IGNORE: the walker's scale, its refusals, and the places where
//! "relative" has exactly one meaning. Spec cases live in `walk_spec.rs` / `ignore_spec.rs`;
//! these add to them and never weaken them.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
#![cfg(unix)]
use opencrayast_core::ErrorCode;
use opencrayast_core::boundary::{Boundary, BoundaryConfig};
use opencrayast_core::limits::Limits;
use opencrayast_core::walk::*;
use std::ffi::OsStr;
use std::fs;
use std::os::unix::ffi::OsStrExt;
use std::time::Instant;

fn mk() -> (tempfile::TempDir, Boundary) {
    let ws = tempfile::tempdir().unwrap();
    let b = Boundary::new(BoundaryConfig {
        root: ws.path().to_path_buf(),
        limits: Limits::default(),
        read_roots: Vec::new(),
        state_dir: None,
        extra_protected: Vec::new(),
    })
    .unwrap();
    (ws, b)
}

fn put(ws: &tempfile::TempDir, rel: &str, body: &str) {
    let p = ws.path().join(rel);
    fs::create_dir_all(p.parent().unwrap()).unwrap();
    fs::write(p, body).unwrap();
}

/// The name of the errno that means "this filesystem will not store that name", if the error
/// is that. `EILSEQ` is what APFS and HFS+ answer for a name that is not valid UTF-8; `EINVAL`
/// is the other answer the same refusal produces on some kernels. Any other error is returned
/// to the caller, so a genuine bug cannot hide behind a skip.
fn name_too_exotic(errno: &std::io::Error) -> Option<&'static str> {
    // `std` reports the C errno as `Option<i32>`, `rustix` as a bare `i32`; wrap the latter
    // rather than unwrap the former, so no errno can panic here.
    let raw = errno.raw_os_error();
    if raw == Some(rustix::io::Errno::ILSEQ.raw_os_error()) {
        Some("EILSEQ")
    } else if raw == Some(rustix::io::Errno::INVAL.raw_os_error()) {
        Some("EINVAL")
    } else {
        None
    }
}

fn rels(r: &WalkResult) -> Vec<String> {
    r.files.iter().map(|f| f.rel.clone()).collect()
}

/// The entry ceiling is a caller-supplied limit, and over it the answer is a refusal, never a
/// shorter list: 100 entries with a limit of 100 is fine, 101 is `limit_exceeded`.
#[test]
fn a_read_dir_limited_refuses_past_the_limit_instead_of_truncating() {
    let (ws, b) = mk();
    for i in 0..101 {
        put(&ws, &format!("f{i:03}.txt"), "");
    }
    let dir = b.resolve_read(".").unwrap();

    let err = b
        .read_dir_limited(&dir, 100)
        .expect_err("101 entries must not be listed as 100");
    assert_eq!(err.code, ErrorCode::LimitExceeded, "{err}");
    assert!(!err.message.contains(ws.path().to_str().unwrap()), "{err}");

    // Exactly at the limit is fine, and a larger limit is fine too.
    assert_eq!(b.read_dir_limited(&dir, 101).unwrap().len(), 101);
    assert_eq!(b.read_dir_limited(&dir, 5_000).unwrap().len(), 101);
    assert_eq!(
        b.read_dir(&dir).unwrap().len(),
        101,
        "read_dir is the 200,000 case"
    );

    // A limit of zero accepts an empty directory and refuses a non-empty one.
    fs::create_dir(ws.path().join("empty")).unwrap();
    let empty = b.resolve_read("empty").unwrap();
    assert!(b.read_dir_limited(&empty, 0).unwrap().is_empty());
    assert_eq!(
        b.read_dir_limited(&dir, 0).unwrap_err().code,
        ErrorCode::LimitExceeded
    );
}

/// A 100,000-line ignore file is a real thing to find in a monorepo, so parsing it and
/// walking with it must be ordinary work, not a special case. The plain names go into a hash
/// index at parse time, which is what keeps this linear instead of a per-path scan of every
/// rule.
#[test]
fn b_a_hundred_thousand_line_ignore_file_is_usable() {
    let (ws, b) = mk();
    let mut text = String::with_capacity(1024 * 1024);
    for i in 0..100_000 {
        text.push_str(&format!("v{i}\n"));
    }
    text.push_str("*.log\n");
    text.push_str("!keep.log\n");
    text.push_str("/only_here.txt\n");
    // Short names on purpose: 100k LONG names would be over the 1 MiB ignore-file ceiling,
    // and then this test would be measuring the ceiling instead of the rule set.
    assert!(
        text.len() as u64 <= 1024 * 1024,
        "the ignore file must stay under the ceiling, got {}",
        text.len()
    );
    put(&ws, ".gitignore", &text);
    for i in 0..50 {
        put(&ws, &format!("src/mod{i}.rs"), "");
    }
    put(&ws, "v99999", "");
    put(&ws, "v0", "");
    put(&ws, "keep.log", "");
    put(&ws, "only_here.txt", "");
    put(&ws, "nested/only_here.txt", "");

    let start = b.resolve_read(".").unwrap();
    let t = Instant::now();
    let r = walk(&b, &start, &WalkOptions::default()).unwrap();
    let elapsed = t.elapsed();

    assert_eq!(
        rels(&r),
        [
            ".gitignore",
            "keep.log",
            "nested/only_here.txt",
            "src/mod0.rs",
            "src/mod1.rs",
            "src/mod10.rs",
            "src/mod11.rs",
            "src/mod12.rs",
            "src/mod13.rs",
            "src/mod14.rs",
            "src/mod15.rs",
            "src/mod16.rs",
            "src/mod17.rs",
            "src/mod18.rs",
            "src/mod19.rs",
            "src/mod2.rs",
            "src/mod20.rs",
            "src/mod21.rs",
            "src/mod22.rs",
            "src/mod23.rs",
            "src/mod24.rs",
            "src/mod25.rs",
            "src/mod26.rs",
            "src/mod27.rs",
            "src/mod28.rs",
            "src/mod29.rs",
            "src/mod3.rs",
            "src/mod30.rs",
            "src/mod31.rs",
            "src/mod32.rs",
            "src/mod33.rs",
            "src/mod34.rs",
            "src/mod35.rs",
            "src/mod36.rs",
            "src/mod37.rs",
            "src/mod38.rs",
            "src/mod39.rs",
            "src/mod4.rs",
            "src/mod40.rs",
            "src/mod41.rs",
            "src/mod42.rs",
            "src/mod43.rs",
            "src/mod44.rs",
            "src/mod45.rs",
            "src/mod46.rs",
            "src/mod47.rs",
            "src/mod48.rs",
            "src/mod49.rs",
            "src/mod5.rs",
            "src/mod6.rs",
            "src/mod7.rs",
            "src/mod8.rs",
            "src/mod9.rs"
        ]
    );
    assert_eq!(
        r.skipped_ignored, 3,
        "the two ignored names and the anchored one; the negation keeps keep.log"
    );
    assert!(
        elapsed.as_secs() < 20,
        "100k rules must be usable, took {elapsed:?}"
    );

    // And the rules themselves answer in the same time, without the walk.
    let rules = IgnoreRules::parse(&text);
    let t = Instant::now();
    for i in 0..2_000 {
        assert_eq!(rules.matches(&format!("deep/dir/v{i}"), false), Some(true));
        assert_eq!(rules.matches(&format!("deep/dir/v{i}.rs"), false), None);
    }
    assert!(
        t.elapsed().as_secs() < 20,
        "matching against 100k rules must not be quadratic, took {:?}",
        t.elapsed()
    );
}

/// 200 levels is deeper than the depth ceiling (64), so the walk has to stop on purpose
/// rather than by running out of stack, and has to say that it stopped. Nothing below the
/// ceiling is reported as a file, because the boundary could not have opened those paths
/// either.
#[test]
fn c_a_two_hundred_level_tree_terminates_at_the_depth_ceiling() {
    let (ws, b) = mk();
    let deep: String = (1..=200).map(|i| format!("d{i}/")).collect();
    put(&ws, &format!("{deep}leaf.rs"), "");
    put(&ws, "shallow.rs", "");
    put(&ws, "top.rs", "");

    let start = b.resolve_read(".").unwrap();
    let r = walk(&b, &start, &WalkOptions::default()).unwrap();
    assert_eq!(rels(&r), ["shallow.rs", "top.rs"]);
    assert!(
        r.skipped_ignored >= 1,
        "the deep chain was counted, not hidden"
    );
    assert!(!r.truncated);

    // The boundary itself: a file whose workspace-relative path is exactly
    // `path_max_depth` components long is still found, one deeper is not. That is the same
    // ceiling `resolve_read` enforces, so the walk and the boundary agree on how deep a path
    // is allowed to be.
    let max_depth = opencrayast_core::limits::Limits::default().path_max_depth;
    let at: String = (1..=max_depth - 1).map(|i| format!("d{i}/")).collect();
    put(&ws, &format!("{at}edge.rs"), "");
    let past: String = (1..=max_depth).map(|i| format!("d{i}/")).collect();
    put(&ws, &format!("{past}too_deep.rs"), "");
    let r = walk(&b, &b.resolve_read(".").unwrap(), &WalkOptions::default()).unwrap();
    assert!(
        rels(&r).contains(&format!("{at}edge.rs")),
        "a path of exactly {max_depth} components must be found: {:?}",
        rels(&r)
    );
    assert!(
        !rels(&r).iter().any(|f| f.ends_with("too_deep.rs")),
        "a path of {max_depth} + 1 components must not be"
    );

    // The ceiling is counted in WORKSPACE-RELATIVE components, so a walk that starts 63
    // levels down can still reach files at exactly 64 components and nothing deeper -
    // measured from the start instead, it would return paths `resolve_read` refuses.
    let mid: String = (1..=63).map(|i| format!("d{i}/")).collect();
    let inside = b.resolve_read(mid.trim_end_matches('/')).unwrap();
    assert_eq!(inside.rel.split('/').count(), 63);
    let r = walk(&b, &inside, &WalkOptions::default()).unwrap();
    assert_eq!(
        rels(&r),
        [format!("{mid}edge.rs")],
        "only the path that is exactly at the ceiling"
    );
    assert!(r.skipped_ignored >= 1, "the deeper chain was counted");
}

/// Names that are awkward in a shell, in JSON and in a regex all have to survive the walk
/// unchanged and still sort by bytes.
#[test]
fn d_spaces_and_unicode_names_are_walked_and_sorted_by_bytes() {
    let (ws, b) = mk();
    // Written the way every filesystem stores them, on purpose:
    // - "combining e\u{301}.rs" is ALREADY in NFD. APFS and HFS+ normalise names to NFD, so a
    //   precomposed spelling here would come back decomposed and the assertion below would fail
    //   on macOS while passing on Linux. Do not "fix" this into "\u{00e9}".
    // - "back\\slash.rs" is a legal unix name (a backslash is an ordinary byte there). On
    //   Windows it would be a separator, which is why this whole file is `#![cfg(unix)]`.
    // - "\u{00ff}" is not used here: it has no decomposition, but neither does the tab or the
    //   en dash, and every name below is compared against the literal that was written.
    let names = [
        "a b/c d.rs",
        "with space.rs",
        "tab\tname.rs",
        "\u{65e5}\u{672c}\u{8a9e}/\u{30d5}\u{30a1}\u{30a4}\u{30eb}.rs",
        "emoji \u{1f600}.rs",
        "combining e\u{301}.rs",
        "quote'and\"dquote.rs",
        "back\\slash.rs",
        "dash-\u{2013}endash.rs",
    ];
    for n in names {
        put(&ws, n, "");
    }
    let start = b.resolve_read(".").unwrap();
    let r = walk(&b, &start, &WalkOptions::default()).unwrap();
    let got = rels(&r);
    assert_eq!(got.len(), names.len());
    let mut sorted = got.clone();
    sorted.sort_by(|a, b| a.as_bytes().cmp(b.as_bytes()));
    assert_eq!(got, sorted);
    for n in names {
        assert!(got.iter().any(|g| g == n), "{n} missing from {got:?}");
    }
}

/// `rel` is a WORKSPACE-relative path in every case, including a walk that starts in a
/// subdirectory, and including the ignore rules of that subdirectory.
#[test]
fn e_a_walk_starting_in_a_subdirectory_still_reports_workspace_relative_paths() {
    let (ws, b) = mk();
    put(&ws, "src/a.rs", "");
    put(&ws, "src/b.rs", "");
    put(&ws, "src/deep/c.rs", "");
    put(&ws, "src/.gitignore", "b.rs\n");
    put(&ws, "outside.rs", "");

    let start = b.resolve_read("src").unwrap();
    assert_eq!(start.rel, "src");
    let r = walk(&b, &start, &WalkOptions::default()).unwrap();
    assert_eq!(rels(&r), ["src/.gitignore", "src/a.rs", "src/deep/c.rs"]);

    // `extra_ignore` is relative to the START, so it applies from there down and nowhere
    // above: the same spelling would have ignored something else entirely from the root.
    let r = walk(
        &b,
        &start,
        &WalkOptions {
            extra_ignore: vec!["deep/".into()],
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(rels(&r), ["src/.gitignore", "src/a.rs"]);
}

/// A walk that starts inside a read-only root is labelled `@root1/...`, like every other
/// result (OUT-02): the label is part of the path the caller gets back.
#[test]
fn f_a_walk_inside_a_read_root_is_labelled() {
    let ws = tempfile::tempdir().unwrap();
    let lib = tempfile::tempdir().unwrap();
    fs::create_dir(lib.path().join("crate")).unwrap();
    fs::write(lib.path().join("crate/lib.rs"), "").unwrap();
    fs::write(lib.path().join("top.rs"), "").unwrap();
    let b = Boundary::new(BoundaryConfig {
        root: ws.path().to_path_buf(),
        read_roots: vec![lib.path().to_path_buf()],
        limits: Limits::default(),
        state_dir: None,
        extra_protected: Vec::new(),
    })
    .unwrap();

    let start = b
        .resolve_read(lib.path().canonicalize().unwrap().to_str().unwrap())
        .unwrap();
    assert_eq!(start.rel, "@root1/");
    let r = walk(&b, &start, &WalkOptions::default()).unwrap();
    assert_eq!(rels(&r), ["@root1/crate/lib.rs", "@root1/top.rs"]);
    for f in &r.files {
        assert!(
            f.abs.starts_with(lib.path().canonicalize().unwrap()),
            "abs must stay inside the read root"
        );
        assert!(!f.rel.contains(lib.path().to_str().unwrap()));
    }
}

/// An ignore file that is too large, or not text, is not used at all and is counted once.
/// The alternative - applying the readable prefix - would produce a result nobody could
/// explain, because the rules that were dropped are exactly the ones at the end.
#[test]
fn g_an_unusable_ignore_file_is_counted_once_and_applies_nothing() {
    let (ws, b) = mk();
    let mut text = "# padding\n".repeat(120_000); // > 1 MiB
    text.push_str("secret.rs\n");
    assert!(text.len() as u64 > 1024 * 1024);
    put(&ws, ".gitignore", &text);
    put(&ws, "secret.rs", "");
    put(&ws, "a.rs", "");

    let start = b.resolve_read(".").unwrap();
    let r = walk(&b, &start, &WalkOptions::default()).unwrap();
    assert_eq!(rels(&r), [".gitignore", "a.rs", "secret.rs"]);
    assert_eq!(r.skipped_special, 1);

    // Not UTF-8: same treatment.
    let (ws2, b2) = mk();
    let mut raw = b"# comment\n".to_vec();
    raw.extend_from_slice(&[0xff, 0xfe]);
    raw.push(b'\n');
    raw.extend_from_slice(b"secret.rs\n");
    fs::write(ws2.path().join(".gitignore"), raw).unwrap();
    put(&ws2, "secret.rs", "");
    let start2 = b2.resolve_read(".").unwrap();
    let r2 = walk(&b2, &start2, &WalkOptions::default()).unwrap();
    assert_eq!(rels(&r2), [".gitignore", "secret.rs"]);
    assert_eq!(r2.skipped_special, 1);
}

/// A name that is not valid UTF-8 becomes `Other` in the listing, so the walk counts it as
/// skipped instead of dropping it (OUT-07). The counts have to add up to what is on disk.
///
/// SKIPPED where the filesystem cannot store such a name at all: APFS and HFS+ require valid
/// UTF-8 file names and refuse the `open` with `EILSEQ` (or `EINVAL`), so there is no entry to
/// count. On Linux the file is really created and really walked.
#[test]
fn h_a_non_utf8_name_is_counted_as_special_not_dropped() {
    let (ws, b) = mk();
    put(&ws, "ok.rs", "");
    if let Err(e) = fs::write(ws.path().join(OsStr::from_bytes(b"weird\xff.rs")), "") {
        match name_too_exotic(&e) {
            Some(why) => {
                println!("SKIPPED: this filesystem cannot hold non-UTF-8 names ({why})");
                return;
            }
            None => panic!("creating a non-UTF-8 name failed for an unrelated reason: {e}"),
        }
    }
    let start = b.resolve_read(".").unwrap();
    let r = walk(&b, &start, &WalkOptions::default()).unwrap();
    assert_eq!(rels(&r), ["ok.rs"]);
    assert_eq!(r.skipped_special, 1);
    assert_eq!(r.skipped_links, 0);
    assert_eq!(r.skipped_ignored, 0);
}

/// The built-in VCS directories are counted once each, wherever they are, and a `!` rule
/// cannot bring them back: they are not content.
#[test]
fn i_vcs_directories_are_never_entered_and_cannot_be_negated() {
    let (ws, b) = mk();
    put(&ws, ".git/objects/aa/bb", "");
    put(&ws, "sub/.hg/x", "");
    put(&ws, "sub/.svn/y", "");
    put(&ws, "sub/deep/.bzr/z", "");
    put(&ws, "a.rs", "");
    put(&ws, ".gitignore", "!.git\n!sub/.hg\n");

    let start = b.resolve_read(".").unwrap();
    let r = walk(&b, &start, &WalkOptions::default()).unwrap();
    assert_eq!(rels(&r), [".gitignore", "a.rs"]);
    assert_eq!(r.skipped_ignored, 4, "one per VCS directory, not per file");
}

/// Directories that disappear or turn into links while the walk is running are counted, not
/// fatal: one directory changing under a walk must not lose the rest of the tree.
#[test]
fn j_a_directory_that_cannot_be_listed_is_counted_not_fatal() {
    let (ws, b) = mk();
    for i in 0..40 {
        put(&ws, &format!("d{i:02}/keep.rs"), "");
    }
    put(&ws, "top.rs", "");
    let start = b.resolve_read(".").unwrap();
    // Remove and restore directories underneath the walk, repeatedly.
    for round in 0..200 {
        for i in 0..40 {
            let _ = fs::remove_dir_all(ws.path().join(format!("d{i:02}")));
        }
        let r = walk(&b, &start, &WalkOptions::default()).unwrap();
        assert!(
            r.files.iter().any(|f| f.rel == "top.rs"),
            "round {round}: the walk lost a file that never moved"
        );
        for i in 0..40 {
            put(&ws, &format!("d{i:02}/keep.rs"), "");
        }
    }
}

/// `max_files` cuts the FRONT of the sorted result, and the count is exact: fewer files than
/// the limit is not a truncation, more is.
#[test]
fn k_max_files_cuts_the_sorted_front_exactly() {
    let (ws, b) = mk();
    for i in 0..200 {
        put(&ws, &format!("f{i:03}.rs"), "");
    }
    let start = b.resolve_read(".").unwrap();
    for (limit, want, expect_truncated) in [
        (0u64, 0usize, true),
        (1, 1, true),
        (199, 199, true),
        (200, 200, false),
        (500, 200, false),
    ] {
        let r = walk(
            &b,
            &start,
            &WalkOptions {
                max_files: limit,
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(r.files.len(), want, "limit {limit}");
        assert_eq!(r.truncated, expect_truncated, "limit {limit}");
        if limit > 0 {
            assert_eq!(r.files[0].rel, "f000.rs", "the front of the sorted order");
        }
    }
}
