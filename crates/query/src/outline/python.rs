//! Python outline (docs/LANGUAGES.md "Outline queries").
//!
//! Node kinds were taken from a dump of `tree-sitter-python` 0.25 against
//! `tests/fixtures/sample.py` (and extras): `function_definition`, `class_definition`,
//! `decorated_definition` (field `definition`), `assignment` under `expression_statement`,
//! docstring = first `expression_statement`/`string` in `body`.

use super::{Symbol, SymbolKind};
use opencrayast_lang::{Language, ParsedFile};
use tree_sitter::Node;

/// All symbols of the file at every depth (any order; the caller sorts).
pub(crate) fn collect(parsed: &ParsedFile, source: &str) -> Vec<Symbol> {
    let root = parsed.tree.root_node();
    let mut out = Vec::new();
    walk_body(root, source, &[], 1, false, &mut out);
    out
}

/// Python docstrings live inside the body: never extend upward.
pub(crate) fn doc_start_line(_source: &str, symbol: &Symbol) -> usize {
    symbol.start_line
}

/// Walk a `module` / `block` / similar container of statements.
fn walk_body(
    parent: Node<'_>,
    source: &str,
    class_path: &[String],
    depth: usize,
    in_function: bool,
    out: &mut Vec<Symbol>,
) {
    let mut cursor = parent.walk();
    for child in parent.children(&mut cursor) {
        if skip_bad(child) {
            continue;
        }
        match child.kind() {
            "function_definition" => {
                if in_function {
                    // Nested function inside a function: not part of the outline.
                    continue;
                }
                push_function(child, child, source, class_path, depth, out);
            }
            "class_definition" => {
                if in_function {
                    continue;
                }
                push_class(child, child, source, class_path, depth, out);
            }
            "decorated_definition" => {
                if in_function {
                    continue;
                }
                let Some(inner) = child.child_by_field_name("definition") else {
                    continue;
                };
                if skip_bad(inner) {
                    continue;
                }
                match inner.kind() {
                    "function_definition" => {
                        push_function(child, inner, source, class_path, depth, out);
                    }
                    "class_definition" => {
                        push_class(child, inner, source, class_path, depth, out);
                    }
                    _ => {}
                }
            }
            "expression_statement" => {
                if in_function {
                    continue;
                }
                let mut stmt_walk = child.walk();
                if let Some(assign) = child
                    .named_children(&mut stmt_walk)
                    .find(|n| n.kind() == "assignment")
                {
                    push_assignment(assign, source, class_path, depth, out);
                }
            }
            _ => {}
        }
    }
}

fn push_function(
    extent: Node<'_>,
    def: Node<'_>,
    source: &str,
    class_path: &[String],
    depth: usize,
    out: &mut Vec<Symbol>,
) {
    let Some(name) = field_text(def, "name", source) else {
        return;
    };
    let Some(body) = def.child_by_field_name("body") else {
        return;
    };
    let (kind, qualified) = if class_path.is_empty() {
        (SymbolKind::Fn, name.clone())
    } else {
        (
            SymbolKind::Method,
            format!("{}.{}", class_path.join("."), name),
        )
    };
    if let Some(sym) = make_symbol(
        extent,
        kind,
        name,
        qualified,
        depth,
        signature_before_body(def, body, source),
        docstring_first_line(body, source),
    ) {
        out.push(sym);
    }
}

fn push_class(
    extent: Node<'_>,
    def: Node<'_>,
    source: &str,
    class_path: &[String],
    depth: usize,
    out: &mut Vec<Symbol>,
) {
    let Some(name) = field_text(def, "name", source) else {
        return;
    };
    let Some(body) = def.child_by_field_name("body") else {
        return;
    };
    let qualified = if class_path.is_empty() {
        name.clone()
    } else {
        format!("{}.{}", class_path.join("."), name)
    };
    if let Some(sym) = make_symbol(
        extent,
        SymbolKind::Class,
        name.clone(),
        qualified,
        depth,
        signature_before_body(def, body, source),
        docstring_first_line(body, source),
    ) {
        out.push(sym);
    }
    let mut path = class_path.to_vec();
    path.push(name);
    walk_body(body, source, &path, depth + 1, false, out);
}

