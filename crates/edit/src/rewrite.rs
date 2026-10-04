//! The rewrite generator: a pattern, an optional rule and a replacement template turned into an
//! **edit set** for one file (docs/EDIT-MODEL.md "`rewrite`", docs/PATTERNS.md "Rewrite
//! templates"). A pure function of bytes: it reads nothing, writes nothing. The shell validates
//! the result with `validate_edits` regardless (E-1), builds the plan, and runs the gates.
//!
//! What this module adds on top of search and `expand_template`:
//! 1. **Overlap resolution**: the outermost match is rewritten, nested ones are dropped and
//!    reported.
//! 2. **Meaning preservation** (the "grouping" rule): substituting captured text can change the
//!    meaning (`$X * 2` with `$X = a + b` would become `a + b * 2`). A captured *expression* is
//!    wrapped in the language's grouping parentheses exactly when the substitution would stop it
//!    from being a single syntax node, and every wrap is reported.
//! 3. **Comment preservation**: a comment that sits inside a match but outside every capture
//!    would silently disappear; that is refused unless the request allows it, and always reported.
//!
//! # How the grouping check locates a substituted capture
//!
//! Re-parsing the whole rewritten file gives a tree, but finding "the node that came from capture
//! `$X`" by its text is ambiguous (the same text may appear elsewhere). So each expansion records
//! where it placed every single capture, and the offsets are translated into the rewritten file's
//! coordinate space as the edits are applied. The check then asks the tree for the named nodes
//! covering that exact range: exactly one means the substitution did not change the structure,
//! more than one (or none) means it did.

use crate::editset::Edit;
use crate::template::{ExpandOptions, expand_template_wrapping, indent_of_line, resolve_overlaps};
use opencrayast_core::error::{ErrorCode, ToolError};
use opencrayast_lang::{Language, ParseBudget, ParsedFile, parse};
use opencrayast_query::pattern::{
    Capture, CaptureKind, CompiledRule, Match, Pattern, SearchBudget, search,
};
use std::ops::Range;

/// A byte range of the **original** source, `[start, end)`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Span {
    /// First byte.
    pub start: usize,
    /// One past the last byte.
    pub end: usize,
}

/// What to rewrite and how.
#[derive(Debug, Clone)]
pub struct RewriteRequest<'a> {
    /// The pattern, compiled for the file's language.
    pub pattern: &'a Pattern,
    /// Optional rule, compiled for the same language.
    pub rule: Option<&'a CompiledRule>,
    /// The replacement template (`$NAME`, `$$$NAME`, `$$`; see `expand_template`).
    pub replacement: &'a str,
    /// Accept dropping comments that sit inside a match but outside every capture.
    pub allow_comment_loss: bool,
    /// Budget for the search (steps, deadline, and `max_matches`).
    pub search_budget: SearchBudget,
    /// Budget for re-parsing the rewritten text (the grouping check).
    pub parse_budget: ParseBudget,
    /// Largest expansion of the template for one match, in bytes.
    pub max_expansion_bytes: usize,
}

/// The result for one file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RewriteOutcome {
    /// The edits, strictly ascending and non-overlapping, valid for `validate_edits` against the
    /// source. A match whose expansion is identical to the text it replaces produces **no** edit.
    pub edits: Vec<Edit>,
    /// How many matches the search found (before overlap resolution).
    pub matches_found: usize,
    /// Spans of matches that were dropped because a larger match contains them.
    pub overlaps_dropped: Vec<Span>,
    /// Spans of the rewritten matches in which at least one captured expression was wrapped in
    /// grouping parentheses.
    pub wrapped: Vec<Span>,
    /// Spans of the comments that the rewrite removes (inside a match, outside every capture).
    /// Non-empty only when `allow_comment_loss` was set.
    pub comments_dropped: Vec<Span>,
}

/// How many times the grouping check may add a round of parentheses before giving up.
const GROUPING_ROUNDS: usize = 3;

/// One kept match, with everything the checks need.
struct Candidate {
    /// The match as the search reported it.
    m: Match,
    /// Captures, in pattern order. `start_byte`/`end_byte` stay in ORIGINAL source coordinates.
    captures: Vec<Capture>,
    /// Indentation of the line the match starts on.
    indent: String,
    /// Whether the node a single capture bound was an EXPRESSION (the only wrappable kind). Parallel
    /// to `captures`; `None` for list captures and anonymous ones.
    capture_is_expression: Vec<bool>,
}

