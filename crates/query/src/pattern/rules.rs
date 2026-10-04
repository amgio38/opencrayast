//! Rule compilation and evaluation. See `Rule` in the `pattern` module.

use super::compile::Program;
use super::matcher::{Bindings, Steps};
use super::{CaptureKind, Pattern, PatternError, Rule, RuleOperand};
use opencrayast_core::error::{ErrorCode, ToolError};
use opencrayast_lang::Language;

/// One compiled rule node.
#[derive(Debug, Clone)]
pub(crate) struct RNode {
    /// Required kind id.
    pub(crate) kind: Option<u16>,
    /// `inside`.
    pub(crate) inside: Option<Box<ROperand>>,
    /// `has`.
    pub(crate) has: Option<Box<ROperand>>,
    /// `not`.
    pub(crate) not: Option<Box<ROperand>>,
    /// `all`.
    pub(crate) all: Vec<RNode>,
    /// `any`.
    pub(crate) any: Vec<RNode>,
    /// `where` constraints keyed by variable name WITHOUT the `$`.
    pub(crate) wheres: Vec<(String, WhereC)>,
}

/// A compiled operand of `inside` / `has` / `not`.
#[derive(Debug, Clone)]
pub(crate) enum ROperand {
    /// A compiled code pattern.
    Pattern(Program),
    /// A nested rule.
    Rule(Box<RNode>),
}

/// A compiled `where` constraint.
#[derive(Debug, Clone)]
pub(crate) struct WhereC {
    /// Compiled regular expression.
    pub(crate) regex: Option<regex::Regex>,
    /// Required kind id.
    pub(crate) kind: Option<u16>,
}

/// Compiler output of a whole rule.
#[derive(Debug, Clone)]
pub(crate) struct RuleProgram {
    /// The root rule node.
    pub(crate) root: RNode,
}

/// Longest `where` regex, in bytes. A megabyte of pattern is already absurd; the limit is what
/// keeps compilation time and the compiled program bounded.
const REGEX_MAX_BYTES: usize = 1024;

/// Deepest nesting of rule nodes. Eight is enough for anything readable; deeper nesting is a
/// sign the caller wants a pattern, not a rule.
const RULE_MAX_DEPTH: usize = 8;

/// Most rule nodes in one rule. Bounds compile time and, more importantly, how much work one
/// rule can make every match do.
const RULE_MAX_NODES: usize = 64;

/// Longest capture a `where` regex is run against. Bigger than this the regex is not worth
/// running and the constraint is treated as unsatisfied.
const REGEX_MAX_INPUT_BYTES: usize = 1024 * 1024;

/// Regex program budget, in bytes. Set well above anything a 1 KiB pattern needs and well below
/// anything that would make compilation slow: a pattern that needs more than this is refused at
/// compile time rather than at match time.
const REGEX_SIZE_LIMIT: usize = 1 << 20;

/// The linear-time engine's own limit on the compiled program.
const REGEX_DFA_SIZE_LIMIT: usize = 1 << 20;

/// The compiled program of a rule. Compilation is the only place a rule is inspected, so a rule
/// that would be ambiguous, unbounded or unmatchable is refused here and never at match time.
pub(super) fn compile(
    language: Language,
    rule: &Rule,
) -> Result<super::CompiledRule, PatternError> {
    let grammar = language.grammar().ok_or_else(|| PatternError {
        message: format!("{} has no grammar in this build", language.id()),
        position: None,
        suggestion: "Rules need a grammar; use a language that is built in.".to_string(),
    })?;
    let mut nodes = 0usize;
    let root = compile_node(language, &grammar, rule, 1, &mut nodes)?;
    Ok(super::CompiledRule {
        program: RuleProgram { root },
    })
}

