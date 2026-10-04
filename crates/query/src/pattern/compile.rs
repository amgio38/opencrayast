//! Pattern compilation. See the module documentation of `pattern`.
//!
//! The data structure below is the contract between compilation and the matcher: compilation
//! produces it, the matcher consumes it. It is plain data, so the matcher can be written and
//! tested against hand-built values before compilation exists.

use super::{CaptureKind, MetaVar, Pattern, PatternError};
use opencrayast_lang::{Language, ParseBudget, parse};
use std::time::Duration;
use tree_sitter::Node;

/// A node of a compiled pattern. Comments (`is_extra` nodes) are already removed from every
/// `children` list; field names are not kept.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum PNode {
    /// A metavariable. `name` is `None` for the anonymous forms `$_` and `$$$`. `list` is true for
    /// `$$$NAME` / `$$$` (zero or more siblings), false for `$NAME` / `$_` (exactly one named node).
    Meta {
        /// Variable name without `$` signs, or `None` when anonymous.
        name: Option<String>,
        /// A list variable.
        list: bool,
    },
    /// A node without children: matches a source leaf with the same kind id AND the same text.
    Leaf {
        /// tree-sitter kind id (valid for the pattern's own grammar, which is also the source's).
        kind_id: u16,
        /// Grammar kind name (for `explain`).
        kind: &'static str,
        /// Whether the node is named (anonymous tokens are not).
        named: bool,
        /// The leaf text.
        text: String,
    },
    /// A node with children: same kind id, and the children match as sequences.
    Interior {
        /// tree-sitter kind id.
        kind_id: u16,
        /// Grammar kind name.
        kind: &'static str,
        /// Whether the node is named.
        named: bool,
        /// Children in order, comments removed.
        children: Vec<PNode>,
    },
}

/// Compiler output: the pattern tree plus what `explain` and `metavars` need.
#[derive(Debug, Clone)]
pub(crate) struct Program {
    /// The root. A root that is a `Meta` matches any named node.
    pub(crate) root: PNode,
    /// Named metavariables in order of first appearance.
    pub(crate) vars: Vec<MetaVar>,
    /// Set when the pattern only parsed inside a context (names it); shown by `explain`.
    pub(crate) warning: Option<String>,
}

const MICRO: char = '\u{00B5}'; // µ — identifier letter in all five languages
const MAX_PATTERN_BYTES: usize = 16 * 1024;
const MAX_NAMED_VARS: usize = 64;
const MAX_TREE_DEPTH: usize = 64;

fn pattern_budget() -> ParseBudget {
    ParseBudget {
        // Pattern ≤16 KiB plus a small context wrapper.
        max_bytes: (MAX_PATTERN_BYTES + 256) as u64,
        timeout: Duration::from_secs(1),
        max_depth: 256,
        max_nodes: 100_000,
    }
}

