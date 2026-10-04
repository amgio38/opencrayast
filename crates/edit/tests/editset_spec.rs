//! Spec for ISSUE-EDIT-1, part 1: edit sets (E-1, EDT-01). Never weaken; add cases.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use opencrayast_core::ErrorCode;
use opencrayast_core::limits::Limits;
use opencrayast_edit::{
    Edit, apply_edits, bytes_added, bytes_removed, changed_bytes, validate_edits,
};

fn e(start: usize, end: usize, r: &str) -> Edit {
    Edit {
        start,
        end,
        replacement: r.to_string(),
    }
}

fn code(src: &str, edits: &[Edit]) -> ErrorCode {
    validate_edits(src, edits, &Limits::default())
        .unwrap_err()
        .code
}

#[test]
fn a_good_edit_set_is_valid_in_any_order_and_applies_deterministically() {
    let src = "hello world";
    let edits = vec![e(6, 11, "there"), e(0, 5, "howdy")];
    validate_edits(src, &edits, &Limits::default()).unwrap();
    assert_eq!(apply_edits(src, &edits).unwrap(), "howdy there");
    let sorted = vec![e(0, 5, "howdy"), e(6, 11, "there")];
    assert_eq!(apply_edits(src, &sorted).unwrap(), "howdy there");
}

/// EDIT-12: `bytes_added` is only inserts, `bytes_removed` is only deletes, and
/// `changed_bytes` is their sum — never store a second truth by stuffing the sum
/// into `bytes_removed` (that bug made `+A −B` print removed+inserted as −B).
#[test]
fn edit12_bytes_added_and_removed_are_not_a_sum_disguised() {
    // Insert only: start == end, non-empty replacement.
    let insert_only = [e(3, 3, "XYZ")];
    assert_eq!(
        bytes_removed(&insert_only),
        0,
        "insert must not count as removed"
    );
    assert_eq!(bytes_added(&insert_only), 3);
    assert_eq!(changed_bytes(&insert_only), 3);

    // Delete only: empty replacement over a range.
    let delete_only = [e(1, 5, "")];
    assert_eq!(bytes_removed(&delete_only), 4);
    assert_eq!(
        bytes_added(&delete_only),
        0,
        "delete must not count as added"
    );
    assert_eq!(changed_bytes(&delete_only), 4);

    // Both: remove 4 bytes, insert 2.
    let both = [e(0, 4, "ab")];
    assert_eq!(bytes_removed(&both), 4);
    assert_eq!(bytes_added(&both), 2);
    assert_eq!(changed_bytes(&both), 6);
    // The bug shape: treating changed_bytes as bytes_removed would claim 6 removed.
    assert_ne!(
        bytes_removed(&both),
        changed_bytes(&both),
        "bytes_removed must not equal the sum when there are also inserts"
    );
}

#[test]
fn insertions_deletions_and_adjacent_edits() {
    let src = "abcdef";
    assert_eq!(apply_edits(src, &[e(3, 3, "X")]).unwrap(), "abcXdef");
    assert_eq!(apply_edits(src, &[e(1, 4, "")]).unwrap(), "aef");
    assert_eq!(
        apply_edits(src, &[e(0, 0, ">"), e(6, 6, "<")]).unwrap(),
        ">abcdef<"
    );
    // touching ranges are not overlapping
    assert_eq!(
        apply_edits(src, &[e(0, 3, "1"), e(3, 6, "2")]).unwrap(),
        "12"
    );
    // an insertion at the boundary of a replaced range is fine, on either side
    assert_eq!(
        apply_edits(src, &[e(2, 4, "-"), e(2, 2, "<"), e(4, 4, ">")]).unwrap(),
        "ab<->ef"
    );
    // replacing everything, and replacing nothing with nothing
    assert_eq!(apply_edits(src, &[e(0, 6, "z")]).unwrap(), "z");
    assert_eq!(apply_edits("", &[e(0, 0, "new")]).unwrap(), "new");
}

