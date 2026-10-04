//! BND-06: escape through a *middle* path component that is a link, and the Windows
//! junction / reparse-point cases that have the same shape.
//!
//! The escape this file is about is not "a symlink is the target". That shape is already
//! covered by `boundary_spec.rs` (`BND-03`) and by the write policy's final-component check.
//! It is `root/a/b/c` where **`a`** is the link and `b`, `c` are ordinary files on the far
//! side of it. On Windows a directory junction is semantically a directory symlink, so this
//! is exactly the shape a junction escape takes, and the whole junction question reduces to
//! it: does the resolver follow a middle link, and where does it decide?
//!
//! What is pinned here, by observation rather than by assumption (`resolve_existing`,
//! `crates/core/src/boundary.rs`):
//!
//!   * a middle link that leaves the workspace is refused on **both** `resolve_read` and
//!     `resolve_write`, with the same `outside_workspace` refusal as any other escape, and
//!     the refusal is decided on the *canonical* path — the link is followed by the OS during
//!     canonicalisation and the containment check then fails;
//!   * a middle link that stays inside the workspace is followed for both directions, and
//!     the resolved path is reported relative to the real location, not to the link;
//!   * the criterion is "does it leave the boundary", never "is it a link" — except for the
//!     final component of a *write* target, which the existing policy refuses whatever it
//!     points at (`boundary.rs`, `resolve_write`).
//!
//! The Windows half (`#[cfg(windows)]`, in `boundary_junction_windows_spec.rs`) is compiled
//! and type-checked for `x86_64-pc-windows-gnu` on every change but **has never been
//! executed**, because it was authored on a Linux container.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::field_reassign_with_default
)]
#![cfg(unix)]

use opencrayast_core::ErrorCode;
use opencrayast_core::boundary::*;
use opencrayast_core::limits::Limits;
use std::fs;
use std::os::unix::fs::symlink;
use std::path::{Path, PathBuf};

