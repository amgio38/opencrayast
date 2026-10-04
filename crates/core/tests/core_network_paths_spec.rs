//! Windows-shaped paths must be refused by the string layer, before any filesystem call
//! (BND-11, BND-18, BND-24).
//!
//! These cases come from a Windows CI failure in `core_hostile_spec`: on Windows
//! `resolve_read("C:.:\\WiWi")` answered `io_error` where a missing path answers
//! `outside_workspace`, and `workspace_id("\\\\ser\\share")` spent its whole budget waiting on a
//! DNS/SMB timeout. On Linux both are merely odd file names, which is exactly why these tests
//! can run here: the rule under test is pure string analysis, and the *evidence* that it
//! happens before the filesystem is the error code. A path that does not exist and is refused
//! as `invalid_args` could not have been refused by a canonicalisation, which would have said
//! `not_found`.
//!
//! Portable on purpose: no `cfg`, so this file is also compiled by the Windows job that
//! found the bug.
//!
//! One correction to that rule, made here: `\\?\C:\x` is **not** a network path. It is a local
//! volume in the verbatim (extended-length) spelling, and it is what Windows' own
//! `canonicalize` returns for an ordinary `C:\...` root - so refusing it made two of the tests
//! below fail on the Windows job. The two leading separators of `\\?\` are only a UNC prefix
//! when what follows is not a drive; `\\?\UNC\...`, `\\?\GLOBALROOT\...`, `\\?\Volume{...}\...`
//! and `\\.\...` are not local disks and stay refused. See `core::boundary`.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use opencrayast_core::ErrorCode;
use opencrayast_core::boundary::{Boundary, BoundaryConfig};
use opencrayast_core::limits::Limits;
use opencrayast_core::workspace::workspace_id;
use std::path::Path;

/// Every spelling of "somewhere else": drives in all their forms, UNC in the mixtures a
/// JSON payload or a URL parser produces, and the two Windows device prefixes.
const ELSEWHERE: &[&str] = &[
    // Drives. The last two are the shape that got through: `X:` with a *third byte*.
    "C:\\x",
    "C:/x",
    "C:foo",
    "C:",
    "C:.:\\WiWi",
    "c:.:\\x",
    "z:x",
    // UNC, including the mixed separators and the device prefixes. `\\?\C:\x` is *not* here:
    // it is a local volume, and it is what `canonicalize` returns on Windows for an ordinary
    // `C:\...` root, so refusing it refuses the most common Windows root there is. The
    // namespaces that share the prefix but are not a local disk are here, and stay refused.
    "\\\\ser\\share",
    "//ser/share",
    "\\/ser/share",
    "/\\ser",
    "\\\\?\\UNC\\srv\\share",
    "\\\\?\\GLOBALROOT\\Device\\HarddiskVolumeShadowCopy1",
    "\\\\.\\pipe\\x",
    "\\\\.\\C:\\x",
];

/// A workspace root that names the network. Not one spelling of them - the whole point is
/// that a caller cannot reach the network through any of these spellings.
const NETWORK_ROOTS: &[&str] = &[
    "\\\\ser\\share",
    "//ser/share",
    "\\/ser/share",
    "/\\ser",
    "\\\\?\\UNC\\srv\\share",
    "\\\\?\\GLOBALROOT\\Device\\HarddiskVolumeShadowCopy1",
    "\\\\?\\Volume{12345678-1234-1234-1234-123456789abc}\\x",
    "\\\\.\\pipe\\x",
    "\\\\.\\C:\\x",
    // Drive-relative: on Windows this canonicalises to the current directory of that drive,
    // so as a root it is a location escape, not just an odd spelling.
    "C:proj",
    "C:",
    "c:.:\\WiWi",
];

/// The verbatim **disk** spelling is a local path and stays a legal root: `\\?\` with a drive
/// letter, a colon and a separator behind it. It is the one form the string rule must let
/// through, and it is exactly what Windows' own `canonicalize` produces for a `C:\...` root,
/// so refusing it is what made this file's own tests fail on the Windows job.
const VERBATIM_DISK_ROOTS: &[&str] = &["\\\\?\\C:\\x", "\\\\?\\c:/x", "\\\\?\\Z:\\Users\\me"];