pub(super) fn compile(language: Language, source: &str) -> Result<Pattern, PatternError> {
    if language.grammar().is_none() {
        return Err(PatternError {
            message: format!("no grammar for {} in this build", language.id()),
            position: None,
            suggestion: "rebuild with this language enabled, or pick a supported language".into(),
        });
    }
    if source.len() > MAX_PATTERN_BYTES {
        return Err(PatternError {
            message: format!(
                "pattern is {} bytes, over the {} byte limit",
                source.len(),
                MAX_PATTERN_BYTES
            ),
            position: None,
            suggestion: "shorten the pattern".into(),
        });
    }
    let core = source.trim();
    if core.is_empty() {
        return Err(PatternError {
            message: "pattern is empty".into(),
            position: None,
            suggestion: "give a non-empty pattern in the target language".into(),
        });
    }

    let (rewritten, pos_map) = rewrite_metavars(core);
    // `pattern_start` is where the pattern's own text begins inside the wrapped source, so the
    // context can be asked whether it produced a DECLARATION for exactly the pattern text.
    let contexts = contexts_for(language);
    let budget = pattern_budget();

    let mut best_fail: Option<FailCtx> = None;
    let mut chosen: Option<ChosenCtx> = None;

    for ctx in &contexts {
        let (ctx_name, prefix, suffix) = (&ctx.name, &ctx.prefix, &ctx.suffix);
        let wrapped = format!("{prefix}{rewritten}{suffix}");
        let parsed = match parse(language, &wrapped, &budget) {
            Ok(p) => p,
            Err(e) => {
                // Treat a refused parse as worse than a tree with errors: keep going.
                let _ = e;
                continue;
            }
        };
        // Bare-metavar ERROR nodes (text is exactly one µ-token) do not invalidate a context
        // (mod.rs §4); other ERROR / MISSING still do.
        let (err_count, problem_byte) = scan_context_errors(&parsed.tree, &wrapped);
        if err_count == 0
            && ctx.declaration_only
            && !declares_pattern(&parsed.tree, prefix.as_str(), rewritten.as_str())
        {
            // A declaration-only context that read the pattern as an EXPRESSION is not a good
            // choice: an expression also parses at Go's top level (as a type conversion), so
            // keeping it would silently mistype expression patterns. Skip to the next context.
            continue;
        }
        if err_count == 0 {
            chosen = Some(ChosenCtx {
                name: ctx_name,
                prefix_len: prefix.len(),
                rewritten_len: rewritten.len(),
                wrapped,
                tree: parsed.tree,
                used_context: !prefix.is_empty() || !suffix.is_empty(),
            });
            break;
        }
        let mapped = map_wrapped_to_original(problem_byte, prefix.len(), rewritten.len(), &pos_map);
        let fail = FailCtx {
            err_count,
            position: mapped,
        };
        match &best_fail {
            None => best_fail = Some(fail),
            Some(prev) if fail.err_count < prev.err_count => best_fail = Some(fail),
            Some(prev) if fail.err_count == prev.err_count && fail.position < prev.position => {
                best_fail = Some(fail);
            }
            _ => {}
        }
    }

    let chosen = match chosen {
        Some(c) => c,
        None => {
            let pos = best_fail.and_then(|f| f.position);
            return Err(PatternError {
                message: "the pattern does not parse in any context".into(),
                position: pos,
                suggestion: "fix the syntax, or wrap the pattern in a complete statement or block"
                    .into(),
            });
        }
    };

    let pat_start = chosen.prefix_len;
    let pat_end = chosen.prefix_len + chosen.rewritten_len;
    let root_ts =
        find_exact_root(chosen.tree.root_node(), pat_start, pat_end).ok_or_else(|| {
            PatternError {
                message: "Multiple AST nodes are detected in the pattern".into(),
                position: Some(0),
                suggestion: "wrap it in a block, or give a complete statement".into(),
            }
        })?;

    // A file-level or statement-list node that holds several top-level statements is not a
    // single pattern root (PATTERNS.md: one root node). Go's `statement_list` covers `a; b;`
    // inside a function body without braces.
    if is_multi_statement_container(root_ts.kind()) {
        let mut named = 0usize;
        let mut c = root_ts.walk();
        for ch in root_ts.children(&mut c) {
            if ch.is_extra() {
                continue;
            }
            if ch.is_named() {
                named += 1;
            }
        }
        if named != 1 {
            return Err(PatternError {
                message: "Multiple AST nodes are detected in the pattern".into(),
                position: Some(0),
                suggestion: "wrap it in a block, or give a complete statement".into(),
            });
        }
    }

    let mut vars = Vec::new();
    let mut one_names = std::collections::HashSet::new();
    let mut list_names = std::collections::HashSet::new();
    let grammar = language.grammar().ok_or_else(|| PatternError {
        message: format!("no grammar for {} in this build", language.id()),
        position: None,
        suggestion: "rebuild with this language enabled, or pick a supported language".into(),
    })?;
    let root = build_pnode(
        root_ts,
        &chosen.wrapped,
        &grammar,
        1,
        &mut vars,
        &mut one_names,
        &mut list_names,
    )?;

    if matches!(&root, PNode::Meta { list: true, .. }) {
        return Err(PatternError {
            message: "a list metavariable cannot be the pattern root".into(),
            position: Some(0),
            suggestion: "wrap $$$NAME in a larger pattern such as a call or a block".into(),
        });
    }

    let warning = if chosen.used_context {
        Some(format!(
            "warning: pattern only parses inside a {} context",
            chosen.name
        ))
    } else {
        None
    };

    Ok(Pattern {
        language,
        source: source.to_string(),
        program: Program {
            root,
            vars,
            warning,
        },
    })
}

