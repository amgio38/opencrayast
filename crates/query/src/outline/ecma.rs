//! TypeScript / TSX / JavaScript outline (docs/LANGUAGES.md "Outline queries").
//! ECMA outline (JavaScript, TypeScript, TSX).
//!
//! Node kinds from dumps of `tree-sitter-typescript` (TS + TSX grammars) and
//! `tree-sitter-javascript` against `tests/fixtures/sample.{ts,js}` and extras:
//! `export_statement` (field `declaration`, optional `decorator`), `class_declaration`,
//! `abstract_class_declaration`, `interface_declaration` / `method_signature`,
//! `enum_declaration`, `type_alias_declaration`, `function_declaration`,
//! `method_definition`, `lexical_declaration`/`variable_declarator`,
//! `internal_module` (namespace), `ambient_declaration` / `function_signature`.

use super::{Symbol, SymbolKind};
use opencrayast_lang::{Language, ParsedFile};
use tree_sitter::Node;

/// All symbols of the file at every depth (any order; the caller sorts).
pub(crate) fn collect(parsed: &ParsedFile, source: &str) -> Vec<Symbol> {
    let mut out = Vec::new();
    walk(
        parsed.tree.root_node(),
        source,
        parsed.language,
        &[],
        1,
        false,
        &mut out,
    );
    out
}

/// First line of the contiguous doc block immediately above `symbol.start_line`.
pub(crate) fn doc_start_line(source: &str, symbol: &Symbol) -> usize {
    leading_doc(source, symbol.start_line)
        .map(|(line, _)| line)
        .unwrap_or(symbol.start_line)
}

fn walk(
    parent: Node<'_>,
    source: &str,
    language: Language,
    owner_path: &[String],
    depth: usize,
    in_function: bool,
    out: &mut Vec<Symbol>,
) {
    let mut cursor = parent.walk();
    let children: Vec<Node<'_>> = parent.children(&mut cursor).collect();
    let mut i = 0;
    while i < children.len() {
        let child = children[i];
        if skip_bad(child) {
            i += 1;
            continue;
        }
        match child.kind() {
            "export_statement" => {
                handle_export(child, source, language, owner_path, depth, in_function, out);
            }
            "expression_statement" if !in_function => {
                if let Some(ns) = first_named_of_kinds(child, &["internal_module", "module"]) {
                    push_namespace(child, ns, source, language, owner_path, depth, out);
                }
            }
            "internal_module" | "module" if !in_function => {
                push_namespace(child, child, source, language, owner_path, depth, out);
            }
            "ambient_declaration" if !in_function => {
                handle_ambient(child, source, language, owner_path, depth, out);
            }
            "class_declaration" | "abstract_class_declaration" if !in_function => {
                push_class(child, child, source, language, owner_path, depth, out);
            }
            "interface_declaration" if !in_function => {
                push_interface(child, child, source, language, owner_path, depth, out);
            }
            "enum_declaration" if !in_function => {
                push_enum(child, child, source, language, owner_path, depth, out);
            }
            "type_alias_declaration" if !in_function => {
                push_type_alias(child, child, source, language, owner_path, depth, out);
            }
            "function_declaration" | "generator_function_declaration" if !in_function => {
                push_function(child, child, source, language, owner_path, depth, out);
            }
            "lexical_declaration" if !in_function => {
                push_lexical(child, child, source, language, owner_path, depth, out);
            }
            "class_body" => {
                walk_class_body(child, source, language, owner_path, depth, out);
            }
            "interface_body" => {
                walk_interface_body(child, source, language, owner_path, depth, out);
            }
            "statement_block" | "program" => {
                walk(child, source, language, owner_path, depth, in_function, out);
            }
            _ => {}
        }
        i += 1;
    }
}

fn handle_export(
    export: Node<'_>,
    source: &str,
    language: Language,
    owner_path: &[String],
    depth: usize,
    in_function: bool,
    out: &mut Vec<Symbol>,
) {
    if in_function {
        return;
    }
    let Some(decl) = export.child_by_field_name("declaration") else {
        return;
    };
    if skip_bad(decl) {
        return;
    }
    // Extent is the whole export_statement (includes `export` / `default` / leading decorators).
    match decl.kind() {
        "class_declaration" | "abstract_class_declaration" => {
            push_class(export, decl, source, language, owner_path, depth, out);
        }
        "interface_declaration" => {
            push_interface(export, decl, source, language, owner_path, depth, out);
        }
        "enum_declaration" => {
            push_enum(export, decl, source, language, owner_path, depth, out);
        }
        "type_alias_declaration" => {
            push_type_alias(export, decl, source, language, owner_path, depth, out);
        }
        "function_declaration" | "generator_function_declaration" => {
            push_function(export, decl, source, language, owner_path, depth, out);
        }
        "lexical_declaration" => {
            push_lexical(export, decl, source, language, owner_path, depth, out);
        }
        "internal_module" | "module" => {
            push_namespace(export, decl, source, language, owner_path, depth, out);
        }
        "ambient_declaration" => {
            // `export declare ...` — keep the export_statement as the outer extent.
            handle_ambient_with_extent(export, decl, source, language, owner_path, depth, out);
        }
        _ => {}
    }
}