/// Compile one rule node, counting nodes and depth as it goes.
fn compile_node(
    language: Language,
    grammar: &tree_sitter::Language,
    rule: &Rule,
    depth: usize,
    nodes: &mut usize,
) -> Result<RNode, PatternError> {
    *nodes += 1;
    if *nodes > RULE_MAX_NODES {
        return Err(PatternError {
            message: format!("a rule may hold at most {RULE_MAX_NODES} nodes"),
            position: None,
            suggestion: "Split the rule, or express the repeated part as a pattern operand."
                .to_string(),
        });
    }
    if depth > RULE_MAX_DEPTH {
        return Err(PatternError {
            message: format!("a rule may nest at most {RULE_MAX_DEPTH} deep"),
            position: None,
            suggestion: "Flatten the nesting with `all` / `any`.".to_string(),
        });
    }

    let kind = match &rule.kind {
        Some(name) => Some(lookup_kind(grammar, name)?),
        None => None,
    };

    let mut wheres = Vec::with_capacity(rule.where_.len());
    for (key, constraint) in &rule.where_ {
        // The key is written `$NAME`; it is stored without the `$` so it matches a capture's
        // name directly. Anything else is a typo, and a typo here would silently never match.
        let Some(name) = key
            .strip_prefix('$')
            .filter(|rest| !rest.is_empty() && rest.starts_with(|c: char| c.is_ascii_uppercase()))
        else {
            return Err(PatternError {
                message: format!("`where` key {key} is not a variable"),
                position: None,
                suggestion: "Write it as `$NAME`, with NAME starting with an upper-case letter."
                    .to_string(),
            });
        };
        if !name
            .chars()
            .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_')
        {
            return Err(PatternError {
                message: format!("`where` key {key} is not a variable"),
                position: None,
                suggestion: "Write it as `$NAME`, with NAME starting with an upper-case letter."
                    .to_string(),
            });
        }
        wheres.push((name.to_string(), compile_where(grammar, constraint)?));
    }

    Ok(RNode {
        kind,
        inside: compile_operand(language, grammar, rule.inside.as_deref(), depth, nodes)?,
        has: compile_operand(language, grammar, rule.has.as_deref(), depth, nodes)?,
        not: compile_operand(language, grammar, rule.not.as_deref(), depth, nodes)?,
        all: compile_list(language, grammar, &rule.all, depth, nodes)?,
        any: compile_list(language, grammar, &rule.any, depth, nodes)?,
        wheres,
    })
}

/// Compile the `all` / `any` lists.
fn compile_list(
    language: Language,
    grammar: &tree_sitter::Language,
    rules: &[Rule],
    depth: usize,
    nodes: &mut usize,
) -> Result<Vec<RNode>, PatternError> {
    rules
        .iter()
        .map(|r| compile_node(language, grammar, r, depth + 1, nodes))
        .collect()
}

/// Compile the operand of `inside` / `has` / `not`.
fn compile_operand(
    language: Language,
    grammar: &tree_sitter::Language,
    operand: Option<&RuleOperand>,
    depth: usize,
    nodes: &mut usize,
) -> Result<Option<Box<ROperand>>, PatternError> {
    match operand {
        None => Ok(None),
        Some(RuleOperand::Rule(rule)) => Ok(Some(Box::new(ROperand::Rule(Box::new(
            compile_node(language, grammar, rule, depth + 1, nodes)?,
        ))))),
        Some(RuleOperand::Pattern(text)) => {
            // A pattern operand is compiled like the main pattern. Its error says which part of
            // the rule it came from, because "unexpected token" with no location is useless
            // when the pattern is three levels down inside a rule.
            Pattern::compile(language, text)
                .map(|p| Some(Box::new(ROperand::Pattern(p.program))))
                .map_err(|e| PatternError {
                    message: format!("in rule operand: {}", e.message),
                    position: e.position,
                    suggestion: e.suggestion,
                })
        }
    }
}

/// Compile one `where` constraint.
fn compile_where(
    grammar: &tree_sitter::Language,
    constraint: &super::VarConstraint,
) -> Result<WhereC, PatternError> {
    let regex = match &constraint.regex {
        None => None,
        Some(pattern) => {
            if pattern.len() > REGEX_MAX_BYTES {
                return Err(PatternError {
                    message: format!(
                        "a `where` regex may be at most {REGEX_MAX_BYTES} bytes, this one is {}",
                        pattern.len()
                    ),
                    position: None,
                    suggestion: "Shorten the regex, or narrow the capture instead.".to_string(),
                });
            }
            // The linear-time engine, not a backtracking one: look-around and back-references
            // do not exist in it, so they are refused here instead of being run.
            Some(
                regex::RegexBuilder::new(pattern)
                    .size_limit(REGEX_SIZE_LIMIT)
                    .dfa_size_limit(REGEX_DFA_SIZE_LIMIT)
                    .build()
                    .map_err(|e| PatternError {
                        message: format!(
                            "`where` regex does not compile: {}",
                            short(&e.to_string())
                        ),
                        position: None,
                        suggestion:
                            "The engine is linear-time: no look-around and no back-references."
                                .to_string(),
                    })?,
            )
        }
    };
    let kind = match &constraint.kind {
        Some(name) => Some(lookup_kind(grammar, name)?),
        None => None,
    };
    Ok(WhereC { regex, kind })
}

/// The kind id of `name`: named first, then anonymous.
///
/// Both are tried because `id_for_node_kind` answers 0 for the wrong `named` flag, and a rule
/// should be able to say `{ kind: "+" }` as well as `{ kind: "call_expression" }`.
fn lookup_kind(grammar: &tree_sitter::Language, name: &str) -> Result<u16, PatternError> {
    let named = grammar.id_for_node_kind(name, true);
    if named != 0 {
        return Ok(named);
    }
    let anon = grammar.id_for_node_kind(name, false);
    if anon != 0 {
        return Ok(anon);
    }
    Err(PatternError {
        message: format!("{name} is not a node kind of this grammar"),
        position: None,
        suggestion: suggest_kinds(grammar, name),
    })
}