struct ChosenCtx {
    name: &'static str,
    prefix_len: usize,
    rewritten_len: usize,
    wrapped: String,
    tree: tree_sitter::Tree,
    used_context: bool,
}

struct FailCtx {
    err_count: usize,
    position: Option<usize>,
}

/// One candidate parse context.
struct Context {
    /// Name shown by `explain` when this context is the one used.
    name: &'static str,
    /// Text wrapped before the pattern.
    prefix: String,
    /// Text wrapped after the pattern.
    suffix: String,
    /// True when this context is only suitable if the pattern's own text parses here as a
    /// DECLARATION. Used for Go's first context: at Go's top level an expression parses too (a type
    /// conversion), so a top-level parse is only trusted when it is a declaration.
    declaration_only: bool,
}

/// Whether `tree` parses exactly the pattern text as a DECLARATION (Go/Rust-style declarations that
/// only live at the top level: functions, methods, types, constants, variables).
fn declares_pattern(tree: &tree_sitter::Tree, prefix: &str, rewritten: &str) -> bool {
    let start = prefix.len();
    let end = start + rewritten.len();
    match find_exact_root(tree.root_node(), start, end) {
        Some(node) => is_declaration_kind(node.kind()),
        // No node covers the pattern text exactly: not a clean declaration, so let the caller fall
        // through to a context that suits it.
        None => false,
    }
}

/// Whether `kind` names a top-level declaration in Go or Rust.
fn is_declaration_kind(kind: &str) -> bool {
    matches!(
        kind,
        "function_declaration"
            | "method_declaration"
            | "type_declaration"
            | "const_declaration"
            | "var_declaration"
            | "item_declaration"
            | "mod_item"
            | "macro_definition"
    )
}

fn contexts_for(language: Language) -> Vec<Context> {
    let c = |name, prefix: String, suffix: String, declaration_only| Context {
        name,
        prefix,
        suffix,
        declaration_only,
    };
    match language {
        Language::Rust => vec![
            c("function body", "fn _() {\n".into(), "\n}".into(), false),
            c("top level", String::new(), String::new(), false),
        ],
        // Go: DECLARATIONS first at the top level. `func f(a int) {}` is a `function_declaration`
        // there but a `func_literal` inside a body, so the body context would never match real code;
        // it is tried second. An EXPRESSION also parses at Go's top level (a type conversion), so
        // the first context only accepts a top-level parse that is a declaration, and expressions
        // fall through to the body context - which is what that context is for.
        Language::Go => vec![
            c("top level", "package p\n".into(), String::new(), true),
            c(
                "function body",
                "package p\nfunc _() {\n".into(),
                "\n}".into(),
                false,
            ),
            c("top level", "package p\n".into(), String::new(), false),
        ],
        Language::TypeScript | Language::Tsx | Language::JavaScript => vec![
            c("top level", String::new(), String::new(), false),
            c("class body", "class _ {\n".into(), "\n}".into(), false),
        ],
        Language::Python => vec![c("top level", String::new(), String::new(), false)],
    }
}

