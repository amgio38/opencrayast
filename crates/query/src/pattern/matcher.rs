//! The structural matcher. See the module documentation of `pattern`.
//!
//! # How this works
//!
//! The two inputs have very different shapes, so they are handled differently:
//!
//! - The PATTERN is bounded: compilation limits it to 64 levels. Matching a pattern node against a
//!   source node therefore recurses over the pattern only ([`match_node`]), and the call depth is
//!   a function of the pattern, never of the source.
//! - A source child SEQUENCE is unbounded: a file with 100 000 siblings has 100 000 children in
//!   one sequence. [`match_seq`] walks it with an explicit stack, and every iteration ticks the
//!   budget.
//!
//! `match_seq` is the only unbounded loop here and the only place that backtracks, so the step
//! budget is what bounds it: several list variables over a long sequence can try many partitions,
//! and `budget_exceeded` is how that stops.
//!
//! List variables absorb **zero or more** consecutive source children, **lazily** (shortest
//! first). Each growth costs a step, so the budget bounds the backtracking too.
//!
//! # Structural equality of rebound metavariables
//!
//! A repeated metavariable must rebind only to a *token-equal* node: the same sequence of leaf
//! texts, depth-first, skipping comments and zero-width nodes. [`Tokens`] collects exactly that.
//!
//! Kinds are deliberately not compared, because a node's kind depends on its role - `T` in `<T>` is
//! a `type_parameter` and `T` in `: T` is a `type_identifier`, yet the code reads the same.
//! Whitespace and comments never appear (only leaves are collected); string, regex and template text
//! does appear, verbatim, because it is leaf text.
//!
//! The tokens are stored as owned data rather than a `tree_sitter::Node` because a node carries a
//! lifetime, and putting one in `Bindings` would force a lifetime parameter onto the struct that
//! `rules::eval` also uses. Collecting them walks the tree iteratively, so a deeply nested binding
//! cannot overflow the stack.

use super::compile::{PNode, Program};
use super::{CaptureKind, CompiledRule, Match, Pattern, SearchBudget, SearchOutcome};
use opencrayast_core::error::{ErrorCode, ToolError};
use opencrayast_lang::ParsedFile;
use std::time::Instant;

/// How often the wall-clock deadline is read. An `Instant::now()` per step would cost more than
/// the step it guards; every 1024 steps still bounds the overshoot tightly.
const DEADLINE_CHECK_EVERY: u64 = 1024;

/// The step and time accounting shared by the matcher and rule evaluation.
#[derive(Debug)]
pub(crate) struct Steps {
    /// Steps consumed so far.
    pub(crate) used: u64,
    /// The limit.
    pub(crate) max: u64,
    /// Wall-clock deadline.
    pub(crate) deadline: Option<Instant>,
}

impl Steps {
    /// Count one step. `budget_exceeded` (message naming "steps") when over the limit; `timeout`
    /// when the deadline has passed (checked at least every 1024 steps).
    pub(crate) fn tick(&mut self) -> Result<(), ToolError> {
        self.used += 1;
        if self.used > self.max {
            return Err(ToolError::new(
                ErrorCode::BudgetExceeded,
                format!("pattern matching exceeded the budget of {} steps", self.max),
                "narrow the search, simplify the pattern, or raise the step limit",
            ));
        }
        // Reading the clock on every step would dominate the cost of the step itself.
        if self.deadline.is_some() && self.used.is_multiple_of(DEADLINE_CHECK_EVERY) {
            self.check_deadline()?;
        }
        Ok(())
    }

    /// Read the deadline now, whatever the step count.
    pub(crate) fn check_deadline(&self) -> Result<(), ToolError> {
        match self.deadline {
            Some(deadline) if Instant::now() >= deadline => Err(ToolError::new(
                ErrorCode::Timeout,
                "pattern matching exceeded its wall-clock deadline",
                "narrow the search or raise the time limit",
            )),
            _ => Ok(()),
        }
    }
}

/// What one successful match bound: `(name, kind, start_byte, end_byte)` in order of first
/// appearance in the pattern.
#[derive(Debug, Clone, Default)]
pub(crate) struct Bindings {
    /// The bindings.
    pub(crate) vars: Vec<(String, CaptureKind, usize, usize)>,
    /// The SHAPE of the subtree each name was bound to, kept so a rebinding compares syntax trees
    /// rather than text. `vars` alone cannot answer that: `a + b` and `a+b` have different byte
    /// ranges but the same structure, while `/a b/` and `/a  b/` have the same collapsed text and
    /// different meaning.
    ///
    /// A shape is owned data rather than a `tree_sitter::Node` on purpose: a node carries a
    /// lifetime, and putting one here would force a lifetime parameter onto `Bindings`, which is
    /// part of the contract shared with `rules::eval`.
    tokens: Vec<(String, Tokens)>,
}

impl Bindings {
    /// Record a binding. A name already present keeps its first position: captures are reported in
    /// order of first appearance, and a second occurrence was equal to the first anyway.
    fn bind(&mut self, name: &str, kind: CaptureKind, start: usize, end: usize) {
        if self.vars.iter().any(|(n, ..)| n == name) {
            return;
        }
        self.vars.push((name.to_string(), kind, start, end));
    }

    /// The range already bound to `name`, if any.
    fn get(&self, name: &str) -> Option<(usize, usize)> {
        self.vars
            .iter()
            .find(|(n, ..)| n == name)
            .map(|(_, _, s, e)| (*s, *e))
    }

    /// Record a binding, keeping the node's shape so a rebinding compares subtrees.
    fn bind_node(
        &mut self,
        name: &str,
        kind: CaptureKind,
        node: tree_sitter::Node<'_>,
        source: &str,
        steps: &mut Steps,
    ) -> Result<(), ToolError> {
        if self.tokens.iter().any(|(n, _)| n == name) {
            return Ok(());
        }
        self.tokens
            .push((name.to_string(), Tokens::of(node, source, steps)?));
        self.bind(name, kind, node.start_byte(), node.end_byte());
        Ok(())
    }

    /// The tokens bound to `name`, if any.
    fn get_tokens(&self, name: &str) -> Option<&Tokens> {
        self.tokens.iter().find(|(n, _)| n == name).map(|(_, t)| t)
    }
}

/// The token shape of a bound subtree: the sequence of its LEAF texts, depth-first.
///
/// The specification asks repeated metavariables to be *token-equal*: same sequence of leaf texts,
/// with comments and zero-width nodes skipped. Kinds are deliberately NOT recorded, because a node's
/// kind depends on its role - `T` in `<T>` is a `type_parameter` and `T` in `: T` is a
/// `type_identifier`, yet the code reads the same. Whitespace and comments never appear because only
/// leaves are collected; string, regex and template text does appear, verbatim, because it is leaf
/// text.
///
/// Flattening into an owned vector keeps the comparison trivial and keeps recursion out of the type;
/// [`Tokens::of`] walks the tree iteratively, so a deeply nested binding cannot overflow.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Tokens(Vec<String>);

impl Tokens {
    /// The leaf texts of `node`'s subtree, depth-first, skipping `is_extra` nodes and zero-width
    /// nodes.
    fn of(
        node: tree_sitter::Node<'_>,
        source: &str,
        steps: &mut Steps,
    ) -> Result<Tokens, ToolError> {
        let mut out: Vec<String> = Vec::new();
        // Explicit stack, children pushed in reverse so they pop left to right.
        let mut stack: Vec<tree_sitter::Node<'_>> = vec![node];
        while let Some(current) = stack.pop() {
            steps.tick()?;
            if current.is_extra() {
                // Comments and other extras carry no tokens.
                continue;
            }
            let children: Vec<tree_sitter::Node<'_>> = {
                let mut cursor = current.walk();
                current
                    .children(&mut cursor)
                    .filter(|c| !c.is_extra())
                    .collect()
            };
            if children.is_empty() {
                // A zero-width leaf (a missing or synthesised node) contributes no token.
                if current.end_byte() > current.start_byte() {
                    out.push(
                        source
                            .get(current.start_byte()..current.end_byte())
                            .unwrap_or_default()
                            .to_string(),
                    );
                }
                continue;
            }
            for child in children.into_iter().rev() {
                stack.push(child);
            }
        }
        Ok(Tokens(out))
    }

    /// Whether these tokens equal those of `node`'s subtree.
    fn equals_node(
        &self,
        node: tree_sitter::Node<'_>,
        source: &str,
        steps: &mut Steps,
    ) -> Result<bool, ToolError> {
        steps.tick()?;
        Ok(Tokens::of(node, source, steps)? == *self)
    }
}

/// Does `program` match `node`? `None` when it does not. Used by [`search`] for every candidate
/// root and by rule evaluation for `inside` / `has` / `not` pattern operands (whose bindings are
/// discarded by the caller).
pub(crate) fn match_at(
    program: &Program,
    node: tree_sitter::Node<'_>,
    source: &str,
    steps: &mut Steps,
) -> Result<Option<Bindings>, ToolError> {
    match_node(&program.root, node, source, steps, &mut Bindings::default())
}

