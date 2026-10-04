//! Extra EDIT4-xx cases for ISSUE-EDIT-4 (do not weaken journal_model_spec).
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use opencrayast_core::ErrorCode;
use opencrayast_core::hash::ContentHash;
use opencrayast_edit::{
    FileClass, JournalFile, JournalState, Manifest, Recovery, classify, plan_recovery, plan_undo,
};

const WS: &str = "w-00112233445566778899aabbccddeeff";
const PID: &str = "p-jpqn7mmlutagkvspztoqj7hxra";

fn h(b: &[u8]) -> ContentHash {
    ContentHash::of(b)
}

fn sample() -> Manifest {
    Manifest {
        plan_id: PID.into(),
        plan_digest: h(b"a plan that is not this one"),
        workspace_id: WS.into(),
        state: JournalState::Writing,
        files: vec![
            JournalFile {
                path: "a.rs".into(),
                pre_hash: h(b"before"),
                post_hash: h(b"after"),
            },
            JournalFile {
                path: "b/c.rs".into(),
                pre_hash: h(b"x"),
                post_hash: h(b"y"),
            },
        ],
        progress: 1,
        created_at: 100,
        updated_at: 105,
    }
}

/// EDIT4-01: diverged message lists every path with pre|post|other and never quotes content.
#[test]
fn edit4_01_diverged_message_lists_every_classification() {
    let m = sample();
    let cur = vec![Some(h(b"after")), Some(h(b"FOREIGN_BYTES"))];
    let e = plan_recovery(&m, &cur).unwrap_err();
    assert_eq!(e.code, ErrorCode::Diverged);
    assert!(e.message.contains("a.rs: post"), "{}", e.message);
    assert!(e.message.contains("b/c.rs: other"), "{}", e.message);
    assert!(!e.message.contains("FOREIGN"));
}

/// EDIT4-02: Undoing → Applied is a legal transition (abort undo).
#[test]
fn edit4_02_undoing_can_become_applied() {
    assert!(JournalState::Undoing.can_become(JournalState::Applied));
    assert!(!JournalState::Applied.can_become(JournalState::Writing));
}

/// EDIT4-03: `prepared` is checked against the filesystem, not believed.
///
/// This test used to pass `&[None, None]` — both files *missing* — and expect a bare
/// `MarkRolledBack`, which is the defect: "prepared means nothing was touched" is a claim stored in
/// one unauthenticated field, and trusting it is how a forged or torn manifest reports a rollback
/// that never happened while edited files stay edited and the journal goes terminal. The claim is
/// now verified (E-16), and a missing file is `other`, so the whole call is refused instead.
#[test]
fn edit4_03_prepared_is_verified_against_the_files_not_believed() {
    let mut m = sample();
    m.state = JournalState::Prepared;
    // Every target still at pre_hash: the claim holds, and there is nothing to restore.
    assert_eq!(
        plan_recovery(&m, &[Some(h(b"before")), Some(h(b"x"))]).unwrap(),
        Recovery::MarkRolledBack
    );
    // A target that does match post_hash: the claim is FALSE, so the originals are restored rather
    // than thrown away — same direction, honest work.
    assert_eq!(
        plan_recovery(&m, &[Some(h(b"after")), Some(h(b"x"))]).unwrap(),
        Recovery::RestoreThenRolledBack(vec![0])
    );
    // A target that matches neither: `diverged`, and nothing is written.
    let e = plan_recovery(&m, &[Some(h(b"before")), None]).unwrap_err();
    assert_eq!(e.code, ErrorCode::Diverged);
    assert!(e.message.contains("a.rs: pre"), "{}", e.message);
    assert!(e.message.contains("b/c.rs: other"), "{}", e.message);
}

/// EDIT4-04: plan_undo message names the actual state.
#[test]
fn edit4_04_plan_undo_names_wrong_state() {
    let mut m = sample();
    m.state = JournalState::Writing;
    let e = plan_undo(&m, &[Some(h(b"after")), Some(h(b"y"))]).unwrap_err();
    assert_eq!(e.code, ErrorCode::InvalidArgs);
    assert!(e.message.contains("writing"), "{}", e.message);
}

/// EDIT4-05: classify None is Other even when pre equals post.
#[test]
fn edit4_05_missing_file_is_other_when_hashes_equal() {
    let f = JournalFile {
        path: "a".into(),
        pre_hash: h(b"same"),
        post_hash: h(b"same"),
    };
    assert_eq!(classify(None, &f), FileClass::Other);
    assert_eq!(classify(Some(&h(b"same")), &f), FileClass::Post);
}