fn handle_ambient(
    ambient: Node<'_>,
    source: &str,
    language: Language,
    owner_path: &[String],
    depth: usize,
    out: &mut Vec<Symbol>,
) {
    handle_ambient_with_extent(ambient, ambient, source, language, owner_path, depth, out);
}

fn handle_ambient_with_extent(
    extent: Node<'_>,
    ambient: Node<'_>,
    source: &str,
    language: Language,
    owner_path: &[String],
    depth: usize,
    out: &mut Vec<Symbol>,
) {
    let mut c = ambient.walk();
    for ch in ambient.named_children(&mut c) {
        if skip_bad(ch) {
            continue;
        }
        match ch.kind() {
            "function_signature" => {
                let Some(name) = identifier_field_text(ch, "name", source) else {
                    continue;
                };
                let sig = strip_semi(collapse_ws(&node_text(extent, source)));
                push_sym(
                    out,
                    make_symbol(
                        extent,
                        source,
                        language,
                        SymbolKind::Fn,
                        name.clone(),
                        qualify(owner_path, &name),
                        depth,
                        sig,
                    ),
                );
            }
            "class_declaration" | "abstract_class_declaration" => {
                push_class(extent, ch, source, language, owner_path, depth, out);
            }
            "lexical_declaration" => {
                push_lexical(extent, ch, source, language, owner_path, depth, out);
            }
            "interface_declaration" => {
                push_interface(extent, ch, source, language, owner_path, depth, out);
            }
            "type_alias_declaration" => {
                push_type_alias(extent, ch, source, language, owner_path, depth, out);
            }
            _ => {}
        }
    }
}

fn push_class(
    extent: Node<'_>,
    decl: Node<'_>,
    source: &str,
    language: Language,
    owner_path: &[String],
    depth: usize,
    out: &mut Vec<Symbol>,
) {
    let Some(name) = field_text(decl, "name", source) else {
        return;
    };
    let Some(body) = decl.child_by_field_name("body") else {
        return;
    };
    let sig = signature_before_body(extent, body, source);
    push_sym(
        out,
        make_symbol(
            extent,
            source,
            language,
            SymbolKind::Class,
            name.clone(),
            qualify(owner_path, &name),
            depth,
            sig,
        ),
    );
    let mut path = owner_path.to_vec();
    path.push(name);
    walk_class_body(body, source, language, &path, depth + 1, out);
}

fn walk_class_body(
    body: Node<'_>,
    source: &str,
    language: Language,
    owner_path: &[String],
    depth: usize,
    out: &mut Vec<Symbol>,
) {
    let mut c = body.walk();
    let kids: Vec<Node<'_>> = body.named_children(&mut c).collect();
    let mut i = 0;
    while i < kids.len() {
        let node = kids[i];
        if skip_bad(node) {
            i += 1;
            continue;
        }
        if node.kind() == "method_definition" {
            // Leading contiguous decorators belong to the method's extent, not its signature.
            let mut start_idx = i;
            while start_idx > 0 && kids[start_idx - 1].kind() == "decorator" {
                start_idx -= 1;
            }
            let extent_start = kids[start_idx];
            push_method(extent_start, node, source, language, owner_path, depth, out);
        } else if node.kind() == "method_signature" {
            // Ambient / .d.ts class bodies use method_signature (no body).
            let Some(name) = identifier_field_text(node, "name", source) else {
                i += 1;
                continue;
            };
            let sig = strip_semi(collapse_ws(&node_text(node, source)));
            push_sym(
                out,
                make_symbol(
                    node,
                    source,
                    language,
                    SymbolKind::Method,
                    name.clone(),
                    qualify(owner_path, &name),
                    depth,
                    sig,
                ),
            );
        }
        // public_field_definition / fields: not outlined this milestone.
        i += 1;
    }
}

