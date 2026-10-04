//! Extra EDIT2-xx cases for ISSUE-EDIT-2 (do not weaken plan_spec).
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use opencrayast_core::ErrorCode;
use opencrayast_core::hash::ContentHash;
use opencrayast_core::limits::Limits;
use opencrayast_edit::{Edit, Plan, PlanFile, PlanRequest};

const WS: &str = "w-00112233445566778899aabbccddeeff";

fn sample() -> Plan {
    Plan {
        format: 1,
        workspace_id: WS.into(),
        engine_format: 1,
        request: PlanRequest {
            kind: "rewrite".into(),
            summary: "console.log -> logger.debug".into(),
            note: None,
        },
        files: vec![PlanFile {
            path: "src/a.ts".into(),
            language: "typescript".into(),
            pre_hash: ContentHash::of(b"before"),
            pre_size: 20,
            pre_errors: 0,
            post_hash: ContentHash::of(b"after"),
            post_size: 31,
            post_errors: 0,
            edits: vec![Edit {
                start: 5,
                end: 12,
                replacement: "logger.debug(a, b)".into(),
            }],
        }],
    }
}

fn lim() -> Limits {
    Limits::default()
}

/// EDIT2-01: duplicate object keys (even with identical values) fail the canonical-bytes gate.
#[test]
fn edit2_01_duplicate_keys_are_plan_corrupt() {
    let g = String::from_utf8(sample().canonical_bytes()).unwrap();
    // serde_json keeps the last value; re-serialisation has one key → bytes differ.
    let dup = g.replacen("\"format\":1", "\"format\":1,\"format\":1", 1);
    assert_eq!(
        Plan::parse(dup.as_bytes(), &lim()).unwrap_err().code,
        ErrorCode::PlanCorrupt
    );
}

/// EDIT2-02: `check` error messages name indexes / classes, never path or replacement text.
#[test]
fn edit2_02_check_messages_never_quote_content() {
    let marker = "SECRET_PATH_TOKEN_xyz";
    let mut p = sample();
    p.files[0].path = format!("src/{marker}.ts");
    // Unsorted relative to a second file forces a path-order failure after path_ok.
    let mut q = p.files[0].clone();
    q.path = "aaa.ts".into();
    p.files.push(q);
    // files are [src/SECRET…, aaa.ts] → not ascending
    let e = p.check(&lim()).unwrap_err();
    assert_eq!(e.code, ErrorCode::PlanCorrupt);
    assert!(
        !e.message.contains(marker),
        "must not quote path: {}",
        e.message
    );

    let mut p = sample();
    p.files[0].edits[0].replacement = format!("leak-{marker}");
    p.files[0].edits.push(Edit {
        start: 6,
        end: 6,
        replacement: "x".into(),
    }); // insertion strictly inside [5,12)
    let e = p.check(&lim()).unwrap_err();
    assert_eq!(e.code, ErrorCode::PlanCorrupt);
    assert!(
        !e.message.contains(marker),
        "must not quote replacement: {}",
        e.message
    );
}

/// EDIT2-03: `request.kind == "symbol"` is accepted by check.
#[test]
fn edit2_03_kind_symbol_is_accepted() {
    let mut p = sample();
    p.request.kind = "symbol".into();
    p.check(&lim()).unwrap();
}

/// EDIT2-04: touching (non-overlapping) ranges are valid; post_size must still match.
#[test]
fn edit2_04_touching_edits_are_valid() {
    let mut p = sample();
    p.files[0].edits = vec![
        Edit {
            start: 0,
            end: 2,
            replacement: "AB".into(),
        },
        Edit {
            start: 2,
            end: 2,
            replacement: "x".into(),
        },
        Edit {
            start: 2,
            end: 5,
            replacement: "".into(),
        },
    ];
    // removed 2+0+3=5, inserted 2+1+0=3 → post = 20-5+3 = 18
    p.files[0].pre_size = 20;
    p.files[0].post_size = 18;
    p.check(&lim()).unwrap();
}

/// EDIT2-05: slash in a string is raw UTF-8 in canonical form (not `\/`).
#[test]
fn edit2_05_slash_is_not_escaped() {
    let mut p = sample();
    p.files[0].edits[0].replacement = "a/b".into();
    p.files[0].post_size = 20 - 7 + 3;
    let s = String::from_utf8(p.canonical_bytes()).unwrap();
    assert!(s.contains("\"replacement\":\"a/b\""), "{s}");
    assert!(!s.contains(r#"\/"#), "{s}");
    assert_eq!(Plan::parse(s.as_bytes(), &lim()).unwrap(), p);
}