/// Match one pattern node against one source node. Recurses over the PATTERN only.
fn match_node(
    p: &PNode,
    node: tree_sitter::Node<'_>,
    source: &str,
    steps: &mut Steps,
    bindings: &mut Bindings,
) -> Result<Option<Bindings>, ToolError> {
    match p {
        // `$NAME` / `$_`: any NAMED node. A second occurrence of the same name must be structurally
        // equal to the first.
        PNode::Meta { name, list: false } => {
            steps.tick()?;
            if !node.is_named() {
                return Ok(None);
            }
            match name {
                None => Ok(Some(bindings.clone())),
                Some(name) => match bindings.get_tokens(name) {
                    Some(bound) => {
                        if bound.equals_node(node, source, steps)? {
                            Ok(Some(bindings.clone()))
                        } else {
                            Ok(None)
                        }
                    }
                    None => {
                        bindings.bind_node(name, CaptureKind::One, node, source, steps)?;
                        Ok(Some(bindings.clone()))
                    }
                },
            }
        }
        // A list metavariable reached as a node rather than as a sequence element. The compiler
        // turns `$$$NAME` into a list element inside its parent; this arm exists so an unexpected
        // shape cannot fall through, and refuses rather than inventing a match.
        PNode::Meta {
            list: true, name, ..
        } => {
            steps.tick()?;
            if !node.is_named() {
                return Ok(None);
            }
            match name {
                None => Ok(Some(bindings.clone())),
                Some(name) => match bindings.get_tokens(name) {
                    Some(bound) => Ok(bound
                        .equals_node(node, source, steps)?
                        .then(|| bindings.clone())),
                    None => {
                        bindings.bind_node(name, CaptureKind::List, node, source, steps)?;
                        Ok(Some(bindings.clone()))
                    }
                },
            }
        }
        // A leaf: same kind and same text.
        PNode::Leaf { kind_id, text, .. } => {
            steps.tick()?;
            let same = node.kind_id() == *kind_id
                && source.get(node.start_byte()..node.end_byte()) == Some(text.as_str());
            Ok(same.then(|| bindings.clone()))
        }
        PNode::Interior {
            kind_id, children, ..
        } => {
            steps.tick()?;
            if node.kind_id() != *kind_id {
                return Ok(None);
            }
            let src_children = children_without_extras(node);
            let r = match_seq(children, &src_children, source, steps, bindings)?;
            if std::env::var_os("OCAST_DBG4").is_some() {
                eprintln!(
                    "interior {} children_pat={} children_src={} -> {:?}",
                    kind_id,
                    children.len(),
                    src_children.len(),
                    r.as_ref().map(|b| &b.vars)
                );
            }
            Ok(r)
        }
    }
}

/// The children of `node` that take part in matching: extras (comments) are ignored, but anonymous
/// tokens are kept, because `a + b` must not match `a - b`.
fn children_without_extras(node: tree_sitter::Node<'_>) -> Vec<tree_sitter::Node<'_>> {
    let mut out = Vec::new();
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if !child.is_extra() {
            out.push(child);
        }
    }
    out
}