fn push_method(
    extent_start: Node<'_>,
    method: Node<'_>,
    source: &str,
    language: Language,
    owner_path: &[String],
    depth: usize,
    out: &mut Vec<Symbol>,
) {
    let Some(name) = identifier_field_text(method, "name", source) else {
        return;
    };
    let Some(body) = method.child_by_field_name("body") else {
        return;
    };
    // Signature excludes decorators: measured from the method_definition itself.
    let sig = signature_before_body(method, body, source);
    let start_byte = extent_start.start_byte();
    let end_byte = method.end_byte();
    let start_line = extent_start.start_position().row + 1;
    let end_line = method.end_position().row + 1;
    push_sym(
        out,
        make_symbol_ext(
            source,
            language,
            SymbolKind::Method,
            name.clone(),
            qualify(owner_path, &name),
            depth,
            start_line,
            end_line,
            start_byte,
            end_byte,
            sig,
        ),
    );
}

fn push_interface(
    extent: Node<'_>,
    decl: Node<'_>,
    source: &str,
    language: Language,
    owner_path: &[String],
    depth: usize,
    out: &mut Vec<Symbol>,
) {
    let Some(name) = field_text(decl, "name", source) else {
        return;
    };
    let Some(body) = decl.child_by_field_name("body") else {
        return;
    };
    let sig = signature_before_body(extent, body, source);
    push_sym(
        out,
        make_symbol(
            extent,
            source,
            language,
            SymbolKind::Interface,
            name.clone(),
            qualify(owner_path, &name),
            depth,
            sig,
        ),
    );
    let mut path = owner_path.to_vec();
    path.push(name);
    walk_interface_body(body, source, language, &path, depth + 1, out);
}

fn walk_interface_body(
    body: Node<'_>,
    source: &str,
    language: Language,
    owner_path: &[String],
    depth: usize,
    out: &mut Vec<Symbol>,
) {
    let mut c = body.walk();
    for child in body.named_children(&mut c) {
        if skip_bad(child) || child.kind() != "method_signature" {
            continue;
        }
        let Some(name) = identifier_field_text(child, "name", source) else {
            continue;
        };
        let sig = strip_semi(collapse_ws(&node_text(child, source)));
        push_sym(
            out,
            make_symbol(
                child,
                source,
                language,
                SymbolKind::Method,
                name.clone(),
                qualify(owner_path, &name),
                depth,
                sig,
            ),
        );
    }
}

fn push_enum(
    extent: Node<'_>,
    decl: Node<'_>,
    source: &str,
    language: Language,
    owner_path: &[String],
    depth: usize,
    out: &mut Vec<Symbol>,
) {
    let Some(name) = field_text(decl, "name", source) else {
        return;
    };
    let Some(body) = decl.child_by_field_name("body") else {
        return;
    };
    let sig = signature_before_body(extent, body, source);
    push_sym(
        out,
        make_symbol(
            extent,
            source,
            language,
            SymbolKind::Enum,
            name.clone(),
            qualify(owner_path, &name),
            depth,
            sig,
        ),
    );
}

fn push_type_alias(
    extent: Node<'_>,
    decl: Node<'_>,
    source: &str,
    language: Language,
    owner_path: &[String],
    depth: usize,
    out: &mut Vec<Symbol>,
) {
    let Some(name) = field_text(decl, "name", source) else {
        return;
    };
    let sig = strip_semi(collapse_ws(&node_text(extent, source)));
    push_sym(
        out,
        make_symbol(
            extent,
            source,
            language,
            SymbolKind::Type,
            name.clone(),
            qualify(owner_path, &name),
            depth,
            sig,
        ),
    );
}

fn push_function(
    extent: Node<'_>,
    decl: Node<'_>,
    source: &str,
    language: Language,
    owner_path: &[String],
    depth: usize,
    out: &mut Vec<Symbol>,
) {
    let Some(name) = field_text(decl, "name", source) else {
        return;
    };
    let Some(body) = decl.child_by_field_name("body") else {
        return;
    };
    let sig = signature_before_body(extent, body, source);
    push_sym(
        out,
        make_symbol(
            extent,
            source,
            language,
            SymbolKind::Fn,
            name.clone(),
            qualify(owner_path, &name),
            depth,
            sig,
        ),
    );
    // Nested functions inside the body are not outlined.
}

