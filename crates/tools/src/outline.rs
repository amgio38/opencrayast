//! `ast_outline` (docs/TOOLS.md).

use crate::context::ToolContext;
use crate::source::{LoadedSource, Skip, classify, load};
use opencrayast_core::boundary::ResolvedPath;
use opencrayast_core::error::{ErrorCode, ToolError};
use opencrayast_core::render::{EscapeCounts, escape_inline};
use opencrayast_core::walk::{WalkOptions, WalkResult, walk};
use opencrayast_query::{OutlineOptions, Symbol, SymbolKind, outline as query_outline};

/// Arguments of `ast_outline`.
#[derive(Debug, Clone, Default)]
pub struct OutlineArgs {
    /// A file or a directory (directories are walked, honouring ignore rules). Required.
    pub path: String,
    /// Nesting depth of symbols to show, 1..=6 (default 3).
    pub depth: Option<u64>,
    /// Filter by kind names (`fn`, `struct`, ...); unknown names are `invalid_args`.
    pub kinds: Option<Vec<String>>,
    /// Add the first line of each doc comment (default false).
    pub include_docs: Option<bool>,
    /// Maximum symbols in the output, 1..=`limits.max_results` (default 200).
    pub limit: Option<u64>,
}

/// The skeleton of a file or directory.
///
/// ## Output format (exact; golden-tested)
///
/// Per outlined file, in `rel` byte order:
///
/// ```text
/// <rel>  <language id>  <N> lines[ (<K> syntax errors)]
/// <indent><kind> <name> L<start>[-<end>]  <signature>
/// ```
///
/// - `N` is `text.lines().count()`; ` (K syntax errors)` only when `K > 0` (`1 syntax error` for 1);
/// - `indent` is two spaces per `depth` (depth 1 = two spaces);
/// - `<name>` is `Symbol::name` (not qualified); `L<start>` alone when `start == end`;
/// - `<signature>` is `Symbol::signature`; when `include_docs` is true and a doc exists, a line
///   `<indent>  /// <doc first line>` follows (the indent of the symbol plus two spaces);
/// - `<rel>`, names, signatures and docs go through `render::escape_inline`.
///
/// Footer lines, each only when applicable, in this order:
///
/// ```text
/// [truncated: showing <shown> of <total> symbols in <F> files; narrow `path`, lower `depth` or filter `kinds`]
/// [skipped: <a> ignored, <b> links, <c> special, <d> unsupported language, <e> too large, <f> not utf-8, <g> unreadable]   (only nonzero parts)
/// [walk truncated at <max_scan_files> files; narrow `path`]
/// [escaped: <c> control, <b> bidi, <i> invisible characters in names or paths]   (only nonzero parts, joined by ", ")
/// ```
///
/// A file argument that cannot be outlined is an error (`unsupported_language`, `file_too_large`,
/// `not_utf8`, `budget_exceeded`, `timeout`, boundary refusals); inside a directory walk such files
/// are counted in `[skipped: ...]`. A directory with no outlinable file returns the footer lines only.
/// The whole output is capped at `limits.max_output_bytes`: lines are added until the next would
/// exceed it, then the truncation line (which counts toward the cap's reserve) is added.
pub fn ast_outline(ctx: &ToolContext, args: &OutlineArgs) -> Result<String, ToolError> {
    let query_options = OutlineOptions {
        max_depth: arg_depth(args)?,
        kinds: arg_kinds(args)?,
        include_docs: args.include_docs.unwrap_or(false),
    };
    let limit = arg_limit(ctx, args)? as usize;

    let resolved = ctx.boundary.resolve_read(&args.path)?;
    // Whether the argument is a file or a directory decides how a load failure is reported, so
    // it has to be settled before anything is read. Both answers come from the boundary: a
    // successful `open_read` means a regular file (a FIFO, a socket and a directory all fail
    // there), and a successful `read_dir` means a directory. There is no third way to ask, and
    // no `stat` on a path string.
    let single_file = match ctx.boundary.open_read(&resolved) {
        Ok(_) => true,
        Err(open_error) => match ctx.boundary.read_dir(&resolved) {
            Ok(_) => false,
            // Neither a file nor a directory. Report the `open_read` wording, which knows the
            // difference between a special file and a permission error.
            Err(_) => return Err(open_error),
        },
    };

    // Files in `rel` byte order (the walk sorts), with everything the walk skipped counted
    // (OUT-07). A file argument is not walked, so nothing can be skipped: its errors are
    // returned to the caller instead of being counted.
    let mut out = Output::new(ctx.limits.max_output_bytes as usize);
    let mut skips = SkipCounts::default();
    if single_file {
        out.symbols_of_file(ctx, &resolved, &query_options, limit, &mut skips, true)?;
    } else {
        let result = walk(
            &ctx.boundary,
            &resolved,
            &WalkOptions {
                max_files: ctx.limits.max_scan_files,
                respect_gitignore: ctx.respect_gitignore,
                extra_ignore: ctx.extra_ignore.clone(),
            },
        )?;
        skips = SkipCounts::from_walk(&result);
        out.walk_stopped = result.truncated;
        for file in &result.files {
            out.symbols_of_file(ctx, file, &query_options, limit, &mut skips, false)?;
        }
    }
    Ok(out.finish(&skips, ctx.limits.max_scan_files))
}

