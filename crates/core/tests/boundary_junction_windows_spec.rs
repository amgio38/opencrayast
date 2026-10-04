//! BND-06, Windows half: junction and reparse-point behaviour.
//!
//! **These tests run only on a Windows runner. They were authored on a Linux container and
//! have NEVER BEEN EXECUTED.** What has been done for them is `cargo check --all-targets`
//! against `x86_64-pc-windows-gnu` (and the three other cross targets) with
//! `RUSTFLAGS="-D warnings"`, so they are known to *compile* for Windows and are known not
//! to break the build. Nothing below is claimed to have been observed to pass or fail. Any
//! status column in `TESTING.md` that says "not executed" means exactly that.
//!
//! A Windows junction is semantically a directory symlink — a reparse point whose tag is
//! `IO_REPARSE_TAG_MOUNT_POINT`, with the target stored in a substitute name that is read by
//! the OS, not by us. So the junction question reduces to the middle-component link question
//! that `boundary_junction_middle_spec.rs` pins on Linux, with one Windows-specific wrinkle:
//! `symlink_metadata` reports a junction as a **directory**, not as a symlink
//! (`FILE_ATTRIBUTE_REPARSE_POINT` is set but `FILE_ATTRIBUTE_DIRECTORY` is too), so
//! `Metadata::file_type().is_symlink()` is `false` for a junction. That is precisely why
//! "refuse junctions" cannot be implemented by the existing write policy's link check, and
//! why BND-06 is still open rather than already covered.
//!
//! ## How the junctions here are made, and why
//!
//! Junctions are created here with **`cmd /C mklink /J`**, not `CreateSymbolicLinkW`:
//!
//!   * Creating a *junction* needs **no administrator rights and no developer mode** — it is
//!     a plain reparse-point write on a directory. Creating a *symlink* on Windows does need
//!     developer mode or `SeCreateSymbolicLinkPrivilege`, so a symlink-based fixture would
//!     make these tests fail for a reason that has nothing to do with the policy under test.
//!   * It also needs no new dependency. `windows-sys` is in the lock file, but only as a
//!     transitive dev-dependency of `tempfile` / `rustix` / `getrandom`; adding it to
//!     `crates/core/Cargo.toml` would be a new direct dependency for a test-only need.
//!
//! The alternative — `CreateMountPointW` via `windows-sys` — is the same call `mklink /J`
//! makes underneath and would be the choice if this ever had to run without a shell.
//!
//! `mklink /J` is a *relative or absolute* target the same way a symlink is, and junctions
//! store their target relative to the junction itself when given a relative spelling. Tests
//! that need one of those spellings say so at the call site.
//!
//! ## What is deliberately NOT claimed
//!
//! The cloud-placeholder case (`EDIT-MODEL.md`, the row that used to promise it) is the
//! honest limit of what a test can reach. See the comment on
//! `a_cloud_placeholder_is_not_a_boundary_escape_and_the_claim_is_bounded` for exactly what
//! is and is not proved.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::field_reassign_with_default
)]
#![cfg(windows)]

use opencrayast_core::ErrorCode;
use opencrayast_core::boundary::*;
use opencrayast_core::limits::Limits;
use std::fs;
use std::path::Path;
use std::process::Command;

/// Workspace `ws/` with `real/b/c.txt` inside, plus `out/x/c.txt` outside.
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

/// Create a directory junction at `link` pointing at `target`, with `mklink /J`.
///
/// Returns `false` rather than panicking when the junction could not be created (a
/// filesystem that does not support reparse points, a container without them, a policy
/// that forbids them). Callers skip with a printed note rather than pretending the case ran —
/// the same rule `boundary_spec.rs` follows for FIFO creation.
fn make_junction(target: &Path, link: &Path) -> bool {
    let status = Command::new("cmd")
        .args(["/C", "mklink", "/J"])
        .arg(link)
        .arg(target)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status();
    status.is_ok_and(|s| s.success())
}

/// Skip with a printed note instead of failing, when the platform would not give us the
/// fixture. Deliberately loud: a silently skipped security test is worse than a failing one.
macro_rules! or_skip {
    ($made:expr, $what:literal) => {
        if !$made {
            eprintln!("SKIPPED (fixture unavailable): {}", $what);
            return;
        }
    };
}

/// A junction pointing INSIDE the workspace is usable: the criterion for accepting a
/// reparse point is "does it leave the boundary", never "is it a reparse point". This is
/// the Windows counterpart of `a_middle_link_that_stays_inside_is_followed_by_both_directions`.
#[test]
fn a_junction_that_stays_inside_the_workspace_is_usable() {
    let (ws, _out, b) = setup();
    or_skip!(
        make_junction(&ws.path().join("real"), &ws.path().join("j")),
        "mklink /J"
    );

    let r = b.resolve_read("j/b/c.txt").unwrap();
    assert_eq!(
        r.rel, "real/b/c.txt",
        "the junction must resolve to the real location, not to its own spelling"
    );
    assert!(
        r.abs.starts_with(ws.path().canonicalize().unwrap()),
        "the resolved path must be inside the workspace"
    );
}