fn push_lexical(
    extent: Node<'_>,
    decl: Node<'_>,
    source: &str,
    language: Language,
    owner_path: &[String],
    depth: usize,
    out: &mut Vec<Symbol>,
) {
    if !lexical_is_const(decl, source) {
        return;
    }
    let mut c = decl.walk();
    for child in decl.named_children(&mut c) {
        if skip_bad(child) || child.kind() != "variable_declarator" {
            continue;
        }
        let Some(name) = identifier_field_text(child, "name", source) else {
            continue;
        };
        let value = child.child_by_field_name("value");
        let is_fn = value.is_some_and(|v| is_function_init(v));
        let kind = if is_fn {
            SymbolKind::Fn
        } else {
            SymbolKind::Const
        };
        let sig = if is_fn {
            // Arrow / function-valued const: signature not required by the golden tests.
            strip_semi(collapse_ws(&node_text(extent, source)))
        } else {
            strip_semi(collapse_ws(&node_text(extent, source)))
        };
        push_sym(
            out,
            make_symbol(
                extent,
                source,
                language,
                kind,
                name.clone(),
                qualify(owner_path, &name),
                depth,
                sig,
            ),
        );
    }
}

fn push_namespace(
    extent: Node<'_>,
    decl: Node<'_>,
    source: &str,
    language: Language,
    owner_path: &[String],
    depth: usize,
    out: &mut Vec<Symbol>,
) {
    // A namespace's name is a single identifier like any other declaration's: `namespace "s" {}`
    // has a `string` name child, and pasting its text in would publish a symbol called `"s"` -
    // quotes included - that nothing can address. Same rule as `identifier_field_text` everywhere
    // else in this module (ECMA-NAMEFIX).
    let Some(name) = identifier_field_text(decl, "name", source) else {
        return;
    };
    let Some(body) = decl.child_by_field_name("body") else {
        return;
    };
    let sig = signature_before_body(extent, body, source);
    // Prefer `namespace Name` wording even when wrapped in expression_statement.
    let sig = if sig.starts_with("namespace ") || sig.starts_with("module ") {
        sig
    } else {
        format!("namespace {name}")
    };
    push_sym(
        out,
        make_symbol(
            extent,
            source,
            language,
            SymbolKind::Namespace,
            name.clone(),
            qualify(owner_path, &name),
            depth,
            sig,
        ),
    );
    let mut path = owner_path.to_vec();
    path.push(name);
    walk(body, source, language, &path, depth + 1, false, out);
}

fn lexical_is_const(decl: Node<'_>, source: &str) -> bool {
    let mut c = decl.walk();
    let kids: Vec<_> = decl.children(&mut c).collect();
    for (i, ch) in kids.iter().enumerate() {
        if decl.field_name_for_child(i as u32) == Some("kind") {
            return node_text(*ch, source) == "const";
        }
    }
    node_text(decl, source).trim_start().starts_with("const ")
}

fn is_function_init(value: Node<'_>) -> bool {
    matches!(
        value.kind(),
        "arrow_function"
            | "function_expression"
            | "function"
            | "generator_function"
            | "generator_function_expression"
    )
}

fn signature_before_body(extent: Node<'_>, body: Node<'_>, source: &str) -> String {
    if body.start_byte() < extent.start_byte() || body.start_byte() > extent.end_byte() {
        return strip_semi(collapse_ws(&node_text(extent, source)));
    }
    strip_semi(collapse_ws(&source[extent.start_byte()..body.start_byte()]))
}

fn leading_doc(source: &str, start_line: usize) -> Option<(usize, String)> {
    if start_line <= 1 {
        return None;
    }
    let lines: Vec<&str> = source.lines().collect();
    let above = start_line - 1;
    if above == 0 || above > lines.len() {
        return None;
    }
    let trimmed = lines[above - 1].trim();

    // Contiguous `//` block.
    if trimmed.starts_with("//") {
        let mut collected: Vec<(usize, String)> = Vec::new();
        let mut line_no = above;
        loop {
            let t = lines[line_no - 1].trim_start();
            if let Some(rest) = t.strip_prefix("//") {
                collected.push((line_no, rest.trim().to_string()));
                if line_no == 1 {
                    break;
                }
                line_no -= 1;
            } else {
                break;
            }
        }
        let first_line = collected.last()?.0;
        collected.reverse();
        let text = collected.first()?.1.clone();
        return Some((first_line, text));
    }

    // Block comment ending on the line immediately above (`/** ... */` or multi-line).
    if trimmed.ends_with("*/") || trimmed.starts_with("/*") || trimmed.starts_with('*') {
        let mut start = above;
        while start > 1 && !lines[start - 1].trim().contains("/*") {
            let prev = lines[start - 2].trim();
            if prev.contains("/*") || prev.starts_with('*') {
                start -= 1;
            } else {
                break;
            }
        }
        if !lines[start - 1].trim().contains("/*") {
            return None;
        }
        let block: String = lines[start - 1..above].join("\n");
        let text = extract_block_comment_first_line(&block)?;
        return Some((start, text));
    }
    None
}

