//! `ast_get` (docs/TOOLS.md).

use crate::context::ToolContext;
use crate::source::{LoadedSource, load};
use opencrayast_core::boundary::ResolvedPath;
use opencrayast_core::error::{ErrorCode, ToolError};
use opencrayast_core::render::{EscapeCounts, escape_inline, fenced_block};
use opencrayast_core::walk::{WalkOptions, walk};
use opencrayast_query::{Symbol, SymbolText, find_symbols, symbol_text};
use std::time::{Duration, Instant};

/// Longest `symbol` argument, in bytes.
const SYMBOL_MAX_BYTES: usize = 256;

/// How many candidates an `ambiguous` message lists before it summarises the rest.
const CANDIDATES_MAX: usize = 20;

/// Arguments of `ast_get`.
#[derive(Debug, Clone, Default)]
pub struct GetArgs {
    /// Name or qualified name (`Config::load`, `Config.load`). Required.
    pub symbol: String,
    /// Restrict to a file or directory (default: the whole workspace, i.e. `.`).
    pub path: Option<String>,
    /// Lines of surrounding code, 0..=20 (default 0).
    pub context_lines: Option<u64>,
    /// Include the leading doc comment (default true).
    pub include_doc: Option<bool>,
}

/// One symbol's source, by name.
///
/// ## Output format (exact; golden-tested)
///
/// `<rel>:<first>-<last>  <kind> <qualified>  (<language id>)` then the text of lines
/// `first..=last` (as returned by `query::symbol_text`, so doc and context are included) in a
/// block from `render::fenced_block(text, <language id>)`. `first`/`last` are the lines of the
/// returned text, not of the bare symbol. The header goes through `escape_inline`.
///
/// Search: walk `path` (as `ast_outline` does), parse each supported file, `find_symbols`, stop
/// at the deadline `limits.call_timeout_ms` (`timeout`). Zero matches: `not_found`
/// ("No symbol named `<escaped symbol>` found in <N> files." Next: "Check the name with
/// ast_outline."). More than one match: `ambiguous` with message
/// "<N> symbols match `<symbol>`:" followed by lines `<i>. <rel>:L<start> <kind> <qualified>`
/// (max 20 listed, then "... and <M> more") and next "Repeat the call with `path` set to one of
/// these files." Exactly one match: the output above.
/// Errors for the path argument are those of `ast_outline`. `symbol` must be 1..=256 bytes and
/// contain no control characters (`invalid_args`).
pub fn ast_get(ctx: &ToolContext, args: &GetArgs) -> Result<String, ToolError> {
    let symbol = arg_symbol(args)?;
    let context_lines = arg_context_lines(args)?;
    let include_doc = args.include_doc.unwrap_or(true);

    // The deadline covers the WHOLE call, not just the search: reading one file and rendering
    // its symbol are part of it. It is checked between files and once more before the result is
    // assembled, so a walk of ten thousand files cannot run long just because each individual
    // step is fast (LMT-05).
    let deadline = Deadline::new(ctx.limits.call_timeout_ms);

    let resolved = ctx
        .boundary
        .resolve_read(args.path.as_deref().unwrap_or("."))?;
    // The same file-or-directory question `ast_outline` asks, asked the same way: with the
    // boundary, never with a `stat` on a path string.
    let (files, single_file) = match ctx.boundary.open_read(&resolved) {
        Ok(_) => (vec![resolved.clone()], true),
        Err(open_error) => match ctx.boundary.read_dir(&resolved) {
            Ok(_) => {
                let result = walk(
                    &ctx.boundary,
                    &resolved,
                    &WalkOptions {
                        max_files: ctx.limits.max_scan_files,
                        respect_gitignore: ctx.respect_gitignore,
                        extra_ignore: ctx.extra_ignore.clone(),
                    },
                )?;
                (result.files, false)
            }
            Err(_) => return Err(open_error),
        },
    };

    // Every match is COUNTED, but only the first `CANDIDATES_MAX` are kept: the ambiguity
    // message names the true total ("<N> symbols match") and then lists at most twenty, so
    // holding every candidate would be memory spent on output that is never printed.
    let mut candidates: Vec<Match> = Vec::new();
    let mut total = 0usize;
    let mut parsed_files = 0usize;
    for file in &files {
        // Between files, which is where a long walk spends its time. A single enormous file is
        // bounded by `max_file_bytes` and `parse_timeout_ms` instead.
        deadline.check()?;
        let loaded = match load(ctx, file) {
            Ok(loaded) => loaded,
            // A named file that cannot be read is an error; a file met while walking is not
            // this call's problem - `ast_outline` is the tool that counts those.
            Err(e) if single_file => return Err(e),
            Err(_) => continue,
        };
        parsed_files += 1;
        for symbol in find_symbols(&loaded.parsed, &loaded.text, &symbol) {
            total += 1;
            if candidates.len() < CANDIDATES_MAX {
                candidates.push(Match {
                    file: file.clone(),
                    symbol,
                });
            }
        }
    }
    // Once more, so a call that spent its whole budget inside the last file still says so
    // instead of returning a result it no longer had time to produce.
    deadline.check()?;

    match total {
        0 => Err(not_found(&symbol, parsed_files)),
        1 => {
            let found = &candidates[0];
            let loaded = load(ctx, &found.file)?;
            render(found, &loaded, include_doc, context_lines)
        }
        n => Err(ambiguous(&symbol, &candidates, n)),
    }
}

