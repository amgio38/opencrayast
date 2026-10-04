//! SECFIX1-06: the production write path calls the pre-rename recheck — witnessed
//! deterministically through the crate-private seam.
//!
//! CR F2: the previous suite claimed this guarantee was pinned, the reviewer removed the
//! `recheck_target` call from `write_and_replace`, and every test stayed green. `fsio_harden_spec`
//! calls `recheck_target` directly, so it pins the function and never the production path.
//!
//! This is an in-crate test because the seam is crate-private by design: a dependent must not be
//! able to inject a callback into the write path. It lives in `src/` for the same reason the
//! write-mode specs live in `opencrayast-edit` — the thing under test is deliberately unreachable
//! from outside, and the test that proves it needs to reach it.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use crate::ErrorCode;
use crate::fsio::BeforeRename;
use std::fs;

/// A temp workspace with its policy, built here because the shared helper lives in `tests/`.
struct Ws {
    /// Kept alive for the test's duration; the temp dir must outlive every path in it.
    _dir: tempfile::TempDir,
    root: std::path::PathBuf,
    boundary: crate::boundary::Boundary,
}

impl Ws {
    fn new() -> Ws {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("ws");
        fs::create_dir_all(&root).unwrap();
        let state = dir.path().join("state");
        let mut cfg = BoundaryConfig::new(root.clone(), crate::limits::Limits::default());
        cfg.state_dir = Some(state);
        let boundary = crate::boundary::Boundary::new(cfg).unwrap();
        Ws {
            _dir: dir,
            root,
            boundary,
        }
    }

    fn put(&self, rel: &str, content: &[u8]) {
        let p = self.root.join(rel);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(&p, content).unwrap();
    }

    fn abs(&self, rel: &str) -> std::path::PathBuf {
        self.root.join(rel)
    }

    fn resolved(&self, rel: &str) -> crate::boundary::ResolvedPath {
        self.boundary.resolve_write(rel).unwrap()
    }

    fn read(&self, rel: &str) -> Vec<u8> {
        fs::read(self.root.join(rel)).unwrap()
    }

    fn temp_leftovers(&self) -> Vec<std::path::PathBuf> {
        let mut out = Vec::new();
        let mut stack = vec![self.root.clone()];
        while let Some(d) = stack.pop() {
            for e in fs::read_dir(&d).unwrap().flatten() {
                let p = e.path();
                if e.file_type().unwrap().is_dir() {
                    stack.push(p);
                } else if p
                    .file_name()
                    .map(|n| n.to_string_lossy().starts_with(".opencrayast-tmp-"))
                    .unwrap_or(false)
                {
                    out.push(p);
                }
            }
        }
        out
    }
}

use crate::boundary::BoundaryConfig;

/// SECFIX1-06 (CR F2): the PRODUCTION path calls the pre-rename recheck — deterministically.
///
/// `fsio_harden_spec` calls `recheck_target` DIRECTLY, so it pins the function and says nothing
/// about whether the write path calls it. The reviewer removed that call and every test stayed green.
///
/// My first attempt at this witness also stayed green, for the same reason SECFIX1-05 had to be
/// written: adding a hard link is caught by `resolve_write`, before the primitive is reached. The
/// window this needs is the one AFTER the write's own first check, and that window is
/// microseconds wide — so it is reached through the crate-private seam that fires immediately
/// before the rename, not by racing.
#[test]
fn production_path_calls_the_pre_rename_recheck() {
    let ws = Ws::new();
    ws.put("f.txt", b"original");
    let resolved = ws.resolved("f.txt");

    // Replace the target from inside the seam: the write has already checked it and staged its temp
    // file, and this lands the "someone else wrote here" case. Nothing up to this point could have
    // seen it, so only the pre-rename recheck can refuse.
    let victim = ws.abs("f.txt");
    let moved = ws.abs("stolen.txt");
    let seam = BeforeRename {
        hook: Some(&move |_leaf: &std::ffi::OsStr| {
            // Keep the old inode alive (renaming, not deleting: ext4 reuses a freed inode at once,
            // which would hand the same number back and make this test prove nothing).
            let _ = std::fs::rename(&victim, &moved);
            std::fs::write(&victim, b"someone else's work").unwrap();
        }),
    };

    let err = ws
        .boundary
        .replace_file_with_seam(
            &resolved,
            b"mine",
            None,
            &seam,
            &crate::fsio::PropertyCopy::default(),
        )
        .expect_err("a target replaced after the write's own check must be refused");

    eprintln!("pre-rename recheck refusal -> {}", err.message);
    assert!(
        err.code == ErrorCode::IoError || err.code == ErrorCode::UnsupportedTarget,
        "{err}"
    );
    assert_eq!(
        ws.read("f.txt"),
        b"someone else's work",
        "the foreign write must survive: this replace is refused, not applied over it"
    );
    assert!(
        ws.temp_leftovers().is_empty(),
        "and no temp file is left behind"
    );
}