/// The oracle: a path outside the workspace and a path that does not exist must be
/// indistinguishable, whatever shape the attacker spells them in (BND-18, T-20).
#[test]
fn an_attacker_shaped_path_is_refused_as_outside_and_never_as_an_io_error() {
    let ws = tempfile::tempdir().unwrap();
    std::fs::write(ws.path().join("a.rs"), "fn a() {}\n").unwrap();
    let boundary = Boundary::new(BoundaryConfig {
        root: ws.path().to_path_buf(),
        limits: Limits::default(),
        read_roots: Vec::new(),
        state_dir: None,
        extra_protected: Vec::new(),
    })
    .unwrap();

    for path in ELSEWHERE {
        for result in [boundary.resolve_read(path), boundary.resolve_write(path)] {
            let code = result.as_ref().err().map(|e| e.code);
            assert_eq!(
                code,
                Some(ErrorCode::OutsideWorkspace),
                "{path:?} must be refused as outside the workspace; an io_error here is the \
                 probe oracle the resolver exists to prevent (got {result:?})"
            );
        }
    }

    // Alternate data streams: bytes that belong to no file the agent can reason about, and a
    // write target no `stat` ever mentions. Refused as outside, like every other spelling of
    // "not in here".
    for path in ["dir/file.txt:stream", "dir/a:b/c", "a/b:c"] {
        for result in [boundary.resolve_read(path), boundary.resolve_write(path)] {
            assert_eq!(
                result.as_ref().err().map(|e| e.code),
                Some(ErrorCode::OutsideWorkspace),
                "{path:?} is an alternate data stream and must be refused as outside \
                 (got {result:?})"
            );
        }
    }

    // The extended-length / device prefix is refused *anywhere* in an agent path, not only
    // leading. `\\?\` means "the rest of this is not normalised": the double separator stops
    // `..` collapsing, so a `..` behind it is passed to the object manager as a real parent
    // reference. An operator root may use the verbatim *disk* form (it is what
    // `canonicalize` returns on Windows); an agent path never legitimately needs it, because
    // it is never canonicalisation output.
    for path in [
        "\\\\?\\C:\\x",
        "\\\\?\\UNC\\srv\\share",
        "a/\\\\?\\b",
        "a/\\\\?\\..",
        "dir\\\\?\\..\\..",
        "dir\\\\.\\x",
        "a/\\\\/?\\b",
        "a/\\\\?/b",
    ] {
        for result in [boundary.resolve_read(path), boundary.resolve_write(path)] {
            assert_eq!(
                result.as_ref().err().map(|e| e.code),
                Some(ErrorCode::OutsideWorkspace),
                "{path:?} carries an extended-length or device prefix and must be refused \
                 lexically, wherever it appears (got {result:?})"
            );
        }
    }

    // And the control: a path that is merely absent still answers `not_found` - so the
    // refusals above really are the shape rules and not a blanket "everything is missing".
    assert_eq!(
        boundary
            .resolve_read("definitely-not-here.rs")
            .unwrap_err()
            .code,
        ErrorCode::NotFound
    );
}

/// `workspace_id` and `Boundary::new` must judge the string before canonicalising it: on
/// Windows, canonicalising a UNC path resolves a host name and opens an SMB session, which for
/// an attacker-chosen host is a DNS lookup plus an NTLM challenge sent to a machine they
/// control (BND-24, S-5).
///
/// The evidence that nothing was touched: none of these paths exists, anywhere, on any
/// platform - so a canonicalisation that ran would answer `not_found`. `invalid_args` can only
/// have come from the string check.
#[test]
fn a_network_root_is_refused_before_the_filesystem_is_consulted() {
    // Every call site that takes an operator-supplied root, and one helper that asserts the
    // *string* rule answered - not merely that something refused.
    let refused_by_the_string_rule = |err: &opencrayast_core::ToolError, what: &str| {
        assert_eq!(
            err.code,
            ErrorCode::InvalidArgs,
            "{what} must be refused as invalid_args without canonicalising it"
        );
        // The code alone is not the evidence. A path that does not exist is also
        // invalid_args, and a canonicalisation that ran would say exactly that; only the
        // string check can produce this wording, so the wording is what proves the order.
        assert!(
            err.message
                .to_lowercase()
                .contains("network, device and drive-relative"),
            "{what} must be refused by the string rule, before the filesystem is touched: {:?}",
            err.message
        );
    };

    for path in NETWORK_ROOTS {
        refused_by_the_string_rule(
            &workspace_id(Path::new(path)).unwrap_err(),
            &format!("workspace_id({path:?})"),
        );
        refused_by_the_string_rule(
            &Boundary::new(BoundaryConfig {
                root: Path::new(path).to_path_buf(),
                limits: Limits::default(),
                read_roots: Vec::new(),
                state_dir: None,
                extra_protected: Vec::new(),
            })
            .unwrap_err(),
            &format!("a network workspace root {path:?}"),
        );
        refused_by_the_string_rule(
            &Boundary::new(BoundaryConfig {
                root: ws_root(),
                read_roots: vec![Path::new(path).to_path_buf()],
                limits: Limits::default(),
                state_dir: None,
                extra_protected: Vec::new(),
            })
            .unwrap_err(),
            &format!("a network read root {path:?}"),
        );
    }
}

