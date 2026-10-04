//! Spec for ISSUE-CORE-TEXT.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::field_reassign_with_default
)]
use opencrayast_core::ErrorCode;
use opencrayast_core::text::*;

#[test]
fn decode_ok_and_errors() {
    assert_eq!(decode_utf8("héllo".as_bytes(), 100).unwrap(), "héllo");
    assert_eq!(
        decode_utf8(&[0xff, 0xfe], 100).unwrap_err().code,
        ErrorCode::NotUtf8
    );
    assert_eq!(
        decode_utf8(b"abcdef", 3).unwrap_err().code,
        ErrorCode::FileTooLarge
    );
    // size is checked before validity
    assert_eq!(
        decode_utf8(&[0xff; 10], 3).unwrap_err().code,
        ErrorCode::FileTooLarge
    );
}

#[test]
fn bom_is_kept_and_detected() {
    let b = b"\xEF\xBB\xBFfn main() {}";
    assert!(has_bom(b));
    assert!(decode_utf8(b, 100).unwrap().starts_with('\u{feff}'));
    assert!(!has_bom(b"abc"));
}

#[test]
fn line_endings() {
    assert_eq!(detect_line_ending("a\nb\n"), LineEnding::Lf);
    assert_eq!(detect_line_ending("a\r\nb\r\n"), LineEnding::Crlf);
    assert_eq!(detect_line_ending("abc"), LineEnding::None);
    assert_eq!(detect_line_ending("a\nb\r\n"), LineEnding::Mixed);
}

#[test]
fn line_index() {
    let t = "ab\ncd\r\nef\rg";
    let ix = LineIndex::new(t);
    assert_eq!(ix.line_count(), 4);
    assert_eq!(ix.line_col(0), Some((1, 1)));
    assert_eq!(ix.line_col(3), Some((2, 1)));
    // offsets: a0 b1 \n2 c3 d4 \r5 \n6 e7 f8 \r9 g10
    assert_eq!(ix.line_col(7), Some((3, 1)));
    assert_eq!(ix.line_col(8), Some((3, 2)));
    assert_eq!(ix.line_col(10), Some((4, 1)));
    assert_eq!(ix.line_col(t.len()), Some((4, 2)));
    assert_eq!(ix.line_col(t.len() + 1), None);
    assert_eq!(LineIndex::new("").line_count(), 1);
}
