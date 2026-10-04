//! SECFIX5 — the operator's `path_max_depth` / `path_max_bytes` must actually bind.
//!
//! Ticket: `Y20261002/REQ-SECURITY-REVIEW/ISSUE-SEC-FIX-PATH-LIMITS` (SEC-A1 F-03).
//! Before this change `Boundary` held no `Limits` at all, and the two path checks each
//! built a `Limits::default()` of their own — `boundary.rs` for the resolver and
//! `walk.rs` for the walker. A configured value therefore could not reach either of them,
//! so `[limits] path_max_depth` in the user file bound nothing while `CONFIGURATION.md`
//! said it did.
//!
//! These are the ticket's four invariants, one test each. `SECFIX5-01`/`02` are the two
//! call sites; `SECFIX5-03` is the structural one (no second copy of the default);
//! `SECFIX5-04` is the "unchanged at defaults" witness.
//!
//! Nothing here weakens an existing assertion: no pre-existing spec file is modified,
//! and `SECFIX5-04` reads the documented numbers rather than restating a threshold this
//! change could have moved.
#![cfg(unix)]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use opencrayast_core::ErrorCode;
use opencrayast_core::boundary::{Boundary, BoundaryConfig};
use opencrayast_core::limits::Limits;
use opencrayast_core::walk::{WalkOptions, walk};
use std::fs;
use std::path::PathBuf;

/// A workspace with `depth` nested directories and a file in the deepest one.
fn deep_tree(depth: usize) -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("ws");
    fs::create_dir_all(&root).unwrap();
    let mut p = root.clone();
    for i in 0..depth {
        p = p.join(format!("d{i}"));
    }
    fs::create_dir_all(&p).unwrap();
    fs::write(p.join("f.txt"), b"x").unwrap();
    (dir, root)
}

fn boundary(root: &std::path::Path, limits: Limits) -> Boundary {
    Boundary::new(BoundaryConfig {
        root: root.to_path_buf(),
        limits,
        read_roots: Vec::new(),
        state_dir: None,
        extra_protected: Vec::new(),
    })
    .unwrap()
}

/// The relative path of the file `deep_tree` created.
fn deep_rel(depth: usize) -> String {
    (0..depth)
        .map(|i| format!("d{i}"))
        .collect::<Vec<_>>()
        .join("/")
        + "/f.txt"
}

/// SECFIX5-01 — the RESOLVER honours a configured `path_max_depth`.
///
/// This is the invariant that was false: with `path_max_depth = 4` a 21-component path
/// resolved `Ok` and returned the full relative path.
#[test]
fn secfix5_01_resolve_read_honours_configured_path_max_depth() {
    let (_d, root) = deep_tree(20);
    let rel = deep_rel(20);
    let strict = boundary(
        &root,
        Limits {
            path_max_depth: 4,
            ..Limits::default()
        },
    );

    let err = strict.resolve_read(&rel).unwrap_err();
    assert_eq!(
        err.code,
        ErrorCode::LimitExceeded,
        "a 21-component path must be refused when the operator configured depth 4, got {err:?}"
    );
    // The message has to name the ceiling that was hit, or the operator cannot act on it.
    assert!(
        err.message.contains('4') && err.next.contains("path_max_depth"),
        "the refusal must name the ceiling and how to change it: {err:?}"
    );
}

/// SECFIX5-02 — the WALKER honours the same configured value.
///
/// This is the second call site. Before the change `walk.rs` built its own
/// `Limits::default()`, so tightening the operator's depth ceiling changed the resolver
/// (once it read the config at all) and left the walker's ceiling at 64.
#[test]
fn secfix5_02_walk_honours_configured_path_max_depth() {
    let (_d, root) = deep_tree(20);
    let strict = boundary(
        &root,
        Limits {
            path_max_depth: 4,
            ..Limits::default()
        },
    );

    let start = strict.resolve_read(".").unwrap();
    let result = walk(&strict, &start, &WalkOptions::default()).unwrap();

    let deepest = result
        .files
        .iter()
        .map(|f| f.rel.matches('/').count())
        .max()
        .unwrap_or(0);
    assert!(
        deepest <= 4,
        "the walk returned a path {deepest} components deep under a ceiling of 4: {:?}",
        result.files.iter().map(|f| &f.rel).collect::<Vec<_>>()
    );
    // The walk must not silently hide what it did not enter (OUT-07): the skipped subtree
    // is counted, and the deepest file is provably not in the result.
    assert!(
        !result.files.iter().any(|f| f.rel == deep_rel(20)),
        "the file below the ceiling must not be returned"
    );
    assert!(
        result.skipped_ignored > 0,
        "the refused subtree must be counted, not silently dropped: {result:?}"
    );
}

