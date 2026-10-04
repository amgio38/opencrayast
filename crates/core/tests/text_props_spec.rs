//! Extra cases for ISSUE-CORE-TEXT: multibyte columns, the `\r\n` boundary, CR-only files,
//! and the documented lone-`\r` classification. Refs: LMT-06 (UTF-8 only), EDT-14 (file
//! properties preserved on write).
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
use opencrayast_core::ErrorCode;
use opencrayast_core::text::*;

/// LMT-06: only UTF-8 is accepted, and the size check happens before validation so an
/// over-budget input is refused without a UTF-8 walk.
#[test]
fn size_is_checked_before_validity_and_boundary_is_inclusive() {
    // Exactly at the limit is fine.
    assert_eq!(decode_utf8(b"abc", 3).unwrap(), "abc");
    // One byte over is refused, whatever the bytes are.
    assert_eq!(
        decode_utf8(b"abcd", 3).unwrap_err().code,
        ErrorCode::FileTooLarge
    );
    // Invalid UTF-8 within the budget is `not_utf8`, not `file_too_large`.
    assert_eq!(
        decode_utf8(&[0xC3, 0x28], 100).unwrap_err().code,
        ErrorCode::NotUtf8
    );
    // Truncated multi-byte sequence, a lone continuation byte, an embedded NUL-byte
    // sequence and an over-long encoding are all invalid.
    for bad in [
        vec![0xE2, 0x82],
        vec![0x80],
        vec![0xC0, 0xAF],
        vec![0xED, 0xA0, 0x80],
        vec![0xF5, 0x80, 0x80, 0x80],
    ] {
        assert_eq!(
            decode_utf8(&bad, 100).unwrap_err().code,
            ErrorCode::NotUtf8,
            "{bad:?}"
        );
    }
    // A NUL byte is valid UTF-8: the content policy is UTF-8, not "text without NULs".
    assert_eq!(decode_utf8(&[0x61, 0x00, 0x62], 100).unwrap(), "a\0b");
    // Empty input is valid and at the limit.
    assert_eq!(decode_utf8(b"", 0).unwrap(), "");
}

/// EDT-14: the BOM survives decoding and is detected only as an exact `EF BB BF` prefix.
#[test]
fn bom_detection_is_exact() {
    assert!(has_bom(&[0xEF, 0xBB, 0xBF]));
    // A shorter or near-miss prefix is not a BOM.
    assert!(!has_bom(&[0xEF, 0xBB]));
    assert!(!has_bom(&[0xEF, 0xBB, 0xBE]));
    assert!(!has_bom(&[0xFE, 0xFF]));
    assert!(!has_bom(b""));
    // A BOM in the middle is data, not a marker.
    assert!(!has_bom(b"a\xEF\xBB\xBF"));
    // The decoded text keeps U+FEFF as the first character.
    let decoded = decode_utf8(b"\xEF\xBB\xBFx", 100).unwrap();
    assert!(decoded.starts_with('\u{feff}'));
    assert_eq!(decoded.chars().next(), Some('\u{feff}'));
}

/// The documented choice: a lone `\r` is a line break but is neither `Lf` nor `Crlf`, so
/// it makes the file `Mixed`, alone or beside another style.
#[test]
fn lone_cr_classification_is_documented_and_stable() {
    assert_eq!(detect_line_ending("a\rb"), LineEnding::Mixed);
    assert_eq!(detect_line_ending("a\rb\nc"), LineEnding::Mixed);
    assert_eq!(detect_line_ending("a\nb\rc"), LineEnding::Mixed);
    // A trailing lone `\r` at the very end is still a break.
    assert_eq!(detect_line_ending("a\r"), LineEnding::Mixed);
    // Only the two canonical styles, and the empty/one-line cases.
    assert_eq!(detect_line_ending(""), LineEnding::None);
    assert_eq!(detect_line_ending("a"), LineEnding::None);
    assert_eq!(detect_line_ending("a\n"), LineEnding::Lf);
    assert_eq!(detect_line_ending("a\r\n"), LineEnding::Crlf);
}