/// Workspace `ws/` with `real/b/c.txt` inside it, plus a second temp dir `out/` holding
/// `x/c.txt`. Returns both roots and a boundary over `ws`.
fn setup() -> (tempfile::TempDir, tempfile::TempDir, Boundary) {
    let ws = tempfile::tempdir().unwrap();
    let out = tempfile::tempdir().unwrap();
    fs::create_dir_all(ws.path().join("real/b")).unwrap();
    fs::write(ws.path().join("real/b/c.txt"), "inside the workspace").unwrap();
    fs::create_dir_all(out.path().join("x")).unwrap();
    fs::write(out.path().join("x/c.txt"), "secret").unwrap();
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

/// A boundary over `ws` that additionally grants read-only access to `rr`.
fn setup_with_read_root(
    ws: &tempfile::TempDir,
    rr: &tempfile::TempDir,
) -> Result<Boundary, opencrayast_core::ToolError> {
    Boundary::new(BoundaryConfig {
        root: ws.path().to_path_buf(),
        limits: Limits::default(),
        read_roots: vec![rr.path().to_path_buf()],
        state_dir: None,
        extra_protected: Vec::new(),
    })
}

/// The junction-escape shape: the link is a MIDDLE component, the leaf is a real file.
#[test]
fn a_middle_link_leaving_the_workspace_is_refused_for_read_and_for_write() {
    let (ws, out, b) = setup();
    symlink(out.path().join("x"), ws.path().join("a")).unwrap();
    // The leaf is a perfectly ordinary file; only the component above it is a link.
    assert!(
        out.path().join("x/c.txt").is_file(),
        "the escape target must really exist, or the test proves nothing"
    );

    assert_eq!(
        b.resolve_read("a/c.txt").unwrap_err().code,
        ErrorCode::OutsideWorkspace,
        "reading through a middle link that leaves the workspace"
    );
    assert_eq!(
        b.resolve_write("a/c.txt").unwrap_err().code,
        ErrorCode::OutsideWorkspace,
        "writing through a middle link that leaves the workspace"
    );
}

/// The same refusal when the link is the whole path: `a` on its own names the outside dir.
/// A directory can never be a write target anyway, but the *read* refusal is the point.
#[test]
fn a_link_to_an_outside_directory_is_refused_on_its_own() {
    let (ws, out, b) = setup();
    symlink(out.path().join("x"), ws.path().join("a")).unwrap();

    assert_eq!(
        b.resolve_read("a").unwrap_err().code,
        ErrorCode::OutsideWorkspace
    );
}

/// The link is refused on the same terms as every other escape: the message never names a
/// path, and an outside directory that exists and one that does not are indistinguishable
/// (BND-18 / T-20). A middle-component link must not become an existence probe.
#[test]
fn a_middle_link_refusal_never_probes_outside() {
    let (ws, out, b) = setup();
    symlink(out.path().join("x"), ws.path().join("present")).unwrap();
    symlink(out.path().join("absent"), ws.path().join("missing")).unwrap();

    let present = b.resolve_read("present/c.txt").unwrap_err();
    let missing = b.resolve_read("missing/c.txt").unwrap_err();
    assert_eq!(
        present, missing,
        "existence outside the workspace must not leak"
    );
    assert!(
        !present.message.contains(out.path().to_str().unwrap()),
        "the refusal must not name an absolute path"
    );
}

/// The link chain a junction escape really looks like: `a` links to `a2`, `a2` to the
/// outside directory. Two hops, still one refusal, and no traversal of the chain survives.
#[test]
fn a_middle_link_chain_leaving_the_workspace_is_refused() {
    let (ws, out, b) = setup();
    symlink(out.path().join("x"), ws.path().join("a2")).unwrap();
    symlink(ws.path().join("a2"), ws.path().join("a")).unwrap();

    assert_eq!(
        b.resolve_read("a/c.txt").unwrap_err().code,
        ErrorCode::OutsideWorkspace
    );
    assert_eq!(
        b.resolve_write("a/c.txt").unwrap_err().code,
        ErrorCode::OutsideWorkspace
    );
}

/// The criterion is "does it leave the boundary", not "is it a link": a middle link that
/// stays inside is followed, for both directions, and the result is reported under the real
/// name so nothing downstream has to care how it was spelled.
#[test]
fn a_middle_link_that_stays_inside_is_followed_by_both_directions() {
    let (ws, _out, b) = setup();
    symlink(ws.path().join("real"), ws.path().join("d")).unwrap();

    assert_eq!(b.resolve_read("d/b/c.txt").unwrap().rel, "real/b/c.txt");
    assert_eq!(
        b.resolve_write("d/b/c.txt").unwrap().rel,
        "real/b/c.txt",
        "an inside-pointing middle link must not be refused the way an escaping one is"
    );
    // The link as the whole path is a directory that exists inside: read is fine.
    assert_eq!(b.resolve_read("d").unwrap().rel, "real");
}

/// A middle link whose target is itself a link, both staying inside: the resolver resolves
/// the chain once and reports the real location.
#[test]
fn a_middle_link_chain_that_stays_inside_is_followed() {
    let (ws, _out, b) = setup();
    symlink(ws.path().join("real"), ws.path().join("d2")).unwrap();
    symlink(ws.path().join("d2"), ws.path().join("d")).unwrap();

    assert_eq!(b.resolve_read("d/b/c.txt").unwrap().rel, "real/b/c.txt");
    assert_eq!(b.resolve_write("d/b/c.txt").unwrap().rel, "real/b/c.txt");
}

/// A *relative* middle link is resolved against the link's own directory, which is what the
/// OS does when the resolver canonicalises the prefix. A link spelled `real` under `rel/`
/// therefore dangles (it means `rel/real`), and the dangle is refused as an escape rather
/// than reported as `not_found` — the deliberate uniformity of BND-18. A link spelled
/// `../real` is the one that works. Both are pinned so the distinction is a decision on
/// record rather than a surprise.
#[test]
fn a_relative_middle_link_is_resolved_against_its_own_directory() {
    let (ws, _out, b) = setup();
    fs::create_dir(ws.path().join("rel")).unwrap();
    symlink("real", ws.path().join("rel/dangling")).unwrap();
    symlink("../real", ws.path().join("rel/working")).unwrap();

    // `rel/dangling` means `rel/real`, which does not exist: refused as an escape, because a
    // link we cannot follow proves nothing about where it led (BND-18, T-20).
    assert_eq!(
        b.resolve_read("rel/dangling/b/c.txt").unwrap_err().code,
        ErrorCode::OutsideWorkspace
    );
    // `rel/working` really is `real/`, so it is followed.
    assert_eq!(
        b.resolve_read("rel/working/b/c.txt").unwrap().rel,
        "real/b/c.txt"
    );
}

/// The boundary that decides is every configured root, not just the workspace: a middle link
/// into a read root is inside *that* boundary and is readable, while the same link is never
/// writable — a read root is not writable however the path was spelled (BND-19).
#[test]
fn a_middle_link_into_a_read_root_is_inside_that_boundary() {
    let ws = tempfile::tempdir().unwrap();
    let rr = tempfile::tempdir().unwrap();
    fs::create_dir_all(ws.path().join("real/b")).unwrap();
    fs::write(ws.path().join("real/b/c.txt"), "inside").unwrap();
    fs::create_dir_all(rr.path().join("lib")).unwrap();
    fs::write(rr.path().join("lib/r.txt"), "read root").unwrap();

    symlink(rr.path(), ws.path().join("rr")).unwrap();
    let b = setup_with_read_root(&ws, &rr).unwrap();

    let r = b.resolve_read("rr/lib/r.txt").unwrap();
    assert!(
        r.rel.starts_with("@root1/"),
        "a read-root path is reported under its own root, got {:?}",
        r.rel
    );
    assert_eq!(
        b.resolve_write("rr/lib/r.txt").unwrap_err().code,
        ErrorCode::OutsideWorkspace,
        "a read root is never writable, link or no link"
    );
}

/// A `..` that lexically normalises away across a middle link must not be evaluated against
/// the link target's parent, and a link that is climbed *through* still lands inside. This
/// is the `dotdot_after_a_symlink_stays_in_the_root` case with the link in the middle rather
/// than at the front.
#[test]
fn dotdot_across_a_middle_link_does_not_reach_outside() {
    let (ws, out, b) = setup();
    symlink(out.path().join("x"), ws.path().join("a")).unwrap();

    // `a/..` is collapsed *lexically*, before anything is walked, so it means the
    // workspace root and not the link target's parent: `a/../c.txt` normalises to `c.txt`,
    // which is simply not there. That is `not_found`, not `outside_workspace`, and both are
    // refusals — the `..` never gets a chance to be applied after following the link.
    assert_eq!(
        b.resolve_read("a/../c.txt").unwrap_err().code,
        ErrorCode::NotFound
    );
    // These two climb far enough to leave the root, and are refused as escapes.
    for p in ["a/../../escape.txt", "a/b/../../../escape.txt"] {
        assert_eq!(
            b.resolve_read(p).unwrap_err().code,
            ErrorCode::OutsideWorkspace,
            "{p}"
        );
    }
    // Nothing in this family may succeed, whatever the code.
    for p in [
        "a/../c.txt",
        "a/../../escape.txt",
        "a/b/../../../escape.txt",
    ] {
        assert!(b.resolve_read(p).is_err(), "{p} must not resolve");
    }
    // Climbing through an inside link and back down is fine.
    assert_eq!(
        b.resolve_read("real/../real/b/c.txt").unwrap().rel,
        "real/b/c.txt"
    );
}

/// The refusal is decided on where the link *led*, so a middle link that leaves the
/// workspace cannot be smuggled past by spelling the tail differently.
#[test]
fn a_middle_link_leaving_the_workspace_is_refused_however_the_tail_is_spelled() {
    let (ws, out, b) = setup();
    symlink(out.path().join("x"), ws.path().join("a")).unwrap();

    for p in [
        "a/c.txt",
        "./a/c.txt",
        "a//c.txt",
        "a/./c.txt",
        "a/b/../c.txt",
        "a/c.txt/",
        "a/../a/c.txt",
    ] {
        assert_eq!(
            b.resolve_read(p).unwrap_err().code,
            ErrorCode::OutsideWorkspace,
            "{p}"
        );
    }
}

/// The write policy's own refusal must not be loosened by any of this: the final component
/// being a link is refused even when the link stays inside the workspace, while a middle
/// link that stays inside is fine. Both halves together are the whole policy in two lines.
#[test]
fn only_the_final_component_of_a_write_target_must_not_be_a_link() {
    let (ws, _out, b) = setup();
    // Leaf link, inside: refused.
    symlink(ws.path().join("real/b/c.txt"), ws.path().join("leaf")).unwrap();
    // Middle link, inside: allowed.
    symlink(ws.path().join("real"), ws.path().join("mid")).unwrap();

    assert!(
        b.resolve_write("leaf").is_err(),
        "a write target must be reached by a stable name (T-02, T-03)"
    );
    assert_eq!(b.resolve_write("mid/b/c.txt").unwrap().rel, "real/b/c.txt");
}

/// Absolute input spelling of the same shape: the link is still a link, and the answer is
/// still "inside or not", never "spelled absolutely".
#[test]
fn an_absolute_middle_link_leaving_the_workspace_is_refused() {
    let (ws, out, b) = setup();
    symlink(out.path().join("x"), ws.path().join("a")).unwrap();
    let abs = ws.path().join("a/c.txt");

    assert_eq!(
        b.resolve_read(abs.to_str().unwrap()).unwrap_err().code,
        ErrorCode::OutsideWorkspace
    );
}

/// Nothing here may panic, hang or loop: a middle link cycle is bounded like any other.
#[test]
fn a_middle_link_cycle_terminates() {
    let (ws, _out, b) = setup();
    fs::create_dir(ws.path().join("l")).unwrap();
    symlink(ws.path().join("l/x"), ws.path().join("l/y")).unwrap();
    symlink(ws.path().join("l/y"), ws.path().join("l/x")).unwrap();

    // `l` itself is a real directory, so it resolves — a cycle two levels down does not
    // make the parent unopenable.
    assert_eq!(b.resolve_read("l").unwrap().rel, "l");

    // Each link in the cycle, and a walk that goes through it, must terminate and refuse.
    // The refusal code differs with how far the walk got (`outside_workspace` when a link
    // was on the way, `io_error` when the cycle was hit mid-walk on a relative spelling),
    // and that difference is a property of BND-18's uniformity, not an escape: what matters
    // here is that every one of them terminates and none succeeds.
    for p in ["l/x", "l/y", "l/x/y", "l/x/c.txt", "l/y/x/y"] {
        assert!(b.resolve_read(p).is_err(), "{p} must be refused");
        assert!(b.resolve_write(p).is_err(), "{p} must be refused");
    }
}

/// The property the mutation proof in the ticket rests on: the *only* thing standing between
/// an escaping middle link and a read of `secret` is the containment check on the canonical
/// path. Take that check out and every refusal in this file becomes a successful read. This
/// test is here so the mutation is not "remove the containment check" alone but "remove the
/// middle-link case" as well.
#[test]
fn the_canonical_target_of_an_escaping_middle_link_is_outside() {
    let (ws, out, b) = setup();
    symlink(out.path().join("x"), ws.path().join("a")).unwrap();
    // Recording where the link leads, so the test asserts the shape of the attack and not
    // merely that the resolver said no. Both sides are canonicalised: on macOS the temp root
    // lives under /var, which is itself a symlink to /private/var, so a raw join on one side
    // and a canonicalise on the other compares two spellings of the same directory.
    let led_to: PathBuf = ws.path().join("a/c.txt").canonicalize().unwrap();
    assert_eq!(led_to, out.path().join("x/c.txt").canonicalize().unwrap());
    assert!(
        b.resolve_read("a/c.txt").is_err(),
        "a link that resolves to {} must be refused",
        led_to.display()
    );
    assert!(!Path::new(&led_to).starts_with(ws.path().canonicalize().unwrap()));
}