/// One expansion, with the position of each single capture inside it.
struct Expansion {
    /// The expanded text.
    text: String,
    /// `(capture index, start, end)` inside `text`, for the SINGLE captures only.
    placed: Vec<(usize, usize, usize)>,
}

/// Generate the edit set for one file.
///
/// ## Decision table (first applicable row)
///
/// | Condition | Result |
/// |---|---|
/// | the template names a metavariable the pattern does not bind, or uses `$X` for a list / `$$$X` for a single capture | `invalid_pattern`, **even when nothing matches** (never silently empty) |
/// | `pattern` / `rule` were compiled for another language than `parsed.language`, or `source` is not what `parsed` was parsed from (length differs) | `invalid_args` |
/// | the search reports `truncated` (more matches than `search_budget.max_matches`) | `limit_exceeded`, no edits: a partial rewrite is never produced |
/// | the search runs out of steps / time | its `budget_exceeded` / `timeout` |
/// | an expansion exceeds `max_expansion_bytes` | `limit_exceeded` |
/// | a comment would be dropped and `!allow_comment_loss` | `comment_loss`, no edits; the message gives the count and says to retry with comment loss allowed or to capture the comment's region |
/// | a grouping parenthesis would be needed for a capture that is not an expression (statement, type, pattern, …) so the substitution breaks the tree | `gate_failed` ("the replacement cannot place this capture here"), no edits |
/// | otherwise | `Ok` |
///
/// ## Semantics
/// - **Overlap**: matches are resolved with `resolve_overlaps` on their spans; only kept matches
///   are rewritten; dropped ones are listed in `overlaps_dropped`. Text captured by a kept match
///   is copied verbatim, never rewritten again (no recursion into the replacement).
/// - **Layout**: `expand_template` is called with `indent = indent_of_line(source, match.start)`,
///   the file's line ending (`\r\n` if the file's detected style is CRLF, else `\n`; for a file
///   with mixed endings the ending of the first line break in the file), and `verbatim` ranges
///   covering the template's string, regex, template-literal and comment leaves (found by
///   parsing the template in the pattern's language and context with the metavariables
///   rewritten to identifiers).
/// - **Grouping check (language independent, no precedence tables)**: build the rewritten text
///   with all edits applied, parse it with `parse_budget`, and for every *single* (`$NAME`, not
///   list) capture that was substituted, require that its substituted bytes form exactly one
///   **named** syntax node. Captures that fail are wrapped in `(` `)` and the check runs again
///   (at most 3 rounds). Only captures whose original node was an *expression* are wrapped (a
///   node kind that ends in `expression`, or one of Python's `binary_operator`,
///   `boolean_operator`, `comparison_operator`, `unary_operator`, `not_operator`,
///   `conditional_expression`, `lambda`, `await`; Rust/Go/JS/TS names all end in `expression`);
///   anything else that fails is the `gate_failed` row above. A capture that already forms one
///   node is **never** wrapped (`f($X)`, `return $X`, `$X;`, `-$X * 2` stay as they are).
///   List captures (`$$$NAME`) are never wrapped or checked.
/// - **`verbatim` is layout only**: `$NAME`, `$$$NAME` and `$$` are substituted inside a string,
///   template literal or comment of the template exactly as outside them (see
///   `expand_template`). A capture that lands in one of those is source TEXT in that position, not
///   an expression: it is inserted as written, never parenthesised, and the grouping check skips it.
///   The decision is per OCCURRENCE, so `g("$X", $X * 2)` parenthesises only the second one. Text
///   that would break the literal is caught later by the syntax gate at preview/apply.
/// - **Comments**: for each kept match, every comment node (`is_extra`, kind containing
///   `comment`) that lies inside the match span and outside all capture spans of that match is
///   "dropped". A comment inside a capture (including inside a list capture) is kept with it.
///   Comments outside the match are untouched.
/// - **No-ops**: an expansion equal to the matched text yields no edit and counts as found.
/// - Deterministic; never panics; recursion-free over the tree.
pub fn rewrite_file(
    parsed: &ParsedFile,
    source: &str,
    req: &RewriteRequest<'_>,
) -> Result<RewriteOutcome, ToolError> {
    // --- Preconditions ------------------------------------------------------------------------
    // The template is checked BEFORE the search so a bad template is refused even when nothing
    // matches: never silently empty.
    let bound: Vec<(String, CaptureKind)> = req
        .pattern
        .metavars()
        .into_iter()
        .map(|v| (v.name, v.kind))
        .collect();
    check_template_variables(req.replacement, &bound)?;

    if req.pattern.language() != parsed.language {
        return Err(invalid_args(
            "the pattern was compiled for a different language than the file",
        ));
    }
    // `CompiledRule` carries no language, so a mismatched rule cannot be detected here; the
    // pattern's language is checked above and the rule is evaluated by the query crate, which owns
    // its own check. Nothing else in this file depends on the rule's language.
    // The root node covers the whole file, so a source of a different length is not what was
    // parsed. Comparing the length catches both "shorter" and "longer".
    if source.len() != parsed.tree.root_node().end_byte() {
        return Err(invalid_args("the source is not what was parsed"));
    }

    // --- Search --------------------------------------------------------------------------------
    let outcome = search(parsed, source, req.pattern, req.rule, &req.search_budget)?;
    if outcome.truncated {
        return Err(ToolError::new(
            ErrorCode::LimitExceeded,
            format!(
                "the search found more matches than the limit of {}; nothing was rewritten",
                req.search_budget.max_matches
            ),
            "raise the match limit, or narrow the pattern so it matches fewer places",
        ));
    }
    let matches_found = outcome.matches.len();

    // --- Overlap resolution --------------------------------------------------------------------
    let spans: Vec<(usize, usize)> = outcome
        .matches
        .iter()
        .map(|m| (m.start_byte, m.end_byte))
        .collect();
    let (kept_idx, dropped_idx) = resolve_overlaps(&spans);
    let overlaps_dropped: Vec<Span> = dropped_idx
        .iter()
        .map(|&i| Span {
            start: outcome.matches[i].start_byte,
            end: outcome.matches[i].end_byte,
        })
        .collect();

    let candidates: Vec<Candidate> = kept_idx
        .iter()
        .map(|&i| {
            let m = outcome.matches[i].clone();
            let capture_is_expression = m
                .captures
                .iter()
                .map(|c| {
                    c.kind == CaptureKind::One
                        && capture_node_kind(parsed, source, c.start_byte, c.end_byte)
                            .is_some_and(|k| is_expression_kind(&k))
                })
                .collect();
            Candidate {
                indent: indent_of_line(source, m.start_byte).to_string(),
                captures: m.captures.clone(),
                m,
                capture_is_expression,
            }
        })
        .collect();

    // --- Comments that would vanish ---------------------------------------------------------------
    let mut comments_dropped: Vec<Span> = Vec::new();
    for c in &candidates {
        for comment in comments_inside(parsed, c.m.start_byte, c.m.end_byte) {
            let covered = c
                .captures
                .iter()
                .any(|cap| cap.start_byte <= comment.0 && comment.1 <= cap.end_byte);
            if !covered {
                comments_dropped.push(Span {
                    start: comment.0,
                    end: comment.1,
                });
            }
        }
    }
    comments_dropped.sort_by_key(|s| s.start);
    comments_dropped.dedup();
    if !comments_dropped.is_empty() && !req.allow_comment_loss {
        return Err(ToolError::new(
            ErrorCode::CommentLoss,
            format!(
                "the rewrite would remove {} comment(s) that are not part of any capture",
                comments_dropped.len()
            ),
            "retry with comment loss allowed, or capture the region around the comment so it is copied",
        ));
    }

    // --- Expansion and the grouping check ----------------------------------------------------------
    let verbatim = verbatim_ranges(req.pattern.language(), req.replacement, &req.parse_budget);

    // Whether each candidate needs at least one round of parentheses.
    // Per-candidate list of "this capture is parenthesised", so one variable used twice (once in a
    // string, once as code) only gets parentheses at the code position.
    let mut wrap: Vec<Vec<bool>> = candidates
        .iter()
        .map(|c| vec![false; c.captures.len()])
        .collect();
    let mut expansions: Vec<Expansion> = Vec::new();
    for round in 0..GROUPING_ROUNDS {
        expansions = expand_all(req, &candidates, &wrap, source, &verbatim)?;
        let (probe, probe_places) = assemble(source, &candidates, &expansions);
        let reparsed = parse(parsed.language, &probe, &req.parse_budget)?;
        let failing = failing_captures(&reparsed, &probe, &probe_places, &candidates);
        if failing.is_empty() {
            break;
        }
        // Every remaining failure must be an expression we are allowed to parenthesise; anything
        // else is the `gate_failed` row.
        let mut any = false;
        for f in &failing {
            if !candidates[f.candidate].capture_is_expression[f.capture] {
                return Err(gate_failed(&failing));
            }
            wrap[f.candidate][f.capture] = true;
            any = true;
        }
        if !any || round + 1 == GROUPING_ROUNDS {
            return Err(gate_failed(&failing));
        }
    }

    // --- Edits -------------------------------------------------------------------------------------
    let mut edits: Vec<Edit> = Vec::new();
    let mut wrapped: Vec<Span> = Vec::new();
    for (i, c) in candidates.iter().enumerate() {
        let matched = source.get(c.m.start_byte..c.m.end_byte).unwrap_or_default();
        // A no-op expansion produces no edit but still counts as found.
        if expansions[i].text == matched {
            continue;
        }
        edits.push(Edit {
            start: c.m.start_byte,
            end: c.m.end_byte,
            replacement: expansions[i].text.clone(),
        });
        if wrap[i].iter().any(|w| *w) {
            wrapped.push(Span {
                start: c.m.start_byte,
                end: c.m.end_byte,
            });
        }
    }
    edits.sort_by_key(|e| e.start);

    Ok(RewriteOutcome {
        edits,
        matches_found,
        overlaps_dropped,
        wrapped,
        comments_dropped,
    })
}