/// Up to five grammar node kinds within edit distance three of `name`.
fn suggest_kinds(grammar: &tree_sitter::Language, name: &str) -> String {
    let mut close: Vec<(usize, String)> = Vec::new();
    for id in 1..=grammar.node_kind_count() {
        let id = u16::try_from(id).unwrap_or(u16::MAX);
        let Some(kind) = grammar.node_kind_for_id(id) else {
            continue;
        };
        // Only names a caller could plausibly have meant: an anonymous token like `+` is not a
        // typo of `call_expression`.
        if !grammar.node_kind_is_named(id) || !grammar.node_kind_is_visible(id) {
            continue;
        }
        let distance = edit_distance(name, kind);
        if distance <= 3 {
            close.push((distance, kind.to_string()));
        }
    }
    // Closest first, and alphabetical within a distance, so the suggestion list does not
    // depend on the grammar's internal order.
    close.sort_by(|a, b| a.0.cmp(&b.0).then(a.1.cmp(&b.1)));
    close.truncate(5);
    if close.is_empty() {
        return "Check the grammar's node kind names (ast_explain_pattern lists them for this \
                language)."
            .to_string();
    }
    let names: Vec<&str> = close.iter().map(|(_, k)| k.as_str()).collect();
    format!("Did you mean: {}?", names.join(", "))
}

/// Levenshtein distance, early-exit once it passes `limit`.
///
/// Two rows rather than a matrix: the names are at most a few dozen bytes, and this runs once per
/// suggestion request over every kind in the grammar, so the allocation has to be small.
fn edit_distance(a: &str, b: &str) -> usize {
    const LIMIT: usize = 4;
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    if a.len().abs_diff(b.len()) >= LIMIT {
        return LIMIT;
    }
    let mut row: Vec<usize> = (0..=b.len()).collect();
    for (i, ca) in a.iter().enumerate() {
        let mut previous = row[0];
        row[0] = i + 1;
        let mut best = row[0];
        for (j, cb) in b.iter().enumerate() {
            let old = row[j + 1];
            let cost = if ca == cb { 0 } else { 1 };
            row[j + 1] = (row[j] + 1).min(row[j + 1] + 1).min(previous + cost);
            previous = old;
            best = best.min(row[j + 1]);
        }
        if best >= LIMIT {
            return LIMIT;
        }
    }
    row[b.len()]
}

/// Trim an engine message so it cannot become a wall of text in an error.
fn short(message: &str) -> String {
    let one_line = message.lines().next().unwrap_or(message).trim();
    if one_line.len() <= 120 {
        one_line.to_string()
    } else {
        format!("{}...", &one_line[..120])
    }
}

/// Does `node` (a match of the main pattern, with its `bindings`) satisfy the rule? Pattern
/// operands are matched with `matcher::match_at`; every step is counted in `steps`.
pub(crate) fn eval(
    rule: &RuleProgram,
    node: tree_sitter::Node<'_>,
    bindings: &Bindings,
    source: &str,
    steps: &mut Steps,
) -> Result<bool, ToolError> {
    eval_node(&rule.root, node, bindings, source, steps)
}

/// Evaluate one rule node. Every key must hold (AND); an empty `any` holds.
fn eval_node(
    rule: &RNode,
    node: tree_sitter::Node<'_>,
    bindings: &Bindings,
    source: &str,
    steps: &mut Steps,
) -> Result<bool, ToolError> {
    // One step per node visit, counted in the same budget as the matching itself: a rule is
    // evaluated per candidate match, so an expensive rule is an expensive search.
    steps.tick()?;

    if let Some(kind) = rule.kind
        && node.kind_id() != kind
    {
        return Ok(false);
    }
    if let Some(operand) = &rule.inside
        && !any_ancestor(operand, node, bindings, source, steps)?
    {
        return Ok(false);
    }
    if let Some(operand) = &rule.has
        && !any_descendant(operand, node, bindings, source, steps)?
    {
        return Ok(false);
    }
    if let Some(operand) = &rule.not
        && operand_holds(operand, node, bindings, source, steps)?
    {
        return Ok(false);
    }
    for sub in &rule.all {
        if !eval_node(sub, node, bindings, source, steps)? {
            return Ok(false);
        }
    }
    // An empty `any` is "no constraint", so it holds; otherwise one of them must.
    if !rule.any.is_empty() {
        let mut any_holds = false;
        for sub in &rule.any {
            if eval_node(sub, node, bindings, source, steps)? {
                any_holds = true;
                break;
            }
        }
        if !any_holds {
            return Ok(false);
        }
    }
    for (name, constraint) in &rule.wheres {
        if !where_holds(name, constraint, node, bindings, source, steps)? {
            return Ok(false);
        }
    }
    Ok(true)
}

