//! `ast_search` (docs/TOOLS.md, docs/PATTERNS.md).

use crate::context::ToolContext;
use crate::source::{LoadedSource, Skip, classify, load};
use opencrayast_core::boundary::ResolvedPath;
use opencrayast_core::error::{ErrorCode, ToolError};
use opencrayast_core::render::{EscapeCounts, escape_inline};
use opencrayast_core::walk::{WalkOptions, WalkResult, walk};
use opencrayast_lang::Language;
use opencrayast_query::pattern::{
    CaptureKind, CompiledRule, Match, Pattern, Rule, SearchBudget, search,
};
use std::time::{Duration, Instant};

/// Total comparison-step budget of one call (all files together). Exposed in the user
/// configuration in milestone M5; until then this constant is the budget.
pub const SEARCH_STEP_BUDGET: u64 = 5_000_000;

/// Arguments of `ast_search`.
#[derive(Debug, Clone, Default)]
pub struct SearchArgs {
    /// Code in the target language with metavariables. Required, 1..=16384 bytes.
    pub pattern: String,
    /// Language id (`Language::from_id`). Required when the searched files are of several
    /// languages; when given, only files of that language are searched.
    pub language: Option<String>,
    /// Files or directories (default `["."]`); 1..=64 entries.
    pub paths: Vec<String>,
    /// Extra constraints (docs/PATTERNS.md "Rules").
    pub rule: Option<Rule>,
    /// Lines of context around each match, 0..=5 (default 0).
    pub context_lines: Option<u64>,
    /// Maximum matches, 1..=`limits.max_results` (default 100).
    pub limit: Option<u64>,
}