/// Match a source child sequence against a pattern child sequence, with list variables absorbing
/// zero or more consecutive source children lazily.
///
/// RECURSION IS SAFE HERE, and the reason matters: this recurses over PATTERN elements, so the call
/// depth is bounded by the pattern (compilation caps it at 64 levels and the text at 16 KiB), never
/// by the source. A 5000-deep source is walked by [`search`]'s iterative pre-order walk and arrives
/// here as a node, not as 5000 stack frames. The earlier explicit-stack version of this function
/// was harder to verify and had a real frame-popping bug; the simple version is what ships.
///
/// Laziness: a list variable tries absorbing zero children first and grows by one per retry, so the
/// shortest absorption that lets the rest match is the one taken.
fn match_seq(
    pats: &[PNode],
    nodes: &[tree_sitter::Node<'_>],
    source: &str,
    steps: &mut Steps,
    bindings: &mut Bindings,
) -> Result<Option<Bindings>, ToolError> {
    match_seq_from(0, pats, 0, nodes, source, steps, bindings)
}

/// Match `pats[pi..]` against `nodes[si..]`.
fn match_seq_from(
    pi: usize,
    pats: &[PNode],
    si: usize,
    nodes: &[tree_sitter::Node<'_>],
    source: &str,
    steps: &mut Steps,
    bindings: &mut Bindings,
) -> Result<Option<Bindings>, ToolError> {
    // Every iteration of the (possibly exponential) search ticks, so the budget bounds the search.
    steps.tick()?;

    if pi == pats.len() {
        return Ok((si == nodes.len()).then(|| bindings.clone()));
    }

    if matches!(&pats[pi], PNode::Meta { list: true, .. }) {
        let name = list_name(&pats[pi]);
        let mut take = 0usize;
        loop {
            // `take` is how many source children this list variable absorbs.
            if si + take > nodes.len() {
                return Ok(None);
            }
            let mut candidate = bindings.clone();
            let ok = bind_list(
                name,
                nodes,
                si,
                take,
                // A repeated list variable is compared against what it absorbed the first time.
                bindings.get(name.unwrap_or("")),
                source,
                steps,
                &mut candidate,
            )?;
            if ok
                && let Some(found) = match_seq_from(
                    pi + 1,
                    pats,
                    si + take,
                    nodes,
                    source,
                    steps,
                    &mut candidate,
                )?
            {
                return Ok(Some(found));
            }
            take += 1;
        }
    }

    // An ordinary element consumes exactly one source child. `match_node` returns the bindings it
    // produced (an Interior matches a whole child sequence and may bind inside it), so the
    // continuation must start from THAT, not from the unchanged local clone.
    if si >= nodes.len() {
        return Ok(None);
    }
    let mut inner = bindings.clone();
    let Some(mut next) = match_node(&pats[pi], nodes[si], source, steps, &mut inner)? else {
        return Ok(None);
    };
    match_seq_from(pi + 1, pats, si + 1, nodes, source, steps, &mut next)
}

/// The name of a list metavariable, if it has one.
fn list_name(p: &PNode) -> Option<&str> {
    match p {
        PNode::Meta { name, list: true } => name.as_deref(),
        _ => None,
    }
}

/// Bind (or check) the range a list variable absorbs, for `take` source children starting at
/// `start_si`.
///
/// Returns `false` without touching `out` when a repeated list variable would not absorb an equal
/// sequence, which tells the caller to absorb one more child instead.
#[allow(clippy::too_many_arguments)]
fn bind_list(
    name: Option<&str>,
    nodes: &[tree_sitter::Node<'_>],
    start_si: usize,
    take: usize,
    previous: Option<(usize, usize)>,
    source: &str,
    steps: &mut Steps,
    out: &mut Bindings,
) -> Result<bool, ToolError> {
    let (start, end) = list_range(nodes, start_si, take);
    let Some(name) = name else {
        // An anonymous `$$$` absorbs freely and captures nothing.
        return Ok(true);
    };
    match previous {
        None => {
            out.bind(name, CaptureKind::List, start, end);
            Ok(true)
        }
        Some((prev_start, prev_end)) => {
            if absorbed_sequences_equal(nodes, start_si, take, prev_start, prev_end, source, steps)?
            {
                Ok(true)
            } else {
                Ok(false)
            }
        }
    }
}

/// Whether the source children a list variable absorbs now are structurally equal to the ones it
/// absorbed the first time.
///
/// The earlier binding is a byte RANGE (`prev_start..prev_end`), because that is what a capture
/// reports; the new absorption is a run of NODES. So the nodes the earlier range covers are
/// collected and compared with the new ones, as trees.
#[allow(clippy::too_many_arguments)]
fn absorbed_sequences_equal(
    nodes: &[tree_sitter::Node<'_>],
    start_si: usize,
    take: usize,
    prev_start: usize,
    prev_end: usize,
    source: &str,
    steps: &mut Steps,
) -> Result<bool, ToolError> {
    let new = &nodes[start_si..(start_si + take).min(nodes.len())];
    let old: Vec<tree_sitter::Node<'_>> = nodes
        .iter()
        .copied()
        .filter(|n| n.start_byte() >= prev_start && n.end_byte() <= prev_end)
        .collect();
    if old.len() != new.len() {
        return Ok(false);
    }
    for (o, n) in old.iter().zip(new.iter()) {
        steps.tick()?;
        // Comparing two single nodes: kind id, arity and leaf texts.
        if !Tokens::of(*o, source, steps)?.equals_node(*n, source, steps)? {
            return Ok(false);
        }
    }
    Ok(true)
}

/// The byte range a list variable covers: the first absorbed node's start to the last one's end, so
/// a list's text includes the separators between its nodes (`"start", id`).
///
/// An EMPTY list has `start == end` at the position where it would sit: immediately after the
/// previous sibling, or at the end of the parent's children when there is no previous sibling.
fn list_range(nodes: &[tree_sitter::Node<'_>], start_si: usize, take: usize) -> (usize, usize) {
    match (take, nodes.get(start_si)) {
        (0, Some(previous)) => (previous.end_byte(), previous.end_byte()),
        (0, None) => {
            let end = nodes.last().map_or(0, |n| n.end_byte());
            (end, end)
        }
        (_, Some(first)) => {
            let start = first.start_byte();
            let end = nodes
                .get(start_si + take.saturating_sub(1))
                .map_or(first.end_byte(), |n| n.end_byte());
            (start, end)
        }
        (_, None) => (0, 0),
    }
}

/// The token stream of a byte range: comments removed, whitespace collapsed, string literals kept
/// verbatim so `'a b'` does not compare equal to `'a'` `b`.
fn tokens(source: &str, start: usize, end: usize) -> Vec<String> {
    let text = source.get(start..end).unwrap_or_default();
    let mut out: Vec<String> = Vec::new();
    let mut current = String::new();
    let mut chars = text.chars().peekable();
    let flush = |out: &mut Vec<String>, current: &mut String| {
        if !current.is_empty() {
            out.push(std::mem::take(current));
        }
    };
    while let Some(c) = chars.next() {
        match c {
            '"' | '\'' | '`' => {
                flush(&mut out, &mut current);
                let mut literal = String::from(c);
                while let Some(c) = chars.next() {
                    literal.push(c);
                    if c == '\\' {
                        if let Some(next) = chars.next() {
                            literal.push(next);
                        }
                    } else if c == c_quote_opened(&literal) {
                        break;
                    }
                }
                out.push(literal);
            }
            '/' if chars.peek() == Some(&'/') => {
                for c in chars.by_ref() {
                    if c == '\n' {
                        break;
                    }
                }
            }
            '/' if chars.peek() == Some(&'*') => {
                chars.next();
                let mut prev = '\0';
                for c in chars.by_ref() {
                    if prev == '*' && c == '/' {
                        break;
                    }
                    prev = c;
                }
            }
            c if c.is_whitespace() => flush(&mut out, &mut current),
            c => current.push(c),
        }
    }
    flush(&mut out, &mut current);
    out
}

/// The closing quote of a literal, i.e. the quote it was opened with.
fn c_quote_opened(literal: &str) -> char {
    literal.chars().next().unwrap_or('"')
}

pub(super) fn search(
    parsed: &ParsedFile,
    source: &str,
    pattern: &Pattern,
    rule: Option<&CompiledRule>,
    budget: &SearchBudget,
) -> Result<SearchOutcome, ToolError> {
    // The pattern must have been compiled for this file's language: kind ids are grammar-specific,
    // so comparing across languages would be meaningless (and the spec calls for `invalid_args`).
    if pattern.language != parsed.language {
        return Err(ToolError::new(
            ErrorCode::InvalidArgs,
            format!(
                "pattern was compiled for {} but the file is {}",
                pattern.language.id(),
                parsed.language.id()
            ),
            "compile the pattern for the language of the file being searched",
        ));
    }

    let mut steps = Steps {
        used: 0,
        max: budget.max_steps,
        deadline: budget.deadline,
    };
    let rule_program = rule.map(|r| &r.program);
    let line_starts = LineIndex::new(source);

    let mut matches: Vec<Match> = Vec::new();
    let mut truncated = false;
    let root_is_meta = matches!(&pattern.program.root, PNode::Meta { .. });

    // Every node is a candidate root, in pre-order (a parent before its children). The walk is
    // iterative: the source can be thousands of levels deep and must not use the stack.
    let mut pending: Vec<tree_sitter::Node<'_>> = vec![parsed.tree.root_node()];
    while let Some(node) = pending.pop() {
        // Between candidate roots is one of the two places the deadline is read regardless of steps.
        steps.check_deadline()?;

        // A non-meta pattern root never matches an ERROR node, but ERROR descendants still can.
        let usable =
            root_is_meta || !(node.is_error() || node.is_missing() || node.kind() == "ERROR");
        if usable && let Some(bindings) = match_at(&pattern.program, node, source, &mut steps)? {
            {
                let accepted = match rule_program {
                    // The rule evaluator is a separate ticket; until it lands the rule is treated
                    // as satisfied so a compiled rule cannot silently drop every match.
                    Some(r) => super::rules::eval(r, node, &bindings, source, &mut steps)?,
                    None => true,
                };
                if accepted {
                    if matches.len() >= budget.max_matches {
                        truncated = true;
                        break;
                    }
                    matches.push(build_match(node, source, &bindings, &line_starts));
                }
            }
        }

        // Push children in reverse so they pop in document order.
        let mut cursor = node.walk();
        let children: Vec<tree_sitter::Node<'_>> = node.children(&mut cursor).collect();
        for child in children.into_iter().rev() {
            pending.push(child);
        }
    }

    // Already in pre-order, but the sort is what the contract promises: by start byte, outer first
    // for equal starts. Pre-order traversal gives that, and sorting again makes it independent of
    // how the walk produced them.
    matches.sort_by(|a, b| {
        a.start_byte
            .cmp(&b.start_byte)
            .then(b.end_byte.cmp(&a.end_byte))
    });

    Ok(SearchOutcome {
        matches,
        truncated,
        steps_used: steps.used,
    })
}

/// Build the reported match for a node and its bindings.
fn build_match(
    node: tree_sitter::Node<'_>,
    source: &str,
    bindings: &Bindings,
    lines: &LineIndex,
) -> Match {
    let (start_byte, end_byte) = (node.start_byte(), node.end_byte());
    let (start_line, start_col) = lines.position(start_byte);
    // `end_col` is one past the last byte, so it is the column of `end_byte`.
    let (end_line, end_col) = lines.position(end_byte);
    let captures = bindings
        .vars
        .iter()
        .map(|(name, kind, start, end)| super::Capture {
            name: name.clone(),
            kind: *kind,
            start_byte: *start,
            end_byte: *end,
            text: source.get(*start..*end).unwrap_or_default().to_string(),
        })
        .collect();
    Match {
        start_byte,
        end_byte,
        start_line,
        start_col,
        end_line,
        end_col,
        text: source
            .get(start_byte..end_byte)
            .unwrap_or_default()
            .to_string(),
        captures,
    }
}

/// Byte offset to 1-based (line, byte column), built once per search so each match is O(log lines).
struct LineIndex {
    /// Byte offset at which each line starts.
    starts: Vec<usize>,
}

impl LineIndex {
    fn new(source: &str) -> LineIndex {
        let mut starts = vec![0usize];
        for (i, b) in source.bytes().enumerate() {
            if b == b'\n' {
                starts.push(i + 1);
            }
        }
        LineIndex { starts }
    }

    fn position(&self, byte: usize) -> (usize, usize) {
        match self.starts.binary_search(&byte) {
            Ok(i) => (i + 1, 1),
            Err(0) => (1, byte + 1),
            Err(i) => (i, byte - self.starts[i - 1] + 1),
        }
    }
}
#[cfg(test)]
mod tests {
    //! Unit tests for the matcher core, written against HAND-BUILT `PNode` values.
    //!
    //! The compiler is a separate ticket, so these tests build the pattern tree directly: that
    //! keeps the matcher's contract testable on its own, and makes each test say exactly which
    //! shape it exercises.
    //!
    //! Source nodes come from a real tree-sitter parse of JavaScript (the query crate depends on
    //! `tree-sitter` directly), so the matching sees genuine grammar nodes.

    use super::*;
    use crate::pattern::compile::{PNode, Program};

    /// Kind ids of the JavaScript grammar, resolved once so the tests assert against real ids.
    struct Js {
        lang: tree_sitter::Language,
    }

    impl Js {
        fn new() -> Js {
            // The grammar comes from the `lang` crate, which owns the feature-gated grammars; the
            // query crate has no grammar dependency of its own.
            Js {
                lang: opencrayast_lang::Language::JavaScript
                    .grammar()
                    .expect("javascript grammar is built in"),
            }
        }

        fn parse(&self, src: &str) -> tree_sitter::Tree {
            let mut p = tree_sitter::Parser::new();
            p.set_language(&self.lang).expect("javascript grammar");
            p.parse(src, None).expect("parses")
        }

        fn root(&self, src: &str) -> tree_sitter::Tree {
            self.parse(src)
        }
    }

    /// The kind id of `name`. `named` must match how the grammar declares it: the `+` token is
    /// anonymous, and asking with `true` returns 0, which would make every anonymous pattern
    /// element match the wrong thing.
    fn kind_id_named(lang: &tree_sitter::Language, name: &str, named: bool) -> u16 {
        let id = lang.id_for_node_kind(name, named);
        assert_ne!(id, 0, "unknown node kind {name:?} (named={named})");
        id
    }

    /// Find the first node of `kind` in document order.
    fn find<'a>(root: tree_sitter::Node<'a>, kind: &str) -> Option<tree_sitter::Node<'a>> {
        let mut stack = vec![root];
        while let Some(n) = stack.pop() {
            if n.kind() == kind {
                return Some(n);
            }
            let mut c = n.walk();
            for ch in n.children(&mut c) {
                stack.push(ch);
            }
        }
        None
    }

    fn leaf(lang: &tree_sitter::Language, kind: &str, text: &str) -> PNode {
        PNode::Leaf {
            kind_id: kind_id_named(lang, kind, true),
            kind: leak(kind),
            named: true,
            text: text.to_string(),
        }
    }

    fn interior(
        lang: &tree_sitter::Language,
        kind: &str,
        named: bool,
        children: Vec<PNode>,
    ) -> PNode {
        PNode::Interior {
            kind_id: kind_id_named(lang, kind, named),
            kind: leak(kind),
            named,
            children,
        }
    }

    fn one(name: Option<&str>) -> PNode {
        PNode::Meta {
            name: name.map(str::to_string),
            list: false,
        }
    }

    fn list(name: Option<&str>) -> PNode {
        PNode::Meta {
            name: name.map(str::to_string),
            list: true,
        }
    }

    /// The pattern kinds are `&'static str` in the contract; the tests build them from literals.
    fn leak(s: &str) -> &'static str {
        Box::leak(s.to_string().into_boxed_str())
    }

    fn program(root: PNode) -> Program {
        Program {
            root,
            vars: Vec::new(),
            warning: None,
        }
    }

    fn steps() -> Steps {
        Steps {
            used: 0,
            max: 10_000_000,
            deadline: None,
        }
    }

    #[test]
    fn a_leaf_matches_only_the_same_kind_and_text() {
        let js = Js::new();
        let src = "a + 1;\n";
        let tree = js.root(src);
        let root = tree.root_node();
        let num = find(root, "number").expect("a number leaf");
        let ident = find(root, "identifier").expect("an identifier leaf");

        let mut s = steps();
        // Same kind, same text.
        let prog = program(leaf(&js.lang, "identifier", "a"));
        assert!(match_at(&prog, ident, src, &mut s).unwrap().is_some());
        // Same kind, different text.
        let prog = program(leaf(&js.lang, "identifier", "z"));
        assert!(match_at(&prog, ident, src, &mut s).unwrap().is_none());
        // Different kind, same text.
        let prog = program(leaf(&js.lang, "property_identifier", "a"));
        assert!(match_at(&prog, ident, src, &mut s).unwrap().is_none());
        // A number is not an identifier even when the texts match by accident.
        let prog = program(leaf(&js.lang, "identifier", "1"));
        assert!(match_at(&prog, num, src, &mut s).unwrap().is_none());
    }

    #[test]
    fn a_metavariable_matches_any_named_node_and_binds_it() {
        let js = Js::new();
        let src = "a + 1;\n";
        let tree = js.root(src);
        let root = tree.root_node();
        let num = find(root, "number").expect("a number leaf");
        let mut s = steps();

        let prog = program(one(Some("X")));
        let b = match_at(&prog, num, src, &mut s).unwrap().expect("matches");
        assert_eq!(b.vars.len(), 1);
        assert_eq!(b.vars[0].0, "X");
        assert_eq!(b.vars[0].1, CaptureKind::One);
        assert_eq!(
            (b.vars[0].2, b.vars[0].3),
            (num.start_byte(), num.end_byte())
        );

        // An anonymous metavariable matches but binds nothing.
        let prog = program(one(None));
        assert!(match_at(&prog, num, src, &mut s).unwrap().is_some());
    }

    #[test]
    fn a_metavariable_does_not_match_an_anonymous_token() {
        let js = Js::new();
        let src = "a + b;\n";
        let tree = js.root(src);
        let bin = find(tree.root_node(), "binary_expression").expect("binary expression");
        let mut cursor = bin.walk();
        let plus = bin
            .children(&mut cursor)
            .into_iter()
            .find(|n| n.kind() == "+")
            .expect("the + token");
        let mut s = steps();
        let prog = program(one(Some("X")));
        assert!(
            match_at(&prog, plus, src, &mut s).unwrap().is_none(),
            "an anonymous token is not a named node"
        );
    }

    #[test]
    fn a_repeated_metavariable_requires_structural_equality() {
        let js = Js::new();
        // `(a + b)` and `(a+b)` are the same structure written differently.
        let src = "(a + b) == (a+b);\nx == x;\ny == z;\n";
        let tree = js.root(src);
        let mut s = steps();
        let prog = program(one(Some("A")));

        let paren1 = find_node_at(src, &tree, "(a + b)");
        let paren2 = find_node_at(src, &tree, "(a+b)");
        let y = find_node_at(src, &tree, "y");
        let z = find_node_at(src, &tree, "z");

        // Bind to the first node, then require a second occurrence to be structurally equal. The
        // binding carries the node's shape, so this compares trees.
        let rebound = |bound: tree_sitter::Node<'_>, other: tree_sitter::Node<'_>| {
            let mut bs = steps();
            let tokens = Tokens::of(bound, src, &mut bs).expect("tokens");
            tokens.equals_node(other, src, &mut bs).expect("compare")
        };
        assert!(
            rebound(paren1, paren2),
            "whitespace is not structure: `(a + b)` and `(a+b)` rebind"
        );
        assert!(
            !rebound(y, z),
            "different identifiers are not structurally equal"
        );
        // The original single-node match still works.
        assert!(match_at(&prog, paren1, src, &mut s).unwrap().is_some());
    }

    #[test]
    fn comments_and_whitespace_are_ignored_by_equality() {
        let js = Js::new();
        let src = "(a /* c */ + b) == (a + b);\n";
        let tree = js.root(src);
        let p1 = find_node_at(src, &tree, "(a /* c */ + b)");
        let p2 = find_node_at(src, &tree, "(a + b)");
        let mut s = steps();
        let tokens = Tokens::of(p1, src, &mut s).expect("tokens");
        assert!(
            tokens.equals_node(p2, src, &mut s).expect("compare"),
            "a comment inside the expression is an extra, not structure"
        );
    }

    #[test]
    fn string_literals_compare_by_content_not_by_tokens() {
        let js = Js::new();
        let src = "'a b' == \"a b\";\n";
        let tree = js.root(src);
        let s1 = find_node_at(src, &tree, "'a b'");
        let s2 = find_node_at(src, &tree, "\"a b\"");
        assert_ne!(s1.start_byte(), s2.start_byte(), "two distinct literals");
        let mut s = steps();
        // `'a b'` and `"a b"` are different literals: the quote style is part of the leaf text.
        let tokens = Tokens::of(s1, src, &mut s).expect("tokens");
        assert!(
            !tokens.equals_node(s2, src, &mut s).expect("compare"),
            "the two literals are not structurally equal"
        );
    }

    /// The node whose text is exactly `text`.
    fn find_node_at<'a>(
        src: &str,
        tree: &'a tree_sitter::Tree,
        text: &str,
    ) -> tree_sitter::Node<'a> {
        let mut stack = vec![tree.root_node()];
        while let Some(n) = stack.pop() {
            if &src[n.start_byte()..n.end_byte()] == text {
                return n;
            }
            let mut c = n.walk();
            for ch in n.children(&mut c) {
                stack.push(ch);
            }
        }
        panic!("no node with text {text:?}");
    }

    #[test]
    fn an_interior_requires_the_same_kind() {
        let js = Js::new();
        let src = "a + b;\n";
        let tree = js.root(src);
        let bin = find(tree.root_node(), "binary_expression").expect("binary expression");
        let mut s = steps();

        // Same kind, with the children it really has: two identifiers and the `+` token.
        let prog = program(interior(
            &js.lang,
            "binary_expression",
            true,
            vec![
                leaf(&js.lang, "identifier", "a"),
                interior(&js.lang, "+", false, vec![]),
                leaf(&js.lang, "identifier", "b"),
            ],
        ));
        assert!(match_at(&prog, bin, src, &mut s).unwrap().is_some());

        // A pattern Interior with NO children means the source node must have none, and the
        // binary_expression has three: this is a mismatch, not a wildcard.
        let prog = program(interior(&js.lang, "binary_expression", true, vec![]));
        assert!(match_at(&prog, bin, src, &mut s).unwrap().is_none());

        // Different kind.
        let prog = program(interior(
            &js.lang,
            "call_expression",
            true,
            vec![leaf(&js.lang, "identifier", "a")],
        ));
        assert!(match_at(&prog, bin, src, &mut s).unwrap().is_none());
    }

    #[test]
    fn children_are_matched_in_order_and_kind_by_kind() {
        let js = Js::new();
        let src = "a + b;\n";
        let tree = js.root(src);
        let bin = find(tree.root_node(), "binary_expression").expect("binary expression");
        let mut s = steps();

        // `+` must match `+`: the anonymous operator is a child and is compared.
        let prog = program(interior(
            &js.lang,
            "binary_expression",
            true,
            vec![
                one(Some("L")),
                interior(&js.lang, "+", false, vec![]),
                one(Some("R")),
            ],
        ));
        assert!(match_at(&prog, bin, src, &mut s).unwrap().is_some());

        // `a - b` must not match a pattern written with `+`.
        let src2 = "a - b;\n";
        let tree2 = js.root(src2);
        let bin2 = find(tree2.root_node(), "binary_expression").unwrap();
        let mut s2 = steps();
        let prog = program(interior(
            &js.lang,
            "binary_expression",
            true,
            vec![
                one(Some("L")),
                interior(&js.lang, "+", false, vec![]),
                one(Some("R")),
            ],
        ));
        assert!(match_at(&prog, bin2, src2, &mut s2).unwrap().is_none());
    }

    #[test]
    fn a_list_variable_absorbs_zero_or_more_and_is_lazy() {
        let js = Js::new();
        let src = "f(a, b, c);\n";
        let tree = js.root(src);
        let args = find(tree.root_node(), "arguments").expect("arguments node");
        let mut s = steps();

        // `[$$$ALL]`-style: a single list absorbs every argument.
        let prog = program(interior(
            &js.lang,
            "arguments",
            true,
            vec![list(Some("ALL"))],
        ));
        let b = match_at(&prog, args, src, &mut s)
            .unwrap()
            .expect("matches");
        assert_eq!(b.vars.len(), 1);
        assert_eq!(b.vars[0].1, CaptureKind::List);
        assert_eq!(
            &src[b.vars[0].2..b.vars[0].3],
            "(a, b, c)",
            "the list absorbs every child, parentheses included"
        );

        // An empty source sequence is absorbed by a list variable.
        let src0 = "f();\n";
        let tree0 = js.root(src0);
        let args0 = find(tree0.root_node(), "arguments").unwrap();
        let mut s0 = steps();
        let prog = program(interior(
            &js.lang,
            "arguments",
            true,
            vec![
                interior(&js.lang, "(", false, vec![]),
                list(Some("ALL")),
                interior(&js.lang, ")", false, vec![]),
            ],
        ));
        let b0 = match_at(&prog, args0, src0, &mut s0)
            .unwrap()
            .expect("matches");
        assert_eq!(
            b0.vars[0].2, b0.vars[0].3,
            "an empty list has start == end, just after the previous sibling `(`"
        );
        assert_eq!(&src0[b0.vars[0].2..b0.vars[0].3], "");
        assert!(
            b0.vars[0].2 <= 3 && b0.vars[0].2 >= 1,
            "the empty list sits inside the parentheses, at {}",
            b0.vars[0].2
        );
    }

    #[test]
    fn a_list_variable_is_lazy_so_a_trailing_element_wins() {
        let js = Js::new();
        // `[$$$REST, 3]`: laziness means the list stops at the last element, so `3` binds.
        let src = "[1, 2, 3];\n";
        let tree = js.root(src);
        let arr = find(tree.root_node(), "array").expect("array");
        let mut s = steps();
        let prog = program(interior(
            &js.lang,
            "array",
            true,
            vec![
                interior(&js.lang, "[", false, vec![]),
                list(Some("REST")),
                interior(&js.lang, ",", false, vec![]),
                leaf(&js.lang, "number", "3"),
                interior(&js.lang, "]", false, vec![]),
            ],
        ));
        let b = match_at(&prog, arr, src, &mut s).unwrap().expect("matches");
        // The list absorbed `1, 2` and the trailing `3` matched literally.
        assert_eq!(&src[b.vars[0].2..b.vars[0].3], "1, 2");
    }

    #[test]
    fn two_list_variables_partition_the_sequence() {
        let js = Js::new();
        let src = "[1, 2, 3];\n";
        let tree = js.root(src);
        let arr = find(tree.root_node(), "array").unwrap();
        let mut s = steps();
        // `[$$$A, , $$$B]`: the middle comma separates the two lists.
        let prog = program(interior(
            &js.lang,
            "array",
            true,
            vec![
                interior(&js.lang, "[", false, vec![]),
                list(Some("A")),
                interior(&js.lang, ",", false, vec![]),
                list(Some("B")),
                interior(&js.lang, "]", false, vec![]),
            ],
        ));
        let b = match_at(&prog, arr, src, &mut s).unwrap().expect("matches");
        let by: Vec<_> = b
            .vars
            .iter()
            .map(|(n, _, s, e)| (n.as_str(), &src[*s..*e]))
            .collect();
        assert_eq!(
            by,
            [("A", "1"), ("B", "2, 3")],
            "lazily: the first list absorbs as few children as the rest of the pattern allows"
        );
    }

    #[test]
    fn a_repeated_list_variable_must_absorb_an_equal_sequence() {
        let js = Js::new();
        // Two list variables with the same name must absorb structurally equal sequences.
        let src = "[1, 1];\n";
        let tree = js.root(src);
        let arr = find(tree.root_node(), "array").unwrap();
        let mut s = steps();
        // `[$$$X, , $$$X]`: both absorb one `1`, which is equal.
        let prog = program(interior(
            &js.lang,
            "array",
            true,
            vec![
                interior(&js.lang, "[", false, vec![]),
                list(Some("X")),
                interior(&js.lang, ",", false, vec![]),
                list(Some("X")),
                interior(&js.lang, "]", false, vec![]),
            ],
        ));
        assert!(
            match_at(&prog, arr, src, &mut s).unwrap().is_some(),
            "the same sequence twice rebinds"
        );

        // `[1, 2]` cannot satisfy the same pattern: the lists would differ.
        let src2 = "[1, 2];\n";
        let tree2 = js.root(src2);
        let arr2 = find(tree2.root_node(), "array").unwrap();
        let mut s2 = steps();
        // There is exactly one comma, so the only split is (1) / (2): different sequences, so the
        // repeated variable rejects it. This is the assertion that makes the check meaningful.
        assert!(
            match_at(&prog, arr2, src2, &mut s2).unwrap().is_none(),
            "1 and 2 are different sequences, so $$$X cannot absorb both"
        );
    }

    #[test]
    fn the_step_budget_stops_a_pathological_list_pattern() {
        let js = Js::new();
        // Three list variables over a long argument list: the partitioning space is large, and the
        // budget is what stops it.
        let items: Vec<String> = (0..200).map(|i| i.to_string()).collect();
        let src = format!("f({});\n", items.join(", "));
        let tree = js.root(&src);
        let args = find(tree.root_node(), "arguments").unwrap();
        let prog = program(interior(
            &js.lang,
            "arguments",
            true,
            vec![
                interior(&js.lang, "(", false, vec![]),
                list(Some("A")),
                interior(&js.lang, ",", false, vec![]),
                list(Some("B")),
                interior(&js.lang, ",", false, vec![]),
                list(Some("C")),
                interior(&js.lang, ")", false, vec![]),
            ],
        ));
        let mut s = Steps {
            used: 0,
            max: 50,
            deadline: None,
        };
        let e = match_at(&prog, args, &src, &mut s).unwrap_err();
        assert_eq!(e.code, ErrorCode::BudgetExceeded);
        assert!(e.message.to_lowercase().contains("step"), "{}", e.message);
    }

    #[test]
    fn a_deep_source_does_not_overflow_the_stack() {
        let js = Js::new();
        let n = 5000;
        let src = format!("{}1{};\n", "(".repeat(n), ")".repeat(n));
        let tree = js.root(&src);
        let num = find(tree.root_node(), "number").expect("the number leaf");
        let mut s = steps();
        let prog = program(leaf(&js.lang, "number", "1"));
        assert!(match_at(&prog, num, &src, &mut s).unwrap().is_some());
    }

    #[test]
    fn a_very_wide_source_does_not_overflow_the_stack() {
        let js = Js::new();
        let items: Vec<String> = (0..10_000).map(|i| i.to_string()).collect();
        let src = format!("f({});\n", items.join(", "));
        let tree = js.root(&src);
        let args = find(tree.root_node(), "arguments").unwrap();
        let mut s = steps();
        let prog = program(interior(
            &js.lang,
            "arguments",
            true,
            vec![interior(&js.lang, ",", false, vec![]), list(Some("REST"))],
        ));
        // Whether it matches or not, it must finish and not overflow.
        let _ = match_at(&prog, args, &src, &mut s);
    }

    #[test]
    fn the_deadline_cancels_a_search() {
        let js = Js::new();
        let items: Vec<String> = (0..3000).map(|i| i.to_string()).collect();
        let src = format!("f({});\n", items.join(", "));
        let tree = js.root(&src);
        let args = find(tree.root_node(), "arguments").unwrap();
        let prog = program(interior(
            &js.lang,
            "arguments",
            true,
            vec![
                interior(&js.lang, "(", false, vec![]),
                list(Some("A")),
                interior(&js.lang, ",", false, vec![]),
                list(Some("B")),
                interior(&js.lang, ",", false, vec![]),
                list(Some("C")),
                interior(&js.lang, ")", false, vec![]),
            ],
        ));
        let mut s = Steps {
            used: 0,
            max: u64::MAX,
            deadline: Some(Instant::now()),
        };
        // The deadline is read every 1024 steps, so a huge budget still cancels within the first
        // batch of ticks for a source this size.
        let e = match_at(&prog, args, &src, &mut s).unwrap_err();
        assert_eq!(e.code, ErrorCode::Timeout);
    }

    #[test]
    fn comments_between_children_are_ignored_by_the_sequence_walk() {
        let js = Js::new();
        let src = "f(a /* c */, b);\n";
        let tree = js.root(src);
        let args = find(tree.root_node(), "arguments").unwrap();
        let mut s = steps();
        let prog = program(interior(
            &js.lang,
            "arguments",
            true,
            vec![
                interior(&js.lang, "(", false, vec![]),
                leaf(&js.lang, "identifier", "a"),
                interior(&js.lang, ",", false, vec![]),
                leaf(&js.lang, "identifier", "b"),
                interior(&js.lang, ")", false, vec![]),
            ],
        ));
        assert!(
            match_at(&prog, args, src, &mut s).unwrap().is_some(),
            "the comment is an extra and does not take part in matching"
        );
    }

    #[test]
    fn an_error_node_still_matches_through_its_children() {
        let js = Js::new();
        let src = "f(1);\nf(;\nf(2);\n";
        let tree = js.root(src);
        assert!(
            tree.root_node().has_error(),
            "the fixture must really be broken"
        );
        // The good calls still match: an ERROR sibling does not stop the walk.
        let mut s = steps();
        let prog = program(leaf(&js.lang, "number", "1"));
        let one = find_node_at(src, &tree, "1");
        assert!(match_at(&prog, one, src, &mut s).unwrap().is_some());
        // And the node inside the broken call is still a real node that can be matched.
        let two = find_node_at(src, &tree, "2");
        let prog2 = program(leaf(&js.lang, "number", "2"));
        assert!(match_at(&prog2, two, src, &mut s).unwrap().is_some());
    }

    #[test]
    fn a_list_capture_spans_the_separators_between_its_nodes() {
        let js = Js::new();
        let src = "f(\"start\", id);\n";
        let tree = js.root(src);
        let args = find(tree.root_node(), "arguments").unwrap();
        let mut s = steps();
        let prog = program(interior(
            &js.lang,
            "arguments",
            true,
            vec![list(Some("ARGS"))],
        ));
        let b = match_at(&prog, args, src, &mut s)
            .unwrap()
            .expect("matches");
        // The list's text spans from the first absorbed child's start to the last one's end, so the
        // separating comma is inside the capture.
        assert_eq!(&src[b.vars[0].2..b.vars[0].3], "(\"start\", id)");
    }
}