/// Rewrite metavariables; `pos_map[i]` is the original byte offset of rewritten byte `i`.
/// `pos_map` has length `rewritten.len() + 1` (sentinel at the end = `src.len()`).
fn rewrite_metavars(src: &str) -> (String, Vec<usize>) {
    let bytes = src.as_bytes();
    let mut out = String::new();
    let mut map = Vec::with_capacity(src.len() + 1);
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'$' {
            // $$$NAME or $$$
            if i + 2 < bytes.len() && bytes[i + 1] == b'$' && bytes[i + 2] == b'$' {
                let after = i + 3;
                if let Some((name, name_end)) = scan_meta_name(bytes, after) {
                    let start = i;
                    push_micros(&mut out, &mut map, start, 3);
                    for (k, b) in name.as_bytes().iter().enumerate() {
                        out.push(*b as char);
                        map.push(after + k);
                    }
                    i = name_end;
                    continue;
                }
                // bare $$$
                push_micros(&mut out, &mut map, i, 3);
                i += 3;
                continue;
            }
            // $$ → literal $
            if i + 1 < bytes.len() && bytes[i + 1] == b'$' {
                out.push('$');
                map.push(i);
                i += 2;
                continue;
            }
            // $_
            if i + 1 < bytes.len() && bytes[i + 1] == b'_' {
                push_micros(&mut out, &mut map, i, 1);
                out.push('_');
                map.push(i + 1);
                i += 2;
                continue;
            }
            // $NAME
            if let Some((name, name_end)) = scan_meta_name(bytes, i + 1) {
                push_micros(&mut out, &mut map, i, 1);
                for (k, b) in name.as_bytes().iter().enumerate() {
                    out.push(*b as char);
                    map.push(i + 1 + k);
                }
                i = name_end;
                continue;
            }
            // lone `$` or `$` + non-name: keep as code
            out.push('$');
            map.push(i);
            i += 1;
            continue;
        }
        // copy next UTF-8 character
        let ch = next_char(bytes, i);
        let start = i;
        let len = ch.len_utf8();
        out.push(ch);
        for _ in 0..len {
            map.push(start);
        }
        // For multi-byte chars we pushed `len` map entries but only one char to out —
        // wrong: out.push adds utf8 bytes. Fix: map one entry per output byte.
        // Actually String::push encodes the char; we need map.len() == out.len() during build.
        // Correct approach: after push, pad map to out.len().
        while map.len() < out.len() {
            map.push(start);
        }
        i += len;
    }
    debug_assert_eq!(map.len(), out.len());
    map.push(src.len());
    (out, map)
}

fn push_micros(out: &mut String, map: &mut Vec<usize>, orig: usize, count: usize) {
    for _ in 0..count {
        let before = out.len();
        out.push(MICRO);
        while map.len() < out.len() {
            map.push(orig);
        }
        let _ = before;
    }
}

fn scan_meta_name(bytes: &[u8], start: usize) -> Option<(String, usize)> {
    if start >= bytes.len() {
        return None;
    }
    let b0 = bytes[start];
    if !(b0.is_ascii_uppercase()) {
        return None;
    }
    let mut end = start + 1;
    while end < bytes.len() {
        let b = bytes[end];
        if b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'_' {
            end += 1;
        } else {
            break;
        }
    }
    let name = std::str::from_utf8(&bytes[start..end]).ok()?.to_string();
    Some((name, end))
}

fn next_char(bytes: &[u8], i: usize) -> char {
    let s = std::str::from_utf8(&bytes[i..]).unwrap_or("");
    s.chars().next().unwrap_or('\u{FFFD}')
}

fn map_wrapped_to_original(
    wrapped_byte: usize,
    prefix_len: usize,
    rewritten_len: usize,
    pos_map: &[usize],
) -> Option<usize> {
    if wrapped_byte < prefix_len {
        return Some(0);
    }
    let rel = wrapped_byte - prefix_len;
    if rel > rewritten_len {
        // In the suffix: point at end of pattern.
        return pos_map.last().copied();
    }
    Some(pos_map[rel.min(pos_map.len().saturating_sub(1))])
}