/// A single capture that did not survive substitution as exactly one named node.
struct Failing {
    /// Index into the candidate list.
    candidate: usize,
    /// Index into that candidate's captures.
    capture: usize,
}

/// Where one capture landed in the assembled (all edits applied) text.
struct Place {
    /// Index into the candidate list.
    candidate: usize,
    /// Index into that candidate's captures.
    capture: usize,
    /// Start in the assembled text.
    start: usize,
    /// End in the assembled text.
    end: usize,
}

/// Expand every candidate with the current wrap decisions, recording where each single capture
/// landed inside its expansion.
fn expand_all(
    req: &RewriteRequest<'_>,
    candidates: &[Candidate],
    wrap: &[Vec<bool>],
    source: &str,
    verbatim: &[Range<usize>],
) -> Result<Vec<Expansion>, ToolError> {
    let mut out = Vec::with_capacity(candidates.len());
    for (i, c) in candidates.iter().enumerate() {
        // The captures are passed unchanged: `expand_template_wrapping` parenthesises each
        // OCCURRENCE it was asked to, and never one inside a string, template literal or comment.
        // Only captures whose original node was an expression are eligible at all.
        let captures = c.captures.clone();
        let wrap_here: Vec<bool> = (0..captures.len())
            .map(|k| wrap[i][k] && c.capture_is_expression[k])
            .collect();
        let mut placed: Vec<(usize, usize, usize)> = Vec::new();
        let opts = ExpandOptions {
            indent: &c.indent,
            // This candidate's own site, not the file: see `line_ending_at`.
            line_ending: line_ending_at(source, c.m.start_byte),
            verbatim,
            max_output_bytes: req.max_expansion_bytes,
        };
        // `expand_template` reports where every capture landed, because it is the only code that
        // knows the final offsets (indentation and line-ending rewriting shift them). A capture used
        // twice in the template therefore yields two entries, which is exactly what the grouping
        // check needs: the one in a string is skipped, the one in code is judged.
        let text =
            expand_template_wrapping(req.replacement, &captures, &opts, &mut placed, &wrap_here)?;
        if text.len() > req.max_expansion_bytes {
            return Err(ToolError::new(
                ErrorCode::LimitExceeded,
                format!(
                    "the replacement expands to {} bytes, over the limit of {}",
                    text.len(),
                    req.max_expansion_bytes
                ),
                "shorten the replacement template",
            ));
        }
        out.push(Expansion { text, placed });
    }
    Ok(out)
}

