//! Rewrite templates (docs/PATTERNS.md "Rewrite templates") and overlap resolution.

use opencrayast_core::error::{ErrorCode, ToolError};
use opencrayast_query::pattern::{Capture, CaptureKind};
use std::ops::Range;

/// How the expanded text is laid out at the match site.
#[derive(Debug, Clone)]
pub struct ExpandOptions<'a> {
    /// Indentation of the line where the match starts (see [`indent_of_line`]); prepended to every
    /// line of the template after the first.
    pub indent: &'a str,
    /// The file's line ending, `"\n"` or `"\r\n"`.
    pub line_ending: &'a str,
    /// Byte ranges of the **template** inside a string, template literal, docstring or comment.
    /// They affect LAYOUT ONLY: line breaks inside them are not converted and the lines they start
    /// get no indentation. Metavariables and `$$` are substituted inside them exactly as outside.
    pub verbatim: &'a [Range<usize>],
    /// Maximum size of the expanded text; more is `limit_exceeded`.
    pub max_output_bytes: usize,
}

/// The leading whitespace (spaces and tabs only) of the line of `source` that contains byte
/// offset `pos`. `pos` past the end or inside a character is clamped to a valid position; never
/// panics. A line break is `\n`, `\r\n` or a lone `\r`.
pub fn indent_of_line(source: &str, pos: usize) -> &str {
    let mut pos = pos.min(source.len());
    while pos > 0 && !source.is_char_boundary(pos) {
        pos -= 1;
    }
    let bytes = source.as_bytes();
    let mut line_start = pos;
    while line_start > 0 {
        let b = bytes[line_start - 1];
        if b == b'\n' || b == b'\r' {
            break;
        }
        line_start -= 1;
    }
    let mut end = line_start;
    while end < source.len() && (bytes[end] == b' ' || bytes[end] == b'\t') {
        end += 1;
    }
    &source[line_start..end]
}

fn is_name_start(b: u8) -> bool {
    b.is_ascii_uppercase() || b == b'_'
}

fn is_name_continue(b: u8) -> bool {
    b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'_'
}

/// Scan a metavar name starting at `i` in `template` bytes. Returns `(name, end exclusive)`.
fn scan_name(bytes: &[u8], i: usize) -> Option<(&str, usize)> {
    if i >= bytes.len() || !is_name_start(bytes[i]) {
        return None;
    }
    let mut j = i + 1;
    while j < bytes.len() && is_name_continue(bytes[j]) {
        j += 1;
    }
    match std::str::from_utf8(&bytes[i..j]) {
        Ok(name) => Some((name, j)),
        Err(_) => None,
    }
}

fn in_verbatim(verbatim: &[Range<usize>], pos: usize) -> bool {
    verbatim.iter().any(|r| r.start <= pos && pos < r.end)
}

fn push_checked(out: &mut String, s: &str, max: usize) -> Result<(), ToolError> {
    if out.len().saturating_add(s.len()) > max {
        return Err(ToolError::new(
            ErrorCode::LimitExceeded,
            format!("expanded template would exceed max_output_bytes ({max})"),
            "Shorten the template or captures, or raise the limit.",
        ));
    }
    out.push_str(s);
    Ok(())
}

fn lookup_capture<'a>(
    captures: &'a [Capture],
    name: &str,
    want_list: bool,
) -> Result<&'a str, ToolError> {
    let found = captures.iter().find(|c| c.name == name);
    match found {
        None => Err(ToolError::new(
            ErrorCode::InvalidPattern,
            format!("capture ${name} is not bound by the match"),
            "Use a metavariable name that the pattern actually captured.",
        )),
        Some(c) => {
            let is_list = matches!(c.kind, CaptureKind::List);
            if want_list && !is_list {
                return Err(ToolError::new(
                    ErrorCode::InvalidPattern,
                    format!("${name} is a one-node capture; write ${name}, not $$${name}"),
                    format!("Use ${name} for this capture."),
                ));
            }
            if !want_list && is_list {
                return Err(ToolError::new(
                    ErrorCode::InvalidPattern,
                    format!("${name} is a list capture; write $$${name}, not ${name}"),
                    format!("Use $$${name} for this capture."),
                ));
            }
            Ok(c.text.as_str())
        }
    }
}