/// SECFIX5-02b — `path_max_bytes` binds too.
///
/// The byte ceiling had the same two-call-site shape as the depth ceiling. The path used here
/// is one component with a long name, so it is short in DEPTH and long in BYTES: a byte
/// ceiling and a depth ceiling are independent knobs and this one must not be satisfied by
/// accident.
#[test]
fn secfix5_02b_configured_path_max_bytes_binds() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("ws");
    fs::create_dir(&root).unwrap();
    let long_name = format!("{}.txt", "n".repeat(100));
    fs::write(root.join(&long_name), b"x").unwrap();
    fs::create_dir(root.join("d0")).unwrap();
    fs::write(root.join("d0/a.txt"), b"x").unwrap();
    let strict = boundary(
        &root,
        Limits {
            path_max_bytes: 64,
            ..Limits::default()
        },
    );

    let err = strict.resolve_read(&long_name).unwrap_err();
    assert_eq!(err.code, ErrorCode::LimitExceeded, "got {err:?}");
    assert!(
        err.next.contains("path_max_bytes"),
        "the refusal must say which knob to turn: {err:?}"
    );

    // The same boundary must still accept a path inside its own ceiling, so the test above
    // is not passing because everything is refused.
    assert!(
        strict.resolve_read("d0/a.txt").is_ok(),
        "a short path must still resolve under a tight byte ceiling"
    );
}

/// SECFIX5-03 — there is exactly ONE place that supplies the path ceiling.
///
/// The shape of the defect was two independent call sites each constructing a default, so
/// "there is one source" is asserted structurally rather than by behaviour: outside the
/// `#[cfg(test)]` blocks, neither checkpoint file may build a `Limits` of its own.
#[test]
fn secfix5_03_no_second_copy_of_the_default_remains() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    for file in ["crates/core/src/boundary.rs", "crates/core/src/walk.rs"] {
        let full = root.join(file);
        let src = fs::read_to_string(&full).unwrap();
        // Drop the in-module test module: a test is allowed to name the default on purpose.
        let production = match src.find("#[cfg(test)]") {
            Some(i) => &src[..i],
            None => &src[..],
        };
        // And drop COMMENTS before searching, because the reason this defect existed is
        // written in prose in these very files - a check that matched the string would trip
        // on the explanation of the bug. (SEC-A1 F-05's first PoC made exactly that mistake:
        // it searched for the word "test" and matched the surrounding prose.)
        let code: String = production
            .lines()
            .map(|l| {
                let t = l.trim_start();
                if t.starts_with("//") {
                    ""
                } else {
                    match l.find("//") {
                        Some(i) => &l[..i],
                        None => l,
                    }
                }
            })
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            !code.contains("Limits::default()"),
            "{file} still BUILDS a Limits of its own outside its test module; the ceiling must \
             come from Boundary::limits() so the two call sites cannot drift"
        );
    }
    // And the boundary really does hand out what it was configured with, not a copy.
    let (_d, root_dir) = deep_tree(1);
    let custom = Limits {
        path_max_depth: 7,
        path_max_bytes: 123,
        ..Limits::default()
    };
    let b = boundary(&root_dir, custom.clone());
    assert_eq!(b.limits().path_max_depth, 7);
    assert_eq!(b.limits().path_max_bytes, 123);
}

/// SECFIX5-04 — with no configuration the behaviour is unchanged.
///
/// The ticket's invariant 2: defaults must behave exactly as before. Rather than restate
/// thresholds here (which a change could quietly move), this asserts the documented default
/// numbers, that a boundary built with no `limits` behaves identically to one built with
/// `Limits::default()` explicitly, and it leans on the untouched spec suites for the
/// boundary itself (`boundary_spec.rs::bnd20_overlong_and_deep_refused` and friends).
#[test]
fn secfix5_04_defaults_behave_exactly_as_before() {
    let d = Limits::default();
    assert_eq!(d.path_max_bytes, 4096, "documented default changed");
    assert_eq!(d.path_max_depth, 64, "documented default changed");

    let (_d, root) = deep_tree(20);
    let implicit = boundary(&root, Limits::default());
    let rel = deep_rel(20);

    // The documented ceilings still hold: 64 components is fine, 5000 bytes is too long.
    assert!(
        implicit.resolve_read(&rel).is_ok(),
        "a 21-component path is inside the default ceiling of 64"
    );
    let long = format!("{}/{}", deep_rel(20), "n".repeat(5000));
    assert_eq!(
        implicit.resolve_read(&long).unwrap_err().code,
        ErrorCode::LimitExceeded,
        "the default byte ceiling must still refuse a 5000-byte path"
    );
    // Exactly AT the ceiling still passes (the check is `>`, not `>=`).
    let at64 = deep_rel(63);
    fs::create_dir_all(root.join(std::path::Path::new(&at64).parent().unwrap())).unwrap();
    fs::write(root.join(&at64), b"x").unwrap();
    assert!(
        implicit.resolve_read(&at64).is_ok(),
        "a path of exactly path_max_depth components must still resolve"
    );

    // A boundary that was never told about limits must equal one told explicitly.
    let bare = Boundary::new(BoundaryConfig {
        root: root.clone(),
        limits: Limits::default(),
        read_roots: Vec::new(),
        state_dir: None,
        extra_protected: Vec::new(),
    })
    .unwrap();
    assert_eq!(bare.limits(), &d);
    for p in [".", "d0", "d0/d1", &rel] {
        assert_eq!(
            bare.resolve_read(p).map(|r| r.rel),
            implicit.resolve_read(p).map(|r| r.rel),
            "an unconfigured boundary and an explicitly-default one disagree on {p:?}"
        );
    }
}
