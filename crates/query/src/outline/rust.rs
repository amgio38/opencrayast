//! Rust outline (docs/LANGUAGES.md "Outline queries").
//!
//! # Why a node walk and not a `.scm` query
//!
//! The outline needs three things the grammar does not expose as local node shapes: the *owner*
//! chain (which `impl`/`trait`/`mod` a function sits in, which decides `fn` vs `method` and the
//! qualified name), the attributes that precede a declaration, and the doc block above it. A
//! query can only say "this node shape is a symbol"; the owner chain and the attribute/doc
//! grouping then have to be stitched back together from captures anyway. Walking the children of
//! each declaration states those relations directly, so that is the shape I can guarantee
//! correct.
//!
//! Consequences of that choice, all deliberate:
//! - `impl_item` has no `name` field in the grammar, so the implemented type is read off the type
//!   children directly (see [`impl_name`]).
//! - the walk is iterative over an explicit stack of scopes: a deeply nested module cannot
//!   overflow the stack, the same rule `parse` follows.
//! - `opencrayast-query` deliberately does not depend on `tree-sitter`, so no helper may name a
//!   `tree_sitter::Node` in its signature. Everything below therefore takes plain offsets, and
//!   node-typed values stay local variables whose types are inferred.

use super::{Symbol, SymbolKind, is_clean_name, is_clean_qualified};
use opencrayast_lang::{Language, ParsedFile};
use std::collections::VecDeque;