/// The rewritten text with every candidate's expansion applied, plus where each single capture
/// landed in it.
fn assemble(
    source: &str,
    candidates: &[Candidate],
    expansions: &[Expansion],
) -> (String, Vec<Place>) {
    let mut probe = String::with_capacity(source.len());
    let mut places = Vec::new();
    let mut cursor = 0usize;
    for (i, c) in candidates.iter().enumerate() {
        let expansion = &expansions[i];
        let base = probe.len();
        probe.push_str(source.get(cursor..c.m.start_byte).unwrap_or_default());
        let base = base + (c.m.start_byte - cursor);
        for (k, start, end) in &expansion.placed {
            places.push(Place {
                candidate: i,
                capture: *k,
                start: base + start,
                end: base + end,
            });
        }
        probe.push_str(&expansion.text);
        cursor = c.m.end_byte;
    }
    probe.push_str(source.get(cursor..).unwrap_or_default());
    (probe, places)
}

/// The single captures whose substituted bytes do NOT form exactly one named node of `tree`.
fn failing_captures(
    tree: &ParsedFile,
    probe: &str,
    places: &[Place],
    candidates: &[Candidate],
) -> Vec<Failing> {
    let mut out = Vec::new();
    for p in places {
        let Some(c) = candidates.get(p.candidate) else {
            continue;
        };
        if p.start >= p.end || p.end > probe.len() {
            continue;
        }
        if !probe.is_char_boundary(p.start) || !probe.is_char_boundary(p.end) {
            continue;
        }
        // A LIST capture (`$$$NAME`) spans several nodes by design: it is inserted as its text and
        // is never parenthesised or checked.
        let Some(cap) = c.captures.get(p.capture) else {
            continue;
        };
        if cap.kind != CaptureKind::One {
            continue;
        }
        // A capture substituted into a string, template literal or comment is source TEXT in that
        // position, not an expression: it is inserted as written, never parenthesised, and has no
        // syntax node of its own to protect. Decided on the re-parsed text, so a `$X` before the
        // opening quote is still treated as code.
        if inside_literal(tree, p.start, p.end) {
            continue;
        }
        if named_nodes_covering(tree, p.start, p.end) != 1 {
            out.push(Failing {
                candidate: p.candidate,
                capture: p.capture,
            });
        }
    }
    out
}