/// Search by syntactic shape.
///
/// ## Output format (exact; golden-tested in `tests/search_tools_spec.rs`)
///
/// ```text
/// Found <N> matches in <F> files for: <pattern, escaped, first line only>
/// <rel>:<l1>:<c1>-<l2>:<c2>  <match text, first line only, at most 120 bytes then "...">[  $NAME = <text>[; $NAME2 = <text>]]
/// ```
///
/// - `<F>` is the number of files that were searched (parsed), `<N>` the matches shown;
/// - lines are sorted by `rel` (byte order), then document order within a file;
/// - the capture part lists the named captures in pattern order, each as `$NAME = <text>` (list
///   captures as `$$$NAME = <text>`), the text first-line-only and at most 80 bytes then `...`,
///   joined by `; `; the part and its two leading spaces are omitted when there are no captures;
/// - with `context_lines > 0` each match is followed by the lines `start-ctx ..= end+ctx`
///   (clamped to the file), each as `  <line>: <text>` (context) or `  <line>> <text>` (a line the
///   match touches); the number is not padded; the text is the line as it is, escaped;
/// - zero matches is a success: `0 matches in <F> files (<language id>)` and nothing else but footers.
///
/// Footer lines, each only when applicable, in this order:
///
/// ```text
/// [truncated: showing <limit> matches, more exist; narrow `paths`, add a `rule` or raise `limit`]
/// [syntax errors: <rel> (<k>)[, <rel> (<k>)...]]            (searched files that did not parse cleanly; at most 10, then ", and <M> more")
/// [skipped: <a> ignored, <b> links, <c> special, <d> unsupported language, <e> other language, <f> too large, <g> not utf-8, <h> unreadable]   (only nonzero parts)
/// [walk truncated at <max_scan_files> files; narrow `paths`]
/// [escaped: <c> control, <b> bidi, <i> invisible characters in names, paths or text]   (only nonzero parts)
/// ```
///
/// ## Semantics
/// - Language: `args.language` if given (`invalid_args` if unknown), else the language of the
///   files found (all supported files must share one; otherwise `invalid_args` with message
///   `mixed languages: <id>, <id>` and next "Pass `language`."). A file of another language than
///   the chosen one is counted as `other language`. `typescript` and `tsx` are different
///   languages. No supported file at all and no `language` => `invalid_args`.
/// - The pattern is compiled once (`invalid_pattern` via `PatternError`), the rule once.
/// - Files are read and parsed like `ast_outline` (same limits and skip classification) and
///   searched with `SearchBudget { max_steps: remaining of SEARCH_STEP_BUDGET, deadline: now +
///   limits.call_timeout_ms, max_matches: remaining of limit + 1 }`. One shared step budget for the
///   whole call: when it is spent the call fails with `budget_exceeded` (message names "steps"),
///   when the deadline passes with `timeout`; partial results are NOT returned (a partial answer
///   that looks complete is worse than an error).
/// - Argument validation (`invalid_args`): `pattern` non-empty and <= 16384 bytes; `paths` 1..=64
///   entries, each non-empty; `context_lines` 0..=5; `limit` 1..=`limits.max_results`.
pub fn ast_search(ctx: &ToolContext, args: &SearchArgs) -> Result<String, ToolError> {
    let validated = validate(ctx, args)?;
    let deadline = Instant::now() + Duration::from_millis(ctx.limits.call_timeout_ms.max(1));
    let mut skips = SkipCounts::default();
    let mut walk_truncated = false;

    let mut candidates: Vec<ResolvedPath> = Vec::new();
    for path in &validated.paths {
        let resolved = ctx.boundary.resolve_read(path)?;
        match ctx.boundary.open_read(&resolved) {
            Ok(_) => candidates.push(resolved),
            Err(open_err) => match ctx.boundary.read_dir(&resolved) {
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
                    skips.add_walk(&result);
                    walk_truncated |= result.truncated;
                    candidates.extend(result.files);
                }
                Err(_) => return Err(open_err),
            },
        }
    }
    candidates.sort_by(|a, b| a.rel.as_bytes().cmp(b.rel.as_bytes()));
    candidates.dedup_by(|a, b| a.rel == b.rel);

    // Load every candidate; classify skips; keep successful loads for language choice.
    let mut loaded: Vec<(ResolvedPath, LoadedSource)> = Vec::new();
    for file in &candidates {
        if Instant::now() >= deadline {
            return Err(timeout_err());
        }
        match load(ctx, file) {
            Ok(src) => loaded.push((file.clone(), src)),
            Err(e) => skips.add_skip(classify(&e)),
        }
    }

    let language = match validated.language {
        Some(lang) => lang,
        None => {
            let mut ids: Vec<&str> = loaded.iter().map(|(_, s)| s.language.id()).collect();
            ids.sort_unstable();
            ids.dedup();
            if ids.is_empty() {
                return Err(ToolError::new(
                    ErrorCode::InvalidArgs,
                    "no supported file to search and no language was given",
                    "Pass `language`, or point `paths` at a source file.",
                ));
            }
            if ids.len() > 1 {
                return Err(ToolError::new(
                    ErrorCode::InvalidArgs,
                    format!("mixed languages: {}", ids.join(", ")),
                    "Pass `language`.",
                ));
            }
            Language::from_id(ids[0]).ok_or_else(|| {
                ToolError::new(
                    ErrorCode::Internal,
                    "language id from a loaded file was not recognised",
                    "Report this as a bug.",
                )
            })?
        }
    };

    let pattern = Pattern::compile(language, &validated.pattern)?;
    let rule = match &args.rule {
        Some(r) => Some(CompiledRule::compile(language, r)?),
        None => None,
    };

    let mut steps_left = SEARCH_STEP_BUDGET;
    let mut matches: Vec<(String, Match, String)> = Vec::new(); // rel, match, source text
    let mut searched_files: Vec<(String, usize)> = Vec::new(); // rel, error_count
    let mut truncated = false;
    let limit = validated.limit;

    for (file, src) in &loaded {
        if Instant::now() >= deadline {
            return Err(timeout_err());
        }
        if src.language != language {
            skips.other_language += 1;
            continue;
        }
        searched_files.push((file.rel.clone(), src.parsed.error_count));

        let room = limit.saturating_add(1).saturating_sub(matches.len());
        if room == 0 {
            truncated = true;
            continue;
        }
        let budget = SearchBudget {
            max_steps: steps_left,
            deadline: Some(deadline),
            max_matches: room,
        };
        let outcome = search(&src.parsed, &src.text, &pattern, rule.as_ref(), &budget)?;
        steps_left = steps_left.saturating_sub(outcome.steps_used);
        if outcome.truncated {
            truncated = true;
        }
        for m in outcome.matches {
            if matches.len() >= limit {
                truncated = true;
                break;
            }
            matches.push((file.rel.clone(), m, src.text.clone()));
        }
    }

    Ok(render(
        &validated,
        language,
        &matches,
        &searched_files,
        &skips,
        truncated,
        walk_truncated,
        ctx.limits.max_scan_files,
    ))
}

struct Validated {
    pattern: String,
    language: Option<Language>,
    paths: Vec<String>,
    context_lines: usize,
    limit: usize,
}