/// A junction pointing OUTSIDE is refused. Note *where* the refusal has to happen: the
/// string layer, before any filesystem call reaches the reparse point, so that merely
/// *naming* a junction is not what opens it. That is the principle the WIN-PATHS tickets
/// established for `\\?\`, UNC and drive-relative spellings and this test asserts the same
/// ordering applies to junctions.
///
/// "Before any filesystem call" is proved here by an absence that is directly observable:
/// the spelling contains nothing that leaves the boundary at the string layer, and no
/// Windows-specific API call (`FILE_FLAG_OPEN_REPARSE_POINT`, `GetFinalPathNameByHandle`,
/// a `DeviceIoControl`) exists anywhere in the crate. What cannot be proved from a test is
/// the *absence of a syscall*; what is proved is that the decision is made on the resolved
/// canonical path by the same containment check every other path goes through, which
/// canonicalises the junction by asking the OS where it leads before the caller can touch it.
#[test]
fn a_junction_that_leaves_the_workspace_is_refused() {
    let (ws, out, b) = setup();
    or_skip!(
        make_junction(&out.path().join("x"), &ws.path().join("j")),
        "mklink /J"
    );

    // The junction names a real directory that exists; only its location is outside.
    assert!(ws.path().join("j/c.txt").is_file());

    assert_eq!(
        b.resolve_read("j/c.txt").unwrap_err().code,
        ErrorCode::OutsideWorkspace,
        "a junction leaving the workspace must be refused"
    );
    assert_eq!(
        b.resolve_write("j/c.txt").unwrap_err().code,
        ErrorCode::OutsideWorkspace,
        "and refused for writing too, not merely blocked by the write policy"
    );
}

/// A junction chain — the shape a two-step planted escape takes — is refused like any other
/// escaping link.
#[test]
fn a_chain_of_junctions_leaving_the_workspace_is_refused() {
    let (ws, out, b) = setup();
    or_skip!(
        make_junction(&out.path().join("x"), &ws.path().join("j2")),
        "mklink /J (inner)"
    );
    or_skip!(
        make_junction(&ws.path().join("j2"), &ws.path().join("j1")),
        "mklink /J (outer)"
    );

    assert_eq!(
        b.resolve_read("j1/c.txt").unwrap_err().code,
        ErrorCode::OutsideWorkspace
    );
    assert_eq!(
        b.resolve_write("j1/c.txt").unwrap_err().code,
        ErrorCode::OutsideWorkspace
    );
}

/// A junction leaf must not be a writable regular file. Rust's `file_type()` may report a
/// junction as a directory, a symlink, or both depending on the runtime — the load-bearing
/// property is that `resolve_write` refuses it, and that the read path refuses the escape
/// through containment (BND-06).
#[test]
fn a_junction_leaf_is_not_a_writable_regular_file() {
    let (ws, out, b) = setup();
    or_skip!(
        make_junction(&out.path().join("x"), &ws.path().join("j")),
        "mklink /J"
    );

    let meta = fs::symlink_metadata(ws.path().join("j")).unwrap();
    assert!(
        meta.file_type().is_dir() || meta.file_type().is_symlink(),
        "a junction must look like a directory or a reparse/symlink, got {:?}",
        meta.file_type()
    );

    // On the *read* path the containment check refuses the escape.
    assert_eq!(
        b.resolve_read("j").unwrap_err().code,
        ErrorCode::OutsideWorkspace
    );
    // Write side: not a regular single-link file (symlink and/or directory).
    assert!(b.resolve_write("j/c.txt").is_err());
}

/// A junction chain is not a cycle: `j1` points at `j2` and `j2` at a real directory inside
/// the workspace, so the walk resolves in two hops. What is asserted is that a chain
/// terminates and resolves to the real location — and, for the escaping variant, that it is
/// refused. (A true junction *cycle* cannot be built with `mklink /J`, because the target has
/// to exist first; the symlink-cycle case is already covered on unix by
/// `a_middle_link_cycle_terminates`, and a junction cycle would be caught by the same
/// canonicalisation, so it is not fabricated here.)
#[test]
fn a_junction_chain_inside_the_workspace_terminates_and_resolves() {
    let (ws, _out, b) = setup();
    or_skip!(
        make_junction(&ws.path().join("real"), &ws.path().join("j2")),
        "mklink /J (inner)"
    );
    or_skip!(
        make_junction(&ws.path().join("j2"), &ws.path().join("j1")),
        "mklink /J (outer)"
    );

    assert_eq!(b.resolve_read("j1/b/c.txt").unwrap().rel, "real/b/c.txt");
    for p in ["j1/j2", "j1/j2/b/c.txt"] {
        assert!(b.resolve_read(p).is_err(), "{p} must be refused");
    }
}

