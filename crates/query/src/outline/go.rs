//! Go outline (docs/LANGUAGES.md "Outline queries").
//!
//! Node kinds from a dump of `tree-sitter-go` 0.25 against `tests/fixtures/sample.go`
//! (and extras): `type_declaration` → `type_spec` / `type_alias`; `function_declaration`;
//! `method_declaration` (receiver field); `method_elem` inside `interface_type`;
//! `const_declaration` / `const_spec`; `var_declaration` / `var_spec`. Docs are contiguous
//! `//` lines immediately above the symbol (no blank line).

use super::{Symbol, SymbolKind};
use opencrayast_lang::{Language, ParsedFile};
use tree_sitter::Node;

/// All symbols of the file at every depth (any order; the caller sorts).
pub(crate) fn collect(parsed: &ParsedFile, source: &str) -> Vec<Symbol> {
    let root = parsed.tree.root_node();
    let mut out = Vec::new();
    let mut cursor = root.walk();
    for child in root.children(&mut cursor) {
        if skip_bad(child) {
            continue;
        }
        match child.kind() {
            "type_declaration" => collect_type_decl(child, source, &mut out),
            "function_declaration" => {
                collect_function(child, source, &mut out);
                collect_absorbed(child, source, &mut out);
            }
            "method_declaration" => {
                collect_method(child, source, &mut out);
                collect_absorbed(child, source, &mut out);
            }
            "const_declaration" => collect_const_decl(child, source, &mut out),
            "var_declaration" => collect_var_decl(child, source, &mut out),
            // package_clause and comments are not symbols.
            _ => {}
        }
    }
    out
}

/// First line of the contiguous `//` block immediately above `symbol.start_line`.
pub(crate) fn doc_start_line(source: &str, symbol: &Symbol) -> usize {
    match leading_line_comments(source, symbol.start_line) {
        Some((first, _)) => first,
        None => symbol.start_line,
    }
}

fn collect_type_decl(decl: Node<'_>, source: &str, out: &mut Vec<Symbol>) {
    let mut cursor = decl.walk();
    for child in decl.named_children(&mut cursor) {
        if skip_bad(child) {
            continue;
        }
        match child.kind() {
            "type_spec" => push_type_spec(child, decl, source, out),
            "type_alias" => push_type_alias(child, decl, source, out),
            _ => {}
        }
    }
}

fn push_type_spec(spec: Node<'_>, decl: Node<'_>, source: &str, out: &mut Vec<Symbol>) {
    let Some(name) = field_text(spec, "name", source) else {
        return;
    };
    let Some(ty) = spec.child_by_field_name("type") else {
        return;
    };
    let (kind, signature) = match ty.kind() {
        "struct_type" => (
            SymbolKind::Struct,
            signature_to_brace(decl, ty, source).unwrap_or_else(|| format!("type {name} struct")),
        ),
        "interface_type" => {
            let sig = signature_to_brace(decl, ty, source)
                .unwrap_or_else(|| format!("type {name} interface"));
            let sym = make_symbol(
                spec,
                source,
                SymbolKind::Interface,
                name.clone(),
                name.clone(),
                1,
                sig,
            );
            push_sym(out, sym);
            collect_interface_methods(ty, &name, source, out);
            return;
        }
        _ => {
            // Defined type (`type Y int`) or other non-struct/interface type_spec.
            (SymbolKind::Type, collapse_ws(&first_line_of(decl, source)))
        }
    };
    push_sym(
        out,
        make_symbol(spec, source, kind, name.clone(), name, 1, signature),
    );
}

fn push_type_alias(alias: Node<'_>, decl: Node<'_>, source: &str, out: &mut Vec<Symbol>) {
    let Some(name) = field_text(alias, "name", source) else {
        return;
    };
    // Whole `type X = Y` line (from the declaration for the leading `type` keyword).
    let signature = collapse_ws(&first_line_of(decl, source));
    // When the alias sits in a `type ( ... )` group, prefer the alias's own line with `type `.
    let signature = if signature.contains('(') {
        format!("type {}", collapse_ws(&first_line_of(alias, source)))
    } else {
        signature
    };
    push_sym(
        out,
        make_symbol(
            alias,
            source,
            SymbolKind::Type,
            name.clone(),
            name,
            1,
            signature,
        ),
    );
}