fn push_assignment(
    assign: Node<'_>,
    source: &str,
    class_path: &[String],
    depth: usize,
    out: &mut Vec<Symbol>,
) {
    // Only simple `NAME = ...` whose left is a real (non-ERROR/MISSING) identifier.
    // Broken trees often invent a MISSING `identifier` with empty text — skip those.
    let Some(left) = assign.child_by_field_name("left") else {
        return;
    };
    if skip_bad(left) || left.kind() != "identifier" {
        return;
    }
    let name = node_text(left, source);
    if !usable_name(&name) {
        return;
    }
    // Judgment (ISSUE supplemental): `f = lambda ...` is still an assignment → Variable/Const,
    // never Fn. Only `function_definition` nodes become Fn/Method.
    let kind = if is_const_name(&name) {
        SymbolKind::Const
    } else {
        SymbolKind::Variable
    };
    let qualified = if class_path.is_empty() {
        name.clone()
    } else {
        format!("{}.{}", class_path.join("."), name)
    };
    let first_line = node_text(assign, source)
        .lines()
        .next()
        .unwrap_or("")
        .to_string();
    let sig = collapse_ws(&first_line);
    if let Some(sym) = make_symbol(assign, kind, name, qualified, depth, sig, None) {
        out.push(sym);
    }
}

fn docstring_first_line(body: Node<'_>, source: &str) -> Option<String> {
    let mut cursor = body.walk();
    let first_stmt = body.named_children(&mut cursor).next()?;
    if skip_bad(first_stmt) {
        return None;
    }
    let string_node = match first_stmt.kind() {
        "expression_statement" => {
            let mut w = first_stmt.walk();
            first_stmt
                .named_children(&mut w)
                .find(|n| n.kind() == "string")?
        }
        "string" => first_stmt,
        _ => return None,
    };
    // Prefer `string_content` children (handles triples); fall back to stripping quotes.
    let mut content = String::new();
    let mut c = string_node.walk();
    for ch in string_node.named_children(&mut c) {
        if ch.kind() == "string_content" {
            content.push_str(&node_text(ch, source));
        }
    }
    if content.is_empty() {
        content = strip_py_string_quotes(&node_text(string_node, source));
    }
    let line = content.lines().map(str::trim).find(|l| !l.is_empty())?;
    Some(line.to_string())
}

fn strip_py_string_quotes(raw: &str) -> String {
    let s = raw.trim();
    for q in ["\"\"\"", "'''", "\"", "'"] {
        if let Some(inner) = s.strip_prefix(q).and_then(|r| r.strip_suffix(q)) {
            return inner.to_string();
        }
    }
    s.to_string()
}

fn signature_before_body(def: Node<'_>, body: Node<'_>, source: &str) -> String {
    let raw = &source[def.start_byte()..body.start_byte()];
    let collapsed = collapse_ws(raw);
    collapsed.trim_end_matches(':').trim().to_string()
}

fn is_const_name(name: &str) -> bool {
    let mut has_letter = false;
    for c in name.chars() {
        if c.is_ascii_uppercase() {
            has_letter = true;
        } else if c == '_' || c.is_ascii_digit() {
            // ok
        } else {
            return false;
        }
    }
    has_letter
}

fn make_symbol(
    extent: Node<'_>,
    kind: SymbolKind,
    name: String,
    qualified: String,
    depth: usize,
    signature: String,
    doc_first_line: Option<String>,
) -> Option<Symbol> {
    // Never emit empty / whitespace-only names (ERROR/MISSING recovery artefacts).
    if !usable_name(&name) {
        return None;
    }
    Some(Symbol {
        language: Language::Python,
        kind,
        name,
        qualified,
        depth,
        start_line: extent.start_position().row + 1,
        end_line: extent.end_position().row + 1,
        start_byte: extent.start_byte(),
        end_byte: extent.end_byte(),
        signature,
        doc_first_line,
    })
}

fn usable_name(name: &str) -> bool {
    !name.trim().is_empty()
}

fn field_text(node: Node<'_>, field: &str, source: &str) -> Option<String> {
    let n = node.child_by_field_name(field)?;
    if skip_bad(n) {
        return None;
    }
    let text = node_text(n, source);
    usable_name(&text).then_some(text)
}

fn node_text(node: Node<'_>, source: &str) -> String {
    source[node.start_byte()..node.end_byte()].to_string()
}

fn collapse_ws(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut pending_space = false;
    for c in s.chars() {
        if c.is_whitespace() {
            pending_space = true;
        } else {
            if pending_space && !out.is_empty() {
                out.push(' ');
            }
            pending_space = false;
            out.push(c);
        }
    }
    out
}

fn skip_bad(node: Node<'_>) -> bool {
    node.is_error() || node.is_missing() || node.kind() == "ERROR" || node.kind() == "MISSING"
}