/// Does the operand hold at `node`?
fn operand_holds(
    operand: &ROperand,
    node: tree_sitter::Node<'_>,
    bindings: &Bindings,
    source: &str,
    steps: &mut Steps,
) -> Result<bool, ToolError> {
    match operand {
        // A pattern operand only has to match there; its captures are discarded, because they
        // belong to a pattern the caller did not ask to bind.
        ROperand::Pattern(program) => {
            Ok(super::matcher::match_at(program, node, source, steps)?.is_some())
        }
        ROperand::Rule(rule) => eval_node(rule, node, bindings, source, steps),
    }
}

/// Does any PROPER ancestor satisfy the operand? The node itself is not its own ancestor, so
/// `{ not: { has: ... } }` cannot cancel itself out.
fn any_ancestor(
    operand: &ROperand,
    node: tree_sitter::Node<'_>,
    bindings: &Bindings,
    source: &str,
    steps: &mut Steps,
) -> Result<bool, ToolError> {
    let mut current = node.parent();
    while let Some(ancestor) = current {
        if operand_holds(operand, ancestor, bindings, source, steps)? {
            return Ok(true);
        }
        current = ancestor.parent();
    }
    Ok(false)
}

/// Does any PROPER descendant satisfy the operand? Iterative pre-order, so a deep file cannot
/// overflow the stack here.
fn any_descendant(
    operand: &ROperand,
    node: tree_sitter::Node<'_>,
    bindings: &Bindings,
    source: &str,
    steps: &mut Steps,
) -> Result<bool, ToolError> {
    let mut stack = children_of(node);
    stack.reverse();
    while let Some(child) = stack.pop() {
        if operand_holds(operand, child, bindings, source, steps)? {
            return Ok(true);
        }
        let mut pushed = children_of(child);
        pushed.reverse();
        stack.extend(pushed);
    }
    Ok(false)
}

/// The children of `node`, oldest first.
fn children_of<'a>(node: tree_sitter::Node<'a>) -> Vec<tree_sitter::Node<'a>> {
    let mut cursor = node.walk();
    node.children(&mut cursor).collect()
}

/// Does the capture satisfy one `where` constraint?
///
/// A name the main pattern never bound is unsatisfied, not an error: a rule is compiled on its
/// own, without the pattern, so at compile time it cannot know which variables exist. Treating
/// it as unsatisfied can only narrow the result, which is the direction the rules promise.
fn where_holds(
    name: &str,
    constraint: &WhereC,
    node: tree_sitter::Node<'_>,
    bindings: &Bindings,
    source: &str,
    steps: &mut Steps,
) -> Result<bool, ToolError> {
    let Some((kind, start, end)) = bindings
        .vars
        .iter()
        .find(|(n, ..)| n == name)
        .map(|(_, k, s, e)| (*k, *s, *e))
    else {
        return Ok(false);
    };

    if let Some(required) = constraint.kind {
        if kind == CaptureKind::List {
            // A list capture has no single kind: `$$$NAMES` is several nodes and any of them
            // could be the "right" one, so answering yes or no would be a guess. The rule is
            // refused instead. It cannot be caught at compile time, because the rule is compiled
            // without the pattern that would say whether the capture is a list.
            return Err(ToolError::new(
                ErrorCode::InvalidPattern,
                format!(
                    "a `kind` constraint applies to a single-node capture, but ${name} is a list"
                ),
                "Drop the `kind` constraint, or bind the node as `$NAME` in the pattern.",
            ));
        }
        let Some(captured) = node_for(node, start, end) else {
            return Ok(false);
        };
        if captured.kind_id() != required {
            return Ok(false);
        }
    }

    if let Some(re) = &constraint.regex {
        // The regex runs over the capture's whole text, so a list's separators are part of it.
        let Some(text) = source.get(start..end) else {
            return Ok(false);
        };
        // A capture larger than this is not worth a regex pass, and passing it would be the
        // one input a rule could use to burn the search budget.
        if text.len() > REGEX_MAX_INPUT_BYTES {
            return Ok(false);
        }
        steps.tick()?;
        if !re.is_match(text) {
            return Ok(false);
        }
    }
    Ok(true)
}

/// The node of `root`'s tree that spans exactly `start..end`, if there is one.
///
/// `descendant_for_byte_range` narrows to the smallest node covering the range, which for a
/// single-node capture is that node.
fn node_for<'a>(
    root: tree_sitter::Node<'a>,
    start: usize,
    end: usize,
) -> Option<tree_sitter::Node<'a>> {
    let mut node = root.descendant_for_byte_range(start, end)?;
    while node.start_byte() < start || node.end_byte() > end {
        node = node.parent()?;
    }
    Some(node)
}