/// The validated symbol-depth argument: 1..=6, default 3 (docs/TOOLS.md `ast_outline`).
fn arg_depth(args: &OutlineArgs) -> Result<usize, ToolError> {
    let depth = args.depth.unwrap_or(3);
    if !(1..=6).contains(&depth) {
        return Err(ToolError::new(
            ErrorCode::InvalidArgs,
            format!("depth is {depth}, outside 1..=6"),
            "Pass a depth between 1 and 6.",
        ));
    }
    Ok(depth as usize)
}

/// The validated `limit`: 1..=`limits.max_results`, default `limits.max_results`.
fn arg_limit(ctx: &ToolContext, args: &OutlineArgs) -> Result<u64, ToolError> {
    let max = ctx.limits.max_results;
    let limit = args.limit.unwrap_or(max);
    if limit < 1 || limit > max {
        return Err(ToolError::new(
            ErrorCode::InvalidArgs,
            format!("limit is {limit}, outside 1..={max}"),
            format!("Pass a limit between 1 and {max}."),
        ));
    }
    Ok(limit)
}

/// The validated `kinds` filter. An unknown name is an error and never an empty result: a
/// silently empty outline reads like "this file has no symbols".
fn arg_kinds(args: &OutlineArgs) -> Result<Option<Vec<SymbolKind>>, ToolError> {
    let Some(names) = &args.kinds else {
        return Ok(None);
    };
    let mut kinds = Vec::with_capacity(names.len());
    for name in names {
        let kind = SymbolKind::parse(name).ok_or_else(|| {
            ToolError::new(
                ErrorCode::InvalidArgs,
                format!("{name} is not a symbol kind."),
                "Use kinds from this list: module, namespace, class, struct, enum, interface, \
                 trait, impl, fn, method, const, static, type, field, variable, macro.",
            )
        })?;
        kinds.push(kind);
    }
    Ok(Some(kinds))
}

/// Everything skipped in a directory walk, in one place, so the footer can print the nonzero
/// parts in the fixed order of the format.
#[derive(Debug, Default, Clone, Copy)]
struct SkipCounts {
    /// Entries an ignore rule, a VCS directory or the depth ceiling kept out.
    ignored: usize,
    /// Symbolic links, never followed.
    links: usize,
    /// Special files, undecodable names, unusable ignore files, unlistable directories.
    special: usize,
    /// Files with no grammar.
    unsupported: usize,
    /// Files over `max_file_bytes`.
    too_large: usize,
    /// Files that are not valid UTF-8.
    not_utf8: usize,
    /// Files that could not be parsed in budget, opened or read.
    unreadable: usize,
}

impl SkipCounts {
    /// Start from what `core::walk` already counted. The walk knows nothing about languages or
    /// file contents, so its three counters and the four content ones are disjoint.
    fn from_walk(result: &WalkResult) -> Self {
        Self {
            ignored: result.skipped_ignored,
            links: result.skipped_links,
            special: result.skipped_special,
            ..Self::default()
        }
    }

    /// Count one file that could not be outlined.
    fn add(&mut self, skip: Skip) {
        match skip {
            Skip::UnsupportedLanguage => self.unsupported += 1,
            Skip::TooLarge => self.too_large += 1,
            Skip::NotUtf8 => self.not_utf8 += 1,
            // A file that ran out of parse budget is reported as unreadable: the walk counts
            // files it could not produce symbols for, and both mean "this file is not in the
            // outline". The distinction would only matter to a caller who can act on it, and
            // the fix for both is a narrower `path` or a smaller file.
            Skip::Budget | Skip::Unreadable => self.unreadable += 1,
        }
    }

    /// The `[skipped: ...]` line, or `None` when nothing was skipped.
    fn line(self) -> Option<String> {
        let parts: Vec<String> = [
            (self.ignored, "ignored"),
            (self.links, "links"),
            (self.special, "special"),
            (self.unsupported, "unsupported language"),
            (self.too_large, "too large"),
            (self.not_utf8, "not utf-8"),
            (self.unreadable, "unreadable"),
        ]
        .into_iter()
        .filter(|(count, _)| *count > 0)
        .map(|(count, label)| format!("{count} {label}"))
        .collect();
        (!parts.is_empty()).then(|| format!("[skipped: {}]\n", parts.join(", ")))
    }
}

