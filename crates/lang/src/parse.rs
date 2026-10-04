//! Budgeted parsing (docs/LANGUAGES.md "Syntax errors"; SECURITY-MODEL T-07, T-08r; PRS-01..04, PRS-09).

use crate::language::Language;
use opencrayast_core::error::ToolError;
use opencrayast_core::limits::Limits;
use std::ops::ControlFlow;
use std::time::{Duration, Instant};

/// The resource budget of one parse. Source files are hostile input (S-6).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParseBudget {
    /// Maximum source size in bytes (checked first, before any parsing): over => `file_too_large`.
    pub max_bytes: u64,
    /// Wall-clock budget: parse cancelled => `timeout`.
    pub timeout: Duration,
    /// Maximum tree depth: exceeded => `budget_exceeded` (message names "depth").
    pub max_depth: u64,
    /// Maximum node count: exceeded => `budget_exceeded` (message names "node").
    pub max_nodes: u64,
}

impl From<&Limits> for ParseBudget {
    /// `max_bytes = max_file_bytes`, `timeout = parse_timeout_ms`, depth/nodes from
    /// `parse_max_depth` / `parse_max_nodes`.
    fn from(l: &Limits) -> Self {
        ParseBudget {
            max_bytes: l.max_file_bytes,
            timeout: Duration::from_millis(l.parse_timeout_ms),
            max_depth: l.parse_max_depth,
            max_nodes: l.parse_max_nodes,
        }
    }
}

/// The result of a successful parse: the tree plus the numbers every tool reports.
pub struct ParsedFile {
    /// The language it was parsed as.
    pub language: Language,
    /// The syntax tree (owned; valid for the `source` it was parsed from).
    pub tree: tree_sitter::Tree,
    /// Syntax-error count: the number of nodes that are `ERROR` nodes or `MISSING` nodes. Each
    /// node counted once. 0 for a file that parses cleanly. This is the number the syntax gate
    /// compares (EDIT-MODEL "Gates").
    pub error_count: usize,
    /// Total node count (named and anonymous).
    pub node_count: usize,
    /// Maximum depth of the tree (the root has depth 1).
    pub max_depth: usize,
}

/// Parse `source` as `language` within `budget`.
///
/// Order of checks and their errors:
/// 1. `language` not built in => `unsupported_language`;
/// 2. `source.len() > budget.max_bytes` => `file_too_large` (before any parsing);
/// 3. parse with a wall-clock cancel: expired => `timeout`;
/// 4. walk the resulting tree ITERATIVELY (no recursion: a pathological depth must not overflow
///    the stack) counting nodes, depth and errors; node count over `max_nodes` or depth over
///    `max_depth` => `budget_exceeded`, with the walk stopping as soon as a limit is crossed;
/// 5. otherwise `Ok(ParsedFile)`.
///
/// Never panics on any input. Error messages never contain source text.
///
/// # What these budgets do and do not cover
///
/// tree-sitter has no way to cap memory or node count *while* it builds a tree: the size budget
/// ([`ParseBudget::max_bytes`]) is therefore checked before parsing starts, and the depth/node
/// budgets are enforced by truncating the walk *after* the tree already exists. So `max_nodes` and
/// `max_depth` bound the work this crate does and the memory it retains, not the work tree-sitter
/// did. Hard memory containment is the isolated parse worker's job (ADR-004, SECURITY-MODEL
/// T-08), which can kill the process; nothing here can.
pub fn parse(
    language: Language,
    source: &str,
    budget: &ParseBudget,
) -> Result<ParsedFile, ToolError> {
    let grammar = language.grammar().ok_or_else(|| {
        ToolError::new(
            opencrayast_core::error::ErrorCode::UnsupportedLanguage,
            format!("no grammar for {} in this build", language.id()),
            "rebuild with this build's language features enabled, or use a supported language",
        )
    })?;

    // Size is checked before anything else touches the source.
    if source.len() as u64 > budget.max_bytes {
        return Err(ToolError::new(
            opencrayast_core::error::ErrorCode::FileTooLarge,
            format!(
                "source is {} bytes, over the {} byte budget",
                source.len(),
                budget.max_bytes
            ),
            "raise the file size limit for this workspace or read a smaller file",
        ));
    }

    let tree = parse_with_timeout(language, grammar, source, budget.timeout)?;

    walk_budgeted(tree, language, budget)
}

