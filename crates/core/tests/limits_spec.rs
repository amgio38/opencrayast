//! Spec for ISSUE-CORE-LIMITS (CFG-04).
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::field_reassign_with_default
)]
use opencrayast_core::ErrorCode;
use opencrayast_core::limits::Limits;

#[test]
fn defaults_match_the_documentation_and_validate() {
    let l = Limits::default();
    assert_eq!(l.max_file_bytes, 4 * 1024 * 1024);
    assert_eq!(l.max_output_bytes, 64 * 1024);
    assert_eq!(l.max_results, 200);
    assert_eq!(l.plan_ttl_minutes, 15);
    assert_eq!(l.plan_max_files, 50);
    assert_eq!(l.plan_max_edits, 500);
    assert_eq!(l.plan_max_changed_bytes, 1024 * 1024);
    assert_eq!(l.plan_max_plans, 100);
    assert_eq!(l.plan_max_plans_per_process, 25);
    assert_eq!(l.journal_max_plan_mib, 64);
    assert_eq!(l.journal_retention_days, 7);
    assert_eq!(l.note_max_bytes, 1024);
    assert!(l.validate().is_ok());
}

#[test]
fn above_hard_max_is_rejected_naming_the_field() {
    let mut l = Limits::default();
    l.max_file_bytes = 16 * 1024 * 1024 + 1;
    let e = l.validate().unwrap_err();
    assert_eq!(e.code, ErrorCode::InvalidArgs);
    assert!(e.message.contains("max_file_bytes"), "{}", e.message);
}

#[test]
fn exactly_hard_max_is_allowed_and_zero_is_rejected() {
    let mut l = Limits::default();
    l.max_file_bytes = 16 * 1024 * 1024;
    l.plan_max_edits = 5000;
    assert!(l.validate().is_ok());
    l.plan_max_edits = 0;
    assert!(l.validate().is_err());
}