/// How many NAMED nodes cover exactly `[start, end)`: 1 means the substitution kept the structure.
fn named_nodes_covering(parsed: &ParsedFile, start: usize, end: usize) -> usize {
    let root = parsed.tree.root_node();
    // Walk down: a node counts when it starts at `start`, ends at `end` and is named. The type is
    // inferred so this crate does not need a `tree-sitter` dependency to name it.
    let mut node = root;
    loop {
        if node.start_byte() > start || node.end_byte() < end {
            return 0;
        }
        let mut cursor = node.walk();
        let covering = node
            .children(&mut cursor)
            .find(|c| c.start_byte() <= start && c.end_byte() >= end);
        match covering {
            Some(child) => {
                if child.start_byte() == start && child.end_byte() == end {
                    // The deepest node with exactly this range decides.
                    return usize::from(child.is_named());
                }
                node = child;
            }
            None => {
                // No child covers it: this node is the deepest one containing the range.
                return usize::from(
                    node.start_byte() == start && node.end_byte() == end && node.is_named(),
                );
            }
        }
    }
}

/// Whether `[start, end)` lies inside a string, template literal, regex or comment of the
/// RE-PARSED text.
///
/// This is the other half of the "verbatim is layout only" rule: a `$X` inside a string is inserted
/// as its source text, so it is never parenthesised and the grouping check does not judge it.
fn inside_literal(tree: &ParsedFile, start: usize, end: usize) -> bool {
    let mut node = tree.tree.root_node();
    loop {
        let mut cursor = node.walk();
        let covering = node
            .children(&mut cursor)
            .find(|c| c.start_byte() <= start && c.end_byte() >= end);
        let Some(child) = covering else {
            return false;
        };
        if is_literal_kind(child.kind()) {
            return true;
        }
        node = child;
    }
}

/// Whether a node kind holds literal text rather than code.
fn is_literal_kind(kind: &str) -> bool {
    kind.contains("string")
        || kind.contains("comment")
        || kind.contains("regex")
        || kind.contains("template_string")
}

