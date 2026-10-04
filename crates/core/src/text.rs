//! UTF-8 policy, line index, BOM and line-ending detection (ADR-010).

use crate::error::{ErrorCode, ToolError};

/// The UTF-8 byte order mark.
pub const BOM: [u8; 3] = [0xEF, 0xBB, 0xBF];

/// Line-ending style of a file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LineEnding {
    /// `\n`
    Lf,
    /// `\r\n`
    Crlf,
    /// No line break present.
    None,
    /// Both styles appear.
    Mixed,
}

/// Validate and borrow `bytes` as UTF-8. Errors: `file_too_large` if `bytes.len() > max_bytes`
/// (checked first), `not_utf8` if invalid. A leading UTF-8 BOM is NOT stripped here.
///
/// The size check comes first on purpose: a huge file must be refused before any UTF-8
/// validation walks it, so an over-budget input costs nothing beyond the length (LMT-01).
/// The BOM is kept because a write has to reproduce the file's properties byte for byte
/// (EDT-14); stripping it here would lose that information.
pub fn decode_utf8(bytes: &[u8], max_bytes: u64) -> Result<&str, ToolError> {
    // u64::try_from never fails for usize on any supported platform, but stay total.
    let len = u64::try_from(bytes.len()).unwrap_or(u64::MAX);
    if len > max_bytes {
        return Err(ToolError::new(
            ErrorCode::FileTooLarge,
            format!("File is {len} bytes, over the {max_bytes} byte limit"),
            "Read a smaller file or raise limits.max_file_bytes (up to its hard maximum).",
        ));
    }
    std::str::from_utf8(bytes).map_err(|_| {
        ToolError::new(
            ErrorCode::NotUtf8,
            "File is not valid UTF-8",
            "Convert the file to UTF-8; binary files are not editable.",
        )
    })
}

/// True if `bytes` starts with the UTF-8 BOM (EF BB BF).
pub fn has_bom(bytes: &[u8]) -> bool {
    bytes.starts_with(&BOM)
}

/// Classify the line endings of `text`.
///
/// Classification chosen here, spelled out because the spec leaves it to the
/// implementation: a lone `\r` is a line break (see [`LineIndex`]) but is neither `Lf`
/// nor `Crlf`, so it makes the file [`LineEnding::Mixed`] — whether it appears alone or
/// beside the other styles. Only a file with no break at all is [`LineEnding::None`].
pub fn detect_line_ending(text: &str) -> LineEnding {
    let bytes = text.as_bytes();
    let mut has_bare_lf = false;
    let mut has_crlf = false;
    let mut has_lone_cr = false;
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'\n' => {
                // A `\n` directly after a `\r` belongs to a `\r\n` pair.
                let prev_is_cr = i > 0 && bytes[i - 1] == b'\r';
                if !prev_is_cr {
                    has_bare_lf = true;
                }
            }
            b'\r' => {
                if i + 1 < bytes.len() && bytes[i + 1] == b'\n' {
                    has_crlf = true;
                    i += 1; // consume the `\n` of the pair
                } else {
                    has_lone_cr = true;
                }
            }
            _ => {}
        }
        i += 1;
    }
    let styles = has_bare_lf as u8 + has_crlf as u8 + has_lone_cr as u8;
    match styles {
        0 => LineEnding::None,
        1 if has_crlf => LineEnding::Crlf,
        1 if has_bare_lf => LineEnding::Lf,
        // A lone `\r` on its own is reported as Mixed: there is no `LoneCr` style, and
        // inventing one would give callers a case the write path does not understand.
        _ => LineEnding::Mixed,
    }
}

/// Maps byte offsets to 1-based (line, column-in-bytes) and back. `\n`, `\r\n` and lone `\r`
/// are line breaks.
#[derive(Debug, Clone)]
pub struct LineIndex {
    /// Byte offset of the first byte of each line, ascending; always at least `[0]`.
    starts: Vec<usize>,
    /// Total byte length of the indexed text.
    len: usize,
}

impl LineIndex {
    /// Build an index over `text`.
    pub fn new(text: &str) -> Self {
        let bytes = text.as_bytes();
        let mut starts = Vec::with_capacity(bytes.len() / 24 + 1);
        starts.push(0);
        let mut i = 0;
        while i < bytes.len() {
            match bytes[i] {
                b'\r' => {
                    if i + 1 < bytes.len() && bytes[i + 1] == b'\n' {
                        i += 2; // `\r\n` is a single break
                    } else {
                        i += 1;
                    }
                    starts.push(i);
                }
                b'\n' => {
                    i += 1;
                    starts.push(i);
                }
                _ => i += 1,
            }
        }
        Self {
            starts,
            len: bytes.len(),
        }
    }

    /// 1-based `(line, column)` of byte `offset`; `None` if `offset > len`.
    ///
    /// The column counts bytes, not characters, so it lines up with the byte offsets the
    /// engine reports. `offset == len` is valid and points one past the last byte.
    pub fn line_col(&self, offset: usize) -> Option<(usize, usize)> {
        if offset > self.len {
            return None;
        }
        // Index of the last line whose start is <= offset.
        let line = match self.starts.binary_search(&offset) {
            Ok(i) => i,
            Err(i) => i - 1,
        };
        Some((line + 1, offset - self.starts[line] + 1))
    }

    /// Number of lines (an empty text has 1).
    pub fn line_count(&self) -> usize {
        self.starts.len()
    }
}