#[cfg(test)]
mod shape_tests {
    //! The structural-equality cases that a text comparison gets wrong, in every language the
    //! matcher supports. These are the tests that keep `Tokens` honest.

    use super::*;

    fn lang(l: opencrayast_lang::Language) -> tree_sitter::Language {
        l.grammar().expect("grammar is built in")
    }

    fn parse(l: &tree_sitter::Language, src: &str) -> tree_sitter::Tree {
        let mut p = tree_sitter::Parser::new();
        p.set_language(l).expect("grammar");
        p.parse(src, None).expect("parses")
    }

    fn steps() -> Steps {
        Steps {
            used: 0,
            max: 10_000_000,
            deadline: None,
        }
    }

    /// Find the first node whose text is exactly `text`.
    fn node_with_text<'a>(
        src: &str,
        tree: &'a tree_sitter::Tree,
        text: &str,
    ) -> tree_sitter::Node<'a> {
        let mut stack = vec![tree.root_node()];
        while let Some(n) = stack.pop() {
            if &src[n.start_byte()..n.end_byte()] == text {
                return n;
            }
            let mut c = n.walk();
            for ch in n.children(&mut c) {
                stack.push(ch);
            }
        }
        panic!("no node with text {text:?}");
    }

    /// Whether two subtrees of `src` are structurally equal.
    fn equal(l: &tree_sitter::Language, src: &str, a: &str, b: &str) -> Result<bool, ToolError> {
        let tree = parse(l, src);
        let na = node_with_text(src, &tree, a);
        let nb = node_with_text(src, &tree, b);
        Tokens::of(na, src, &mut steps())?.equals_node(nb, src, &mut steps())
    }

    #[test]
    fn spacing_does_not_change_equality() {
        // The case a token comparison got wrong: same tree, different bytes.
        let js = lang(opencrayast_lang::Language::JavaScript);
        assert!(
            equal(&js, concat!("x = a + b;\n", "y = a+b;\n"), "a + b", "a+b").unwrap(),
            "`a + b` and `a+b` are the same expression"
        );
        // Whitespace inside a call, too.
        assert!(
            equal(
                &js,
                concat!("f(a, b);\n", "f( a,b );\n"),
                "f(a, b)",
                "f( a,b )"
            )
            .unwrap(),
            "spacing inside arguments is not structure"
        );
        assert!(
            equal(
                &js,
                concat!("x = f(a, b);\n", "y = f(a,b);\n"),
                "f(a, b)",
                "f(a,b)"
            )
            .unwrap(),
            "no space after the comma is the same call"
        );
    }

    #[test]
    fn different_regex_literals_are_not_equal() {
        // The other case a token comparison got wrong: collapsing whitespace merged two regexes
        // that mean different things.
        let js = lang(opencrayast_lang::Language::JavaScript);
        assert!(
            !equal(
                &js,
                concat!("r = /a b/;\n", "s = /a  b/;\n"),
                "/a b/",
                "/a  b/"
            )
            .unwrap(),
            "a space inside a regex is part of the pattern, not formatting"
        );
        assert!(
            !equal(&js, concat!("r = /a/;\n", "s = /b/;\n"), "/a/", "/b/").unwrap(),
            "different regexes are different nodes"
        );
    }

    #[test]
    fn comments_do_not_change_equality() {
        let js = lang(opencrayast_lang::Language::JavaScript);
        assert!(
            equal(
                &js,
                concat!("x = a + b;\n", "y = a /* why */ + b;\n"),
                "a + b",
                "a /* why */ + b"
            )
            .unwrap(),
            "a comment inside the expression is an extra, not structure"
        );
    }

    #[test]
    fn python_equality_ignores_comments_but_not_structure() {
        let py = lang(opencrayast_lang::Language::Python);
        let src = "x = a + b
y = a +  b
z = a - b
";
        let tree = parse(&py, src);
        let a = node_with_text(src, &tree, "a + b");
        let bb = node_with_text(src, &tree, "a +  b");
        let c = node_with_text(src, &tree, "a - b");
        let mut s = steps();
        let tokens = Tokens::of(a, src, &mut s).unwrap();
        assert!(tokens.equals_node(bb, src, &mut s).unwrap(), "extra spaces");
        assert!(
            !tokens.equals_node(c, src, &mut s).unwrap(),
            "`a - b` is a different expression than `a + b`"
        );
    }

    #[test]
    fn go_equality_ignores_formatting_but_not_structure() {
        let go = lang(opencrayast_lang::Language::Go);
        let src = "package main

func f() {
	x := a + b
	y := a+b
	z := a - b
}
";
        let tree = parse(&go, src);
        let a = node_with_text(src, &tree, "a + b");
        let bb = node_with_text(src, &tree, "a+b");
        let c = node_with_text(src, &tree, "a - b");
        let mut s = steps();
        let tokens = Tokens::of(a, src, &mut s).unwrap();
        assert!(tokens.equals_node(bb, src, &mut s).unwrap());
        assert!(!tokens.equals_node(c, src, &mut s).unwrap());
    }

    #[test]
    fn a_deep_subtree_shape_does_not_overflow() {
        // The shape walk must be iterative: a 3000-deep binding is compared against another one.
        let js = lang(opencrayast_lang::Language::JavaScript);
        let n = 3000;
        let src = format!(
            "x = {}1{};\ny = {}1{};\n",
            "(".repeat(n),
            ")".repeat(n),
            "(".repeat(n),
            ")".repeat(n)
        );
        let tree = parse(&js, &src);
        let mut s = steps();
        let nodes: Vec<tree_sitter::Node<'_>> = {
            let mut stack = vec![tree.root_node()];
            let mut found = Vec::new();
            while let Some(node) = stack.pop() {
                if node.kind() == "number" {
                    found.push(node);
                }
                let mut c = node.walk();
                for ch in node.children(&mut c) {
                    stack.push(ch);
                }
            }
            found
        };
        assert_eq!(nodes.len(), 2, "two `1` leaves");
        let tokens = Tokens::of(nodes[0], &src, &mut s).unwrap();
        assert!(
            tokens.equals_node(nodes[1], &src, &mut s).unwrap(),
            "two identical 3000-deep expressions have the same shape"
        );
    }

    #[test]
    fn different_arity_nodes_are_not_equal() {
        // A leaf and an interior with the same kind cannot happen, but an empty call and one with an
        // argument must differ, which the shape catches on child count.
        let js = lang(opencrayast_lang::Language::JavaScript);
        assert!(
            !equal(&js, concat!("f();\n", "f(a);\n"), "f()", "f(a)").unwrap(),
            "a call with an argument is not the same node as one without"
        );
    }

    #[test]
    fn tokens_cost_steps() {
        // Every node of the shape walk ticks, so the budget bounds a rebinding comparison too.
        let js = lang(opencrayast_lang::Language::JavaScript);
        let src = "x = a + b;
";
        let tree = parse(&js, src);
        let a = node_with_text(src, &tree, "a + b");
        let mut s = Steps {
            used: 0,
            max: u64::MAX,
            deadline: None,
        };
        Tokens::of(a, src, &mut s).unwrap();
        assert!(s.used > 0, "the shape walk must consume steps");
    }
}