/// SECFIX1-08 (CR F1, scenario 1): a target that has become a SYMLINK is refused, and the file the
/// link points at is not written through.
///
/// The bug this pins: `reprove_write_containment` canonicalises, and canonicalising RESOLVES THE
/// LEAF — so a check placed after it could never see a link. The old `is_symlink()` refusal in the
/// write path was therefore a branch that could not fire, while the module documentation advertised
/// it. This test fails if `Boundary::refuse_leaf_symlink` is removed, because the canonical path then
/// points at the link's target and the write lands there.
#[test]
fn a_symlinked_target_is_refused_and_the_link_target_is_untouched() {
    let ws = Ws::new();
    ws.put("real.txt", b"real content");
    ws.put("link.txt", b"placeholder");

    let resolved = ws.resolved("link.txt");
    let (_h, identity) = ws.boundary.open_read(&resolved).unwrap();

    // The attack: replace the target with a symlink to another file in the workspace. Writing
    // through it would silently modify a file the plan never named.
    std::fs::remove_file(ws.abs("link.txt")).unwrap();
    std::os::unix::fs::symlink(ws.abs("real.txt"), ws.abs("link.txt")).unwrap();

    let err = ws
        .boundary
        .replace_file_checked(&resolved, b"OVERWRITTEN", Some(identity))
        .expect_err("a symlinked target must be refused");

    eprintln!(
        "symlinked target -> [{}] {}",
        err.code.as_str(),
        err.message
    );
    assert_eq!(err.code, ErrorCode::UnsupportedTarget, "{err}");
    assert_eq!(
        ws.read("real.txt"),
        b"real content",
        "the file the link points at must be untouched"
    );
}

/// SECFIX1-09 (CR F1, scenario 2): a target replaced by ANOTHER FILE after the caller verified it is
/// refused.
///
/// This is the guard that was silently removed: `apply.rs` used to pass the `FileIdentity` it had
/// observed, and F-01b began discarding it (`let _ = identity;`) with a comment claiming the
/// staleness check was still intact. Passing `None` here — as a caller with no earlier read would —
/// must therefore behave DIFFERENTLY from passing the real identity, or the parameter is decorative.
#[test]
fn a_target_replaced_by_another_file_is_refused_when_the_identity_is_supplied() {
    let ws = Ws::new();
    ws.put("f.txt", b"original");
    let resolved = ws.resolved("f.txt");
    let (_h, identity) = ws.boundary.open_read(&resolved).unwrap();

    // Someone replaces the file with different content. Renaming the old one aside keeps its
    // inode alive, so the filesystem cannot hand the same number to the new file (ext4 reuses a
    // freed inode immediately, which would make this test prove nothing).
    let aside = ws.abs("aside.txt");
    std::fs::rename(ws.abs("f.txt"), &aside).unwrap();
    ws.put("f.txt", b"a different file entirely");

    let err = ws
        .boundary
        .replace_file_checked(&resolved, b"mine", Some(identity))
        .expect_err("a target that changed since it was verified must be refused");

    eprintln!("replaced target -> [{}] {}", err.code.as_str(), err.message);
    assert_eq!(err.code, ErrorCode::IoError, "{err}");
    assert_eq!(
        ws.read("f.txt"),
        b"a different file entirely",
        "the foreign content must survive"
    );
    assert!(ws.temp_leftovers().is_empty());
}

/// SECFIX1-10: the SAME call with `None` does NOT get the identity guard — so the parameter is load-
/// bearing, not decorative.
///
/// If this ever starts failing because `None` is also refused, the guard has been moved somewhere
/// that makes the caller's verified identity irrelevant, which is the CR F1 failure mode again.
#[test]
fn without_a_supplied_identity_there_is_nothing_to_compare_against() {
    let ws = Ws::new();
    ws.put("f.txt", b"original");
    let resolved = ws.resolved("f.txt");

    // No earlier read, so no expectation: the write proceeds against the target as it is now.
    ws.boundary
        .replace_file_checked(&resolved, b"written", None)
        .unwrap_or_else(|e| {
            panic!("with no identity to contradict, the write should proceed: {e}")
        });
    assert_eq!(ws.read("f.txt"), b"written");
}

/// SECFIX1-11: whether `apply.rs` passes `Some` or `None` at each call site, and WHY.
///
/// CR F1 asked for the caller-side identity guard back. Restoring the parameter is not the same as
/// restoring a guard, and the difference is worth pinning rather than asserting in prose: both
/// production call sites in `apply.rs` (`replace_one` and `restore_one`) re-open the file through
/// `Boundary::open_read` immediately before writing, so the identity they hand over was observed
/// microseconds earlier and `Some(x)` is indistinguishable from `None` **at those two sites**.
///
/// That is a fact about where they sit in the sequence, not a general statement. A caller that
/// verifies early and writes late — the shape the CR's own scenario had — gets the guard's full
/// value, which SECFIX1-08 and SECFIX1-09 demonstrate. This test records the arrangement so the next
/// person to move one of these call sites knows the parameter's usefulness there depends on how
/// close the open is to the write.
#[test]
fn the_identity_parameter_is_what_separates_an_early_read_from_a_late_write() {
    let ws = Ws::new();
    ws.put("f.txt", b"original");

    // Verified early...
    let resolved = ws.resolved("f.txt");
    let (_h, identity) = ws.boundary.open_read(&resolved).unwrap();

    // ...and the target changes before the write.
    let aside = ws.abs("aside.txt");
    std::fs::rename(ws.abs("f.txt"), &aside).unwrap();
    ws.put("f.txt", b"a different file entirely");

    // Handing over the early identity: refused.
    let guarded = ws
        .boundary
        .replace_file_checked(&resolved, b"mine", Some(identity));
    assert!(
        guarded.is_err(),
        "with the caller's identity the change is caught"
    );

    // Handing over nothing: nothing to compare against, so the write proceeds against what is there
    // now. This is why both production call sites re-open immediately before writing — and why the
    // parameter must not be quietly dropped for a caller that does not.
    let unguarded = ws.boundary.replace_file_checked(&resolved, b"mine", None);
    assert!(
        unguarded.is_ok(),
        "with no identity there is nothing to contradict, so the write proceeds"
    );
    assert_eq!(ws.read("f.txt"), b"mine");
}