/// All symbols of the file at every depth (any order; the caller sorts), each with
/// `language`, `kind`, `name`, `qualified`, `depth`, line/byte extents, `signature` and
/// `doc_first_line` (filled whenever a doc exists) as specified on [`Symbol`].
///
/// A file with syntax errors is still outlined: `ERROR`/`MISSING` nodes are skipped, and the
/// declarations around them are real symbols.
pub(crate) fn collect(parsed: &ParsedFile, source: &str) -> Vec<Symbol> {
    let bytes = source.as_bytes();
    let line_starts = line_index(bytes);
    // Byte offsets are always at character boundaries here (they come from the grammar), but a
    // bad slice must never panic: an empty string degrades the name, not the run.
    let text = |from: usize, to: usize| -> &str { source.get(from..to).unwrap_or_default() };
    let line_of = |byte: usize| -> usize { line_number(&line_starts, byte) };

    // What a symbol inside the current scope inherits: the qualified path of its owner, whether
    // that owner is a type (impl/trait: its functions are methods) or a module, and the depth its
    // symbols get.
    let mut out: Vec<Symbol> = Vec::new();
    let mut pending: VecDeque<_> =
        VecDeque::from([(parsed.tree.root_node(), 1usize, String::new(), false)]);

    while let Some((scope, depth, path, in_type)) = pending.pop_front() {
        // Attributes and doc comments precede the declaration they belong to, so they are gathered
        // here and applied to the next declaration that turns up in this scope.
        let mut attr_start: Option<usize> = None;
        let mut doc: Option<(usize, String)> = None;

        let mut walker = scope.walk();
        for child in scope.children(&mut walker) {
            let kind = child.kind();
            match kind {
                // An attribute belongs to the declaration below it: remember the outermost one.
                "attribute_item" => {
                    attr_start = Some(
                        attr_start.map_or(child.start_byte(), |a: usize| a.min(child.start_byte())),
                    );
                }
                // A comment is documentation only if it is an outer doc comment. `//!` documents
                // the module itself and belongs to no symbol.
                "line_comment" | "block_comment" => {
                    let raw = text(child.start_byte(), child.end_byte());
                    match doc_text(kind, raw) {
                        // Only the FIRST line of a contiguous block is the doc; later lines of the
                        // same block must not overwrite it.
                        Some(t) => {
                            if doc.is_none() {
                                doc = Some((line_of(child.start_byte()), t));
                            }
                        }
                        // A plain `//` comment or `//!` between docs and the declaration breaks
                        // the block: whatever was pending no longer documents this symbol.
                        None => doc = None,
                    }
                }
                _ => {
                    let start_byte = attr_start.unwrap_or(child.start_byte());
                    let start_line = line_of(start_byte);
                    let end_byte = child.end_byte();
                    let end_line = line_of(if end_byte > start_byte {
                        end_byte - 1
                    } else {
                        start_byte
                    });
                    let body_start = child
                        .child_by_field_name("body")
                        .map(|b| b.start_byte())
                        .filter(|b| *b >= child.start_byte());
                    // `decl` starts at the node start, so `body_start` is an offset into `decl`
                    // too: the signature is the node's text up to its own body.
                    let decl_text = text(child.start_byte(), end_byte);
                    let signature =
                        signature(decl_text, body_start.map(|b| b - child.start_byte()));

                    // `impl_item` has no name field: its name comes from the type it implements.
                    let impl_types: Vec<String> = if kind == "impl_item" {
                        let mut w = child.walk();
                        child
                            .children(&mut w)
                            .filter(|c| {
                                c.is_named()
                                    && !matches!(
                                        c.kind(),
                                        "type_parameters" | "declaration_list" | "trait_bounds"
                                    )
                            })
                            // A damaged region is not a type: `impl Foo for ;` leaves an ERROR
                            // node where the `for` was, and counting it as the implemented type
                            // would name the block "Foo for for". `has_error` is the broader test:
                            // a child can also be intact while containing a broken region, and its
                            // raw text would then carry comments and line breaks into the name.
                            .filter(|c| !c.has_error() && !c.is_missing())
                            .map(|c| base_type_name(text(c.start_byte(), c.end_byte())))
                            // A type that does not reduce to a clean identifier chain is not a
                            // name we are willing to publish.
                            .filter(|n| is_clean_name(n))
                            .collect()
                    } else {
                        Vec::new()
                    };
                    // Anything not named here is not a symbol this milestone emits: struct fields,
                    // `extern` blocks, `use` items, expressions. Such nodes still act as scopes if
                    // they have a declaration-list body.
                    let declared_name = match kind {
                        "impl_item"
                        | "associated_type"
                        | "struct_item"
                        | "enum_item"
                        | "union_item"
                        | "trait_item"
                        | "function_item"
                        | "function_signature_item"
                        | "mod_item"
                        | "const_item"
                        | "static_item"
                        | "type_item"
                        | "macro_definition" => child
                            .child_by_field_name("name")
                            .map(|n| text(n.start_byte(), n.end_byte()).to_string()),
                        _ => None,
                    };
                    let (name, sym_kind, sub_depth, child_path, child_type) =
                        describe(kind, declared_name, impl_types, depth, &path, in_type);

                    // The invariant: a symbol is only emitted when its name AND its qualified name are clean
                    // (non-empty, bounded, no line breaks or comment markers). A damaged node's
                    // raw text must never become a name - see `is_clean_name`. This is the single
                    // place the check happens for Rust, so it covers every kind.
                    if let Some(name) = name.filter(|n| is_clean_name(n)) {
                        let qualified = if path.is_empty() {
                            name.clone()
                        } else {
                            format!("{path}::{name}")
                        };
                        if !is_clean_qualified(&qualified) {
                            // Milestone choice: skip the WHOLE block, methods included, rather than
                            // emitting them with no owner. The owner name is what makes a method's
                            // qualified name (`Config::load`) addressable, so a method of a damaged
                            // impl would be just as unreachable as the impl itself - and it is not
                            // text that exists in the source either. `continue` therefore skips the
                            // scope push below as well, which is what "skip the whole block" means.
                            attr_start = None;
                            doc = None;
                            continue;
                        }
                        out.push(Symbol {
                            language: Language::Rust,
                            kind: sym_kind,
                            name,
                            qualified,
                            depth,
                            start_line,
                            end_line,
                            start_byte,
                            end_byte,
                            signature,
                            doc_first_line: doc.as_ref().map(|(_, t)| t.clone()),
                        });
                    }
                    // FIFO, not LIFO: a scope pushed while walking a parent must be processed after every sibling
                    // of that parent, otherwise the deeper scope is walked first and its
                    // symbols land out of source order. Order does not matter to the caller (it
                    // re-sorts), but processing a parent before its children keeps the qualified
                    // path of a nested module correct, which the stack order gets wrong.
                    if let Some(body) = child
                        .child_by_field_name("body")
                        .filter(|b| b.kind() == "declaration_list" && kind != "foreign_mod_item")
                    {
                        pending.push_back((body, sub_depth, child_path, child_type));
                    }
                    attr_start = None;
                    doc = None;
                }
            }
        }
    }

    out
}