#[test]
fn every_invalid_row_of_the_decision_table_has_its_code() {
    let src = "héllo"; // é is two bytes: 1..3
    assert_eq!(
        code(src, &[e(3, 1, "")]),
        ErrorCode::InvalidEdit,
        "start > end"
    );
    assert_eq!(
        code(src, &[e(0, 7, "")]),
        ErrorCode::InvalidEdit,
        "end past the file"
    );
    assert_eq!(code(src, &[e(99, 100, "")]), ErrorCode::InvalidEdit);
    assert_eq!(
        code(src, &[e(2, 3, "")]),
        ErrorCode::InvalidEdit,
        "start inside a character"
    );
    assert_eq!(
        code(src, &[e(0, 2, "")]),
        ErrorCode::InvalidEdit,
        "end inside a character"
    );
    assert_eq!(
        code(src, &[e(0, 3, "a"), e(2, 4, "b")]),
        ErrorCode::InvalidEdit,
        "overlap"
    );
    assert_eq!(
        code(src, &[e(0, 4, "a"), e(1, 2, "b")]),
        ErrorCode::InvalidEdit,
        "nested"
    );
    assert_eq!(
        code(src, &[e(0, 3, "a"), e(0, 3, "b")]),
        ErrorCode::InvalidEdit,
        "identical ranges"
    );
    assert_eq!(
        code(src, &[e(3, 3, "a"), e(3, 3, "b")]),
        ErrorCode::InvalidEdit,
        "two insertions, one place"
    );
    assert_eq!(
        code(src, &[e(0, 4, "a"), e(2, 2, "b")]),
        ErrorCode::InvalidEdit,
        "insertion strictly inside a replaced range"
    );
    assert_eq!(code(src, &[]), ErrorCode::InvalidEdit, "empty set");
    // the same checks guard apply_edits
    assert_eq!(
        apply_edits(src, &[e(0, 7, "")]).unwrap_err().code,
        ErrorCode::InvalidEdit
    );
    assert_eq!(
        apply_edits(src, &[e(2, 3, "")]).unwrap_err().code,
        ErrorCode::InvalidEdit
    );
    assert_eq!(
        apply_edits(src, &[e(0, 3, "a"), e(2, 4, "b")])
            .unwrap_err()
            .code,
        ErrorCode::InvalidEdit
    );
}

#[test]
fn checks_run_in_the_documented_order_and_limits_are_enforced() {
    let src = "abcdef";
    let l = Limits {
        plan_max_edits: 2,
        ..Limits::default()
    };
    let three = vec![e(0, 1, "x"), e(2, 3, "y"), e(4, 5, "z")];
    assert_eq!(
        validate_edits(src, &three, &l).unwrap_err().code,
        ErrorCode::LimitExceeded
    );
    // an invalid set that is also too big reports the structural error first
    let bad_and_big = vec![e(0, 99, "x"), e(2, 3, "y"), e(4, 5, "z")];
    assert_eq!(
        validate_edits(src, &bad_and_big, &l).unwrap_err().code,
        ErrorCode::InvalidEdit
    );

    let l = Limits {
        plan_max_changed_bytes: 10,
        ..Limits::default()
    };
    assert_eq!(changed_bytes(&[e(0, 4, "ab")]), 6);
    assert_eq!(changed_bytes(&[e(2, 2, "abc"), e(4, 6, "")]), 5);
    // EDIT-12: the two halves stay separate; changed_bytes is only their sum.
    assert_eq!(bytes_removed(&[e(0, 4, "ab")]), 4);
    assert_eq!(bytes_added(&[e(0, 4, "ab")]), 2);
    assert_eq!(bytes_removed(&[e(2, 2, "abc"), e(4, 6, "")]), 2);
    assert_eq!(bytes_added(&[e(2, 2, "abc"), e(4, 6, "")]), 3);
    validate_edits(src, &[e(0, 4, "abcdef")], &l).unwrap(); // 4 + 6 = 10, at the limit
    assert_eq!(
        validate_edits(src, &[e(0, 4, "abcdefg")], &l)
            .unwrap_err()
            .code,
        ErrorCode::LimitExceeded
    );
}

#[test]
fn messages_name_the_edit_but_never_quote_the_source() {
    let src = "SECRET-TOKEN-1234";
    let err = validate_edits(src, &[e(0, 99, "")], &Limits::default()).unwrap_err();
    assert!(!err.message.contains("SECRET"), "{}", err.message);
    assert!(
        err.message.contains("0") && err.message.contains("99"),
        "{}",
        err.message
    );
    assert!(!err.next.is_empty());
}