/// The kind of the smallest node that exactly covers `[start, end)` in the ORIGINAL tree.
fn capture_node_kind(
    parsed: &ParsedFile,
    source: &str,
    start: usize,
    end: usize,
) -> Option<String> {
    if start >= end || end > source.len() {
        return None;
    }
    // Iterative descent to the deepest node covering the range; the node type is inferred so this
    // crate does not need a `tree-sitter` dependency to name it.
    let mut current = parsed.tree.root_node();
    loop {
        let mut found = None;
        let mut c = current.walk();
        for child in current.children(&mut c) {
            if child.start_byte() <= start && child.end_byte() >= end {
                found = Some(child);
                break;
            }
        }
        match found {
            Some(child) if child.start_byte() == start && child.end_byte() == end => {
                return Some(child.kind().to_string());
            }
            Some(child) => current = child,
            None => {
                return Some(current.kind().to_string());
            }
        }
    }
}

/// The comment nodes inside `[start, end)` (kind contains `comment`, `is_extra`), ascending.
fn comments_inside(parsed: &ParsedFile, start: usize, end: usize) -> Vec<(usize, usize)> {
    let mut out = Vec::new();
    let mut stack = vec![parsed.tree.root_node()];
    while let Some(n) = stack.pop() {
        if n.start_byte() >= start
            && n.end_byte() <= end
            && n.is_extra()
            && n.kind().contains("comment")
        {
            out.push((n.start_byte(), n.end_byte()));
        }
        let mut c = n.walk();
        for ch in n.children(&mut c) {
            stack.push(ch);
        }
    }
    out.sort_unstable();
    out.dedup();
    out
}

/// Whether a node kind names an expression in any of the five languages.
fn is_expression_kind(kind: &str) -> bool {
    kind.ends_with("expression")
        || matches!(
            kind,
            "binary_operator"
                | "boolean_operator"
                | "comparison_operator"
                | "unary_operator"
                | "not_operator"
                | "conditional_expression"
                | "lambda"
                | "await"
        )
}

/// The line ending in force at `site`, the byte offset an edit starts at.
///
/// EDIT-MODEL §Preserving file properties: replacement text "is adapted to the file's line
/// ending and to the indentation **at the match site**". For a file with one style that is the
/// file's style; for a **mixed** file the file has no single answer, so the site decides - the
/// first terminator after the edit's start is the style already in force there, and the
/// expansion joins it. A mixed file therefore stays mixed after an edit, which is the only
/// outcome that does not change a line-ending property of the file; preview's encoding gate
/// refuses any edit that would flatten it.
///
/// **One implementation, two callers.** `preview.rs` asks this function for the ending of a
/// symbol edit, and the rewrite engine uses it for each match's expansion. That is deliberate:
/// the two rules are the same rule, and two copies of it were a divergence trap - they were
/// identical apart from a `use`, and nothing would have said so if one of them had changed. It
/// lives here because this module is the lower of the two and already owned `line_ending_of`
/// before either caller existed.
///
/// `detect_line_ending` answers `Mixed` for anything that is not exactly one style (including a
/// lone `\r`), and that answer is **not** folded into LF or CRLF here or in the gate: folding
/// both sides of the comparison to LF is precisely what let a mixed file be rewritten to pure
/// LF without the gate noticing.
///
/// A site with no terminator after it (the last line of a file that ends without one) falls
/// back to the file's classification: there is nothing local to go on.
pub(crate) fn line_ending_at(source: &str, site: usize) -> &'static str {
    use opencrayast_core::text::{LineEnding, detect_line_ending};
    let rest = &source[site.min(source.len())..];
    for (i, b) in rest.as_bytes().iter().enumerate() {
        match b {
            b'\r' => {
                return if rest.as_bytes().get(i + 1) == Some(&b'\n') {
                    "\r\n"
                } else {
                    "\r"
                };
            }
            b'\n' => return "\n",
            _ => {}
        }
    }
    match detect_line_ending(source) {
        LineEnding::Crlf => "\r\n",
        _ => "\n",
    }
}