/// Add one set of escape counts into another. `render` has no such method, and this crate does
/// not get to change core.
fn add_escapes(into: &mut EscapeCounts, from: EscapeCounts) {
    into.control += from.control;
    into.bidi += from.bidi;
    into.invisible += from.invisible;
}

/// One rendered output line: what it says, what it costs, what it contributes.
struct Line {
    /// The line, including its trailing newline.
    text: String,
    /// 1 for a symbol line, 0 for a file header or a doc line.
    symbols: usize,
    /// Zero-based index of the file this line belongs to. Files are rendered in order, so the
    /// files that reached the output are a contiguous run of indices and their number is
    /// `last - first + 1`.
    file: usize,
    /// What had to be escaped to build this line.
    escaped: EscapeCounts,
}

/// Assembles the output and enforces the byte cap (LMT-04, LMT-05).
///
/// # Why the lines are built first and trimmed afterwards
///
/// Two different caps can bite at once: the caller's `limit` on symbols, and
/// `max_output_bytes` on bytes. Neither can be applied while rendering, because the footer that
/// explains the cut is only known once the cut is known - and a footer that does not fit must
/// not push the output over the cap it is reporting. So every line is rendered first (nothing
/// is re-rendered), then a prefix of them is kept, and the footer is rebuilt for that prefix
/// until the whole thing fits. The cost is that the last few lines of a capped outline are
/// chosen by a loop rather than in one pass; the benefit is that the output can never exceed
/// the cap and can never claim to be complete when it is not.
struct Output {
    /// The cap in bytes.
    cap: usize,
    /// Every rendered line, in order.
    lines: Vec<Line>,
    /// Symbols emitted so far, never above the caller's `limit`.
    symbols: usize,
    /// Index of the file being rendered, counting files as they are started. Not the number of
    /// files that emitted a symbol: that is only known once the cap has been applied.
    file_index: usize,
    /// Symbols found after the depth and kind filters, before `limit` and before the byte cap.
    /// This is the denominator of the `[truncated: ...]` line.
    total: usize,
    /// Whether the walk stopped early, for the walk-truncation line.
    walk_stopped: bool,
}

impl Output {
    fn new(cap: usize) -> Self {
        Self {
            cap,
            lines: Vec::new(),
            symbols: 0,
            file_index: 0,
            total: 0,
            walk_stopped: false,
        }
    }

    /// Render one file: its header, then its symbols until the caller's `limit` is reached.
    ///
    /// `fatal` is set for a single-file argument, where a file that cannot be outlined is an
    /// error the caller must see rather than a counter in the footer.
    fn symbols_of_file(
        &mut self,
        ctx: &ToolContext,
        file: &ResolvedPath,
        options: &OutlineOptions,
        limit: usize,
        skips: &mut SkipCounts,
        fatal: bool,
    ) -> Result<(), ToolError> {
        let loaded = match load(ctx, file) {
            Ok(loaded) => loaded,
            Err(e) if fatal => return Err(e),
            Err(e) => {
                skips.add(classify(&e));
                return Ok(());
            }
        };
        let symbols = query_outline(&loaded.parsed, &loaded.text, options);
        self.total += symbols.len();
        self.file_index += 1;
        self.file_header(file, &loaded);
        let room = limit.saturating_sub(self.symbols);
        for symbol in symbols.iter().take(room) {
            self.symbol(symbol);
        }
        Ok(())
    }

    /// `<rel>  <language id>  <N> lines[ (<K> syntax errors)]`.
    ///
    /// The line count comes from the text, so a file with no symbols at all still says how big
    /// it is, and the syntax-error count is in the header because a caller has to know how much
    /// to trust the symbols under it (docs/TOOLS.md "Syntax errors").
    fn file_header(&mut self, file: &ResolvedPath, loaded: &LoadedSource) {
        let (rel, counts) = escape_inline(&file.rel);
        let errors = loaded.parsed.error_count;
        let mut text = format!(
            "{rel}  {}  {} lines",
            loaded.language.id(),
            loaded.text.lines().count()
        );
        if errors > 0 {
            // Singular for exactly one, so the line reads like a sentence.
            let noun = if errors == 1 {
                "syntax error"
            } else {
                "syntax errors"
            };
            text.push_str(&format!(" ({errors} {noun})"));
        }
        text.push('\n');
        self.lines.push(Line {
            text,
            symbols: 0,
            file: self.file_index,
            escaped: counts,
        });
    }

