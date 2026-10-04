//! Spec for ISSUE-CORE-LISTDIR and ISSUE-CORE-IGNORE (walk, read_dir). Add cases; never weaken these.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
#![cfg(unix)]
use opencrayast_core::boundary::{Boundary, BoundaryConfig};
use opencrayast_core::limits::Limits;
use opencrayast_core::walk::*;
use std::fs;
use std::os::unix::fs::symlink;

fn mk() -> (tempfile::TempDir, tempfile::TempDir, Boundary) {
    let ws = tempfile::tempdir().unwrap();
    let out = tempfile::tempdir().unwrap();
    let b = Boundary::new(BoundaryConfig {
        root: ws.path().to_path_buf(),
        limits: Limits::default(),
        read_roots: Vec::new(),
        state_dir: None,
        extra_protected: Vec::new(),
    })
    .unwrap();
    (ws, out, b)
}

fn put(ws: &tempfile::TempDir, rel: &str, body: &str) {
    let p = ws.path().join(rel);
    fs::create_dir_all(p.parent().unwrap()).unwrap();
    fs::write(p, body).unwrap();
}

fn rels(r: &WalkResult) -> Vec<String> {
    r.files.iter().map(|f| f.rel.clone()).collect()
}

#[test]
fn read_dir_lists_sorted_without_dot_entries_and_without_following_links() {
    let (ws, out, b) = mk();
    put(&ws, "b.rs", "");
    put(&ws, "a.rs", "");
    fs::create_dir(ws.path().join("d")).unwrap();
    symlink(out.path(), ws.path().join("lnk")).unwrap();
    let dir = b.resolve_read(".").unwrap();
    let e = b.read_dir(&dir).unwrap();
    let names: Vec<&str> = e.iter().map(|x| x.name.as_str()).collect();
    assert_eq!(names, ["a.rs", "b.rs", "d", "lnk"]);
    let kind = |n: &str| e.iter().find(|x| x.name == n).unwrap().kind;
    assert_eq!(kind("a.rs"), EntryKind::File);
    assert_eq!(kind("d"), EntryKind::Dir);
    assert_eq!(kind("lnk"), EntryKind::Symlink);
}

#[test]
fn walk_is_sorted_skips_vcs_dirs_and_counts_links_and_specials() {
    let (ws, out, b) = mk();
    put(&ws, "src/z.rs", "");
    put(&ws, "src/a.rs", "");
    put(&ws, "README.md", "");
    put(&ws, ".git/config", "x");
    put(&ws, "sub/.git/HEAD", "x");
    symlink(out.path(), ws.path().join("outlink")).unwrap();
    symlink(ws.path().join("README.md"), ws.path().join("filelink")).unwrap();
    let status = std::process::Command::new("mkfifo")
        .arg(ws.path().join("pipe"))
        .status()
        .unwrap();
    assert!(status.success());
    let start = b.resolve_read(".").unwrap();
    let r = walk(&b, &start, &WalkOptions::default()).unwrap();
    assert_eq!(rels(&r), ["README.md", "src/a.rs", "src/z.rs"]);
    assert_eq!(r.skipped_links, 2);
    assert_eq!(r.skipped_special, 1);
    assert!(
        r.skipped_ignored >= 2,
        "the two .git directories are counted"
    );
    assert!(!r.truncated);
    // deterministic
    assert_eq!(walk(&b, &start, &WalkOptions::default()).unwrap(), r);
}

#[test]
fn walk_of_a_single_file_returns_that_file() {
    let (ws, _o, b) = mk();
    put(&ws, "src/a.rs", "");
    let f = b.resolve_read("src/a.rs").unwrap();
    let r = walk(&b, &f, &WalkOptions::default()).unwrap();
    assert_eq!(rels(&r), ["src/a.rs"]);
}

