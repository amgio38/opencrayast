//! Spec for ISSUE-CORE-WORKSPACE-LOCK (STA-07, EDT-07 core part, T-13).
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::field_reassign_with_default
)]
#![cfg(unix)]
use opencrayast_core::ErrorCode;
use opencrayast_core::workspace::*;
use std::os::unix::fs::{PermissionsExt, symlink};
use std::time::{Duration, Instant};

#[test]
fn id_shape_stable_and_distinct() {
    let a = tempfile::tempdir().unwrap();
    let b = tempfile::tempdir().unwrap();
    let ia = workspace_id(a.path()).unwrap();
    assert_eq!(ia, workspace_id(a.path()).unwrap());
    assert_ne!(ia, workspace_id(b.path()).unwrap());
    assert!(ia.starts_with("w-") && ia.len() == 2 + 32);
    assert!(
        ia[2..]
            .bytes()
            .all(|c| matches!(c, b'0'..=b'9' | b'a'..=b'f'))
    );
}

#[test]
fn two_spellings_of_one_root_share_an_id() {
    let a = tempfile::tempdir().unwrap();
    let hold = tempfile::tempdir().unwrap();
    let link = hold.path().join("alias");
    symlink(a.path(), &link).unwrap();
    assert_eq!(
        workspace_id(a.path()).unwrap(),
        workspace_id(&link).unwrap()
    );
}

#[test]
fn lock_is_exclusive_times_out_busy_and_releases_on_drop() {
    let st = tempfile::tempdir().unwrap();
    std::fs::set_permissions(st.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let first = ApplyLock::acquire(
        st.path(),
        "w-00112233445566778899aabbccddeeff",
        Duration::from_millis(200),
    )
    .unwrap();
    let t = Instant::now();
    let e = ApplyLock::acquire(
        st.path(),
        "w-00112233445566778899aabbccddeeff",
        Duration::from_millis(300),
    )
    .unwrap_err();
    assert_eq!(e.code, ErrorCode::Busy);
    assert!(
        t.elapsed() >= Duration::from_millis(250),
        "must actually wait for the timeout"
    );
    drop(first);
    assert!(
        ApplyLock::acquire(
            st.path(),
            "w-00112233445566778899aabbccddeeff",
            Duration::from_millis(200)
        )
        .is_ok()
    );
}

#[test]
fn different_workspaces_do_not_block_each_other() {
    let st = tempfile::tempdir().unwrap();
    let _a = ApplyLock::acquire(
        st.path(),
        "w-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        Duration::from_millis(100),
    )
    .unwrap();
    assert!(
        ApplyLock::acquire(
            st.path(),
            "w-bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
            Duration::from_millis(100)
        )
        .is_ok()
    );
}

#[test]
fn rejects_malformed_workspace_ids() {
    let st = tempfile::tempdir().unwrap();
    for bad in ["", "../x", "w-../..", "w-xyz"] {
        assert!(
            ApplyLock::acquire(st.path(), bad, Duration::from_millis(10)).is_err(),
            "{bad}"
        );
    }
}