/// Count real ERROR/MISSING nodes and pick the problem byte (mod.rs §4–5).
///
/// - An `ERROR` whose full text is exactly one metavariable token is accepted (skipped).
/// - Position preference: first `MISSING` in document order; else the innermost real `ERROR`.
///   When that ERROR has children, point at its last child's start — a lone wrapper ERROR that
///   spans the whole pattern would otherwise always report byte 0.
fn scan_context_errors(tree: &tree_sitter::Tree, source: &str) -> (usize, usize) {
    let root = tree.root_node();
    let mut count = 0usize;
    let mut first_missing: Option<usize> = None;
    let mut innermost_error: Option<(usize, Node<'_>)> = None; // (depth, node)
    let mut stack: Vec<(Node<'_>, usize)> = vec![(root, 1)];
    while let Some((n, depth)) = stack.pop() {
        let text = &source[n.start_byte()..n.end_byte()];
        if n.is_error() && meta_token(text).is_some() {
            // Bare-metavar ERROR: treat the whole subtree as the metavariable.
            continue;
        }
        if n.is_missing() {
            count += 1;
            if first_missing.is_none() {
                first_missing = Some(n.start_byte());
            }
        } else if n.is_error() {
            count += 1;
            match innermost_error {
                Some((d, _)) if d >= depth => {}
                _ => innermost_error = Some((depth, n)),
            }
        }
        let mut c = n.walk();
        let children: Vec<_> = n.children(&mut c).collect();
        for ch in children.into_iter().rev() {
            stack.push((ch, depth + 1));
        }
    }
    let problem = if let Some(b) = first_missing {
        b
    } else if let Some((_, err)) = innermost_error {
        error_problem_byte(err)
    } else {
        0
    };
    (count, problem)
}

/// Byte offset that best locates a syntax problem inside an ERROR node.
fn error_problem_byte(err: Node<'_>) -> usize {
    let mut c = err.walk();
    let children: Vec<_> = err.children(&mut c).collect();
    if let Some(last) = children.last() {
        last.start_byte()
    } else {
        err.start_byte()
    }
}

fn is_multi_statement_container(kind: &str) -> bool {
    matches!(
        kind,
        "program" | "source_file" | "module" | "statement_list"
    )
}

/// Innermost named node whose byte range is exactly `[start, end)`.
fn find_exact_root<'a>(root: Node<'a>, start: usize, end: usize) -> Option<Node<'a>> {
    let mut best: Option<(usize, Node<'a>)> = None;
    let mut stack: Vec<(Node<'a>, usize)> = vec![(root, 1)];
    while let Some((n, depth)) = stack.pop() {
        if n.is_named() && n.start_byte() == start && n.end_byte() == end {
            match best {
                Some((bd, _)) if bd >= depth => {}
                _ => best = Some((depth, n)),
            }
        }
        if n.start_byte() <= start && n.end_byte() >= end {
            let mut c = n.walk();
            let children: Vec<_> = n.children(&mut c).collect();
            for ch in children.into_iter().rev() {
                stack.push((ch, depth + 1));
            }
        }
    }
    best.map(|(_, n)| n)
}

fn build_pnode(
    node: Node<'_>,
    source: &str,
    grammar: &tree_sitter::Language,
    depth: usize,
    vars: &mut Vec<MetaVar>,
    one_names: &mut std::collections::HashSet<String>,
    list_names: &mut std::collections::HashSet<String>,
) -> Result<PNode, PatternError> {
    if depth > MAX_TREE_DEPTH {
        return Err(PatternError {
            message: format!("pattern tree depth exceeds {MAX_TREE_DEPTH}"),
            position: Some(0),
            suggestion: "simplify the pattern".into(),
        });
    }
    let text = &source[node.start_byte()..node.end_byte()];
    if let Some((name, list)) = meta_token(text) {
        record_var(name.clone(), list, vars, one_names, list_names)?;
        return Ok(PNode::Meta { name, list });
    }

    let mut child_nodes = Vec::new();
    let mut c = node.walk();
    for ch in node.children(&mut c) {
        // Comments are `is_extra`. tree-sitter also marks ERROR nodes as extra; those must stay
        // so a bare-metavar ERROR can become `PNode::Meta` (mod.rs §4).
        if ch.is_extra() && !ch.is_error() {
            continue;
        }
        child_nodes.push(ch);
    }

    let kind_id = node.kind_id();
    let kind = kind_name_static(grammar, kind_id);
    let named = node.is_named();

    if child_nodes.is_empty() {
        return Ok(PNode::Leaf {
            kind_id,
            kind,
            named,
            text: text.to_string(),
        });
    }

    let mut children = Vec::with_capacity(child_nodes.len());
    for ch in child_nodes {
        children.push(build_pnode(
            ch,
            source,
            grammar,
            depth + 1,
            vars,
            one_names,
            list_names,
        )?);
    }
    Ok(PNode::Interior {
        kind_id,
        kind,
        named,
        children,
    })
}

fn meta_token(text: &str) -> Option<(Option<String>, bool)> {
    // µµµ / µµµNAME
    let micro = MICRO.to_string();
    let triple = format!("{micro}{micro}{micro}");
    if text == triple.as_str() {
        return Some((None, true));
    }
    if let Some(rest) = text.strip_prefix(&triple)
        && is_meta_name(rest)
    {
        return Some((Some(rest.to_string()), true));
    }
    // µ_ / µNAME
    let single = micro;
    if text == format!("{single}_") {
        return Some((None, false));
    }
    if let Some(rest) = text.strip_prefix(&single)
        && is_meta_name(rest)
    {
        return Some((Some(rest.to_string()), false));
    }
    None
}

fn is_meta_name(s: &str) -> bool {
    let mut chars = s.chars();
    match chars.next() {
        Some(c) if c.is_ascii_uppercase() => {}
        _ => return false,
    }
    chars.all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_')
}

fn record_var(
    name: Option<String>,
    list: bool,
    vars: &mut Vec<MetaVar>,
    one_names: &mut std::collections::HashSet<String>,
    list_names: &mut std::collections::HashSet<String>,
) -> Result<(), PatternError> {
    let Some(n) = name else {
        return Ok(()); // anonymous
    };
    if list {
        if one_names.contains(&n) {
            return Err(one_list_conflict(&n));
        }
        if list_names.insert(n.clone()) && !vars.iter().any(|v| v.name == n) {
            if vars.len() >= MAX_NAMED_VARS {
                return Err(too_many_vars());
            }
            vars.push(MetaVar {
                name: n,
                kind: CaptureKind::List,
            });
        }
    } else {
        if list_names.contains(&n) {
            return Err(one_list_conflict(&n));
        }
        if one_names.insert(n.clone()) && !vars.iter().any(|v| v.name == n) {
            if vars.len() >= MAX_NAMED_VARS {
                return Err(too_many_vars());
            }
            vars.push(MetaVar {
                name: n,
                kind: CaptureKind::One,
            });
        }
    }
    Ok(())
}

fn one_list_conflict(n: &str) -> PatternError {
    PatternError {
        message: format!("metavariable `{n}` is used as both a single capture and a list capture"),
        position: Some(0),
        suggestion: "use different names for $NAME and $$$NAME".into(),
    }
}

fn too_many_vars() -> PatternError {
    PatternError {
        message: format!("pattern has more than {MAX_NAMED_VARS} named metavariables"),
        position: None,
        suggestion: "use fewer named captures, or reuse the same name".into(),
    }
}

/// Intern grammar kind names so `PNode` can hold `&'static str` without `unsafe`.
fn kind_name_static(grammar: &tree_sitter::Language, id: u16) -> &'static str {
    let s = grammar.node_kind_for_id(id).unwrap_or("ERROR");
    intern_str(s)
}

fn intern_str(s: &str) -> &'static str {
    use std::collections::HashMap;
    use std::sync::{Mutex, OnceLock};
    static MAP: OnceLock<Mutex<HashMap<String, &'static str>>> = OnceLock::new();
    let map = MAP.get_or_init(|| Mutex::new(HashMap::new()));
    let mut guard = map.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(v) = guard.get(s) {
        return v;
    }
    let leaked: &'static str = Box::leak(s.to_string().into_boxed_str());
    guard.insert(s.to_string(), leaked);
    leaked
}