/// The index of the capture named `name`, which is what a placement is reported against.
fn capture_index(captures: &[Capture], name: &str) -> Option<usize> {
    captures.iter().position(|c| c.name == name)
}

/// Push substituted text, honouring the layout rules. Inside a `verbatim` range nothing is
/// indented and no line break is rewritten - the text is emitted exactly as captured.
fn emit(
    out: &mut String,
    opts: &ExpandOptions<'_>,
    first_line: &mut bool,
    at_line_start: &mut bool,
    text: &str,
    verbatim: bool,
    placement: Option<(&mut Placements, usize)>,
) -> Result<(), ToolError> {
    let start = out.len();
    let r = emit_plain(out, opts, first_line, at_line_start, text, verbatim);
    r?;
    if let Some((placements, index)) = placement {
        placements.push((index, start, start + text.len()));
    }
    Ok(())
}

fn emit_plain(
    out: &mut String,
    opts: &ExpandOptions<'_>,
    first_line: &mut bool,
    at_line_start: &mut bool,
    text: &str,
    verbatim: bool,
) -> Result<(), ToolError> {
    if verbatim {
        push_checked(out, text, opts.max_output_bytes)?;
        if !text.is_empty() {
            *at_line_start = false;
            *first_line = false;
        }
        return Ok(());
    }
    if *at_line_start && !*first_line && !text.is_empty() {
        push_checked(out, opts.indent, opts.max_output_bytes)?;
    }
    push_checked(out, text, opts.max_output_bytes)?;
    *at_line_start = text.ends_with('\n') || text.ends_with('\r');
    *first_line = false;
    Ok(())
}

/// Expand `template` with the captures of one match.
///
/// ## Syntax (same lexical rule as patterns, so one mental model)
/// - `$$` is a literal `$`.
/// - `$$$NAME` inserts the text of the **list** capture `NAME`; `$NAME` inserts the text of the
///   **one** capture `NAME`. A name is `[A-Z_][A-Z0-9_]*` and extends as far as it can.
/// - A `$` followed by anything else (digit, lowercase, space, end of text) is a literal `$`.
/// - Scanning is left to right: at each `$`, `$$$` followed by a name start is a list reference;
///   otherwise `$$` is a literal `$`; otherwise `$` followed by a name start is a one reference;
///   otherwise a literal `$`. So `$$$$` is two literal `$`, and `$$5` is `$5`.
/// - Captured text is inserted verbatim (it already uses the file's line endings and
///   indentation) and is never re-indented or searched for further `$`.
///
/// ## Failure semantics
///
/// | Condition | Result |
/// |---|---|
/// | name not bound by the match | `invalid_pattern`, message names `$NAME` |
/// | `$NAME` used for a list capture, or `$$$NAME` for a one capture | `invalid_pattern`, message says which form to use |
/// | expanded text longer than `max_output_bytes` | `limit_exceeded` |
///
/// ## Layout (template text outside captures only)
/// Every line break of the template (`\n`, `\r\n` or lone `\r`) outside `verbatim` ranges becomes
/// `opts.line_ending`, and the line it starts gets `opts.indent` prepended unless that line is
/// empty (no trailing whitespace is created). Line breaks inside `verbatim` ranges are copied
/// as they are, and text substituted there gets no indentation either. The first line is never
/// indented (it continues the match site).
///
/// `verbatim` is a LAYOUT concern only: `$NAME`, `$$$NAME` and `$$` are substituted inside those
/// ranges exactly as outside them, and an unbound name there is the same `invalid_pattern` error.
/// A capture that lands inside a string, template literal or comment is inserted as its source
/// text, never parenthesised - it is not an expression in that position (see `rewrite`), and text
/// that would break the literal is caught by the syntax gate at preview/apply.
pub fn expand_template(
    template: &str,
    captures: &[Capture],
    opts: &ExpandOptions<'_>,
) -> Result<String, ToolError> {
    expand_template_with_placements(template, captures, opts, &mut Vec::new())
}