fn collect_interface_methods(
    iface: Node<'_>,
    iface_name: &str,
    source: &str,
    out: &mut Vec<Symbol>,
) {
    let mut cursor = iface.walk();
    for child in iface.named_children(&mut cursor) {
        if skip_bad(child) || child.kind() != "method_elem" {
            continue;
        }
        let Some(name) = field_text(child, "name", source) else {
            continue;
        };
        let qualified = format!("{iface_name}.{name}");
        let signature = collapse_ws(&node_text(child, source));
        push_sym(
            out,
            make_symbol(
                child,
                source,
                SymbolKind::Method,
                name,
                qualified,
                2,
                signature,
            ),
        );
    }
}

fn collect_function(func: Node<'_>, source: &str, out: &mut Vec<Symbol>) {
    let Some(name) = field_text(func, "name", source) else {
        return;
    };
    let signature = match func.child_by_field_name("body") {
        Some(body) => collapse_ws(&source[func.start_byte()..body.start_byte()]),
        None => collapse_ws(&node_text(func, source))
            .trim_end_matches(';')
            .to_string(),
    };
    push_sym(
        out,
        make_symbol(
            func,
            source,
            SymbolKind::Fn,
            name.clone(),
            name,
            1,
            signature,
        ),
    );
}

fn collect_method(method: Node<'_>, source: &str, out: &mut Vec<Symbol>) {
    let Some(name) = field_text(method, "name", source) else {
        return;
    };
    let Some(recv_type) = receiver_type_name(method, source) else {
        return;
    };
    if !usable_name(&recv_type) {
        return;
    }
    let qualified = format!("{recv_type}.{name}");
    let signature = match method.child_by_field_name("body") {
        Some(body) => collapse_ws(&source[method.start_byte()..body.start_byte()]),
        None => collapse_ws(&node_text(method, source)),
    };
    push_sym(
        out,
        make_symbol(
            method,
            source,
            SymbolKind::Method,
            name,
            qualified,
            1,
            signature,
        ),
    );
}

/// Receiver type without pointer or type parameters: `*Config` → `Config`, `Foo[T]` → `Foo`.
fn receiver_type_name(method: Node<'_>, source: &str) -> Option<String> {
    let recv = method.child_by_field_name("receiver")?;
    let mut cursor = recv.walk();
    let param = recv.named_children(&mut cursor).next()?;
    let ty = param.child_by_field_name("type")?;
    Some(strip_go_type_name(ty, source))
}

fn strip_go_type_name(ty: Node<'_>, source: &str) -> String {
    match ty.kind() {
        "pointer_type" => {
            let mut c = ty.walk();
            if let Some(inner) = ty.named_children(&mut c).next() {
                strip_go_type_name(inner, source)
            } else {
                node_text(ty, source)
            }
        }
        "generic_type" => {
            // `Foo[T]` — take the type identifier field if present.
            if let Some(name) = ty.child_by_field_name("type") {
                strip_go_type_name(name, source)
            } else {
                let mut c = ty.walk();
                ty.named_children(&mut c)
                    .find(|n| n.kind() == "type_identifier")
                    .map(|n| node_text(n, source))
                    .unwrap_or_else(|| node_text(ty, source))
            }
        }
        "type_identifier" => node_text(ty, source),
        _ => {
            let mut c = ty.walk();
            ty.named_children(&mut c)
                .find(|n| n.kind() == "type_identifier")
                .map(|n| node_text(n, source))
                .unwrap_or_else(|| node_text(ty, source))
        }
    }
}