#[cfg(test)]
mod differential {
    //! Differential self-check: the real matcher against a deliberately naive reference.
    //!
    //! The reference in this module is written for CLARITY, not speed or safety: it recurses, it
    //! clones the whole environment on every branch, it has no budget, and it makes no attempt to
    //! be clever. It is meant to be obviously correct by inspection. The real matcher is then run
    //! over the same inputs and the two results must be identical - over a long list of random
    //! sources crossed with a set of hand-written patterns.
    //!
    //! Any disagreement is a bug in one of the two, and either way it is a bug worth finding.

    use super::*;
    use crate::pattern::compile::{PNode, Program};

    /// A very small, deliberately unoptimised matcher.
    ///
    /// It shares nothing with the production walk except `PNode` and `Steps::tick`: its own
    /// sequence matching, its own environment, its own backtracking.
    mod reference {
        use super::*;

        #[derive(Clone)]
        pub struct Env {
            pub vars: Vec<(String, CaptureKind, usize, usize)>,
        }

        impl Env {
            pub fn new() -> Env {
                Env { vars: Vec::new() }
            }
            fn get(&self, name: &str) -> Option<(usize, usize)> {
                self.vars
                    .iter()
                    .find(|(n, ..)| n == name)
                    .map(|(_, _, s, e)| (*s, *e))
            }
            fn set(&mut self, name: &str, kind: CaptureKind, s: usize, e: usize) {
                if self.get(name).is_none() {
                    self.vars.push((name.to_string(), kind, s, e));
                }
            }
        }

