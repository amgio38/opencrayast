//! Shared plumbing: read a file through the boundary, decode it, detect and parse it.

use crate::context::ToolContext;
use opencrayast_core::boundary::ResolvedPath;
use opencrayast_core::error::{ErrorCode, ToolError};
use opencrayast_lang::{Language, ParsedFile};

/// A source file that was read, decoded and parsed.
pub(crate) struct LoadedSource {
    /// The language it was parsed as.
    pub language: Language,
    /// The decoded text (a leading BOM is kept).
    pub text: String,
    /// The parse result.
    pub parsed: ParsedFile,
}

/// Why a file in a directory walk was not outlined (counted, never silent).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Skip {
    /// No grammar for this file name.
    UnsupportedLanguage,
    /// Over `max_file_bytes`.
    TooLarge,
    /// Not valid UTF-8.
    NotUtf8,
    /// Parse budget or timeout.
    Budget,
    /// Special file or I/O failure while opening.
    Unreadable,
}

/// Detect the language from the file name (and a shebang from the first line of the content for
/// extension-less files), read at most `max_file_bytes + 1` bytes through
/// `Boundary::open_read`, decode and parse with the limits of `ctx`.
///
/// Errors map exactly as docs/TOOLS.md says: `unsupported_language` (message lists the supported
/// ids), `file_too_large`, `not_utf8`, `budget_exceeded`, `timeout`, plus the boundary's refusals.
///
/// Order of the checks, and why:
///
/// 1. **Open** (`open_read`): the boundary decides whether this file may be read at all, and a
///    FIFO or socket is refused here as a special file instead of blocking (BND-22).
/// 2. **Read at most `max_file_bytes + 1` bytes.** One byte past the limit is what makes "over
///    the limit" detectable without ever holding the whole file: a 4 GiB file costs 4 MiB of
///    memory here and is then refused (LMT-01).
/// 3. **Decode.** `decode_utf8` re-checks the size (it is the same limit) before validating, so
///    an over-budget file is never walked byte by byte looking for valid UTF-8.
/// 4. **Detect** the language, from the name first and the shebang only for extension-less
///    files. Decoding happens before detection because a shebang is only meaningful in text.
/// 5. **Parse** with the budget of `ctx`.
pub(crate) fn load(ctx: &ToolContext, file: &ResolvedPath) -> Result<LoadedSource, ToolError> {
    use std::io::Read;

    let max_bytes = ctx.limits.max_file_bytes;
    let (handle, _identity) = ctx.boundary.open_read(file)?;

    let mut buf = Vec::new();
    handle
        .take(max_bytes.saturating_add(1))
        .read_to_end(&mut buf)
        .map_err(|_| unreadable())?;
    // The decode does the size check, so a file that is over the limit never reaches the UTF-8
    // validation, let alone the parser.
    let text = opencrayast_core::text::decode_utf8(&buf, max_bytes)?;

    let language = Language::detect(&file.rel, text.lines().next()).ok_or_else(|| {
        ToolError::new(
            ErrorCode::UnsupportedLanguage,
            "No grammar is built in for this file.",
            "Outline a file with one of these extensions: rust, typescript, tsx, javascript, \
             python, go.",
        )
    })?;

    let parsed = opencrayast_lang::parse(
        language,
        text,
        &opencrayast_lang::ParseBudget::from(&ctx.limits),
    )?;

    Ok(LoadedSource {
        language,
        text: text.to_string(),
        parsed,
    })
}

/// Classify a [`load`] error for a directory walk: which counter it belongs to.
///
/// Everything that is not one of the four content verdicts is `Unreadable`: a file that
/// vanished, a link, a permission error. Inside a walk those are counted rather than returned,
/// so this is the only place that decides what a walk says about a file it could not outline.
pub(crate) fn classify(error: &ToolError) -> Skip {
    match error.code {
        ErrorCode::UnsupportedLanguage => Skip::UnsupportedLanguage,
        ErrorCode::FileTooLarge => Skip::TooLarge,
        ErrorCode::NotUtf8 => Skip::NotUtf8,
        ErrorCode::BudgetExceeded | ErrorCode::Timeout => Skip::Budget,
        _ => Skip::Unreadable,
    }
}

fn unreadable() -> ToolError {
    ToolError::new(
        ErrorCode::IoError,
        "The file could not be read.",
        "Check the permissions of the file.",
    )
}
