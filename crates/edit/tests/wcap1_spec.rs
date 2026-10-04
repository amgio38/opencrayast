//! WCAP-1: write capability is minted only from a parsed [`WritePermission`].
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use opencrayast_core::config::Settings;
use opencrayast_edit::WriteCap;

/// WCAP-01: `allow_write = true` in the file produces a [`WritePermission`]; false does not.
#[test]
fn wcap01_permission_only_when_policy_allows() {
    let off = Settings::parse("").unwrap();
    assert!(off.write_permission().is_none());
    assert!(!off.policy.allow_write);

    let on = Settings::parse("[policy]\nallow_write = true\n").unwrap();
    assert!(on.policy.allow_write);
    assert!(
        on.write_permission().is_some(),
        "parse must mint WritePermission when allow_write is true"
    );

    let explicit_off = Settings::parse("[policy]\nallow_write = false\n").unwrap();
    assert!(explicit_off.write_permission().is_none());
}

/// WCAP-02: shell helper needs **both** the config token and `--allow-write`.
#[test]
fn wcap02_from_operator_needs_flag_and_permission() {
    let on = Settings::parse("[policy]\nallow_write = true\n").unwrap();
    assert!(WriteCap::from_operator(&on, true).is_some());
    assert!(
        WriteCap::from_operator(&on, false).is_none(),
        "flag alone off → no cap even if the file opted in"
    );

    let off = Settings::default();
    assert!(
        WriteCap::from_operator(&off, true).is_none(),
        "--allow-write alone cannot mint without WritePermission"
    );
}

/// WCAP-04: `Settings::default()` never carries a WritePermission (writing is off).
#[test]
fn wcap04_default_settings_have_no_permission() {
    let defaults = Settings::default();
    assert!(defaults.write_permission().is_none());
}

/// WCAP-03: flipping the public `policy.allow_write` bool on a default Settings does
/// **not** create a permission token — the token only comes from parse/load.
#[test]
fn wcap03_mutating_policy_bool_does_not_mint_permission() {
    let mut s = Settings::default();
    s.policy.allow_write = true;
    assert!(
        s.write_permission().is_none(),
        "a dependent that only flips the bool must not obtain WritePermission"
    );
    assert!(WriteCap::from_operator(&s, true).is_none());
}