        /// Does `p` match `node`? Returns the resulting environment on success.
        pub fn m(
            p: &PNode,
            node: tree_sitter::Node<'_>,
            src: &str,
            env: &Env,
            depth: u64,
        ) -> Option<Env> {
            // The reference has no budget; it is bounded only by the recursion depth the tests use.
            if depth > 500 {
                return None;
            }
            match p {
                PNode::Meta { name, list: false } => {
                    if !node.is_named() {
                        return None;
                    }
                    let (s, e) = (node.start_byte(), node.end_byte());
                    let mut next = env.clone();
                    match name {
                        None => {}
                        Some(n) => match env.get(n) {
                            Some((bs, be)) => {
                                if super::reference_equal(src, bs, be, s, e) {
                                    // unchanged
                                } else {
                                    return None;
                                }
                            }
                            None => next.set(n, CaptureKind::One, s, e),
                        },
                    }
                    Some(next)
                }
                PNode::Leaf { kind_id, text, .. } => {
                    if node.kind_id() != *kind_id {
                        return None;
                    }
                    if src.get(node.start_byte()..node.end_byte())? != text.as_str() {
                        return None;
                    }
                    Some(env.clone())
                }
                PNode::Interior {
                    kind_id, children, ..
                } => {
                    if node.kind_id() != *kind_id {
                        return None;
                    }
                    let mut kids = Vec::new();
                    let mut c = node.walk();
                    for ch in node.children(&mut c) {
                        if !ch.is_extra() {
                            kids.push(ch);
                        }
                    }
                    seq(children, &kids, src, env, depth + 1)
                }
                PNode::Meta { name, list: true } => {
                    // A list metavariable standing as a whole node: treat it like `$NAME`.
                    if !node.is_named() {
                        return None;
                    }
                    let mut next = env.clone();
                    let (s, e) = (node.start_byte(), node.end_byte());
                    if let Some(n) = name {
                        next.set(n, CaptureKind::List, s, e);
                    }
                    Some(next)
                }
            }
        }

