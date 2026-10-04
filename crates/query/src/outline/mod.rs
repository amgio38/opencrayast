//! Outline and symbol lookup (docs/TOOLS.md `ast_outline`, `ast_get`; LANGUAGES "Outline queries").

use opencrayast_core::error::{ErrorCode, ToolError};
use opencrayast_lang::{Language, ParsedFile};

mod ecma;
mod go;
mod python;
mod rust;

/// Portable symbol kinds (docs/TOOLS.md "Symbol kinds in outlines").
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SymbolKind {
    /// `module`
    Module,
    /// `namespace`
    Namespace,
    /// `class`
    Class,
    /// `struct`
    Struct,
    /// `enum`
    Enum,
    /// `interface`
    Interface,
    /// `trait`
    Trait,
    /// `impl`
    Impl,
    /// `fn`
    Fn,
    /// `method`
    Method,
    /// `const`
    Const,
    /// `static`
    Static,
    /// `type`
    Type,
    /// `field`
    Field,
    /// `variable`
    Variable,
    /// `macro`
    Macro,
}

impl SymbolKind {
    /// The wire name: `module`, `namespace`, `class`, `struct`, `enum`, `interface`, `trait`,
    /// `impl`, `fn`, `method`, `const`, `static`, `type`, `field`, `variable`, `macro`.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Module => "module",
            Self::Namespace => "namespace",
            Self::Class => "class",
            Self::Struct => "struct",
            Self::Enum => "enum",
            Self::Interface => "interface",
            Self::Trait => "trait",
            Self::Impl => "impl",
            Self::Fn => "fn",
            Self::Method => "method",
            Self::Const => "const",
            Self::Static => "static",
            Self::Type => "type",
            Self::Field => "field",
            Self::Variable => "variable",
            Self::Macro => "macro",
        }
    }

    /// Inverse of [`SymbolKind::as_str`] (exact, lowercase).
    pub fn parse(s: &str) -> Option<SymbolKind> {
        const ALL: [SymbolKind; 16] = [
            SymbolKind::Module,
            SymbolKind::Namespace,
            SymbolKind::Class,
            SymbolKind::Struct,
            SymbolKind::Enum,
            SymbolKind::Interface,
            SymbolKind::Trait,
            SymbolKind::Impl,
            SymbolKind::Fn,
            SymbolKind::Method,
            SymbolKind::Const,
            SymbolKind::Static,
            SymbolKind::Type,
            SymbolKind::Field,
            SymbolKind::Variable,
            SymbolKind::Macro,
        ];
        ALL.into_iter().find(|k| k.as_str() == s)
    }
}

/// One symbol found in a file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Symbol {
    /// The language of the file the symbol was found in.
    pub language: Language,
    /// Portable kind.
    pub kind: SymbolKind,
    /// The bare name (`load`). For an `impl` block: the implemented type (`Config`), or
    /// `Trait for Type` for a trait impl.
    pub name: String,
    /// Qualified name: owner path joined with the language separator (`Config::load` in Rust,
    /// `Config.load` in TypeScript, JavaScript, Python and Go; `inner::helper` for a function in
    /// `mod inner`). Top-level symbols have `qualified == name`.
    pub qualified: String,
    /// Nesting depth, 1 for top-level symbols.
    pub depth: usize,
    /// First line, 1-based, INCLUDING decorators/attributes that belong to the symbol but
    /// EXCLUDING the doc comment.
    pub start_line: usize,
    /// Last line, 1-based, inclusive.
    pub end_line: usize,
    /// Start byte offset (same extent as `start_line`).
    pub start_byte: usize,
    /// End byte offset, exclusive (same extent as `end_line`).
    pub end_byte: usize,
    /// The declaration up to (not including) its body, whitespace collapsed to single spaces,
    /// trailing `;` and `:` removed (e.g. `pub fn load(path: &str) -> Result<Config, Error>`).
    pub signature: String,
    /// First line of the doc comment (comment markers removed, trimmed), if any and if requested.
    pub doc_first_line: Option<String>,
}

/// The longest a symbol `name` may be. A name is a single identifier or a short `Trait for Type`,
/// so anything past this is damaged source that leaked in rather than a real name.
pub(super) const MAX_NAME_BYTES: usize = 256;

/// The longest a `qualified` name may be. Qualified names are a joined path
/// (`a::b::Config::load`, `Config.load`), so a deeply nested module path can legitimately be much
/// longer than a bare name - 1024 bytes still leaves room for hundreds of nesting levels, while
/// anything past it is damaged text.
pub(super) const MAX_QUALIFIED_BYTES: usize = 1024;