#[test]
fn extreme_values_never_panic_or_overflow() {
    let src = "abc";
    for (s, en) in [
        (usize::MAX, usize::MAX),
        (0, usize::MAX),
        (usize::MAX - 1, usize::MAX),
        (usize::MAX, 0),
    ] {
        assert_eq!(
            code(src, &[e(s, en, "")]),
            ErrorCode::InvalidEdit,
            "{s}..{en}"
        );
        assert!(apply_edits(src, &[e(s, en, "")]).is_err());
    }
    // a huge replacement is a limit error, not an allocation or an overflow
    let big = "x".repeat(2_000_000);
    assert_eq!(code(src, &[e(0, 1, &big)]), ErrorCode::LimitExceeded);
    let huge = Edit {
        start: 0,
        end: usize::MAX,
        replacement: big,
    };
    assert!(
        changed_bytes(&[huge]) >= usize::MAX as u64,
        "saturates instead of wrapping"
    );
}

/// Tiny deterministic generator (no external crates).
struct Lcg(u64);
impl Lcg {
    fn next(&mut self) -> u64 {
        self.0 = self
            .0
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        self.0 >> 33
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next() as usize) % n.max(1)
    }
}

/// An obviously correct oracle: apply edits one by one to a Vec of "cells" and compare.
fn oracle(src: &str, edits: &[Edit]) -> Option<String> {
    let chars: Vec<(usize, char)> = src.char_indices().collect();
    let boundaries: Vec<usize> = chars.iter().map(|c| c.0).chain([src.len()]).collect();
    let mut owner = vec![None::<usize>; src.len()];
    for (i, ed) in edits.iter().enumerate() {
        if ed.start > ed.end
            || ed.end > src.len()
            || !boundaries.contains(&ed.start)
            || !boundaries.contains(&ed.end)
        {
            return None;
        }
        for slot in owner.iter_mut().take(ed.end).skip(ed.start) {
            if slot.is_some() {
                return None;
            }
            *slot = Some(i);
        }
    }
    for (i, ins) in edits.iter().enumerate().filter(|(_, e)| e.start == e.end) {
        if edits
            .iter()
            .enumerate()
            .any(|(j, o)| j != i && o.start < ins.start && ins.start < o.end)
        {
            return None;
        }
    }
    for a in 0..edits.len() {
        for b in a + 1..edits.len() {
            if edits[a].start == edits[a].end
                && edits[b].start == edits[b].end
                && edits[a].start == edits[b].start
            {
                return None;
            }
        }
    }
    let mut out = String::new();
    let mut pos = 0;
    let mut order: Vec<usize> = (0..edits.len()).collect();
    order.sort_by_key(|&i| (edits[i].start, edits[i].end));
    for i in order {
        out.push_str(&src[pos..edits[i].start]);
        out.push_str(&edits[i].replacement);
        pos = edits[i].end;
    }
    out.push_str(&src[pos..]);
    Some(out)
}

#[test]
fn randomised_agreement_with_the_oracle_including_mid_character_offsets() {
    let mut r = Lcg(20261002);
    let pool = ["a", "é", "日", "b", "😀", "\n", "x"];
    let mut valid = 0;
    for _ in 0..4000 {
        let src: String = (0..r.below(12))
            .map(|_| pool[r.below(pool.len())])
            .collect();
        let n = 1 + r.below(4);
        let edits: Vec<Edit> = (0..n)
            .map(|_| {
                let a = r.below(src.len() + 3);
                let b = a + r.below(5);
                e(a, b, ["", "Z", "日本"][r.below(3)])
            })
            .collect();
        let want = oracle(&src, &edits);
        let got_valid = validate_edits(&src, &edits, &Limits::default()).is_ok();
        let got = apply_edits(&src, &edits).ok();
        assert_eq!(got_valid, want.is_some(), "validate {src:?} {edits:?}");
        assert_eq!(got, want, "apply {src:?} {edits:?}");
        valid += usize::from(want.is_some());
    }
    assert!(
        valid > 200,
        "the generator must produce plenty of valid sets, got {valid}"
    );
}
