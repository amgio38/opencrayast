//! Spec for ISSUE-CORE-HASH-ID. Add more cases; never weaken these.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::field_reassign_with_default
)]
use opencrayast_core::hash::*;

#[test]
fn sha256_known_vector() {
    // sha256("abc")
    let h = ContentHash::of(b"abc");
    assert_eq!(
        h.to_string(),
        "sha256:ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
    );
}

#[test]
fn parse_roundtrip_and_rejects_malformed() {
    let h = ContentHash::of(b"x");
    assert_eq!(ContentHash::parse(&h.to_string()), Some(h));
    assert_eq!(ContentHash::parse("sha256:ABC"), None);
    assert_eq!(ContentHash::parse("md5:00"), None);
    let upper = h.to_string().to_uppercase();
    assert_eq!(
        ContentHash::parse(&upper),
        None,
        "uppercase hex must be rejected"
    );
    assert_eq!(ContentHash::parse(""), None);
}

#[test]
fn plan_id_shape_and_determinism() {
    let a = plan_id(b"{\"format\":1}");
    assert_eq!(a, plan_id(b"{\"format\":1}"));
    assert_ne!(a, plan_id(b"{\"format\":2}"));
    assert!(a.starts_with("p-"));
    assert_eq!(a.len(), 2 + 26);
    assert!(
        a[2..]
            .bytes()
            .all(|c| matches!(c, b'a'..=b'z' | b'2'..=b'7'))
    );
    assert!(is_full_plan_id(&a));
}

#[test]
fn full_id_check_rejects_abbreviations_and_junk() {
    let a = plan_id(b"p");
    assert!(
        !is_full_plan_id(&a[..12]),
        "abbreviation must not authorise a write (E-15)"
    );
    assert!(!is_full_plan_id(&format!("{a}x")));
    assert!(!is_full_plan_id(&a.to_uppercase()));
    assert!(!is_full_plan_id("p-0123456789012345678901234x"));
    assert!(!is_full_plan_id(""));
}

#[test]
fn prefix_resolution_is_unique_or_none() {
    let a = plan_id(b"one");
    let b = plan_id(b"two");
    let known = vec![a.clone(), b.clone()];
    assert_eq!(resolve_plan_prefix(&a[..10], &known), Some(a.as_str()));
    assert_eq!(
        resolve_plan_prefix(&a[..9], &known),
        None,
        "shorter than 10 chars"
    );
    assert_eq!(resolve_plan_prefix("p-", &known), None);
    let dup = vec![a.clone(), format!("{}{}", &a[..20], "aaaaaa")];
    assert_eq!(
        resolve_plan_prefix(&a[..10], &dup),
        None,
        "ambiguous prefix"
    );
}