/// The same reason for every spelling, so refusing a UNC path, a device path and a
/// drive-relative path are indistinguishable to a caller - which is why the check runs before
/// `canonicalize` and not after it. A message that named the offending spelling would be an
/// oracle of its own.
///
/// `C:\x` is in the control set, not here: a separator after the drive letter is an ordinary
/// absolute path, and on Windows it is the most ordinary workspace root there is.
#[test]
fn every_network_spelling_is_refused_with_one_message() {
    let mut messages = std::collections::BTreeSet::new();
    for path in NETWORK_ROOTS {
        let err = workspace_id(Path::new(path)).unwrap_err();
        assert_eq!(err.code, ErrorCode::InvalidArgs);
        messages.insert(err.message);
    }
    assert_eq!(messages.len(), 1, "one reason, one message: {messages:?}");
    let root = ws_root();
    assert!(
        messages
            .iter()
            .next()
            .unwrap()
            .to_lowercase()
            .contains("network, device and drive-relative"),
        "the message must say what is refused without naming the spelling"
    );
    // The legal spellings are not swept up with the illegal ones.
    assert!(
        workspace_id(&root.canonicalize().unwrap()).is_ok(),
        "a directory under a single leading slash is still a candidate root"
    );
    // ...and neither are the verbatim disk roots. This is the assertion that used to fail on
    // Windows: `canonicalize` there hands back `\\?\C:\...`, which rule 3 took for a device
    // path. It is not - it is a local volume - so the string rule must let it through, and
    // the test has to say so on every platform or the Windows job is the only thing that can
    // catch a regression.
    for path in VERBATIM_DISK_ROOTS {
        // The string rule must be the thing that allows it: on Linux these are merely odd
        // relative names, so `not_found` (reached by the filesystem) is what "the string rule
        // allowed it" looks like here, as opposed to `invalid_args` (refused by the string).
        let err = workspace_id(Path::new(path)).unwrap_err();
        assert_ne!(
            err.code,
            ErrorCode::InvalidArgs,
            "{path:?} is a local disk, not a network or device spelling, so the string rule \
             must not refuse it"
        );
    }
}

/// The check is about network paths, not about absolute paths: `/tmp` and a relative `proj`
/// are ordinary roots and must stay candidates. A rule that refused every absolute path would
/// pass every test above and be useless.
#[test]
fn an_ordinary_absolute_path_is_still_a_candidate_root() {
    let ws = tempfile::tempdir().unwrap();
    // A real directory under a single leading slash still gets an id.
    let id = workspace_id(&ws.path().canonicalize().unwrap()).unwrap();
    assert!(id.starts_with("w-") && id.len() == 34, "{id}");
    // A path that does not exist is still `not_found` - the network rule did not swallow it.
    assert_eq!(
        workspace_id(Path::new("/definitely/not/here"))
            .unwrap_err()
            .code,
        ErrorCode::NotFound
    );
    // And a relative spelling is judged later, on existence, exactly as before.
    assert_eq!(
        workspace_id(Path::new("relative-project-dir"))
            .unwrap_err()
            .code,
        ErrorCode::NotFound
    );
    // The verbatim disk spelling is an absolute path on a *local* volume, so it belongs in
    // this test rather than in the network one. On Windows `canonicalize` returns exactly
    // this form, which is what used to make this test fail there: rule 3 read the two leading
    // separators of `\\?\` as "UNC" and refused a path that names no host at all.
    for path in VERBATIM_DISK_ROOTS {
        let err = workspace_id(Path::new(path)).unwrap_err();
        assert_eq!(
            err.code,
            ErrorCode::NotFound,
            "{path:?} is refused by the filesystem (it does not exist here), not by the string \
             rule - which is the definition of a local path that was allowed through (got {err:?})"
        );
    }
}

fn ws_root() -> std::path::PathBuf {
    let dir = std::env::temp_dir().join("core-network-paths-spec-root");
    std::fs::create_dir_all(&dir).unwrap();
    dir
}
