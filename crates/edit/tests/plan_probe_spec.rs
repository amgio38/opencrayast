//! CR probes for the plan parser (hostile sizes, every control character, limits at the edge).
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use opencrayast_core::ErrorCode;
use opencrayast_core::hash::ContentHash;
use opencrayast_core::limits::Limits;
use opencrayast_edit::{Edit, Plan, PlanFile, PlanRequest};
use std::time::Instant;

fn base(edits: Vec<Edit>, pre_size: u64, post_size: u64) -> Plan {
    Plan {
        format: 1,
        workspace_id: "w-00112233445566778899aabbccddeeff".into(),
        engine_format: 1,
        request: PlanRequest {
            kind: "rewrite".into(),
            summary: "probe".into(),
            note: None,
        },
        files: vec![PlanFile {
            path: "a.rs".into(),
            language: "rust".into(),
            pre_hash: ContentHash::of(b"x"),
            pre_size,
            pre_errors: 0,
            post_hash: ContentHash::of(b"y"),
            post_size,
            post_errors: 0,
            edits,
        }],
    }
}

#[test]
fn every_control_character_round_trips() {
    let all: String = (0u8..0x20)
        .map(char::from)
        .chain(['\u{7f}', '\u{2028}', '\u{feff}', '\u{10ffff}'])
        .collect();
    let p = base(
        vec![Edit {
            start: 0,
            end: 0,
            replacement: all.clone(),
        }],
        10,
        10 + all.len() as u64,
    );
    let back = Plan::parse(&p.canonical_bytes(), &Limits::default()).unwrap();
    assert_eq!(back.files[0].edits[0].replacement, all);
}

#[test]
fn a_maximal_plan_parses_in_reasonable_time_and_memory_shape() {
    // 5000 edits (the hard maximum), worst-case escapes in the replacement text
    let l = Limits {
        plan_max_edits: 5000,
        plan_max_changed_bytes: 8 * 1024 * 1024,
        ..Limits::default()
    };
    let rep = "\u{1}".repeat(200);
    let edits: Vec<Edit> = (0..5000)
        .map(|i| Edit {
            start: i * 2,
            end: i * 2 + 1,
            replacement: rep.clone(),
        })
        .collect();
    let removed = 5000u64;
    let inserted = 5000u64 * rep.len() as u64;
    let p = base(edits, 20_000, 20_000 - removed + inserted);
    let bytes = p.canonical_bytes();
    assert!(bytes.len() > 5_000_000, "{}", bytes.len());
    let t = Instant::now();
    let back = Plan::parse(&bytes, &l).unwrap();
    assert!(t.elapsed().as_secs() < 5, "{:?}", t.elapsed());
    assert_eq!(back, p);
    // over the default edit limit it is a limit error, not corrupt and not a panic
    assert_eq!(
        Plan::parse(&bytes, &Limits::default()).unwrap_err().code,
        ErrorCode::LimitExceeded
    );
}

#[test]
fn sizes_at_u64_edges_never_panic() {
    for (pre, post) in [
        (u64::MAX, u64::MAX),
        (0, u64::MAX),
        (u64::MAX, 0),
        (1 << 63, 1 << 63),
    ] {
        let p = base(
            vec![Edit {
                start: 0,
                end: 1,
                replacement: "z".into(),
            }],
            pre,
            post,
        );
        let bytes = p.canonical_bytes();
        let r = Plan::parse(&bytes, &Limits::default());
        // canonical and well-formed, but the arithmetic is inconsistent or beyond the file: never a panic
        if let Err(e) = r {
            assert!(
                matches!(e.code, ErrorCode::PlanCorrupt | ErrorCode::LimitExceeded),
                "{e:?}"
            );
        }
    }
}

#[test]
fn duplicate_keys_at_every_level_are_refused() {
    let p = base(
        vec![Edit {
            start: 0,
            end: 1,
            replacement: "z".into(),
        }],
        10,
        10,
    );
    let s = String::from_utf8(p.canonical_bytes()).unwrap();
    for (from, to) in [
        ("\"format\":1", "\"format\":1,\"format\":1"),
        (
            "\"kind\":\"rewrite\"",
            "\"kind\":\"rewrite\",\"kind\":\"rewrite\"",
        ),
        (
            "\"language\":\"rust\"",
            "\"language\":\"rust\",\"language\":\"rust\"",
        ),
        ("\"start\":0", "\"start\":0,\"start\":0"),
        ("\"path\":\"a.rs\"", "\"path\":\"a.rs\",\"path\":\"b.rs\""),
    ] {
        let dup = s.replacen(from, to, 1);
        assert_ne!(dup, s);
        assert_eq!(
            Plan::parse(dup.as_bytes(), &Limits::default())
                .unwrap_err()
                .code,
            ErrorCode::PlanCorrupt,
            "{to}"
        );
    }
}

#[test]
fn lone_surrogates_overlong_utf8_and_nul_are_refused() {
    let p = base(
        vec![Edit {
            start: 0,
            end: 1,
            replacement: "z".into(),
        }],
        10,
        10,
    );
    let s = String::from_utf8(p.canonical_bytes()).unwrap();
    for bad in [
        s.replace("\"z\"", "\"\\ud800\""),
        s.replace("\"z\"", "\"\\udc00\\ud800\""),
    ] {
        assert_eq!(
            Plan::parse(bad.as_bytes(), &Limits::default())
                .unwrap_err()
                .code,
            ErrorCode::PlanCorrupt,
            "{bad}"
        );
    }
    let mut raw = s.clone().into_bytes();
    let at = s.find("\"z\"").unwrap() + 1;
    raw.splice(at..at + 1, [0xc0, 0xaf]); // overlong '/'
    assert_eq!(
        Plan::parse(&raw, &Limits::default()).unwrap_err().code,
        ErrorCode::PlanCorrupt
    );
    let mut raw = s.into_bytes();
    raw.splice(at..at + 1, [0x00]); // raw NUL inside a string
    assert_eq!(
        Plan::parse(&raw, &Limits::default()).unwrap_err().code,
        ErrorCode::PlanCorrupt
    );
}