/// The line index counts columns in bytes, so a multi-byte character advances the column
/// by its encoded length and the column never points inside a character.
#[test]
fn columns_are_byte_offsets_for_multibyte_text() {
    // 'a' (1) + 'é' (2) + '你' (3) + "\n" + "z"
    let t = "aé你\nz";
    assert_eq!(t.len(), 1 + 2 + 3 + 1 + 1);
    let ix = LineIndex::new(t);
    assert_eq!(ix.line_count(), 2);
    assert_eq!(ix.line_col(0), Some((1, 1)));
    // offset 1 is the first byte of 'é' (col 2), offset 2 the second byte (col 3).
    assert_eq!(ix.line_col(1), Some((1, 2)));
    assert_eq!(ix.line_col(2), Some((1, 3)));
    // offset 3 starts '你' (col 4), offset 5 is its last byte.
    assert_eq!(ix.line_col(3), Some((1, 4)));
    assert_eq!(ix.line_col(5), Some((1, 6)));
    // The `\n` is the 7th byte; the next line starts at offset 7.
    assert_eq!(ix.line_col(7), Some((2, 1)));
    assert_eq!(ix.line_col(t.len()), Some((2, 2)));
    assert_eq!(ix.line_col(t.len() + 1), None);
    // Every byte offset maps back to a position, and offsets stay in ascending order.
    let mut prev = (0, 0);
    for offset in 0..=t.len() {
        let lc = ix
            .line_col(offset)
            .unwrap_or_else(|| panic!("no position for {offset}"));
        assert!(lc >= prev, "line_col must be monotonic at {offset}");
        prev = lc;
    }
}

/// A `\r\n` pair is one line break, so the offset of the `\n` and the offset just past the
/// pair both belong to the next line's first byte.
#[test]
fn crlf_pair_is_a_single_break() {
    let t = "a\r\nb";
    let ix = LineIndex::new(t);
    assert_eq!(t.len(), 4);
    assert_eq!(ix.line_count(), 2);
    // 0:'a' 1:'\r' 2:'\n' 3:'b'
    assert_eq!(ix.line_col(0), Some((1, 1)));
    assert_eq!(
        ix.line_col(1),
        Some((1, 2)),
        "the \\r is the last byte of line 1"
    );
    assert_eq!(ix.line_col(2), Some((1, 3)), "the \\n is still line 1");
    assert_eq!(ix.line_col(3), Some((2, 1)), "the pair ends the line");
    assert_eq!(ix.line_col(4), Some((2, 2)));
    // Consecutive pairs: line 2 is empty, which is why the count is 3.
    let t2 = "\r\n\r\nx";
    let ix2 = LineIndex::new(t2);
    assert_eq!(ix2.line_count(), 3);
    assert_eq!(ix2.line_col(0), Some((1, 1)));
    assert_eq!(ix2.line_col(2), Some((2, 1)));
    assert_eq!(ix2.line_col(4), Some((3, 1)));
    assert_eq!(ix2.line_col(5), Some((3, 2)));
}

/// CR-only (classic Mac) files: every line break is a lone `\r`.
#[test]
fn cr_only_files_index_correctly() {
    let t = "one\rtwo\rthree";
    let ix = LineIndex::new(t);
    assert_eq!(ix.line_count(), 3);
    assert_eq!(ix.line_col(0), Some((1, 1)));
    assert_eq!(ix.line_col(4), Some((2, 1)));
    assert_eq!(ix.line_col(8), Some((3, 1)));
    assert_eq!(ix.line_col(t.len()), Some((3, 6)));
    assert_eq!(ix.line_col(t.len() + 1), None);
    assert_eq!(detect_line_ending(t), LineEnding::Mixed);
}

/// A trailing break does not create a phantom extra line beyond the last byte, and an
/// empty text has exactly one addressable position.
#[test]
fn trailing_breaks_and_empty_text() {
    let ix = LineIndex::new("a\n");
    assert_eq!(ix.line_count(), 2);
    assert_eq!(ix.line_col(2), Some((2, 1)));
    let empty = LineIndex::new("");
    assert_eq!(empty.line_count(), 1);
    assert_eq!(empty.line_col(0), Some((1, 1)));
    assert_eq!(empty.line_col(1), None);
    // A BOM is content, not a break: the first line starts at offset 0.
    let bom = LineIndex::new("\u{feff}a\nb");
    assert_eq!(bom.line_count(), 2);
    assert_eq!(bom.line_col(0), Some((1, 1)));
}