fn validate(ctx: &ToolContext, args: &SearchArgs) -> Result<Validated, ToolError> {
    if args.pattern.is_empty() || args.pattern.len() > 16_384 {
        return Err(ToolError::new(
            ErrorCode::InvalidArgs,
            format!("pattern is {} bytes, outside 1..=16384", args.pattern.len()),
            "Pass a non-empty pattern of at most 16384 bytes.",
        ));
    }
    if args.paths.is_empty() || args.paths.len() > 64 {
        return Err(ToolError::new(
            ErrorCode::InvalidArgs,
            format!("paths has {} entries, outside 1..=64", args.paths.len()),
            "Pass between 1 and 64 paths.",
        ));
    }
    for p in &args.paths {
        if p.is_empty() {
            return Err(ToolError::new(
                ErrorCode::InvalidArgs,
                "a path entry is empty",
                "Pass non-empty path strings.",
            ));
        }
    }
    let context_lines = args.context_lines.unwrap_or(0);
    if context_lines > 5 {
        return Err(ToolError::new(
            ErrorCode::InvalidArgs,
            format!("context_lines is {context_lines}, outside 0..=5"),
            "Pass a context_lines between 0 and 5.",
        ));
    }
    let max = ctx.limits.max_results;
    let limit = args.limit.unwrap_or(100);
    if limit < 1 || limit > max {
        return Err(ToolError::new(
            ErrorCode::InvalidArgs,
            format!("limit is {limit}, outside 1..={max}"),
            format!("Pass a limit between 1 and {max}."),
        ));
    }
    let language = match &args.language {
        None => None,
        Some(id) => Some(
            Language::from_id(id)
                .filter(|l| l.is_available())
                .ok_or_else(|| {
                    ToolError::new(
                        ErrorCode::InvalidArgs,
                        format!(
                            "unknown language {id:?}; supported: {}",
                            supported_language_ids()
                        ),
                        "Pass a language id from the supported list.",
                    )
                })?,
        ),
    };
    Ok(Validated {
        pattern: args.pattern.clone(),
        language,
        paths: args.paths.clone(),
        context_lines: context_lines as usize,
        limit: limit as usize,
    })
}

fn timeout_err() -> ToolError {
    ToolError::new(
        ErrorCode::Timeout,
        "the call deadline passed before the search finished",
        "Narrow `paths`, simplify the pattern, or raise `call_timeout_ms`.",
    )
}

fn supported_language_ids() -> String {
    Language::all()
        .iter()
        .filter(|l| l.is_available())
        .map(|l| l.id())
        .collect::<Vec<_>>()
        .join(", ")
}

#[derive(Debug, Default, Clone, Copy)]
struct SkipCounts {
    ignored: usize,
    links: usize,
    special: usize,
    unsupported: usize,
    other_language: usize,
    too_large: usize,
    not_utf8: usize,
    unreadable: usize,
}

impl SkipCounts {
    fn add_walk(&mut self, result: &WalkResult) {
        self.ignored += result.skipped_ignored;
        self.links += result.skipped_links;
        self.special += result.skipped_special;
    }

    fn add_skip(&mut self, skip: Skip) {
        match skip {
            Skip::UnsupportedLanguage => self.unsupported += 1,
            Skip::TooLarge => self.too_large += 1,
            Skip::NotUtf8 => self.not_utf8 += 1,
            Skip::Budget | Skip::Unreadable => self.unreadable += 1,
        }
    }

    fn line(self) -> Option<String> {
        let parts: Vec<String> = [
            (self.ignored, "ignored"),
            (self.links, "links"),
            (self.special, "special"),
            (self.unsupported, "unsupported language"),
            (self.other_language, "other language"),
            (self.too_large, "too large"),
            (self.not_utf8, "not utf-8"),
            (self.unreadable, "unreadable"),
        ]
        .into_iter()
        .filter(|(c, _)| *c > 0)
        .map(|(c, label)| format!("{c} {label}"))
        .collect();
        (!parts.is_empty()).then(|| format!("[skipped: {}]\n", parts.join(", ")))
    }
}

fn add_escapes(into: &mut EscapeCounts, from: EscapeCounts) {
    into.control += from.control;
    into.bidi += from.bidi;
    into.invisible += from.invisible;
}