/// One match: where it was found and what it is. The source text is not kept with it - the
/// file is read again for the one match that is actually returned, so a walk that found
/// twenty candidates never holds twenty files in memory.
struct Match {
    /// The file the symbol is in.
    file: ResolvedPath,
    /// The symbol itself.
    symbol: Symbol,
}

/// The whole-call deadline (`limits.call_timeout_ms`).
struct Deadline {
    at: Instant,
}

impl Deadline {
    fn new(ms: u64) -> Self {
        Self {
            at: Instant::now() + Duration::from_millis(ms),
        }
    }

    /// `timeout` when the budget is gone. The message names no path and no file: it is about
    /// the call, not about anything the caller could go looking for.
    fn check(&self) -> Result<(), ToolError> {
        if Instant::now() >= self.at {
            return Err(ToolError::new(
                ErrorCode::Timeout,
                "The call ran out of time before it finished.",
                "Narrow `path`, or raise limits.call_timeout_ms (up to its hard maximum).",
            ));
        }
        Ok(())
    }
}

/// The validated `symbol`: 1..=256 bytes with no control characters.
///
/// A control character in a symbol name is never something a caller meant, and it is exactly
/// what makes a name display as something else (T-34), so it is refused at the door instead of
/// being escaped into the output later.
fn arg_symbol(args: &GetArgs) -> Result<String, ToolError> {
    let symbol = &args.symbol;
    if symbol.is_empty() || symbol.len() > SYMBOL_MAX_BYTES {
        return Err(ToolError::new(
            ErrorCode::InvalidArgs,
            format!(
                "symbol is {} bytes, outside 1..={SYMBOL_MAX_BYTES}",
                symbol.len()
            ),
            "Pass a symbol name of 1 to 256 bytes.",
        ));
    }
    if symbol.chars().any(char::is_control) {
        return Err(ToolError::new(
            ErrorCode::InvalidArgs,
            "symbol contains a control character.",
            "Pass the symbol name as it is written in the source.",
        ));
    }
    Ok(symbol.clone())
}

/// The validated `context_lines`: 0..=20.
fn arg_context_lines(args: &GetArgs) -> Result<usize, ToolError> {
    let lines = args.context_lines.unwrap_or(0);
    if lines > 20 {
        return Err(ToolError::new(
            ErrorCode::InvalidArgs,
            format!("context_lines is {lines}, outside 0..=20"),
            "Pass between 0 and 20 lines of context.",
        ));
    }
    Ok(lines as usize)
}

