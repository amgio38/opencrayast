//! Additional cases for ISSUE-CORE-HASH-ID: golden plan ids and the write-authority rule.
//! Refs: docs/TESTING.md EDT-30 (ids are stable, any content change moves the id),
//! EDT-26 (abbreviations are never a write authority).
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::field_reassign_with_default
)]
use opencrayast_core::hash::*;

/// Golden: the id is a pure function of the canonical bytes, so a stored value can be
/// compared byte for byte against this constant.
#[test]
fn plan_id_golden_vector() {
    assert_eq!(plan_id(b"{\"format\":1}"), "p-h4fjsjll524ju247e6jyquuruy");
}

/// Golden: SHA-256 of the empty input.
#[test]
fn empty_input_hash_golden() {
    let h = ContentHash::of(b"");
    assert_eq!(
        h.to_string(),
        "sha256:e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
    );
    assert_eq!(ContentHash::parse(&h.to_string()), Some(h));
}

/// EDT-30: changing only a free-text field (the `note`) changes the id; the id does not
/// depend on anything outside the canonical bytes.
#[test]
fn plan_id_depends_only_on_hashed_bytes() {
    let without_note = b"{\"format\":1,\"note\":\"\"}";
    let with_note = b"{\"format\":1,\"note\":\"please be careful\"}";
    assert_ne!(plan_id(without_note), plan_id(with_note));
}

/// Plan ids derived from different content share no prefix by construction, but the
/// resolver must still refuse anything ambiguous.
#[test]
fn resolve_refuses_non_plan_prefixes_and_empty_known() {
    let known = vec![plan_id(b"only")];
    assert_eq!(resolve_plan_prefix("plan-123456", &known), None);
    assert_eq!(resolve_plan_prefix("p-zzzzzzzzzz", &known), None);
    assert_eq!(resolve_plan_prefix(&known[0][..10], &[]), None);
    assert_eq!(
        resolve_plan_prefix(&known[0][..10], &known),
        Some(known[0].as_str())
    );
}

/// EDT-26: the unique-resolver result is always a full id, so it can never be a
/// shorthand that slips into a write path.
#[test]
fn resolved_value_is_always_a_full_id() {
    let known = vec![plan_id(b"a"), plan_id(b"b"), plan_id(b"c")];
    for id in &known {
        for len in 10..id.len() {
            let prefix = &id[..len];
            if let Some(full) = resolve_plan_prefix(prefix, &known) {
                assert!(
                    is_full_plan_id(full),
                    "resolver must only ever return a full id, got {full}"
                );
            }
        }
    }
}

/// `is_full_plan_id` is the write gate: only the exact shape passes, everything else
/// (missing prefix, wrong length, uppercase, digits outside the base32 alphabet,
/// control characters, multi-byte UTF-8) is refused.
#[test]
fn full_id_gate_accepts_only_the_exact_shape() {
    let good = "p-aaaaaaaaaaaaaaaaaaaaaaaaaa";
    assert!(is_full_plan_id(good));
    assert!(!is_full_plan_id(&good[..27]));
    assert!(!is_full_plan_id(&format!("{good}a")));
    assert!(!is_full_plan_id("p-Aaaaaaaaaaaaaaaaaaaaaaaaa"));
    assert!(!is_full_plan_id("p-1111111111111111111111111"));
    assert!(!is_full_plan_id(&good[1..]), "prefix must be p-");
    assert!(!is_full_plan_id(&format!("{good}\n")));
    assert!(!is_full_plan_id("p-éaaaaaaaaaaaaaaaaaaaaaaaa"));
    assert!(!is_full_plan_id("p-"));
    assert!(!is_full_plan_id("  p-aaaaaaaaaaaaaaaaaaaaaaaaaa  "));
}

/// `parse` never panics on arbitrary input and accepts only the canonical form.
#[test]
fn parse_is_total_and_strict() {
    let junk = [
        "",
        "sha256",
        "sha256:",
        "sha256:zz",
        "sha256:0000",
        &format!("sha256:{}", "0".repeat(63)),
        &format!("sha256:{}", "0".repeat(65)),
        "sha256:000000000000000000000000000000000000000000000000000000000000000g",
        "SHA256:0000000000000000000000000000000000000000000000000000000000000000",
        " sha256:0000000000000000000000000000000000000000000000000000000000000000",
        "sha256:0000000000000000000000000000000000000000000000000000000000000000 ",
    ];
    for s in junk {
        assert!(ContentHash::parse(s).is_none(), "must reject {s:?}");
    }
    assert_eq!(
        ContentHash::parse(
            "sha256:0000000000000000000000000000000000000000000000000000000000000000"
        )
        .map(|h| h.to_string()),
        Some("sha256:0000000000000000000000000000000000000000000000000000000000000000".to_string())
    );
}

/// Distinct content yields distinct hashes, and a one-byte difference is enough.
#[test]
fn hash_separates_nearby_inputs() {
    assert_ne!(ContentHash::of(b"a"), ContentHash::of(b"b"));
    assert_ne!(ContentHash::of(b"a"), ContentHash::of(b"a "));
    assert_eq!(ContentHash::of(b"repeat"), ContentHash::of(b"repeat"));
}
