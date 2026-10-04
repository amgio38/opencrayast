//! Extra EDIT1-xx cases for ISSUE-EDIT-1 (do not weaken editset_spec / template_spec).
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use opencrayast_core::ErrorCode;
use opencrayast_core::limits::Limits;
use opencrayast_edit::{
    Edit, ExpandOptions, apply_edits, changed_bytes, expand_template, indent_of_line,
    resolve_overlaps, validate_edits,
};
use opencrayast_query::pattern::{Capture, CaptureKind};

fn e(start: usize, end: usize, r: &str) -> Edit {
    Edit {
        start,
        end,
        replacement: r.to_string(),
    }
}

/// EDIT1-01: insertion strictly inside a replaced range is `invalid_edit` (decision table).
#[test]
fn edit1_01_insertion_inside_replacement_is_invalid() {
    let src = "abcdefgh";
    let edits = [e(2, 6, "X"), e(4, 4, "Y")];
    assert_eq!(
        validate_edits(src, &edits, &Limits::default())
            .unwrap_err()
            .code,
        ErrorCode::InvalidEdit
    );
    assert!(apply_edits(src, &edits).is_err());
}

/// EDIT1-02: `changed_bytes` saturates on extreme ranges.
#[test]
fn edit1_02_changed_bytes_saturates() {
    let ed = Edit {
        start: 0,
        end: usize::MAX,
        replacement: "ab".into(),
    };
    assert_eq!(changed_bytes(&[ed]), u64::MAX);
}

/// EDIT1-03: list capture expands with `$$$NAME` and hits the byte limit.
#[test]
fn edit1_03_list_form_limit_exceeded() {
    let tiny = ExpandOptions {
        indent: "",
        line_ending: "\n",
        verbatim: &[],
        max_output_bytes: 5,
    };
    let caps = [Capture {
        name: "L".into(),
        kind: CaptureKind::List,
        start_byte: 0,
        end_byte: 6,
        text: "123456".into(),
    }];
    assert_eq!(
        expand_template("$$$L", &caps, &tiny).unwrap_err().code,
        ErrorCode::LimitExceeded
    );
}

/// EDIT1-04: indent_of_line treats `\r\n` as one break (indent of next line).
#[test]
fn edit1_04_indent_after_crlf() {
    let src = "a\r\n  b";
    assert_eq!(indent_of_line(src, src.find('b').unwrap()), "  ");
}

/// EDIT1-05: resolve_overlaps keeps a chain of touching spans.
#[test]
fn edit1_05_touching_chain_all_kept() {
    assert_eq!(
        resolve_overlaps(&[(0, 1), (1, 2), (2, 3), (3, 3)]),
        (vec![0, 1, 2, 3], vec![])
    );
}