/// Read the one file that holds the answer again, for its text.
///
/// The search already parsed it; reading it a second time costs one open of a file that is
/// `<rel>:<first>-<last>  <kind> <qualified>  (<language id>)`, then the fenced text, then the
/// escape footer when anything had to be escaped.
fn render(
    found: &Match,
    loaded: &LoadedSource,
    include_doc: bool,
    context_lines: usize,
) -> Result<String, ToolError> {
    let text = symbol_text(&loaded.text, &found.symbol, include_doc, context_lines)?;
    let (header, header_escapes) = header(found, &text, loaded.language.id());
    let (block, block_escapes) = fenced_block(&text.text, loaded.language.id());

    let mut out = header;
    out.push_str(&block);
    // The footer counts every character that had to be escaped to produce what the reader can
    // see: the header and the code block alike, in the same format `ast_outline` uses.
    let mut escaped = header_escapes;
    add_escapes(&mut escaped, block_escapes);
    if escaped.total() > 0 {
        let parts: Vec<String> = [
            (escaped.control, "control"),
            (escaped.bidi, "bidi"),
            (escaped.invisible, "invisible"),
        ]
        .into_iter()
        .filter(|(count, _)| *count > 0)
        .map(|(count, label)| format!("{count} {label}"))
        .collect();
        out.push_str(&format!(
            "[escaped: {} characters in names or paths]\n",
            parts.join(", ")
        ));
    }
    Ok(out)
}

/// The one header line, escaped, plus what it cost to escape it.
fn header(found: &Match, text: &SymbolText, language: &str) -> (String, EscapeCounts) {
    let (rel, mut counts) = escape_inline(&found.file.rel);
    let (kind, kind_counts) = escape_inline(found.symbol.kind.as_str());
    let (qualified, qualified_counts) = escape_inline(&found.symbol.qualified);
    add_escapes(&mut counts, kind_counts);
    add_escapes(&mut counts, qualified_counts);

    // A single-line symbol prints one line number, not a range that starts and ends in the
    // same place: `a.rs:1  fn dup` reads as a place, `a.rs:1-1` reads as a bug.
    let place = if text.first_line == text.last_line {
        format!("{}", text.first_line)
    } else {
        format!("{}-{}", text.first_line, text.last_line)
    };
    (
        format!("{rel}:{place}  {kind} {qualified}  ({language})\n"),
        counts,
    )
}

/// Add one set of escape counts into another. `render` has no such method and this crate does
/// not get to change core.
fn add_escapes(into: &mut EscapeCounts, from: EscapeCounts) {
    into.control += from.control;
    into.bidi += from.bidi;
    into.invisible += from.invisible;
}

/// `not_found`: says how many files were actually parsed, so "no such symbol" can be told
/// apart from "nothing was searched".
fn not_found(symbol: &str, parsed_files: usize) -> ToolError {
    let (symbol, _) = escape_inline(symbol);
    ToolError::new(
        ErrorCode::NotFound,
        format!("No symbol named `{symbol}` found in {parsed_files} files."),
        "Check the name with ast_outline.",
    )
}

/// `ambiguous`: the true number, then as many candidates as fit, so the caller can narrow with
/// `path` instead of guessing.
fn ambiguous(symbol: &str, candidates: &[Match], total: usize) -> ToolError {
    let (symbol, _) = escape_inline(symbol);
    let mut message = format!("{total} symbols match `{symbol}`:");
    for (i, found) in candidates.iter().take(CANDIDATES_MAX).enumerate() {
        let (rel, _) = escape_inline(&found.file.rel);
        let (kind, _) = escape_inline(found.symbol.kind.as_str());
        let (qualified, _) = escape_inline(&found.symbol.qualified);
        message.push_str(&format!(
            "\n{}. {rel}:L{} {kind} {qualified}",
            i + 1,
            found.symbol.start_line
        ));
    }
    if total > CANDIDATES_MAX {
        message.push_str(&format!("\n... and {} more", total - CANDIDATES_MAX));
    }
    ToolError::new(
        ErrorCode::Ambiguous,
        message,
        "Repeat the call with `path` set to one of these files.",
    )
}