/// The line number (1-based) a byte offset falls on.
fn line_number(line_starts: &[usize], byte: usize) -> usize {
    match line_starts.binary_search(&byte) {
        Ok(i) => i + 1,
        // Before the first start would mean a negative offset, which cannot happen; treat it as
        // line 1 rather than panicking.
        Err(0) => 1,
        Err(i) => i,
    }
}

/// The byte offset of the start of every line.
fn line_index(bytes: &[u8]) -> Vec<usize> {
    let mut starts = vec![0usize];
    for (i, b) in bytes.iter().enumerate() {
        if *b == b'\n' {
            starts.push(i + 1);
        }
    }
    starts
}

/// The kind, name, and the scope its children get, for one declaration node.
///
/// Returns `None` for the name when the node is not a symbol this milestone emits (struct fields,
/// `extern` blocks, `use` items and any unexpected shape). Such nodes still act as scopes if they
/// have a declaration-list body, which is why the rest of the tuple is always filled in.
#[allow(clippy::type_complexity)]
fn describe(
    kind: &str,
    name: Option<String>,
    impl_types: Vec<String>,
    depth: usize,
    path: &str,
    in_type: bool,
) -> (Option<String>, SymbolKind, usize, String, bool) {
    // `impl_item` has no name field, so its name comes from the type it implements: one type for
    // `impl Foo`, two for `impl Trait for Foo`.
    let name = match kind {
        "impl_item" => match impl_types.as_slice() {
            [] => None,
            [one] => Some((*one).clone()),
            [trait_name, ty, ..] => Some(format!("{trait_name} for {ty}")),
        },
        _ => name,
    };
    let sym_kind = match kind {
        "struct_item" => SymbolKind::Struct,
        "enum_item" => SymbolKind::Enum,
        "trait_item" => SymbolKind::Trait,
        "impl_item" => SymbolKind::Impl,
        "mod_item" => SymbolKind::Module,
        "const_item" => SymbolKind::Const,
        "static_item" => SymbolKind::Static,
        "type_item" | "associated_type" => SymbolKind::Type,
        "macro_definition" => SymbolKind::Macro,
        "function_item" | "function_signature_item" => {
            if in_type {
                SymbolKind::Method
            } else {
                SymbolKind::Fn
            }
        }
        _ => SymbolKind::Fn,
    };
    // The children of an `impl`/`trait` are one level deeper and inherit the owner name; the
    // children of a `mod` are one level deeper and are qualified through the module path.
    let child_type = matches!(kind, "impl_item" | "trait_item");
    let child_path = match &name {
        Some(n) if !n.is_empty() => {
            if path.is_empty() {
                n.clone()
            } else {
                format!("{path}::{n}")
            }
        }
        _ => path.to_string(),
    };
    (name, sym_kind, depth + 1, child_path, child_type)
}