#[test]
fn max_files_truncates_exactly_and_says_so() {
    let (ws, _o, b) = mk();
    for i in 0..30 {
        put(&ws, &format!("f{i:02}.rs"), "");
    }
    let start = b.resolve_read(".").unwrap();
    let r = walk(
        &b,
        &start,
        &WalkOptions {
            max_files: 10,
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(r.files.len(), 10);
    assert!(r.truncated);
    assert_eq!(rels(&r)[0], "f00.rs");
    let r = walk(
        &b,
        &start,
        &WalkOptions {
            max_files: 30,
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(r.files.len(), 30);
    assert!(!r.truncated);
}

#[test]
fn gitignore_is_honoured_nested_rules_apply_below_their_directory() {
    let (ws, _o, b) = mk();
    put(&ws, ".gitignore", "*.log\nbuild/\n!keep.log\n");
    put(&ws, "a.log", "");
    put(&ws, "keep.log", "");
    put(&ws, "build/x.rs", "");
    put(&ws, "src/main.rs", "");
    put(&ws, "src/.gitignore", "gen.rs\n");
    put(&ws, "src/gen.rs", "");
    put(&ws, "gen.rs", "");
    put(&ws, "other/gen.rs", "");
    let start = b.resolve_read(".").unwrap();
    let r = walk(&b, &start, &WalkOptions::default()).unwrap();
    assert_eq!(
        rels(&r),
        [
            ".gitignore",
            "gen.rs",
            "keep.log",
            "other/gen.rs",
            "src/.gitignore",
            "src/main.rs"
        ]
    );
    // opting out
    let r = walk(
        &b,
        &start,
        &WalkOptions {
            respect_gitignore: false,
            ..Default::default()
        },
    )
    .unwrap();
    assert!(rels(&r).contains(&"a.log".to_string()));
}

#[test]
fn negation_cannot_reinclude_below_an_ignored_directory() {
    let (ws, _o, b) = mk();
    put(&ws, ".gitignore", "build/\n!build/keep.rs\n");
    put(&ws, "build/keep.rs", "");
    let start = b.resolve_read(".").unwrap();
    let r = walk(&b, &start, &WalkOptions::default()).unwrap();
    assert_eq!(rels(&r), [".gitignore"]);
}

#[test]
fn extra_ignore_globs_apply_from_the_start() {
    let (ws, _o, b) = mk();
    put(&ws, "vendor/x.rs", "");
    put(&ws, "src/y.rs", "");
    let start = b.resolve_read(".").unwrap();
    let r = walk(
        &b,
        &start,
        &WalkOptions {
            extra_ignore: vec!["vendor/".into()],
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(rels(&r), ["src/y.rs"]);
}

#[test]
fn bnd23_a_symlinked_gitignore_pointing_outside_is_not_read() {
    let (ws, out, b) = mk();
    fs::write(out.path().join("evil_ignore"), "*.rs\n").unwrap();
    symlink(out.path().join("evil_ignore"), ws.path().join(".gitignore")).unwrap();
    put(&ws, "a.rs", "");
    let start = b.resolve_read(".").unwrap();
    let r = walk(&b, &start, &WalkOptions::default()).unwrap();
    // the link is skipped (counted) and its outside content never applies
    assert_eq!(rels(&r), ["a.rs"]);
    assert_eq!(r.skipped_links, 1);
}

#[test]
fn symlink_loops_and_deep_trees_terminate() {
    let (ws, _o, b) = mk();
    put(&ws, "a/b/c/d/e/f.rs", "");
    symlink(ws.path().join("a"), ws.path().join("a/b/loop")).unwrap();
    let start = b.resolve_read(".").unwrap();
    let r = walk(&b, &start, &WalkOptions::default()).unwrap();
    assert_eq!(rels(&r), ["a/b/c/d/e/f.rs"]);
}

/// The walk never descends into the tool's **own** state directory, by name and at any depth.
///
/// The state directory is no longer inside the workspace — it is the platform user-state base —
/// but a tree checked out with an older build still has one, and an ordinary walk must not be a
/// route to reading the tool's own plans, journals, undo backups, plan ids and before/after
/// hashes back through `ast_get`. No `.gitignore` entry can bring this back, for the same reason
/// a `.gitignore` entry cannot bring back `.git`.
///
/// `mk()` puts the boundary's root at the tempdir root, so the fixture puts the planted
/// directory at the top level and again two levels down: the skip is by name at **any** depth,
/// and both placements are asserted so a depth-sensitive implementation would fail one of them.
#[test]
fn the_walk_never_enters_a_legacy_state_directory() {
    let (ws, _o, b) = mk();
    // Content that would be perfectly readable if the walk entered it.
    put(
        &ws,
        ".opencrayast/ws-w-abc/plans/p-abcdefghij.json",
        "SECRET-PLAN-BYTES",
    );
    put(
        &ws,
        "sub/.opencrayast/ws-w-abc/journal/manifest.json",
        "SECRET-JOURNAL-BYTES",
    );
    put(&ws, "src/keep.rs", "");

    let start = b.resolve_read(".").unwrap();
    let r = walk(&b, &start, &WalkOptions::default()).unwrap();
    let listed = rels(&r);

    assert_eq!(listed, ["src/keep.rs"], "only real content is listed");
    for rel in &listed {
        assert!(
            !rel.contains(".opencrayast"),
            "the walk must never list anything inside the state directory: {rel}"
        );
    }
    // Counted, not silently dropped — the same accounting a `.git` directory gets.
    assert!(
        r.skipped_ignored >= 2,
        "both planted directories are counted as skipped, got {}",
        r.skipped_ignored
    );
}