pub(super) fn metavars(program: &Program) -> Vec<MetaVar> {
    program.vars.clone()
}

pub(super) fn explain(program: &Program) -> String {
    let mut out = String::new();
    if let Some(w) = &program.warning {
        out.push_str(w);
        if !w.ends_with('\n') {
            out.push('\n');
        }
    }
    explain_node(&program.root, 0, &mut out);
    out
}

fn explain_node(node: &PNode, depth: usize, out: &mut String) {
    let indent = "  ".repeat(depth);
    match node {
        PNode::Meta { name, list } => {
            let label = match (name.as_deref(), *list) {
                (Some(n), false) => format!("${n} (one)"),
                (Some(n), true) => format!("$$${n} (list)"),
                (None, false) => "$_ (one)".into(),
                (None, true) => "$$$ (list)".into(),
            };
            out.push_str(&indent);
            out.push_str(&label);
            out.push('\n');
        }
        PNode::Leaf {
            kind, named, text, ..
        } => {
            out.push_str(&indent);
            if *named {
                out.push_str(kind);
                out.push_str(" \"");
                out.push_str(text);
                out.push('"');
            } else {
                out.push('"');
                out.push_str(text);
                out.push('"');
            }
            out.push('\n');
        }
        PNode::Interior {
            kind,
            named,
            children,
            ..
        } => {
            out.push_str(&indent);
            if *named {
                out.push_str(kind);
            } else {
                // Anonymous interior is unusual; still print kind for explainability.
                out.push_str(kind);
            }
            out.push('\n');
            for ch in children {
                explain_node(ch, depth + 1, out);
            }
        }
    }
}