/// The bare type name of a type node: `Foo` for `Foo<T>` and `dyn Foo`, `usize` for `&usize`.
fn base_type_name(raw: &str) -> String {
    // The common shapes are cheap to recognise from the text; anything else keeps its source text
    // rather than being silently mangled.
    if let Some(open) = raw.find('<') {
        let head = raw[..open].trim();
        if !head.is_empty() && head.chars().all(|c| c.is_alphanumeric() || c == '_') {
            return head.to_string();
        }
    }
    let trimmed = raw
        .trim()
        .trim_start_matches('&')
        .trim_start_matches("dyn ")
        .trim();
    let name: String = trimmed
        .chars()
        .take_while(|c| c.is_alphanumeric() || *c == '_')
        .collect();
    if name.is_empty() {
        raw.trim().to_string()
    } else {
        name
    }
}

/// The signature: the declaration text up to (not including) the body's `{`, with whitespace
/// collapsed to single spaces; for a declaration without a body, the whole text minus a trailing
/// `;`.
fn signature(decl: &str, body_start: Option<usize>) -> String {
    // The signature is the declaration node's own text cut at the body; a node may carry tokens
    // after the body, so only the prefix is used. `body_start` is an offset into `decl`.
    let prefix = match body_start {
        Some(b) => {
            let b = b.min(decl.len());
            &decl[..b]
        }
        None => decl,
    };
    let out = collapse(prefix);
    // A body-less declaration ends in `;`, which is not part of the signature.
    if body_start.is_none() {
        out.trim_end_matches(';').trim_end().to_string()
    } else {
        out
    }
}

/// Collapse every run of whitespace to a single space and trim.
fn collapse(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut in_ws = false;
    for c in s.chars() {
        if c.is_whitespace() {
            in_ws = true;
        } else {
            if in_ws && !out.is_empty() {
                out.push(' ');
            }
            in_ws = false;
            out.push(c);
        }
    }
    out.trim().to_string()
}

/// The first line of an outer doc comment, markers removed and trimmed; `None` for any comment that
/// does not document the item below it (a plain `//` comment, or `//!` on the module itself).
fn doc_text(kind: &str, raw: &str) -> Option<String> {
    if kind == "line_comment" {
        let rest = raw.strip_prefix("///")?;
        // `////` is still a doc comment in Rust, but the extra slashes are content.
        let first = rest.lines().next().unwrap_or_default();
        let t = first.trim();
        if t.is_empty() {
            None
        } else {
            Some(t.to_string())
        }
    } else {
        let rest = raw.strip_prefix("/**")?;
        let rest = rest.strip_suffix("*/").unwrap_or(rest);
        // First non-empty line of the block, minus a leading `*` and spaces.
        let first = rest
            .lines()
            .map(|l| l.trim().trim_start_matches('*').trim())
            .find(|l| !l.is_empty())?;
        Some(first.to_string())
    }
}

/// The first line (1-based) of the doc comment block that sits immediately above
/// `symbol.start_line` (contiguous, no blank line in between), or `symbol.start_line` itself if
/// there is none.
pub(crate) fn doc_start_line(source: &str, symbol: &Symbol) -> usize {
    doc_block_start(source, symbol.start_line)
}

/// The first line of the doc block immediately above `start_line` (1-based), or `start_line`.
fn doc_block_start(source: &str, start_line: usize) -> usize {
    let lines: Vec<&str> = source.lines().collect();
    // `ln` counts down from the line above the declaration; `lines[ln - 1]` is that line.
    let mut ln = start_line.saturating_sub(1);
    let mut first: Option<usize> = None;
    while ln >= 1 {
        let line = lines[ln - 1].trim();
        if let Some(rest) = line.strip_prefix("///") {
            let _ = rest;
            first = Some(ln);
            ln -= 1;
            continue;
        }
        if line.ends_with("*/") {
            // The last line of a `/** .. */` block: walk up to the line that opens it.
            let mut j = ln;
            while j >= 1 {
                let candidate = lines[j - 1];
                if candidate.contains("/**") {
                    first = Some(j);
                    break;
                }
                if candidate.trim().is_empty() {
                    break;
                }
                j -= 1;
            }
        }
        break;
    }
    first.unwrap_or(start_line)
}