        /// Match `pats` against `nodes`, trying every split for list variables.
        pub fn seq(
            pats: &[PNode],
            nodes: &[tree_sitter::Node<'_>],
            src: &str,
            env: &Env,
            depth: u64,
        ) -> Option<Env> {
            if depth > 500 {
                return None;
            }
            if pats.is_empty() {
                return if nodes.is_empty() {
                    Some(env.clone())
                } else {
                    None
                };
            }
            let (head, tail) = pats.split_first().expect("non-empty");
            if let PNode::Meta {
                list: true, name, ..
            } = head
            {
                // Every split point, longest LAST would be greedy; the reference tries them in the
                // same lazy order as the real matcher so the comparison is about the mechanism and
                // not about the order. Both must end up with the same SET of successful splits.
                for take in 0..=nodes.len() {
                    let mut next = env.clone();
                    let (s, e) = range(nodes, take);
                    let ok = match name {
                        None => true,
                        Some(n) => match env.get(n) {
                            Some((bs, be)) => super::reference_equal(src, bs, be, s, e),
                            None => {
                                next.set(n, CaptureKind::List, s, e);
                                true
                            }
                        },
                    };
                    if ok && let Some(found) = seq(tail, &nodes[take..], src, &next, depth + 1) {
                        return Some(found);
                    }
                }
                return None;
            }
            if nodes.is_empty() {
                return None;
            }
            let next = m(head, nodes[0], src, env, depth + 1)?;
            seq(tail, &nodes[1..], src, &next, depth + 1)
        }

        fn range(nodes: &[tree_sitter::Node<'_>], take: usize) -> (usize, usize) {
            if take == 0 {
                let e = nodes.first().map_or(0, |n| n.end_byte());
                (e, e)
            } else {
                (nodes[0].start_byte(), nodes[take - 1].end_byte())
            }
        }
    }

    /// Structural equality for the reference, spelled out independently of the production helper.
    fn reference_equal(
        src: &str,
        a_start: usize,
        a_end: usize,
        b_start: usize,
        b_end: usize,
    ) -> bool {
        let a = src.get(a_start..a_end).unwrap_or_default();
        let b = src.get(b_start..b_end).unwrap_or_default();
        if a == b {
            return true;
        }
        scrub(a) == scrub(b)
    }

    /// Remove comments and collapse whitespace, keeping string literals intact.
    fn scrub(text: &str) -> String {
        let mut out = String::new();
        let mut chars = text.chars().peekable();
        while let Some(c) = chars.next() {
            match c {
                '"' | '\'' | '`' => {
                    out.push(c);
                    while let Some(c) = chars.next() {
                        out.push(c);
                        if c == '\\' {
                            if let Some(n) = chars.next() {
                                out.push(n);
                            }
                        } else if c == '"' || c == '\'' || c == '`' {
                            break;
                        }
                    }
                }
                '/' if chars.peek() == Some(&'/') => {
                    for c in chars.by_ref() {
                        if c == '\n' {
                            out.push('\n');
                            break;
                        }
                    }
                }
                '/' if chars.peek() == Some(&'*') => {
                    chars.next();
                    let mut prev = '\0';
                    for c in chars.by_ref() {
                        if prev == '*' && c == '/' {
                            break;
                        }
                        prev = c;
                    }
                }
                c if c.is_whitespace() => out.push(' '),
                c => out.push(c),
            }
        }
        out.split_whitespace().collect::<Vec<_>>().join(" ")
    }

    fn js() -> tree_sitter::Language {
        opencrayast_lang::Language::JavaScript
            .grammar()
            .expect("javascript grammar")
    }

    fn parse(lang: &tree_sitter::Language, src: &str) -> tree_sitter::Tree {
        let mut p = tree_sitter::Parser::new();
        p.set_language(lang).expect("grammar");
        p.parse(src, None).expect("parses")
    }

    fn id(lang: &tree_sitter::Language, kind: &str, named: bool) -> u16 {
        let v = lang.id_for_node_kind(kind, named);
        assert_ne!(v, 0, "unknown kind {kind}");
        v
    }

    fn leak(s: &str) -> &'static str {
        Box::leak(s.to_string().into_boxed_str())
    }

    fn leaf(lang: &tree_sitter::Language, kind: &str, text: &str) -> PNode {
        PNode::Leaf {
            kind_id: id(lang, kind, true),
            kind: leak(kind),
            named: true,
            text: text.to_string(),
        }
    }

    /// The kind id of an anonymous token, resolved by parsing a source that contains it.
    fn tok(lang: &tree_sitter::Language, kind: &str) -> PNode {
        let id = anonymous_kind_id(lang, kind);
        PNode::Leaf {
            kind_id: id,
            kind: leak(kind),
            named: false,
            text: kind.to_string(),
        }
    }

    ///  answers 0 for anonymous tokens, so ask the parser instead:
    /// find a node of that kind in a tiny source.
    fn anonymous_kind_id(lang: &tree_sitter::Language, kind: &str) -> u16 {
        for src in [
            "f(a, b);",
            "a + b;",
            "a == b;",
            "[a, b];",
            "if (a) { b; }",
            "{ a; }",
        ] {
            let tree = parse(lang, src);
            let mut stack = vec![tree.root_node()];
            while let Some(n) = stack.pop() {
                if n.kind() == kind && !n.is_named() {
                    return n.kind_id();
                }
                let mut c = n.walk();
                for ch in n.children(&mut c) {
                    stack.push(ch);
                }
            }
        }
        panic!("anonymous token {kind:?} not found");
    }

    /// A token that has no children. The kind id is resolved from the parser, because
    /// `id_for_node_kind` answers 0 for anonymous tokens: a bracket or operator only exists as a
    /// node the parser produced.
    fn tok_node(lang: &tree_sitter::Language, kind: &str) -> PNode {
        PNode::Leaf {
            kind_id: anonymous_kind_id(lang, kind),
            kind: leak(kind),
            named: false,
            text: kind.to_string(),
        }
    }

    /// An interior node whose kind is named in the grammar.
    fn inner(lang: &tree_sitter::Language, kind: &str, kids: Vec<PNode>) -> PNode {
        PNode::Interior {
            kind_id: id(lang, kind, true),
            kind: leak(kind),
            named: true,
            children: kids,
        }
    }

    fn one(name: Option<&str>) -> PNode {
        PNode::Meta {
            name: name.map(str::to_string),
            list: false,
        }
    }

    fn lst(name: Option<&str>) -> PNode {
        PNode::Meta {
            name: name.map(str::to_string),
            list: true,
        }
    }

    /// The six patterns the differential check runs against every generated source.
    fn patterns(lang: &tree_sitter::Language) -> Vec<PNode> {
        vec![
            // a call with any arguments: `f($X)`, `f($$$A)`
            inner(
                lang,
                "call_expression",
                vec![
                    one(Some("F")),
                    inner(
                        lang,
                        "arguments",
                        vec![tok_node(lang, "("), lst(Some("A")), tok_node(lang, ")")],
                    ),
                ],
            ),
            // `$A + $B`
            inner(
                lang,
                "binary_expression",
                vec![one(Some("L")), tok(lang, "+"), one(Some("R"))],
            ),
            // `[$$$X, $Y]`
            inner(
                lang,
                "array",
                vec![
                    tok_node(lang, "["),
                    lst(Some("X")),
                    tok_node(lang, ","),
                    one(Some("Y")),
                    tok_node(lang, "]"),
                ],
            ),
            // `$A == $A` (rebinding)
            inner(
                lang,
                "binary_expression",
                vec![one(Some("A")), tok(lang, "=="), one(Some("A"))],
            ),
            // `if ($C) { $$$B }`
            inner(
                lang,
                "if_statement",
                vec![
                    tok(lang, "if"),
                    tok_node(lang, "("),
                    one(Some("C")),
                    tok_node(lang, ")"),
                    inner(
                        lang,
                        "statement_block",
                        vec![tok_node(lang, "{"), lst(Some("B")), tok_node(lang, "}")],
                    ),
                ],
            ),
            // a plain number leaf
            leaf(lang, "number", "1"),
        ]
    }

    fn steps() -> Steps {
        Steps {
            used: 0,
            max: 200_000_000,
            deadline: None,
        }
    }

    /// Run both matchers over every node of `tree` and return the found ranges and captures.
    /// `(start, end, captures)` for one found match, sorted for set comparison.
    type Found = (usize, usize, Vec<(String, usize, usize)>);
    type Both = (Vec<Found>, Vec<Found>);

    fn run_both(program: &Program, src: &str, tree: &tree_sitter::Tree) -> Both {
        let mut mine = Vec::new();
        let mut theirs = Vec::new();
        let mut stack = vec![tree.root_node()];
        while let Some(node) = stack.pop() {
            if !node.is_error() {
                let mut s = steps();
                if let Some(b) = match_at(program, node, src, &mut s)
                    .expect("the real matcher never errors here")
                {
                    mine.push((node.start_byte(), node.end_byte(), canon(&b)));
                }
                if let Some(b) = reference::m(&program.root, node, src, &reference::Env::new(), 0) {
                    let mut v: Vec<(String, usize, usize)> = b
                        .vars
                        .iter()
                        .map(|(n, _, s, e)| (n.clone(), *s, *e))
                        .collect();
                    v.sort();
                    theirs.push((node.start_byte(), node.end_byte(), v));
                }
            }
            let mut c = node.walk();
            for ch in node.children(&mut c) {
                stack.push(ch);
            }
        }
        // The walk order differs between the two, so compare as sorted sets.
        mine.sort();
        theirs.sort();
        (mine, theirs)
    }

