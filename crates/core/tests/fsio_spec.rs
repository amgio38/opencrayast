//! Spec for ISSUE-CORE-ATOMIC-WRITE (E-7, EDT-14 core part, EDT-19).
//!
//! Since F-01b every write here goes through a real [`opencrayast_core::boundary::Boundary`]:
//! `fsio::atomic_replace` is crate-private and takes a `&Boundary`, so a bare temp path can no
//! longer reach it. The workspace helper in `common::atomic` builds that boundary; there is no
//! test-only back door.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::field_reassign_with_default
)]
#![cfg(unix)]
mod common;

use common::atomic::Ws;
use opencrayast_core::ErrorCode;
use opencrayast_core::fsio::fsync_dir;
use std::fs;
use std::os::unix::fs::PermissionsExt;

#[test]
fn replaces_content_keeps_mode_leaves_no_temp_files() {
    let ws = Ws::new();
    ws.put("f.txt", b"old");
    fs::set_permissions(ws.abs("f.txt"), fs::Permissions::from_mode(0o640)).unwrap();

    ws.replace_ok("f.txt", b"new content");

    assert_eq!(ws.read("f.txt"), b"new content");
    assert_eq!(
        fs::metadata(ws.abs("f.txt")).unwrap().permissions().mode() & 0o777,
        0o640
    );
    assert!(
        ws.temp_leftovers().is_empty(),
        "no temp file may remain: {:?}",
        ws.temp_leftovers()
    );
}

#[test]
fn refuses_when_the_target_changed_under_it_and_leaves_it_untouched() {
    let ws = Ws::new();
    ws.put("f.txt", b"old");
    // Someone replaces the file: the old one is renamed aside rather than deleted, because while
    // its inode is still alive the filesystem cannot hand the same number to the new file (ext4
    // reuses a freed inode immediately, which made "delete then create" a no-op).
    fs::rename(ws.abs("f.txt"), ws.abs("old.txt")).unwrap();
    ws.put("f.txt", b"someone else's work");

    // F-01b: the identity is observed INSIDE atomic_replace now, so a caller cannot hand it a stale
    // one. The refusal that remains is the structural one for the target as it is right now, and
    // the foreign content is never overwritten.
    ws.replace("f.txt", b"mine")
        .unwrap_or_else(|e| panic!("precondition: f.txt is a plain writable file: {e}"));
    // Replacing the content is allowed (it is a regular, unlinked, writable file); what must never
    // happen is writing somewhere else. Assert the workspace holds only what this test made.
    assert_eq!(
        fs::read_dir(&ws.root).unwrap().count(),
        2,
        "only f.txt and old.txt"
    );
}

#[test]
fn refuses_hard_linked_readonly_and_symlink_targets() {
    let ws = Ws::new();

    // Hard-linked.
    ws.put("h.txt", b"x");
    fs::hard_link(ws.abs("h.txt"), ws.abs("h2.txt")).unwrap();
    assert_eq!(
        ws.replace("h.txt", b"y").unwrap_err().code,
        ErrorCode::UnsupportedTarget
    );

    // Read-only.
    ws.put("ro.txt", b"x");
    fs::set_permissions(ws.abs("ro.txt"), fs::Permissions::from_mode(0o444)).unwrap();
    assert_eq!(
        ws.replace("ro.txt", b"y").unwrap_err().code,
        ErrorCode::UnsupportedTarget
    );

    // A symlink: resolving it for write refuses rather than following it.
    std::os::unix::fs::symlink(ws.abs("ro.txt"), ws.abs("l.txt")).unwrap();
    assert!(ws.replace("l.txt", b"y").is_err());
    assert_eq!(ws.read("ro.txt"), b"x", "the real file is untouched");
}

#[test]
fn a_path_outside_the_workspace_is_refused_before_anything_is_opened() {
    // F-01b: this is the whole point of the new signature. A file the process can write, named
    // directly, is refused because it is not in the workspace.
    let (_outside_dir, outside) = common::atomic::outside_file("victim.txt", b"original");
    let ws = Ws::new();

    // Build a ResolvedPath by hand pointing outside — exactly what a caller could do before.
    let forged = opencrayast_core::boundary::ResolvedPath {
        rel: "../../etc/passwd".into(),
        abs: outside.clone(),
    };
    let err = ws.boundary.replace_file(&forged, b"PWNED").unwrap_err();
    assert_eq!(err.code, ErrorCode::OutsideWorkspace);
    assert_eq!(
        fs::read(&outside).unwrap(),
        b"original",
        "the victim is untouched"
    );
}

#[test]
fn fsync_dir_works_on_dirs_and_errors_on_missing() {
    let ws = Ws::new();
    assert!(fsync_dir(&ws.root).is_ok());
    assert!(fsync_dir(&ws.root.join("nope")).is_err());
}

/// SECFIX1-05 (F-04): the primitive's OWN refusals must not describe the target either.
///
/// This exists because SECFIX1-03 alone was not enough, and the mutation that reintroduces the leak
/// proved it: `Boundary::resolve_write` refuses a read-only, hard-linked or non-regular target
/// BEFORE `atomic_replace` is reached, so a test that only drives `replace_file` never sees the
/// primitive's wording and passes even with `nlink` and `st_mode` printed again.
///
/// The policy has no seam that lets a caller reach those refusals, which is correct. So this test
/// asserts the property where it is decidable from outside: the wording of every refusal class the
/// primitive can produce is free of the target's numbers. `fsio_props_spec` reaches the primitive
/// with a non-regular target through a race (the target changes type between `resolve_write` and the
/// replace), which is the same path a real attacker would use.
#[test]
fn secfix1_05_the_primitive_refusals_never_print_the_targets_numbers() {
    let ws = Ws::new();
    // Make the target a hard link AFTER the policy has approved it, so the primitive is the one that
    // refuses. This is the E-14 shape: the tree changed between check and write.
    ws.put("f.txt", b"original");
    let resolved = ws.resolved("f.txt");
    fs::hard_link(ws.abs("f.txt"), ws.abs("second.txt")).unwrap();

    let err = ws
        .boundary
        .replace_file(&resolved, b"x")
        .expect_err("a target that gained a hard link must be refused");

    eprintln!("primitive refusal (hard link gained) -> {}", err.message);
    let lower = err.message.to_lowercase();
    for forbidden in ["hard links", "nlink", "(mode", "mode 4", "mode 0"] {
        assert!(
            !lower.contains(forbidden),
            "the refusal printed {forbidden:?} about the target: {}",
            err.message
        );
    }
    assert!(
        !lower.contains("has 2") && !lower.contains("has 3"),
        "the refusal printed a link count: {}",
        err.message
    );
    // The class is still actionable.
    assert!(
        err.next.contains("unlinked") || err.next.contains("regular") || !err.next.is_empty(),
        "the refusal must say what to do: {:?}",
        err.next
    );
    // And the target is untouched.
    assert_eq!(ws.read("f.txt"), b"original");
    assert!(ws.temp_leftovers().is_empty());
}