#[allow(clippy::too_many_arguments)]
fn render(
    args: &Validated,
    language: Language,
    matches: &[(String, Match, String)],
    searched: &[(String, usize)],
    skips: &SkipCounts,
    truncated: bool,
    walk_truncated: bool,
    max_scan_files: u64,
) -> String {
    let mut escapes = EscapeCounts::default();
    let mut out = String::new();
    let files_n = searched.len();

    if matches.is_empty() {
        out.push_str(&format!(
            "0 matches in {files_n} files ({})\n",
            language.id()
        ));
    } else {
        let (pat, pat_esc) = escape_inline(first_line(&args.pattern));
        add_escapes(&mut escapes, pat_esc);
        out.push_str(&format!(
            "Found {} matches in {files_n} files for: {pat}\n",
            matches.len()
        ));
        for (rel, m, source) in matches {
            let (rel_e, rel_esc) = escape_inline(rel);
            add_escapes(&mut escapes, rel_esc);
            let text = trunc_bytes(first_line(&m.text), 120);
            let (text_e, text_esc) = escape_inline(&text);
            add_escapes(&mut escapes, text_esc);
            out.push_str(&format!(
                "{rel_e}:{}:{}-{}:{}  {text_e}",
                m.start_line, m.start_col, m.end_line, m.end_col
            ));
            if !m.captures.is_empty() {
                let mut parts = Vec::new();
                for c in &m.captures {
                    let name = match c.kind {
                        CaptureKind::One => format!("${}", c.name),
                        CaptureKind::List => format!("$$${}", c.name),
                    };
                    let ct = trunc_bytes(first_line(&c.text), 80);
                    let (ct_e, ct_esc) = escape_inline(&ct);
                    add_escapes(&mut escapes, ct_esc);
                    parts.push(format!("{name} = {ct_e}"));
                }
                out.push_str("  ");
                out.push_str(&parts.join("; "));
            }
            out.push('\n');

            if args.context_lines > 0 {
                push_context(&mut out, &mut escapes, source, m, args.context_lines);
            }
        }
    }

    if truncated {
        out.push_str(&format!(
            "[truncated: showing {} matches, more exist; narrow `paths`, add a `rule` or raise `limit`]\n",
            args.limit
        ));
    }
    if let Some(line) = syntax_errors_line(searched, &mut escapes) {
        out.push_str(&line);
    }
    if let Some(line) = skips.line() {
        out.push_str(&line);
    }
    if walk_truncated {
        out.push_str(&format!(
            "[walk truncated at {max_scan_files} files; narrow `paths`]\n"
        ));
    }
    if let Some(line) = escaped_line(&escapes) {
        out.push_str(&line);
    }
    out
}

fn syntax_errors_line(searched: &[(String, usize)], escapes: &mut EscapeCounts) -> Option<String> {
    let bad: Vec<&(String, usize)> = searched.iter().filter(|(_, k)| *k > 0).collect();
    if bad.is_empty() {
        return None;
    }
    let mut parts = Vec::new();
    for (i, (rel, k)) in bad.iter().enumerate() {
        if i >= 10 {
            break;
        }
        let (rel_e, esc) = escape_inline(rel);
        add_escapes(escapes, esc);
        parts.push(format!("{rel_e} ({k})"));
    }
    let mut line = format!("[syntax errors: {}]", parts.join(", "));
    if bad.len() > 10 {
        line.push_str(&format!(", and {} more", bad.len() - 10));
    }
    line.push('\n');
    Some(line)
}

fn escaped_line(counts: &EscapeCounts) -> Option<String> {
    let mut parts = Vec::new();
    if counts.control > 0 {
        parts.push(format!("{} control", counts.control));
    }
    if counts.bidi > 0 {
        parts.push(format!("{} bidi", counts.bidi));
    }
    if counts.invisible > 0 {
        parts.push(format!(
            "{} invisible characters in names, paths or text",
            counts.invisible
        ));
    }
    (!parts.is_empty()).then(|| format!("[escaped: {}]\n", parts.join(", ")))
}

fn push_context(out: &mut String, escapes: &mut EscapeCounts, source: &str, m: &Match, ctx: usize) {
    let lines: Vec<&str> = source.split('\n').collect();
    // 1-based lines; end_line may point past the last content line for EOF positions.
    let start = m.start_line.saturating_sub(ctx).max(1);
    let end = (m.end_line + ctx).min(lines.len().max(1));
    let match_lo = m.start_line;
    let match_hi = m.end_line;
    for line_no in start..=end {
        let idx = line_no.saturating_sub(1);
        let raw = if idx < lines.len() { lines[idx] } else { "" };
        let (text, esc) = escape_inline(raw);
        add_escapes(escapes, esc);
        let mark = if line_no >= match_lo && line_no <= match_hi {
            '>'
        } else {
            ':'
        };
        out.push_str(&format!("  {line_no}{mark} {text}\n"));
    }
}

fn first_line(s: &str) -> &str {
    s.split('\n').next().unwrap_or(s)
}

fn trunc_bytes(s: &str, max: usize) -> String {
    if s.len() <= max {
        return s.to_string();
    }
    let mut end = max;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}...", &s[..end])
}