/// Where one substituted capture landed in the expanded text: its index in the `captures` slice
/// that was passed in, and the byte range `[start, end)` it occupies in the result.
///
/// A capture used more than once in the template produces one entry per use, in template order.
pub type Placements = Vec<(usize, usize, usize)>;

/// [`expand_template`], additionally reporting where every SINGLE (`$NAME`) capture landed in the
/// result. The caller needs this to ask the re-parsed tree whether a substituted capture is still one
/// syntax node (the grouping check in `rewrite`).
///
/// The offsets are produced by the same code that writes the bytes, so they cannot drift from the
/// text - which a separate scan of the result could not guarantee, because the same capture text may
/// legitimately appear both inside a string and as code.
pub fn expand_template_with_placements(
    template: &str,
    captures: &[Capture],
    opts: &ExpandOptions<'_>,
    placements: &mut Placements,
) -> Result<String, ToolError> {
    expand_inner(template, captures, opts, placements, &[])
}

/// [`expand_template_with_placements`], additionally parenthesising the captures whose index is
/// marked in `wrap`.
///
/// The decision is made per OCCURRENCE, not per capture: a `$X` inside a string, template literal or
/// comment is source text in that position and is never parenthesised, while the same `$X` used as
/// code in the same template is. That is the difference between `g("$X", $X * 2)` - where only the
/// second one needs parentheses - and a blanket rule that would put them in the string too.
pub fn expand_template_wrapping(
    template: &str,
    captures: &[Capture],
    opts: &ExpandOptions<'_>,
    placements: &mut Placements,
    wrap: &[bool],
) -> Result<String, ToolError> {
    expand_inner(template, captures, opts, placements, wrap)
}

fn expand_inner(
    template: &str,
    captures: &[Capture],
    opts: &ExpandOptions<'_>,
    placements: &mut Placements,
    wrap: &[bool],
) -> Result<String, ToolError> {
    let bytes = template.as_bytes();
    let mut out = String::new();
    let mut i = 0usize;
    let mut first_line = true;
    let mut at_line_start = true;

    while i < bytes.len() {
        // `verbatim` affects layout only, so the metavariable and `$$` handling below runs there too;
        // only the indentation is suppressed.
        if bytes[i] == b'$' {
            let verbatim_here = in_verbatim(opts.verbatim, i);
            let remaining = bytes.len() - i;
            // $$$NAME
            if remaining >= 3
                && bytes[i + 1] == b'$'
                && bytes[i + 2] == b'$'
                && let Some((name, end)) = scan_name(bytes, i + 3)
            {
                let text = lookup_capture(captures, name, true)?;
                let index = capture_index(captures, name);
                emit(
                    &mut out,
                    opts,
                    &mut first_line,
                    &mut at_line_start,
                    text,
                    verbatim_here,
                    index.map(|i| (&mut *placements, i)),
                )?;
                // A list capture is never parenthesised, so nothing else to do here.
                i = end;
                continue;
            }
            // $$ → literal `$`
            if remaining >= 2 && bytes[i + 1] == b'$' {
                emit(
                    &mut out,
                    opts,
                    &mut first_line,
                    &mut at_line_start,
                    "$",
                    verbatim_here,
                    None,
                )?;
                i += 2;
                continue;
            }
            // $NAME
            if let Some((name, end)) = scan_name(bytes, i + 1) {
                let text = lookup_capture(captures, name, false)?;
                let mut text = text.to_string();
                let index = capture_index(captures, name);
                // Parenthesise this occurrence when asked, unless it is inside a string, template
                // literal or comment: there the capture is text, and parentheses would change the
                // string's contents rather than the expression's meaning.
                if !verbatim_here
                    && let Some(i) = index
                    && wrap.get(i).copied().unwrap_or(false)
                {
                    text = format!("({text})");
                }
                // Where the text will land in the output: `emit_plain` prepends the indent when
                // the capture starts a continuation line, so the range must include it.
                let start = if at_line_start && !first_line && !text.is_empty() {
                    out.len() + opts.indent.len()
                } else {
                    out.len()
                };
                let stop = start + text.len();
                emit_plain(
                    &mut out,
                    opts,
                    &mut first_line,
                    &mut at_line_start,
                    &text,
                    verbatim_here,
                )?;
                if let Some(i) = index {
                    placements.push((i, start, stop));
                }
                i = end;
                continue;
            }
            // lone `$`
            emit(
                &mut out,
                opts,
                &mut first_line,
                &mut at_line_start,
                "$",
                verbatim_here,
                None,
            )?;
            i += 1;
            continue;
        }

        if in_verbatim(opts.verbatim, i) {
            let Some(ch) = template[i..].chars().next() else {
                break;
            };
            let n = ch.len_utf8();
            push_checked(&mut out, &template[i..i + n], opts.max_output_bytes)?;
            at_line_start = ch == '\n' || ch == '\r';
            if ch != '\n' && ch != '\r' {
                first_line = false;
            }
            i += n;
            continue;
        }

        if bytes[i] == b'\n' || bytes[i] == b'\r' {
            let mut step = 1;
            if bytes[i] == b'\r' && i + 1 < bytes.len() && bytes[i + 1] == b'\n' {
                step = 2;
            }
            push_checked(&mut out, opts.line_ending, opts.max_output_bytes)?;
            i += step;
            at_line_start = true;
            first_line = false;
            continue;
        }

        let Some(ch) = template[i..].chars().next() else {
            break;
        };
        let n = ch.len_utf8();
        emit_plain(
            &mut out,
            opts,
            &mut first_line,
            &mut at_line_start,
            &ch.to_string(),
            false,
        )?;
        i += n;
    }
    Ok(out)
}