/// Whether `name` is usable as a symbol name: non-empty, bounded, and made only of identifier-ish
/// text - no line breaks, no comment markers, nothing that came from a broken source.
///
/// The failure it prevents is a name built from a damaged node's raw text: on broken input the
/// outline would otherwise report a symbol called `"? where extern <T> for /// d for"` or one
/// containing a newline and a `//!` comment - unreachable through `find_symbols`, impossible to
/// address, and nothing that exists in the source. Skipping such a symbol is the honest answer,
/// because the invariant is that the outline contains only symbols that really exist.
///
/// This is the *shared* rule, and the pipeline in [`collect_all`] applies it to every language;
/// language modules may call it themselves as defence in depth, but it is deliberately not their
/// only line of defence. The same defect has shown up once per language, and a rule that only
/// exists in one module is one more module away from being forgotten.
pub(super) fn is_clean_name(name: &str) -> bool {
    is_clean_within(name, MAX_NAME_BYTES)
}

/// [`is_clean_name`] for a qualified name: the same rules, the longer bound. A qualified name is
/// allowed to be a joined path, so only the limit differs.
pub(super) fn is_clean_qualified(qualified: &str) -> bool {
    is_clean_within(qualified, MAX_QUALIFIED_BYTES)
}

/// The shared rule behind [`is_clean_name`] and [`is_clean_qualified`].
fn is_clean_within(name: &str, max_bytes: usize) -> bool {
    let trimmed = name.trim();
    if trimmed.is_empty() || trimmed.len() > max_bytes {
        return false;
    }
    !name
        .chars()
        .any(|c| c.is_control() || c == '\u{feff}' || c == '\u{2028}' || c == '\u{2029}')
        && !name.contains("//")
        && !name.contains("/*")
        && !name.contains("*/")
        // A doubled space is the fingerprint of damaged text pasted into a name (`Foo  for`).
        && !name.contains("  ")
        && !trimmed.starts_with("for ")
        && !trimmed.ends_with(" for")
        // A path separator at either end means the path was assembled from a broken owner.
        && !name.starts_with("::")
        && !name.ends_with("::")
}

/// What to include in an outline.
#[derive(Debug, Clone)]
pub struct OutlineOptions {
    /// Keep symbols with `depth <= max_depth` (default 3).
    pub max_depth: usize,
    /// If `Some`, keep only these kinds (children of a dropped parent are still considered).
    pub kinds: Option<Vec<SymbolKind>>,
    /// Fill `doc_first_line`.
    pub include_docs: bool,
}

impl Default for OutlineOptions {
    fn default() -> Self {
        Self {
            max_depth: 3,
            kinds: None,
            include_docs: false,
        }
    }
}

/// Every symbol of the file at every depth, in source order (a parent before its children).
/// Each language module returns its symbols with `doc_first_line` filled where a doc exists;
/// the options are applied by the callers below.
fn collect_all(parsed: &ParsedFile, source: &str) -> Vec<Symbol> {
    collect_all_counting_discards(parsed, source).0
}

/// [`collect_all`], plus how many symbols the shared gate in it threw away.
///
/// Hidden from the docs on purpose: it is a seam for the discard-count probe in
/// `tests/outline_namefix_probe_spec.rs` (NAMEFIX item 3), not part of what a
/// caller of this crate should reach for. The count is what makes the ticket's question
/// answerable - "how much damaged text is the collector producing for the pipeline to clean up
/// after it" - and a caller never needs it, because from the outside a discarded symbol simply is
/// not in the outline. [`collect_all`] is the entry point and the one that decides what a caller
/// sees.
#[doc(hidden)]
pub fn collect_all_counting_discards(parsed: &ParsedFile, source: &str) -> (Vec<Symbol>, usize) {
    let all = match parsed.language {
        Language::Rust => rust::collect(parsed, source),
        Language::TypeScript | Language::Tsx | Language::JavaScript => {
            ecma::collect(parsed, source)
        }
        Language::Python => python::collect(parsed, source),
        Language::Go => go::collect(parsed, source),
    };
    // The shared gate: every language's symbols pass this, so the rule is applied once instead of
    // once per language module. It is here rather than only inside each module because the same
    // defect has appeared more than once - one rule, applied once, cannot be forgotten in the fifth
    // module. The per-module checks stay as defence in depth; this is the floor.
    //
    // What this gate is proven to do (CLEAN-NAMES, test
    // `the_pipeline_gate_is_what_removes_damaged_names`): it is load-bearing for the length bound.
    // A single identifier of 257+ bytes is a perfectly well-formed name node that every collector
    // accepts - the ecma `is_identifier_node` guard, `usable_name`, and the per-module checks in
    // `rust.rs` are all satisfied by it - so without this line the outline publishes it. That is
    // not hypothetical: `MAX_NAME_BYTES` is 256, and the size check exists precisely so a name is
    // never too long to be addressed.
    //
    // What this gate is NOT proven to do: it is not proven to be the last line of defence against
    // damaged *text* in a name. A fuzz sweep over every collector (300k generated sources, plus
    // deep-nesting probes for the qualified bound) found no input that reaches any other rule here -
    // the comment-marker, doubled-space, `for`-fragment, control-character and `::` rules are all
    // currently unreachable, because the collectors' own guards reject that text upstream first.
    // Those rules are defence in depth for a collector bug that has not been written yet. They are
    // kept because the cost of a false negative is a symbol nobody can address, but they are not
    // currently witnessed by a test, and this comment should not claim otherwise. If a collector
    // ever does leak damaged text, these are the rules that will catch it, and
    // `the_pipeline_gate_is_what_removes_damaged_names` should be extended to witness it.
    //
    // Only what cannot be addressed is removed; the shape of a symbol is the language module's
    // business.
    let before = all.len();
    let mut all: Vec<Symbol> = all
        .into_iter()
        .filter(|s| is_clean_name(&s.name) && is_clean_qualified(&s.qualified))
        .collect();
    let discarded = before - all.len();
    // Pre-order: by start, and for equal starts the larger (outer) extent first.
    all.sort_by(|a, b| {
        a.start_byte
            .cmp(&b.start_byte)
            .then(b.end_byte.cmp(&a.end_byte))
    });
    (all, discarded)
}