/// Parse `source`, giving up after `timeout`.
///
/// tree-sitter calls the progress callback periodically while parsing, so a wall-clock check there
/// is how the parse gets cancelled (0.27 has no `set_timeout`; the callback is the only in-parse
/// hook). Cancellation makes `parse_with_options` return `None`.
fn parse_with_timeout(
    _language: Language,
    grammar: tree_sitter::Language,
    source: &str,
    timeout: Duration,
) -> Result<tree_sitter::Tree, ToolError> {
    let started = Instant::now();
    let mut parser = tree_sitter::Parser::new();
    if parser.set_language(&grammar).is_err() {
        // Only reachable if a grammar in this build is ABI-incompatible with tree-sitter itself.
        return Err(ToolError::new(
            opencrayast_core::error::ErrorCode::Internal,
            "grammar could not be loaded by the parser",
            "this is a build problem: report the language and the build's version",
        ));
    }

    let mut timed_out = false;
    let mut on_progress = |_state: &tree_sitter::ParseState| {
        if started.elapsed() >= timeout {
            timed_out = true;
            ControlFlow::Break(())
        } else {
            ControlFlow::Continue(())
        }
    };
    let options = tree_sitter::ParseOptions::new().progress_callback(&mut on_progress);

    // The whole source is available up front: the reader callback hands back the not-yet-read
    // tail starting at the offset tree-sitter asks for, and an empty slice at the end.
    //
    // Slice by BYTE, not with `str::get`: a str slice returns None when the offset lands inside a
    // multibyte character, and turning that None into an empty slice would read as an early EOF,
    // silently truncating the parse (and understating the syntax-error count the syntax gate
    // compares). tree-sitter asks for bytes, and `&[u8]` is what this callback may return.
    let mut reader = |offset: usize, _point: tree_sitter::Point| -> &[u8] {
        let start = offset.min(source.len());
        &source.as_bytes()[start..]
    };
    let tree = parser.parse_with_options(&mut reader, None, Some(options));

    match tree {
        Some(tree) => Ok(tree),
        None => Err(ToolError::new(
            opencrayast_core::error::ErrorCode::Timeout,
            if timed_out {
                format!("parse exceeded its {} ms budget", timeout.as_millis())
            } else {
                "parse was cancelled".to_string()
            },
            "raise the parse timeout for this workspace, or read a smaller region of the file",
        )),
    }
}

/// Walk the tree once, iteratively, enforcing the depth and node budgets as it goes.
///
/// Returns the first breach and stops immediately at it. Nothing here recurses, so a
/// pathologically deep tree cannot overflow the stack (PRS-01).
fn walk_budgeted(
    tree: tree_sitter::Tree,
    language: Language,
    budget: &ParseBudget,
) -> Result<ParsedFile, ToolError> {
    let mut cursor = tree.walk();
    let mut node_count: usize = 0;
    let mut error_count: usize = 0;
    let mut deepest: usize = 0;

    loop {
        // The cursor's depth counts the root as 0; `ParsedFile::max_depth` counts it as 1.
        let depth = cursor.depth() as usize + 1;
        deepest = deepest.max(depth);
        if depth as u64 > budget.max_depth {
            return Err(depth_exceeded(depth, budget.max_depth));
        }

        node_count += 1;
        if node_count as u64 > budget.max_nodes {
            return Err(node_exceeded(budget.max_nodes));
        }

        let node = cursor.node();
        if node.is_error() || node.is_missing() {
            error_count += 1;
        }

        // Depth-first without recursion: descend, else advance to the next sibling, else pop.
        if cursor.goto_first_child() {
            continue;
        }
        loop {
            if cursor.goto_next_sibling() {
                break;
            }
            if !cursor.goto_parent() {
                // The cursor borrows the tree, so it has to go before the tree can move.
                drop(cursor);
                return Ok(ParsedFile {
                    language,
                    tree,
                    error_count,
                    node_count,
                    max_depth: deepest,
                });
            }
        }
    }
}

fn depth_exceeded(depth: usize, max: u64) -> ToolError {
    ToolError::new(
        opencrayast_core::error::ErrorCode::BudgetExceeded,
        format!("tree depth exceeds the maximum depth of {max} (at depth {depth})"),
        "lower the parse depth limit only with care, or read the file another way",
    )
}

fn node_exceeded(max: u64) -> ToolError {
    ToolError::new(
        opencrayast_core::error::ErrorCode::BudgetExceeded,
        format!("tree has more nodes than the maximum node count of {max}"),
        "raise the parse node limit for this workspace, or split the file",
    )
}