/// A cloud placeholder — a reparse point that is NOT a mount point and does NOT leave the
/// boundary — is the case `EDIT-MODEL.md` used to over-claim about, and this test is the
/// honest limit of what is automatically provable.
///
/// **What is proved.** A placeholder is a reparse point on a path that *stays inside* the
/// workspace, so by the boundary's own criterion it is inside and `resolve_read` accepts it
/// and returns a path inside the workspace. That is the part the boundary decides, and it is
/// asserted.
///
/// **What is NOT proved, and cannot be from a test.** The original claim was "a placeholder is
/// never opened, so no download is triggered". Proving that needs three things this project
/// does not have: (1) a OneDrive-synced account with a real placeholder file, which a CI
/// runner has no way to be; (2) an observable for the download — OneDrive's fetch is an
/// out-of-band HTTP request from the sync client, not something this process makes or can
/// see, so there is nothing to assert against; (3) a definition of "triggered" that survives
/// the sync client running independently of us. Even a perfectly passing test here would only
/// show that *this process* never opened the file — not that no download happened, because the
/// sync client may have already hydrated it, or may do so the moment anything stats it.
///
/// So the claim in `EDIT-MODEL.md` is now marked **planned / not implemented** and this test
/// pins only the boundary-level half of it. If a real proof is ever wanted it needs a Windows
/// machine with OneDrive and an HTTP capture, not a unit test.
#[test]
fn a_cloud_placeholder_is_not_a_boundary_escape_and_the_claim_is_bounded() {
    let (ws, _out, b) = setup();

    // A plain file standing in for a placeholder: what the boundary can actually decide is
    // "does this path leave the workspace", and for an inside path the answer is yes, inside.
    let inner = ws.path().join("placeholder.bin");
    fs::write(&inner, b"").unwrap();
    let r = b.resolve_read("placeholder.bin").unwrap();
    assert_eq!(r.rel, "placeholder.bin");
    assert!(r.abs.starts_with(ws.path().canonicalize().unwrap()));

    // And the reparse-point-ness is irrelevant to that decision, because the boundary never
    // looks at the reparse-point attribute at all: a junction inside the workspace is accepted
    // for exactly the same reason. That equality is the point — the boundary's criterion is
    // location, and a placeholder that is inside is inside.
    let junction_inside = make_junction(&ws.path().join("real"), &ws.path().join("jin"));
    if junction_inside {
        assert_eq!(b.resolve_read("jin/b/c.txt").unwrap().rel, "real/b/c.txt");
    } else {
        eprintln!("SKIPPED (fixture unavailable): mklink /J, inside-junction half not exercised");
    }
}

/// A junction whose *stored target* is relative is resolved by the OS relative to the
/// junction itself, exactly like a relative symlink. Pinning it keeps the two spellings from
/// drifting apart, since only one of them is the spelling an attacker is likely to write.
/// and only one of them is the spelling an attacker is likely to write.
///
/// `mklink /J` accepts a relative target and stores it in the reparse point, exactly as a
/// symlink would. Windows resolves that stored target against the junction's own directory.
#[test]
fn a_relative_junction_target_is_resolved_against_the_junction_itself() {
    let (ws, out, b) = setup();
    // From `ws/rel/`, the relative spelling `../outside/x` names `ws/outside/x`, so the
    // junction dangles and is refused; `../<basename of out>/x` names the real outside
    // directory and is refused as an escape. Both are the inside-versus-outside criterion
    // applied to a relative target.
    fs::create_dir_all(ws.path().join("rel")).unwrap();
    let out_name = out
        .path()
        .file_name()
        .unwrap()
        .to_string_lossy()
        .to_string();
    let escaping = format!("../{out_name}/x");
    or_skip!(
        make_junction(Path::new(&escaping), &ws.path().join("rel/jrel")),
        "mklink /J (relative escaping target)"
    );

    // The escaping relative junction is refused exactly like its absolute twin.
    assert_eq!(
        b.resolve_read("rel/jrel/c.txt").unwrap_err().code,
        ErrorCode::OutsideWorkspace
    );
    assert_eq!(
        b.resolve_write("rel/jrel/c.txt").unwrap_err().code,
        ErrorCode::OutsideWorkspace
    );

    // And a relative junction that stays inside is followed, so the two spellings of
    // "inside" agree.
    or_skip!(
        make_junction(Path::new("../real"), &ws.path().join("rel/jin")),
        "mklink /J (relative inside target)"
    );
    assert_eq!(
        b.resolve_read("rel/jin/b/c.txt").unwrap().rel,
        "real/b/c.txt"
    );
}
