//! Spec for ISSUE-EDIT-2: the plan model (E-2, E-11, EDT-02, EDT-30). Never weaken; add cases.
//! Golden values were produced by an independent Python implementation of the written rules.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use opencrayast_core::ErrorCode;
use opencrayast_core::hash::{ContentHash, is_full_plan_id};
use opencrayast_core::limits::Limits;
use opencrayast_edit::{Edit, Plan, PlanFile, PlanRequest};

const WS: &str = "w-00112233445566778899aabbccddeeff";

fn sample(note: Option<&str>) -> Plan {
    Plan {
        format: 1,
        workspace_id: WS.into(),
        engine_format: 1,
        request: PlanRequest {
            kind: "rewrite".into(),
            summary: "console.log -> logger.debug".into(),
            note: note.map(str::to_string),
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

const GOLDEN_NO_NOTE: &str = "{\"engine_format\":1,\"files\":[{\"edits\":[{\"end\":12,\"replacement\":\"logger.debug(a, b)\",\"start\":5}],\"language\":\"typescript\",\"path\":\"src/a.ts\",\"post_errors\":0,\"post_hash\":\"sha256:f39592393ef0859cb196a52693d2cea00fb2df784b3c04ae54aa7cadb8e562f8\",\"post_size\":31,\"pre_errors\":0,\"pre_hash\":\"sha256:6db7d803e74f1ffa7d8f5adc0bf95b3e15bf4c8373fffadf546227cc6c6742cb\",\"pre_size\":20}],\"format\":1,\"request\":{\"kind\":\"rewrite\",\"summary\":\"console.log -> logger.debug\"},\"workspace_id\":\"w-00112233445566778899aabbccddeeff\"}";
const GOLDEN_NO_NOTE_ID: &str = "p-pbtrxuwhp2zrkgexqlwemhctom";
const GOLDEN_NOTE: &str = "{\"engine_format\":1,\"files\":[{\"edits\":[{\"end\":12,\"replacement\":\"logger.debug(a, b)\",\"start\":5}],\"language\":\"typescript\",\"path\":\"src/a.ts\",\"post_errors\":0,\"post_hash\":\"sha256:f39592393ef0859cb196a52693d2cea00fb2df784b3c04ae54aa7cadb8e562f8\",\"post_size\":31,\"pre_errors\":0,\"pre_hash\":\"sha256:6db7d803e74f1ffa7d8f5adc0bf95b3e15bf4c8373fffadf546227cc6c6742cb\",\"pre_size\":20}],\"format\":1,\"request\":{\"kind\":\"rewrite\",\"note\":\"check me\",\"summary\":\"console.log -> logger.debug\"},\"workspace_id\":\"w-00112233445566778899aabbccddeeff\"}";
const GOLDEN_NOTE_ID: &str = "p-trxz755lzptyjopo2sjsukp3me";

fn lim() -> Limits {
    Limits::default()
}

#[test]
fn canonical_bytes_and_ids_match_the_independent_reference() {
    let p = sample(None);
    assert_eq!(
        String::from_utf8(p.canonical_bytes()).unwrap(),
        GOLDEN_NO_NOTE
    );
    assert_eq!(p.id(), GOLDEN_NO_NOTE_ID);
    let q = sample(Some("check me"));
    assert_eq!(String::from_utf8(q.canonical_bytes()).unwrap(), GOLDEN_NOTE);
    assert_eq!(q.id(), GOLDEN_NOTE_ID);
    assert!(is_full_plan_id(&p.id()));
}

#[test]
fn string_escaping_is_exact() {
    let mut p = sample(None);
    p.files[0].edits[0].replacement = "q\"\\\n\t\r\u{8}\u{c}\u{1}\u{1f}\u{7f}日本😀/".into();
    let s = String::from_utf8(p.canonical_bytes()).unwrap();
    let want = "\"replacement\":\"q\\\"\\\\\\n\\t\\r\\b\\f\\u0001\\u001f\u{7f}日本😀/\"";
    assert!(s.contains(want), "{s}");
}

#[test]
fn the_id_is_a_function_of_the_content_only() {
    let a = sample(None);
    assert_eq!(a.id(), a.clone().id());
    assert_ne!(
        a.id(),
        sample(Some("x")).id(),
        "changing only the note changes the id (EDT-30)"
    );
    assert_ne!(sample(Some("x")).id(), sample(Some("y")).id());
    let mut b = sample(None);
    b.request.summary.push('!');
    assert_ne!(a.id(), b.id(), "the summary is hashed");
    let mut c = sample(None);
    c.files[0].edits[0].replacement.push(' ');
    assert_ne!(a.id(), c.id());
    // None and Some("") are different documents
    assert_ne!(sample(None).id(), sample(Some("")).id());
}

#[test]
fn parse_round_trips_the_canonical_form() {
    for note in [None, Some("check me"), Some("日本 \"quoted\" \n")] {
        let p = sample(note);
        let back = Plan::parse(&p.canonical_bytes(), &lim()).unwrap();
        assert_eq!(back, p);
        assert_eq!(back.canonical_bytes(), p.canonical_bytes());
        assert_eq!(
            Plan::parse_named(&p.id(), &p.canonical_bytes(), &lim()).unwrap(),
            p
        );
    }
}

fn corrupt(bytes: &[u8]) -> ErrorCode {
    Plan::parse(bytes, &lim()).unwrap_err().code
}

#[test]
fn only_exactly_canonical_bytes_are_accepted() {
    let g = GOLDEN_NO_NOTE;
    let c = |s: String| corrupt(s.as_bytes());
    // whitespace, key order, escapes, number spellings
    assert_eq!(c(format!(" {g}")), ErrorCode::PlanCorrupt);
    assert_eq!(c(format!("{g}\n")), ErrorCode::PlanCorrupt);
    assert_eq!(
        c(g.replace(",\"files\"", ", \"files\"")),
        ErrorCode::PlanCorrupt
    );
    assert_eq!(
        c(g.replace("\"start\":5", "\"start\":5.0")),
        ErrorCode::PlanCorrupt
    );
    assert_eq!(
        c(g.replace("\"start\":5", "\"start\":5e0")),
        ErrorCode::PlanCorrupt
    );
    assert_eq!(
        c(g.replace("\"start\":5", "\"start\":05")),
        ErrorCode::PlanCorrupt
    );
    assert_eq!(
        c(g.replace("\"start\":5", "\"start\":-5")),
        ErrorCode::PlanCorrupt
    );
    assert_eq!(
        c(g.replace("\"start\":5", "\"start\":\"5\"")),
        ErrorCode::PlanCorrupt
    );
    assert_eq!(
        c(g.replace("\"language\"", "\"lang\\u0075age\"")),
        ErrorCode::PlanCorrupt
    );
    assert_eq!(
        c(g.replace("logger", "logge\\u0072")),
        ErrorCode::PlanCorrupt,
        "non-canonical escape"
    );
    assert_eq!(
        c(g.replace("logger.debug(a, b)", "logger.debug(a, b)\\/")),
        ErrorCode::PlanCorrupt
    );
    // swapped key order: move "format" in front of "files"
    let swapped = g.replace("\"format\":1,", "").replace(
        "{\"engine_format\":1,",
        "{\"engine_format\":1,\"format\":1,",
    );
    assert_eq!(c(swapped), ErrorCode::PlanCorrupt);
}

#[test]
fn malformed_and_hostile_documents_are_corrupt_never_a_panic() {
    let g = GOLDEN_NO_NOTE;
    let before = ContentHash::of(b"before").to_string();
    for bad in [
        String::new(),
        "null".into(),
        "[]".into(),
        "{}".into(),
        g[..g.len() - 1].to_string(),
        format!("{g}{g}"),
        format!("{g}x"),
        g.replace("\"format\":1,", "\"format\":1,\"extra\":0,"),
        g.replace("\"engine_format\":1,", ""),
        g.replace("\"format\":1", "\"format\":2"),
        g.replace("\"engine_format\":1", "\"engine_format\":2"),
        g.replace("\"pre_size\":20", "\"pre_size\":18446744073709551616"),
        g.replace("\"post_errors\":0", "\"post_errors\":0,\"post_errors\":0"),
        g.replace("sha256:", "sha512:"),
        g.replace(&before, &before.to_uppercase()),
        g.replace("\"kind\":\"rewrite\"", "\"kind\":null"),
        "[".repeat(100_000),
        "{\"a\":".repeat(100_000),
    ] {
        assert_eq!(
            corrupt(bad.as_bytes()),
            ErrorCode::PlanCorrupt,
            "{}",
            &bad[..bad.len().min(60)]
        );
    }
    assert_eq!(corrupt(&[0xff, 0xfe, 0x00]), ErrorCode::PlanCorrupt);
    assert_eq!(
        corrupt(b"\xef\xbb\xbf{}"),
        ErrorCode::PlanCorrupt,
        "a BOM is not canonical"
    );
    let huge = vec![b' '; opencrayast_edit::MAX_PLAN_BYTES + 1];
    assert_eq!(corrupt(&huge), ErrorCode::PlanCorrupt);
}

#[test]
fn a_tampered_stored_plan_is_rejected_even_when_it_is_still_a_valid_plan() {
    let p = sample(None);
    let id = p.id();
    // change one replacement character: still a perfectly valid canonical plan
    let tampered = GOLDEN_NO_NOTE.replace("logger.debug(a, b)", "logger.debug(a, c)");
    let other = Plan::parse(tampered.as_bytes(), &lim()).unwrap();
    assert_ne!(other.id(), id);
    let e = Plan::parse_named(&id, tampered.as_bytes(), &lim()).unwrap_err();
    assert_eq!(e.code, ErrorCode::PlanCorrupt);
    assert!(
        !e.message.contains("logger"),
        "never quotes content: {}",
        e.message
    );
    // and an id that is merely a different plan's id
    assert_eq!(
        Plan::parse_named(&sample(Some("x")).id(), GOLDEN_NO_NOTE.as_bytes(), &lim())
            .unwrap_err()
            .code,
        ErrorCode::PlanCorrupt
    );
}

fn code_of(p: &Plan) -> ErrorCode {
    p.check(&lim()).unwrap_err().code
}

#[test]
fn check_follows_the_decision_table() {
    sample(None).check(&lim()).unwrap();

    let mut p = sample(None);
    p.format = 2;
    assert_eq!(code_of(&p), ErrorCode::PlanCorrupt);
    let mut p = sample(None);
    p.engine_format = 9;
    assert_eq!(code_of(&p), ErrorCode::PlanCorrupt);
    for ws in [
        "",
        "w-1234",
        "w-00112233445566778899AABBCCDDEEFF",
        "x-00112233445566778899aabbccddeeff",
        "w-00112233445566778899aabbccddeeffaa",
    ] {
        let mut p = sample(None);
        p.workspace_id = ws.into();
        assert_eq!(code_of(&p), ErrorCode::PlanCorrupt, "{ws}");
    }
    let mut p = sample(None);
    p.request.kind = "delete_everything".into();
    assert_eq!(code_of(&p), ErrorCode::PlanCorrupt);
    for s in ["", "two\nlines", "bell\u{7}"] {
        let mut p = sample(None);
        p.request.summary = s.into();
        assert_eq!(code_of(&p), ErrorCode::PlanCorrupt, "{s:?}");
    }
    let mut p = sample(None);
    p.request.summary = "x".repeat(257);
    assert_eq!(code_of(&p), ErrorCode::PlanCorrupt);
    let mut p = sample(Some(&"n".repeat(1025)));
    assert_eq!(code_of(&p), ErrorCode::LimitExceeded);
    p.request.note = Some("n".repeat(1024));
    p.check(&lim()).unwrap();

    let mut p = sample(None);
    p.files.clear();
    assert_eq!(code_of(&p), ErrorCode::PlanCorrupt);
    let mut p = sample(None);
    p.files[0].edits.clear();
    assert_eq!(code_of(&p), ErrorCode::PlanCorrupt);
}

#[test]
fn paths_are_workspace_relative_and_normalised() {
    let long = "p".repeat(4097);
    for bad in [
        "",
        "/etc/passwd",
        "C:/x",
        "c:x",
        "\\\\server\\share",
        "a\\b",
        "a/../b",
        "../a",
        "./a",
        "a/./b",
        "a//b",
        "a/",
        "a\u{0}b",
        "a\u{1b}b",
        long.as_str(),
    ] {
        let mut p = sample(None);
        p.files[0].path = bad.into();
        assert_eq!(
            code_of(&p),
            ErrorCode::PlanCorrupt,
            "{:?}",
            &bad[..bad.len().min(20)]
        );
    }
    for good in [
        "a",
        "src/a.ts",
        "a.b/c-d_e/f g.rs",
        "日本/語.py",
        ".github/ci.yml",
        "..a/b..",
        "a/.b",
    ] {
        let mut p = sample(None);
        p.files[0].path = good.into();
        p.check(&lim())
            .unwrap_or_else(|e| panic!("{good:?}: {e:?}"));
    }
}

fn two_files() -> Plan {
    let mut p = sample(None);
    let mut second = p.files[0].clone();
    second.path = "src/b.ts".into();
    p.files.push(second);
    p
}

#[test]
fn files_are_strictly_sorted_and_unique_and_bounded() {
    two_files().check(&lim()).unwrap();
    let mut p = two_files();
    p.files.reverse();
    assert_eq!(code_of(&p), ErrorCode::PlanCorrupt, "unsorted");
    let mut p = two_files();
    p.files[1].path = "src/a.ts".into();
    assert_eq!(code_of(&p), ErrorCode::PlanCorrupt, "duplicate");
    // byte order, not locale: uppercase sorts before lowercase
    let mut p = two_files();
    p.files[0].path = "src/b.ts".into();
    p.files[1].path = "src/B.ts".into();
    assert_eq!(code_of(&p), ErrorCode::PlanCorrupt);
    let l = Limits {
        plan_max_files: 1,
        ..Limits::default()
    };
    assert_eq!(
        two_files().check(&l).unwrap_err().code,
        ErrorCode::LimitExceeded
    );
}

#[test]
fn edits_are_sorted_non_overlapping_and_consistent_with_the_sizes() {
    let e = |s: usize, en: usize, r: &str| Edit {
        start: s,
        end: en,
        replacement: r.into(),
    };
    let with = |edits: Vec<Edit>, pre: u64, post: u64| {
        let mut p = sample(None);
        p.files[0].edits = edits;
        p.files[0].pre_size = pre;
        p.files[0].post_size = post;
        p
    };
    with(vec![e(0, 2, "abc"), e(5, 5, "x")], 10, 12)
        .check(&lim())
        .unwrap();
    for (name, p) in [
        ("unsorted", with(vec![e(5, 6, "a"), e(0, 1, "b")], 10, 10)),
        ("overlap", with(vec![e(0, 4, "a"), e(3, 5, "b")], 10, 8)),
        ("start > end", with(vec![e(4, 2, "a")], 10, 9)),
        (
            "two insertions",
            with(vec![e(3, 3, "a"), e(3, 3, "b")], 10, 12),
        ),
        (
            "insertion inside",
            with(vec![e(0, 4, ""), e(2, 2, "z")], 10, 7),
        ),
        ("end past the file", with(vec![e(8, 11, "")], 10, 7)),
        ("wrong post size", with(vec![e(0, 2, "abc")], 10, 10)),
        ("underflow", with(vec![e(0, 10, "")], 10, 5)),
    ] {
        assert_eq!(code_of(&p), ErrorCode::PlanCorrupt, "{name}");
    }
    // sizes near u64::MAX never overflow
    let big = with(vec![e(0, 1, "ab")], u64::MAX, u64::MAX);
    assert_eq!(code_of(&big), ErrorCode::PlanCorrupt);
    let l = Limits {
        plan_max_edits: 1,
        ..Limits::default()
    };
    assert_eq!(
        with(vec![e(0, 2, "abc"), e(5, 5, "x")], 10, 12)
            .check(&l)
            .unwrap_err()
            .code,
        ErrorCode::LimitExceeded
    );
    let l = Limits {
        plan_max_changed_bytes: 4,
        ..Limits::default()
    };
    assert_eq!(
        with(vec![e(0, 2, "abc")], 10, 11)
            .check(&l)
            .unwrap_err()
            .code,
        ErrorCode::LimitExceeded,
        "2 removed + 3 inserted = 5 > 4"
    );
}

#[test]
fn a_plan_is_bound_to_its_workspace() {
    let p = sample(None);
    p.check_workspace(WS).unwrap();
    let e = p
        .check_workspace("w-ffffffffffffffffffffffffffffffff")
        .unwrap_err();
    assert_eq!(e.code, ErrorCode::WrongWorkspace);
    assert_eq!(
        p.check_workspace("").unwrap_err().code,
        ErrorCode::WrongWorkspace
    );
}

struct Lcg(u64);
impl Lcg {
    fn below(&mut self, n: usize) -> usize {
        self.0 = self
            .0
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        ((self.0 >> 33) as usize) % n.max(1)
    }
}

#[test]
fn randomised_round_trips_and_mutations_never_panic() {
    let mut r = Lcg(20261002);
    let pool = [
        "a", "é", "日", "\"", "\\", "\n", "\u{0}", "😀", "/", " ", "{", "}",
    ];
    for _ in 0..600 {
        let mut p = sample(None);
        let rep: String = (0..r.below(8)).map(|_| pool[r.below(pool.len())]).collect();
        let inserted = rep.len() as u64;
        p.files[0].edits[0].replacement = rep;
        p.files[0].post_size = 20 - 7 + inserted;
        if r.below(2) == 0 {
            p.request.note = Some((0..r.below(6)).map(|_| pool[r.below(pool.len())]).collect());
        }
        let bytes = p.canonical_bytes();
        assert_eq!(Plan::parse(&bytes, &lim()).unwrap(), p);
        // one-byte mutations of valid canonical bytes: an error or (rarely) another valid
        // canonical plan; never a panic, and an accepted result always round-trips.
        for _ in 0..8 {
            let mut m = bytes.clone();
            let i = r.below(m.len());
            m[i] = r.below(256) as u8;
            if let Ok(q) = Plan::parse(&m, &lim()) {
                assert_eq!(q.canonical_bytes(), m);
            }
        }
    }
}