fn extract_block_comment_first_line(raw: &str) -> Option<String> {
    let mut t = raw.trim();
    if let Some(rest) = t.strip_prefix("/**") {
        t = rest;
    } else if let Some(rest) = t.strip_prefix("/*") {
        t = rest;
    } else {
        return None;
    }
    if let Some(rest) = t.strip_suffix("*/") {
        t = rest;
    }
    for line in t.lines() {
        let l = line.trim().trim_start_matches('*').trim();
        if !l.is_empty() {
            return Some(l.to_string());
        }
    }
    None
}

#[allow(clippy::too_many_arguments)]
fn make_symbol(
    extent: Node<'_>,
    source: &str,
    language: Language,
    kind: SymbolKind,
    name: String,
    qualified: String,
    depth: usize,
    signature: String,
) -> Option<Symbol> {
    make_symbol_ext(
        source,
        language,
        kind,
        name,
        qualified,
        depth,
        extent.start_position().row + 1,
        extent.end_position().row + 1,
        extent.start_byte(),
        extent.end_byte(),
        signature,
    )
}

#[allow(clippy::too_many_arguments)]
fn make_symbol_ext(
    source: &str,
    language: Language,
    kind: SymbolKind,
    name: String,
    qualified: String,
    depth: usize,
    start_line: usize,
    end_line: usize,
    start_byte: usize,
    end_byte: usize,
    signature: String,
) -> Option<Symbol> {
    if !usable_name(&name) {
        return None;
    }
    let doc_first_line = leading_doc(source, start_line).map(|(_, t)| t);
    Some(Symbol {
        language,
        kind,
        name,
        qualified,
        depth,
        start_line,
        end_line,
        start_byte,
        end_byte,
        signature,
        doc_first_line,
    })
}

fn push_sym(out: &mut Vec<Symbol>, sym: Option<Symbol>) {
    if let Some(s) = sym {
        out.push(s);
    }
}

fn qualify(owner_path: &[String], name: &str) -> String {
    if owner_path.is_empty() {
        name.to_string()
    } else {
        format!("{}.{}", owner_path.join("."), name)
    }
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

/// The name child of `node`, but only when it is a single identifier.
///
/// A symbol's name must come from one identifier node: a destructuring pattern
/// (`object_pattern` `{ a, b }`, `array_pattern` `[x, y]`), a computed member name
/// (`[Symbol.iterator]`) or a string literal key (`'a-b'`) is not a name this milestone can
/// publish, and pasting its text into a symbol would invent a name that is not in the source
/// (`declare const {\n}` once produced the name `"{\n}"`). Such a declaration yields no symbol at
/// all - destructuring is deliberately not expanded here.
///
/// `property_identifier` (`get x`) and `private_property_identifier` (`#priv`) are single
/// identifiers and do pass.
fn identifier_field_text(node: Node<'_>, field: &str, source: &str) -> Option<String> {
    let n = node.child_by_field_name(field)?;
    if skip_bad(n) || !is_identifier_node(n) {
        return None;
    }
    let text = node_text(n, source);
    usable_name(&text).then_some(text)
}

/// Whether `node` names a single identifier (a bare name, a property key, or a private name).
fn is_identifier_node(node: Node<'_>) -> bool {
    matches!(
        node.kind(),
        "identifier"
            | "property_identifier"
            | "private_property_identifier"
            | "shorthand_property_identifier"
            | "shorthand_property_identifier_pattern"
    )
}

fn first_named_of_kinds<'a>(node: Node<'a>, kinds: &[&str]) -> Option<Node<'a>> {
    let mut c = node.walk();
    node.named_children(&mut c)
        .find(|n| kinds.contains(&n.kind()) && !skip_bad(*n))
}

fn node_text(node: Node<'_>, source: &str) -> String {
    source[node.start_byte()..node.end_byte()].to_string()
}

fn collapse_ws(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut pending = false;
    for c in s.chars() {
        if c.is_whitespace() {
            pending = true;
        } else {
            if pending && !out.is_empty() {
                out.push(' ');
            }
            pending = false;
            out.push(c);
        }
    }
    out
}

fn strip_semi(s: String) -> String {
    s.trim_end_matches(';').trim().to_string()
}

fn skip_bad(node: Node<'_>) -> bool {
    node.is_error() || node.is_missing() || node.kind() == "ERROR" || node.kind() == "MISSING"
}