/// The template's verbatim ranges: its string, regex, template-literal and comment leaves.
///
/// The template is compiled first (the pattern compiler rewrites metavariables and picks the
/// language's context), then parsed in a rewritten form so it stands alone. The rewriting changes
/// byte offsets, so [`rewrite_for_parsing`] also returns, for each byte of its OUTPUT, the byte of
/// the ORIGINAL template it came from; the verbatim ranges are translated back through that map.
///
/// `A template that does not compile contributes no ranges: `expand_template` will then report the
/// real error instead of this function inventing ranges.
/// The verbatim ranges: the template's strings, template literals, regexes and comments.
fn verbatim_ranges(language: Language, template: &str, budget: &ParseBudget) -> Vec<Range<usize>> {
    raw_verbatim_ranges(language, template, budget)
}

/// The verbatim ranges straight from the parse, before any escaping adjustment.
fn raw_verbatim_ranges(
    language: Language,
    template: &str,
    budget: &ParseBudget,
) -> Vec<Range<usize>> {
    if Pattern::compile(language, template).is_err() {
        return Vec::new();
    }
    let (probe, map) = rewrite_for_parsing(template);
    let Ok(parsed) = parse(language, &probe, budget) else {
        return Vec::new();
    };
    // Collect the verbatim leaves, then translate their probe range back to template coordinates.
    let mut leaves: Vec<(usize, usize)> = Vec::new();
    let mut stack = vec![parsed.tree.root_node()];
    while let Some(n) = stack.pop() {
        if is_verbatim_leaf(n.kind()) {
            leaves.push((n.start_byte(), n.end_byte()));
        } else {
            let mut c = n.walk();
            for ch in n.children(&mut c) {
                stack.push(ch);
            }
        }
    }
    let mut out = Vec::new();
    for (ps, pe) in leaves {
        let start = map.get(ps).copied();
        // The end maps to the byte AFTER the last probe byte's template byte.
        let end = map.get(pe.saturating_sub(1)).copied().map(|b| b + 1);
        if let (Some(start), Some(end)) = (start, end)
            && start < end
            && end <= template.len()
            && template.is_char_boundary(start)
            && template.is_char_boundary(end)
        {
            out.push(start..end);
        }
    }
    out.sort_by_key(|r| r.start);
    out.dedup();
    out
}

/// The template rewritten so it parses on its own, plus a map from each output byte to the byte of
/// the original template it came from.
///
/// A metavariable token becomes a short identifier (`zq`): its length changes, so offsets must be
/// translated rather than reused. `$$` becomes a single `$`, exactly as the pattern compiler
/// rewrote it when the template was compiled - and exactly as `expand_template` would emit it,
/// which matters because a `$` inside a verbatim range is copied verbatim and not converted there.
fn rewrite_for_parsing(template: &str) -> (String, Vec<usize>) {
    let bytes = template.as_bytes();
    let mut out = String::with_capacity(template.len());
    let mut map: Vec<usize> = Vec::with_capacity(template.len());
    let mut i = 0usize;
    let push = |out: &mut String, map: &mut Vec<usize>, byte: u8, origin: usize| {
        out.push(byte as char);
        map.push(origin);
    };
    while i < bytes.len() {
        // `$$` is a literal dollar: one byte out, from the second `$`.
        if bytes[i] == b'$' && bytes.get(i + 1) == Some(&b'$') && bytes.get(i + 2) != Some(&b'$') {
            push(&mut out, &mut map, b'$', i + 1);
            i += 2;
            continue;
        }
        if bytes[i] == b'$' {
            let (skip, name_end) =
                if bytes.get(i + 1) == Some(&b'$') && bytes.get(i + 2) == Some(&b'$') {
                    (3, scan_name(bytes, i + 3).map(|(_, e)| e))
                } else {
                    (1, scan_name(bytes, i + 1).map(|(_, e)| e))
                };
            match name_end {
                Some(end) if end > i + skip => {
                    // The identifier stands for the variable: every byte maps to its first byte.
                    for k in 0..2 {
                        push(&mut out, &mut map, b'z', if k == 0 { i } else { i + skip });
                    }
                    if out.ends_with("zz") {
                        // Two `z` plus a `q` reads better in a parse tree; keep it simple instead.
                    }
                    out.pop();
                    map.pop();
                    push(&mut out, &mut map, b'q', end - 1);
                    i = end;
                    continue;
                }
                _ if skip == 3 => {
                    // `$$$` with no name is the anonymous list variable.
                    push(&mut out, &mut map, b'z', i);
                    push(&mut out, &mut map, b'q', i);
                    i += 3;
                    continue;
                }
                _ => {}
            }
        }
        push(&mut out, &mut map, bytes[i], i);
        i += 1;
    }
    (out, map)
}