/// The symbols of one parsed file, in source order (pre-order: a parent before its children,
/// ordered by `start_byte`). `source` must be the text `parsed` was produced from. A file with
/// syntax errors is still outlined (the symbols that parsed are real).
pub fn outline(parsed: &ParsedFile, source: &str, opts: &OutlineOptions) -> Vec<Symbol> {
    collect_all(parsed, source)
        .into_iter()
        .filter(|s| s.depth <= opts.max_depth)
        .filter(|s| opts.kinds.as_ref().is_none_or(|k| k.contains(&s.kind)))
        .map(|mut s| {
            if !opts.include_docs {
                s.doc_first_line = None;
            }
            s
        })
        .collect()
}

/// Find symbols by name. A query without a separator (`load`) matches `Symbol::name` at any depth;
/// a query with a separator (`Config::load` or `Config.load` — either is accepted for any
/// language) matches `Symbol::qualified` exactly after normalising `::` to `.`. Matching is
/// case-sensitive. Returns every match in source order (the caller decides about ambiguity).
/// Searches ALL depths regardless of any outline depth limit.
pub fn find_symbols(parsed: &ParsedFile, source: &str, query: &str) -> Vec<Symbol> {
    let normalised = query.replace("::", ".");
    let qualified_query = normalised.contains('.');
    collect_all(parsed, source)
        .into_iter()
        .filter(|s| {
            if qualified_query {
                s.qualified.replace("::", ".") == normalised
            } else {
                s.name == query
            }
        })
        .map(|mut s| {
            s.doc_first_line = None;
            s
        })
        .collect()
}

/// The text of a symbol for `ast_get`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SymbolText {
    /// First line of the returned text (1-based) after adding doc and context.
    pub first_line: usize,
    /// Last line (inclusive).
    pub last_line: usize,
    /// The text of those whole lines (`\n`-joined as in the source, no trailing newline added).
    pub text: String,
}

/// Cut the text of `symbol` out of `source`: whole lines from `start_line` to `end_line`; with
/// `include_doc` the contiguous doc-comment lines immediately above `start_line` are added (Rust
/// `///` outer docs, `/** */` and `//` blocks in TypeScript/JavaScript, Go `//` blocks; Python
/// docstrings are inside the body already so nothing is added); then `context_lines` extra lines
/// before and after, clamped to the file.
pub fn symbol_text(
    source: &str,
    symbol: &Symbol,
    include_doc: bool,
    context_lines: usize,
) -> Result<SymbolText, ToolError> {
    let lines: Vec<&str> = source.lines().collect();
    if symbol.start_line == 0
        || symbol.end_line < symbol.start_line
        || symbol.end_line > lines.len()
    {
        return Err(ToolError::new(
            ErrorCode::Internal,
            "Symbol extent lies outside the source text.",
            "Report this with the file's language; no source text is included.",
        ));
    }
    let mut first = symbol.start_line;
    if include_doc {
        first = match symbol.language {
            Language::Rust => rust::doc_start_line(source, symbol),
            Language::TypeScript | Language::Tsx | Language::JavaScript => {
                ecma::doc_start_line(source, symbol)
            }
            Language::Python => python::doc_start_line(source, symbol),
            Language::Go => go::doc_start_line(source, symbol),
        }
        .clamp(1, symbol.start_line);
    }
    let first = first.saturating_sub(context_lines).max(1);
    let last = (symbol.end_line + context_lines).min(lines.len());
    Ok(SymbolText {
        first_line: first,
        last_line: last,
        text: lines[first - 1..last].join("\n"),
    })
}