fn collect_const_decl(decl: Node<'_>, source: &str, out: &mut Vec<Symbol>) {
    let mut cursor = decl.walk();
    for spec in decl.named_children(&mut cursor) {
        if skip_bad(spec) || spec.kind() != "const_spec" {
            continue;
        }
        // A const_spec may declare several names; emit one symbol per name identifier.
        let mut names = Vec::new();
        let mut sw = spec.walk();
        for (i, ch) in spec.children(&mut sw).enumerate() {
            if spec.field_name_for_child(i as u32) == Some("name")
                && ch.kind() == "identifier"
                && !skip_bad(ch)
            {
                let text = node_text(ch, source);
                if usable_name(&text) {
                    names.push(text);
                }
            }
        }
        if names.is_empty() {
            continue;
        }
        let value = spec
            .child_by_field_name("value")
            .map(|v| collapse_ws(&node_text(v, source)));
        for name in names {
            let signature = match &value {
                Some(v) => format!("const {name} = {v}"),
                None => format!("const {name}"),
            };
            push_sym(
                out,
                make_symbol(
                    spec,
                    source,
                    SymbolKind::Const,
                    name.clone(),
                    name,
                    1,
                    signature,
                ),
            );
        }
    }
}

fn collect_var_decl(decl: Node<'_>, source: &str, out: &mut Vec<Symbol>) {
    let mut cursor = decl.walk();
    for spec in decl.named_children(&mut cursor) {
        if skip_bad(spec) || spec.kind() != "var_spec" {
            continue;
        }
        let mut sw = spec.walk();
        for (i, ch) in spec.children(&mut sw).enumerate() {
            if spec.field_name_for_child(i as u32) == Some("name")
                && ch.kind() == "identifier"
                && !skip_bad(ch)
            {
                let name = node_text(ch, source);
                if !usable_name(&name) {
                    continue;
                }
                let signature = collapse_ws(&first_line_of(spec, source));
                let signature = if signature.starts_with("var ") {
                    signature
                } else {
                    format!("var {signature}")
                };
                push_sym(
                    out,
                    make_symbol(
                        spec,
                        source,
                        SymbolKind::Variable,
                        name.clone(),
                        name,
                        1,
                        signature,
                    ),
                );
            }
        }
    }
}

fn signature_to_brace(decl: Node<'_>, ty: Node<'_>, source: &str) -> Option<String> {
    // `{` sits on `field_declaration_list` / interface body, not always a direct child of `ty`.
    let ty_src = &source[ty.start_byte()..ty.end_byte()];
    let rel = ty_src.find('{')?;
    let brace_start = ty.start_byte() + rel;
    Some(collapse_ws(&source[decl.start_byte()..brace_start]))
}

fn make_symbol(
    extent: Node<'_>,
    source: &str,
    kind: SymbolKind,
    name: String,
    qualified: String,
    depth: usize,
    signature: String,
) -> Option<Symbol> {
    if !usable_name(&name) {
        return None;
    }
    let start_line = extent.start_position().row + 1;
    let doc_first_line = leading_line_comments(source, start_line).map(|(_, text)| text);
    Some(Symbol {
        language: Language::Go,
        kind,
        name,
        qualified,
        depth,
        start_line,
        end_line: extent.end_position().row + 1,
        start_byte: extent.start_byte(),
        end_byte: extent.end_byte(),
        signature,
        doc_first_line,
    })
}

fn push_sym(out: &mut Vec<Symbol>, sym: Option<Symbol>) {
    if let Some(s) = sym {
        out.push(s);
    }
}

fn usable_name(name: &str) -> bool {
    let trimmed = name.trim();
    // Go's blank identifier is a real name in the grammar but never a symbol: `var _ = x`
    // discards the value on purpose and there is nothing an agent could address.
    !trimmed.is_empty() && trimmed != "_"
}

/// Contiguous `//` lines immediately above `start_line` (1-based), no blank line between.
fn leading_line_comments(source: &str, start_line: usize) -> Option<(usize, String)> {
    if start_line <= 1 {
        return None;
    }
    let lines: Vec<&str> = source.lines().collect();
    let mut collected: Vec<(usize, String)> = Vec::new();
    let mut line_no = start_line - 1;
    loop {
        let idx = line_no - 1;
        if idx >= lines.len() {
            break;
        }
        let trimmed = lines[idx].trim_start();
        if let Some(rest) = trimmed.strip_prefix("//") {
            collected.push((line_no, rest.trim().to_string()));
            if line_no == 1 {
                break;
            }
            line_no -= 1;
        } else {
            break;
        }
    }
    let first_line = collected.last()?.0; // topmost while walking upward
    collected.reverse();
    let first_text = collected.first()?.1.clone();
    Some((first_line, first_text))
}