/// Whether a node kind holds text that must be copied exactly (never reflowed).
fn is_verbatim_leaf(kind: &str) -> bool {
    kind.contains("string")
        || kind.contains("comment")
        || kind.contains("regex")
        || kind.contains("template_string")
}

/// `invalid_pattern` for a template variable the pattern does not bind, or used with the wrong
/// form (`$NAME` for a list, `$$$NAME` for a single capture).
fn check_template_variables(
    template: &str,
    bound: &[(String, CaptureKind)],
) -> Result<(), ToolError> {
    let bytes = template.as_bytes();
    let mut i = 0usize;
    while i < bytes.len() {
        if bytes[i] != b'$' {
            i += 1;
            continue;
        }
        // `$$` is a literal dollar: skip both bytes, so `'$$5'` is the text `$5` and not a variable.
        if bytes.get(i + 1) == Some(&b'$') && bytes.get(i + 2) != Some(&b'$') {
            i += 2;
            continue;
        }
        if bytes.get(i + 1) == Some(&b'$') && bytes.get(i + 2) == Some(&b'$') {
            let Some((name, end)) = scan_name(bytes, i + 3) else {
                i += 3;
                continue;
            };
            match bound.iter().find(|(n, _)| *n == name) {
                None => return Err(unbound_variable(&name)),
                Some((_, kind)) if *kind != CaptureKind::List => {
                    return Err(wrong_form(&name, true, *kind));
                }
                Some(_) => i = end,
            }
            continue;
        }
        let Some((name, end)) = scan_name(bytes, i + 1) else {
            i += 1;
            continue;
        };
        match bound.iter().find(|(n, _)| *n == name) {
            None => return Err(unbound_variable(&name)),
            Some((_, kind)) if *kind != CaptureKind::One => {
                return Err(wrong_form(&name, false, *kind));
            }
            Some(_) => i = end,
        }
    }
    Ok(())
}

/// The `[A-Z][A-Z0-9_]*` name after a `$`.
fn scan_name(bytes: &[u8], start: usize) -> Option<(String, usize)> {
    let mut i = start;
    let mut name = String::new();
    while i < bytes.len() {
        let c = bytes[i] as char;
        if c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_' {
            name.push(c);
            i += 1;
        } else {
            break;
        }
    }
    if name.is_empty() {
        return None;
    }
    Some((name, i))
}

/// The template names a metavariable the pattern does not bind.
fn unbound_variable(name: &str) -> ToolError {
    ToolError::new(
        ErrorCode::InvalidPattern,
        format!("the replacement uses ${name}, which the pattern does not bind"),
        "use only the metavariables the pattern captures, or change the pattern to capture it",
    )
}

/// The template uses the wrong form for a capture (`$NAME` for a list or `$$$NAME` for a single).
fn wrong_form(name: &str, used_list: bool, actual: CaptureKind) -> ToolError {
    let (message, suggestion) = if actual == CaptureKind::List {
        (
            format!("the replacement uses {name} for a capture that is a list of nodes"),
            "a list capture needs the $$$ form in the replacement",
        )
    } else {
        (
            format!("the replacement uses {name} for a capture that is a single node"),
            "a single capture needs the $ form in the replacement, not $$$",
        )
    };
    let _ = used_list;
    ToolError::new(ErrorCode::InvalidPattern, message, suggestion)
}

/// `gate_failed`: the replacement cannot place this capture here.
fn gate_failed(failing: &[Failing]) -> ToolError {
    let names: Vec<String> = failing
        .iter()
        .map(|f| format!("capture #{}", f.capture))
        .collect();
    ToolError::new(
        ErrorCode::GateFailed,
        format!(
            "the replacement cannot place {} here without changing the syntax tree",
            names.join(", ")
        ),
        "capture the whole construct instead, or use the template so the capture stays a single node",
    )
}

/// `invalid_args`.
fn invalid_args(message: &str) -> ToolError {
    ToolError::new(
        ErrorCode::InvalidArgs,
        message,
        "pass a pattern and a source that belong to the same language and the same bytes",
    )
}