    /// `  <kind> <name> L<start>[-<end>]  <signature>`, and under it the doc line when asked.
    fn symbol(&mut self, symbol: &Symbol) {
        let indent = "  ".repeat(symbol.depth);
        let (name, name_counts) = escape_inline(&symbol.name);
        let (signature, signature_counts) = escape_inline(&symbol.signature);
        let mut escaped = name_counts;
        add_escapes(&mut escaped, signature_counts);
        let extent = if symbol.start_line == symbol.end_line {
            format!("L{}", symbol.start_line)
        } else {
            format!("L{}-{}", symbol.start_line, symbol.end_line)
        };
        self.lines.push(Line {
            text: format!(
                "{indent}{} {name} {extent}  {signature}\n",
                symbol.kind.as_str()
            ),
            symbols: 1,
            file: self.file_index,
            escaped,
        });
        if let Some(doc) = &symbol.doc_first_line {
            let (doc, doc_counts) = escape_inline(doc);
            self.lines.push(Line {
                text: format!("{indent}  /// {doc}\n"),
                symbols: 0,
                file: self.file_index,
                escaped: doc_counts,
            });
        }
        self.symbols += 1;
    }

    /// Keep the longest prefix of lines that fits under the cap together with its footer, and
    /// return the whole output.
    fn finish(self, skips: &SkipCounts, max_scan_files: u64) -> String {
        let mut keep = self.lines.len();
        loop {
            let (body_bytes, symbols, files, escaped) = self.measure(keep);
            let footer = self.footer(symbols, files, &escaped, skips, max_scan_files);
            let footer_bytes: usize = footer.iter().map(String::len).sum();
            if body_bytes + footer_bytes <= self.cap {
                let mut out = String::with_capacity(body_bytes + footer_bytes);
                for line in self.lines.iter().take(keep) {
                    out.push_str(&line.text);
                }
                for line in footer {
                    out.push_str(&line);
                }
                return out;
            }
            if keep == 0 {
                // Even the footer alone does not fit. The cap is a hard limit, so the footer
                // loses whole lines from the end rather than the output overrunning it. With
                // any sane `max_output_bytes` this cannot happen; it is here so that a
                // deliberately tiny limit produces a short answer instead of a wrong one.
                let mut footer = footer;
                while !footer.is_empty() && footer.iter().map(String::len).sum::<usize>() > self.cap
                {
                    footer.pop();
                }
                return footer.concat();
            }
            keep -= 1;
        }
    }

    /// Bytes, symbols, files-with-symbols and escape counts of the first `keep` lines.
    fn measure(&self, keep: usize) -> (usize, usize, usize, EscapeCounts) {
        let mut bytes = 0;
        let mut symbols = 0;
        let mut escaped = EscapeCounts::default();
        let mut span: Option<(usize, usize)> = None;
        for line in self.lines.iter().take(keep) {
            bytes += line.text.len();
            symbols += line.symbols;
            add_escapes(&mut escaped, line.escaped);
            if line.symbols > 0 {
                span = Some(match span {
                    None => (line.file, line.file),
                    Some((first, _)) => (first, line.file),
                });
            }
        }
        // Files are rendered in order and every file that emitted a symbol contributes at least
        // one symbol line, so the files that reached the output are exactly the indices between
        // the first and the last symbol line. A file whose header made it in but whose symbols
        // were all cut contributes nothing, which is what the caller should be told.
        let files = span.map_or(0, |(first, last)| last - first + 1);
        (bytes, symbols, files, escaped)
    }

    /// The footer lines, in the order of the format spec, each only when it applies.
    fn footer(
        &self,
        shown: usize,
        files: usize,
        escaped: &EscapeCounts,
        skips: &SkipCounts,
        max_scan_files: u64,
    ) -> Vec<String> {
        let mut lines = Vec::new();
        if shown < self.total {
            lines.push(format!(
                "[truncated: showing {shown} of {} symbols in {files} files; narrow `path`, \
                 lower `depth` or filter `kinds`]\n",
                self.total
            ));
        }
        lines.extend(skips.line());
        if self.walk_stopped {
            lines.push(format!(
                "[walk truncated at {max_scan_files} files; narrow `path`]\n"
            ));
        }
        lines.extend(escaped_line(escaped));
        lines
    }
}

/// `[escaped: <c> control, <b> bidi, <i> invisible characters in names or paths]`, only when
/// something was escaped. The escape count describes what the reader can actually see, so it
/// is accumulated over the lines that survived the cap.
fn escaped_line(escaped: &EscapeCounts) -> Option<String> {
    let parts: Vec<String> = [
        (escaped.control, "control"),
        (escaped.bidi, "bidi"),
        (escaped.invisible, "invisible"),
    ]
    .into_iter()
    .filter(|(count, _)| *count > 0)
    .map(|(count, label)| format!("{count} {label}"))
    .collect();
    (!parts.is_empty()).then(|| {
        format!(
            "[escaped: {} characters in names or paths]\n",
            parts.join(", ")
        )
    })
}