    fn canon(b: &Bindings) -> Vec<(String, usize, usize)> {
        let mut v: Vec<(String, usize, usize)> = b
            .vars
            .iter()
            .map(|(n, _, s, e)| (n.clone(), *s, *e))
            .collect();
        v.sort();
        v
    }

    #[test]
    fn empty_list_binding_is_reproduced_minimally() {
        let lang = js();
        let pat = inner(
            &lang,
            "call_expression",
            vec![
                one(Some("F")),
                inner(
                    &lang,
                    "arguments",
                    vec![tok_node(&lang, "("), lst(Some("A")), tok_node(&lang, ")")],
                ),
            ],
        );
        let program = Program {
            root: pat.clone(),
            vars: Vec::new(),
            warning: None,
        };
        let src = "g();\n";
        let tree = parse(&lang, src);
        let call = find_first(&tree, "call_expression");
        let mut s = steps();
        let mine = match_at(&program, call, src, &mut s).unwrap();
        println!("MINE {:?}", mine.as_ref().map(|b| b.vars.clone()));
        let theirs = reference::m(&pat, call, src, &reference::Env::new(), 0);
        println!("THEIRS {:?}", theirs.as_ref().map(|e| e.vars.clone()));
        assert_eq!(
            mine.map(|b| b.vars),
            theirs.map(|e| e.vars),
            "empty list must be captured the same way by both"
        );
    }

    fn find_first<'a>(tree: &'a tree_sitter::Tree, kind: &str) -> tree_sitter::Node<'a> {
        let mut stack = vec![tree.root_node()];
        while let Some(n) = stack.pop() {
            if n.kind() == kind {
                return n;
            }
            let mut c = n.walk();
            for ch in n.children(&mut c) {
                stack.push(ch);
            }
        }
        panic!("no {kind}");
    }

    /// Step growth for a pathological list pattern, as the ticket asks: `[$$$A, $$$B, $$$C, x]`
    /// over n children. Prints the measured steps so the growth is visible, and asserts it is
    /// POLYNOMIAL in n - not exponential - because the budget has to be able to stop it.
    #[test]
    fn pathological_list_step_growth_is_polynomial() {
        let lang = js();
        // The pattern as the spec writes it: three list variables then a literal that is absent, so
        // the walk must exhaust every partition before failing.
        // A trailing `y` identifier that never occurs in the source: every partition has to be
        // tried and rejected, which is what makes the case pathological.
        let pattern = inner(
            &lang,
            "array",
            vec![
                tok_node(&lang, "["),
                lst(Some("A")),
                tok_node(&lang, ","),
                lst(Some("B")),
                tok_node(&lang, ","),
                lst(Some("C")),
                leaf(&lang, "identifier", "y"),
                tok_node(&lang, "]"),
            ],
        );
        let program = Program {
            root: pattern,
            vars: Vec::new(),
            warning: None,
        };
        let mut measurements: Vec<(usize, u64)> = Vec::new();
        for n in [50usize, 100, 200] {
            let src = format!(
                "[{}];\n",
                (0..n).map(|i| i.to_string()).collect::<Vec<_>>().join(", ")
            );
            let tree = parse(&lang, &src);
            let arr = find_first(&tree, "array");
            let mut s = steps();
            let r = match_at(&program, arr, &src, &mut s);
            assert!(r.is_ok(), "n={n} must finish, not error");
            measurements.push((n, s.used));
        }
        for (n, used) in &measurements {
            eprintln!("STEPGROWTH n={n} steps={used}");
        }
        let [(_n0, s0), (n1, s1), (n2, s2)] = measurements[..] else {
            panic!("expected three measurements")
        };
        // Doubling n must not multiply the steps by anything near 2^n. Comparing the ratio of the
        // growth in steps against the growth in n is the polynomial check.
        let n_ratio = n2 as f64 / n1 as f64;
        let s_ratio = s2 as f64 / s1 as f64;
        assert!(
            s_ratio < n_ratio * n_ratio * 4.0,
            "step growth looks super-polynomial: n {n1}->{n2} (x{n_ratio:.2}), steps {s1}->{s2} (x{s_ratio:.2})"
        );
        assert!(
            s2 < 100_000_000,
            "n={n2} took {s2} steps, which is too many to be a practical budget"
        );
        assert!(
            s0 < s1 && s1 < s2,
            "steps must grow with n: {measurements:?}"
        );
    }

    /// The budget really does cut the pathological case off, and says so.
    #[test]
    fn the_budget_cuts_the_pathological_case_off() {
        let lang = js();
        // A trailing `y` identifier that never occurs in the source: every partition has to be
        // tried and rejected, which is what makes the case pathological.
        let pattern = inner(
            &lang,
            "array",
            vec![
                tok_node(&lang, "["),
                lst(Some("A")),
                tok_node(&lang, ","),
                lst(Some("B")),
                tok_node(&lang, ","),
                lst(Some("C")),
                leaf(&lang, "identifier", "y"),
                tok_node(&lang, "]"),
            ],
        );
        let program = Program {
            root: pattern,
            vars: Vec::new(),
            warning: None,
        };
        let src = format!(
            "[{}];\n",
            (0..200)
                .map(|i| i.to_string())
                .collect::<Vec<_>>()
                .join(", ")
        );
        let tree = parse(&lang, &src);
        let arr = find_first(&tree, "array");
        let mut s = Steps {
            used: 0,
            max: 10_000,
            deadline: None,
        };
        let e = match_at(&program, arr, &src, &mut s).unwrap_err();
        assert_eq!(e.code, ErrorCode::BudgetExceeded);
        assert!(e.message.to_lowercase().contains("step"), "{}", e.message);
    }

    /// Matching around ERROR nodes: a pattern that matches a whole broken construct must refuse,
    /// while its well-formed parts still match.
    #[test]
    fn error_nodes_are_handled_on_both_sides() {
        let lang = js();
        // `f(;` leaves an ERROR node around the call.
        let src = "f(1);\nf(;\nf(2);\n";
        let tree = parse(&lang, src);
        assert!(tree.root_node().has_error());

        // A call pattern does not match the broken call.
        let call_pattern = Program {
            root: inner(
                &lang,
                "call_expression",
                vec![
                    one(Some("F")),
                    inner(
                        &lang,
                        "arguments",
                        vec![tok_node(&lang, "("), lst(Some("A")), tok_node(&lang, ")")],
                    ),
                ],
            ),
            vars: Vec::new(),
            warning: None,
        };
        let mut matched = 0;
        let mut stack = vec![tree.root_node()];
        while let Some(n) = stack.pop() {
            if !n.is_error() {
                let mut s = steps();
                if match_at(&call_pattern, n, src, &mut s).unwrap().is_some() {
                    matched += 1;
                }
            }
            let mut c = n.walk();
            for ch in n.children(&mut c) {
                stack.push(ch);
            }
        }
        assert_eq!(matched, 2, "only the two well-formed calls match");

        // A pattern with a Meta root would match ANY named node, including inside the broken call.
        let meta_pattern = Program {
            root: one(Some("X")),
            vars: Vec::new(),
            warning: None,
        };
        let broken_child = find_node_text(src, &tree, "f(");
        let mut s = steps();
        assert!(
            match_at(&meta_pattern, broken_child, src, &mut s)
                .unwrap()
                .is_some(),
            "a metavariable root matches any named node, even a damaged one"
        );
    }

    fn find_node_text<'a>(
        src: &str,
        tree: &'a tree_sitter::Tree,
        text: &str,
    ) -> tree_sitter::Node<'a> {
        let mut stack = vec![tree.root_node()];
        while let Some(n) = stack.pop() {
            if &src[n.start_byte()..n.end_byte()] == text {
                return n;
            }
            let mut c = n.walk();
            for ch in n.children(&mut c) {
                stack.push(ch);
            }
        }
        panic!("no node {text:?}");
    }

    #[test]
    fn the_matcher_agrees_with_a_naive_reference_over_random_sources() {
        let lang = js();
        let pats = patterns(&lang);
        // Fragments that produce both clean and broken JavaScript.
        const TOKENS: [&str; 24] = [
            "f", "g", "(", ")", "1", "2", ",", ";", "{", "}", "[", "]", "+", "==", "x", "\n", "/*",
            "*/", "é", "=>", "'a'", "\"b\"", "if", "a",
        ];
        let mut state: u64 = 0x1357_9bdf_2468_ace0;
        let mut next = move || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state
        };
        let mut checked = 0usize;
        let mut with_matches = 0usize;

        for round in 0..2100 {
            let len = 1 + (next() % 24) as usize;
            let mut src = String::new();
            for _ in 0..len {
                src.push_str(TOKENS[(next() % TOKENS.len() as u64) as usize]);
                src.push(' ');
            }
            let tree = parse(&lang, &src);
            for p in &pats {
                let program = Program {
                    root: p.clone(),
                    vars: Vec::new(),
                    warning: None,
                };
                let (mine, theirs) = run_both(&program, &src, &tree);
                assert_eq!(
                    mine, theirs,
                    "round {round}: disagreement on {src:?} with pattern {:?}",
                    p
                );
                checked += 1;
                if !mine.is_empty() {
                    with_matches += 1;
                }
            }
        }
        // The check only means something if it actually found things.
        assert!(checked > 2000, "only {checked} comparisons");
        assert!(
            with_matches > 100,
            "the random sources matched nothing ({with_matches} hits): the check is vacuous"
        );
        eprintln!("DIFFERENTIAL comparisons={checked} with_matches={with_matches}");
    }
}