fn first_line_of(node: Node<'_>, source: &str) -> String {
    let text = &source[node.start_byte()..node.end_byte()];
    text.lines().next().unwrap_or(text).to_string()
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

/// Recover the declarations that ERROR recovery folded into a broken `func`/`method`.
///
/// tree-sitter-go does not stop at the break. An unterminated parameter list or body
/// swallows the declarations that FOLLOW it, and the collector above only walks
/// `source_file`'s direct children, so none of them are emitted. That is silent data loss:
/// the symbols are plainly in the source text, and an agent asking for the outline of a
/// damaged file gets a shorter answer with no hint that anything went missing. Verified in
/// tree-sitter-go 0.25 (the Go absorption fix); the shape survives here so a grammar change that
/// alters it turns these tests red rather than silently reverting to truncation.
///
/// The shapes are distinguished by what is actually in the tree, not by "the declaration
/// looks wrong", because a damaged declaration can nest its swallowed siblings at either
/// depth:
/// - `block` → `statement_list` → {`const_declaration`, `var_declaration`, `type_declaration`,
///   `expression_statement`} — the rest of the file reclassified as statements;
/// - `parameter_list` → {`ERROR`, `parameter_declaration`} — the parser swallowed the
///   *next* declaration as a parameter.
///
/// Both recoveries are top-down, so `out` stays in source order.
fn collect_absorbed(decl: Node<'_>, source: &str, out: &mut Vec<Symbol>) {
    // Guard: only an *unterminated* declaration reclassifies its siblings, and
    // unterminated always shows up as a MISSING `)` / `}` token somewhere below it. Without
    // this, the shapes above are indistinguishable from a healthy body that merely declares
    // locals, and recovery would invent top-level symbols for them.
    if !has_missing_token(decl) {
        return;
    }
    let mut cursor = decl.walk();
    for child in decl.named_children(&mut cursor) {
        match child.kind() {
            "block" => collect_absorbed_in_body(child, source, out),
            "parameter_list" => collect_absorbed_in_params(child, source, out),
            _ => {}
        }
    }
}

/// Any MISSING token at or below `node`. `is_missing` (not `is_error`) is the right test:
/// a broken *expression* produces `ERROR` nodes too, and those are not what causes siblings
/// to be swallowed — only a token the parser had to invent is.
fn has_missing_token(node: Node<'_>) -> bool {
    if node.is_missing() {
        return true;
    }
    let mut cursor = node.walk();
    node.children(&mut cursor).any(has_missing_token)
}

/// Body case: `const`/`var`/`type` siblings that recovery reclassified as statements, plus
/// a later `func` sibling demoted to an `expression_statement` → `func_literal` (recovery
/// cut its `func` keyword and left the name in an `ERROR` node).
///
/// Genuine locals of the broken function are left alone. Recovery produces two shapes and
/// only one of them can be a swallowed sibling:
/// - `:=` and the typed forms (`var x int`, `var (x, y int)`) are statements a real function
///   body contains, so they are omitted;
/// - a bare `var x = …` / `const x = …` / `type x …` directly inside the `statement_list`
///   is not legal Go inside a function — a grouped `var (…)` always carries a type — so it
///   can only be a top-level declaration that got reclassified.
fn collect_absorbed_in_body(body: Node<'_>, source: &str, out: &mut Vec<Symbol>) {
    let mut cursor = body.walk();
    let lists: Vec<Node<'_>> = body
        .named_children(&mut cursor)
        .filter(|n| n.kind() == "statement_list")
        .collect();
    for list in lists {
        let mut statements = list.walk();
        for statement in list.named_children(&mut statements) {
            match statement.kind() {
                "const_declaration" if is_untyped_group(statement, source) => {
                    collect_const_decl(statement, source, out)
                }
                "var_declaration" if is_untyped_group(statement, source) => {
                    collect_var_decl(statement, source, out)
                }
                "type_declaration" => collect_type_decl(statement, source, out),
                "expression_statement" => collect_absorbed_func_literal(statement, source, out),
                _ => {}
            }
        }
    }
}

/// Parameter-list case: a `func` sibling folded in as a `parameter_declaration`.
/// `method_declaration` siblings land the same way; the receiver is not recoverable, so
/// they are qualified by the bare name and marked with `?` like any recovered function.
fn collect_absorbed_in_params(params: Node<'_>, source: &str, out: &mut Vec<Symbol>) {
    let mut cursor = params.walk();
    let declarations: Vec<Node<'_>> = params
        .named_children(&mut cursor)
        .filter(|n| n.kind() == "parameter_declaration")
        .collect();
    for declaration in declarations {
        let Some(function_type) = declaration.named_child(0) else {
            continue;
        };
        if function_type.kind() != "function_type" {
            continue;
        }
        let Some(name) = function_type.named_child(0) else {
            continue;
        };
        // Recovery leaves the name as a bare `identifier` or wrapped in an `ERROR` node;
        // both are the name, and `usable_name` rejects the blank identifier either way.
        if name.kind() != "identifier" && !skip_bad(name) {
            continue;
        }
        let name = node_text(name, source);
        if !usable_name(&name) {
            continue;
        }
        let signature = format!(
            "func {name}{} /*?*/",
            suffix_after_error(function_type, source)
        );
        push_sym(
            out,
            make_symbol(
                declaration,
                source,
                SymbolKind::Fn,
                name.clone(),
                name,
                1,
                signature,
            ),
        );
    }
}

/// `expression_statement` → `func_literal` shape: the `func` keyword survives but the name
/// is an `ERROR` node, which is why the name is taken from the `func_literal` and not from
/// a `name` field. The trailing `()` / `{}` are still normal tokens, so the signature is
/// rebuilt from the source.
fn collect_absorbed_func_literal(statement: Node<'_>, source: &str, out: &mut Vec<Symbol>) {
    let Some(literal) = statement.named_child(0) else {
        return;
    };
    if literal.kind() != "func_literal" {
        return;
    }
    let mut cursor = literal.walk();
    // Recovery cut the `func` keyword, so the name is left wrapped in an `ERROR` node and
    // there is no `name` field to read. A plain `identifier` is the already-recovered shape.
    let Some(name_node) = literal
        .named_children(&mut cursor)
        .find(|n| skip_bad(*n) || n.kind() == "identifier")
    else {
        return;
    };
    let name = node_text(name_node, source);
    if !usable_name(&name) {
        return;
    }
    let params = literal
        .named_children(&mut literal.walk())
        .find(|n| n.kind() == "parameter_list")
        .map(|n| collapse_ws(&node_text(n, source)))
        .unwrap_or_default();
    let signature = format!("func {name}{params}");
    push_sym(
        out,
        make_symbol(
            literal,
            source,
            SymbolKind::Fn,
            name.clone(),
            name,
            1,
            signature,
        ),
    );
}

/// A declaration written as `var x = …` / `const x = …`, with no type. `var x int = …` /
/// `var (x, y int = …)` are forms recovery legitimately produces for real locals.
fn is_untyped_group(decl: Node<'_>, source: &str) -> bool {
    let mut cursor = decl.walk();
    for spec in decl.named_children(&mut cursor) {
        if spec.kind() != "var_spec" && spec.kind() != "const_spec" {
            continue;
        }
        // A parenthesised `var (…)` group implies an explicit type exists somewhere.
        let has_type = spec.child_by_field_name("type").is_some()
            || source[spec.start_byte()..spec.end_byte()].contains('(');
        if has_type {
            return false;
        }
        return true;
    }
    false
}

/// `signature_to_brace` cannot be reused: the body is a `block`, not the `{` the caller
/// was written for, and `kind`/`name` have already been trimmed by `collapse_ws`.
fn suffix_after_error(function_type: Node<'_>, source: &str) -> String {
    let mut cursor = function_type.walk();
    let mut tail = String::new();
    let mut seen_error = false;
    for child in function_type.children(&mut cursor) {
        if seen_error && !child.is_named() {
            tail.push_str(node_text(child, source).trim());
        }
        if skip_bad(child) {
            seen_error = true;
        }
    }
    tail
}