fn spans_conflict(a: (usize, usize), b: (usize, usize)) -> bool {
    let (as_, ae) = a;
    let (bs, be) = b;
    let a_empty = as_ == ae;
    let b_empty = bs == be;
    if a_empty && b_empty {
        return as_ == bs;
    }
    if a_empty {
        return bs < as_ && as_ < be;
    }
    if b_empty {
        return as_ < bs && bs < ae;
    }
    as_ < be && bs < ae
}

/// Choose which of possibly overlapping match spans `[start, end)` are rewritten
/// (docs/PATTERNS.md "Overlap"): the **outermost** wins and what it contains is dropped.
///
/// Returns `(kept, dropped)` as indices into `spans`, each ascending. Rules:
/// - disjoint or merely touching spans (`a.end == b.start`) are all kept;
/// - a span contained in another (including equal spans) is dropped; for equal spans the lower
///   index is kept;
/// - spans that partially overlap (cannot happen for tree nodes, but the function is total): the
///   one that starts first is kept, the later one is dropped;
/// - empty spans (`start == end`) follow the same rules (an empty span strictly inside another
///   span is dropped; one at its boundary is kept).
/// - `kept` is pairwise non-overlapping, and every dropped span overlaps some kept span.
pub fn resolve_overlaps(spans: &[(usize, usize)]) -> (Vec<usize>, Vec<usize>) {
    let n = spans.len();
    let mut order: Vec<usize> = (0..n).collect();
    order.sort_by(|&i, &j| {
        let (as_, ae) = spans[i];
        let (bs, be) = spans[j];
        as_.cmp(&bs)
            .then_with(|| be.cmp(&ae))
            .then_with(|| i.cmp(&j))
    });
    let mut kept: Vec<usize> = Vec::new();
    let mut dropped: Vec<usize> = Vec::new();
    for i in order {
        if kept.iter().any(|&k| spans_conflict(spans[k], spans[i])) {
            dropped.push(i);
        } else {
            kept.push(i);
        }
    }
    kept.sort_unstable();
    dropped.sort_unstable();
    (kept, dropped)
}
