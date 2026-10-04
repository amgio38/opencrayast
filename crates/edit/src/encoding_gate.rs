//! Shared encoding-preservation gate (EDIT-MODEL §Gates `encoding`; EDIT-11).
//!
//! Preview and apply must refuse the same attribute changes with the same reason
//! strings. This module is the **one** implementation: BOM presence, line-ending
//! classification (LF / CRLF / Mixed / None — never folded), and trailing-newline
//! presence. Do not copy these checks into `preview` or `apply`.
//!
//! This is not [`crate::rewrite::line_ending_at`]: that picks the line ending at an
//! edit site for template expansion. This module compares whole-file properties
//! before and after an edit set.

use opencrayast_core::text::{LineEnding, detect_line_ending};

/// Whether `after` keeps the encoding attributes of `before`.
pub(crate) fn encoding_preserved(before: &str, after: &str) -> bool {
    encoding_fault(before, after).is_none()
}

/// What the `encoding` gate objected to, named so the refusal can say which attribute
/// changed. Return values are stable reason fragments used by both preview and apply:
/// `"byte order mark"`, `"trailing newline"`, `"line ending"`.
pub(crate) fn encoding_fault(before: &str, after: &str) -> Option<&'static str> {
    if has_bom(before) != has_bom(after) {
        return Some("byte order mark");
    }
    // Checked before the style, because a file with **no** line break at all is
    // `LineEnding::None` and adding a trailing newline moves it to `Lf`: both
    // properties changed, and "trailing newline" is the one the caller can act on.
    if has_trailing_newline(before) != has_trailing_newline(after) {
        return Some("trailing newline");
    }
    // The **classification** is compared, not a folded string: a mixed file flattened
    // to LF is a change of the file's line-ending property, and folding both sides to
    // "\n" is what hid it.
    if classify_line_endings(before) != classify_line_endings(after) {
        return Some("line ending");
    }
    None
}

/// The line-ending style of a text, as a **classification**.
///
/// [`detect_line_ending`] answers `Mixed` for anything that is not exactly one style.
/// That answer is kept as a distinct value rather than folded into LF or CRLF.
pub(crate) fn classify_line_endings(source: &str) -> LineEnding {
    detect_line_ending(source)
}

fn has_trailing_newline(text: &str) -> bool {
    text.ends_with('\n')
}

/// BOM on decoded text (same notion as `core::text::has_bom` on bytes).
fn has_bom(text: &str) -> bool {
    text.starts_with('\u{feff}')
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Attribute-by-attribute naming (moved from preview with the shared gate).
    #[test]
    fn the_encoding_gate_names_the_attribute_that_changed() {
        assert_eq!(
            encoding_fault("log(1);\n", "\u{feff}log(1);\n"),
            Some("byte order mark")
        );
        assert_eq!(
            encoding_fault("\u{feff}log(1);\n", "log(1);\n"),
            Some("byte order mark")
        );
        assert_eq!(
            encoding_fault("\u{feff}log(1);\n", "\u{feff}log(2);\n"),
            None
        );

        assert_eq!(
            encoding_fault("log(1);", "log(1);\n"),
            Some("trailing newline")
        );
        assert_eq!(
            encoding_fault("log(1);\n", "log(1);"),
            Some("trailing newline")
        );
        assert_eq!(encoding_fault("log(1);\r\n", "log(1);\r\n"), None);

        assert_eq!(
            encoding_fault("log(1);\r\n", "log(1);\n"),
            Some("line ending")
        );
        assert_eq!(
            encoding_fault("log(1);\n", "log(1);\r\n"),
            Some("line ending")
        );
        assert_eq!(
            encoding_fault("log(1);\r\nlog(2);\n", "log(1);\nlog(2);\n"),
            Some("line ending")
        );
        assert_eq!(
            encoding_fault("log(1);\r\nlog(2);\n", "log9(1);\r\nlog9(2);\n"),
            None
        );
        assert_eq!(encoding_fault("log(1);", "log(2);"), None);
    }
}
