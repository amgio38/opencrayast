//! Language adapter and search for the ast-grep-core oracle.
//!
//! Two adapter requirements (both match `ast-grep-language` 0.45.3):
//!
//! 1. **Expando**: Python / Go / Rust reject `$` as an identifier lead-in, so
//!    `expando_char` is `µ` and `pre_process_pattern` rewrites `$NAME` / `$$$`
//!    before the grammar sees the pattern.
//! 2. **Pattern context**: Go / Rust often need the pattern wrapped in a
//!    function-body (same idea as our `compile.rs` contexts). Bare
//!    `Pattern::try_new("fmt.Println($X)")` parses as a *type conversion*, not a
//!    `call_expression`, and then finds nothing in a real file. We therefore also
//!    try `Pattern::contextual` over the language's body wrappers and keep the
//!    candidate that yields the most hits.

use ast_grep_core::Language;
use ast_grep_core::meta_var::MetaVariable;
use ast_grep_core::tree_sitter::{LanguageExt, StrDoc, TSLanguage};
use opencrayast_lang::Language as OcLang;
use std::borrow::Cow;

use super::normalize::{CaptureSpan, NormalizedHit};

/// Wrapper so each opencrayast language can talk to ast-grep-core.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DiffLang(pub OcLang);

impl DiffLang {
    #[allow(dead_code)] // used when wiring dual-side reports
    pub fn id(self) -> &'static str {
        self.0.id()
    }

    /// Languages whose tree-sitter grammar rejects `$` as an identifier lead-in.
    fn needs_expando(self) -> bool {
        matches!(self.0, OcLang::Python | OcLang::Go | OcLang::Rust)
    }

    /// Prefix/suffix wrappers mirroring `compile.rs` `contexts_for` (non-empty only).
    fn body_contexts(self) -> Vec<(&'static str, &'static str)> {
        match self.0 {
            OcLang::Rust => vec![("fn _() {\n", "\n}")],
            OcLang::Go => vec![("package p\nfunc _() {\n", "\n}")],
            _ => vec![],
        }
    }
}

/// Same rewrite as `ast-grep-language::pre_process_pattern` (0.45.3).
fn pre_process_with_expando(expando: char, query: &str) -> Cow<'_, str> {
    let mut ret = Vec::with_capacity(query.len());
    let mut dollar_count = 0;
    for c in query.chars() {
        if c == '$' {
            dollar_count += 1;
            continue;
        }
        let need_replace = matches!(c, 'A'..='Z' | '_') // $A / $$A / $$$A
            || dollar_count == 3; // anonymous multiple `$$$`
        let sigil = if need_replace { expando } else { '$' };
        ret.extend(std::iter::repeat_n(sigil, dollar_count));
        dollar_count = 0;
        ret.push(c);
    }
    let sigil = if dollar_count == 3 { expando } else { '$' };
    ret.extend(std::iter::repeat_n(sigil, dollar_count));
    Cow::Owned(ret.into_iter().collect())
}

impl Language for DiffLang {
    fn kind_to_id(&self, kind: &str) -> u16 {
        self.get_ts_language().id_for_node_kind(kind, true)
    }

    fn field_to_id(&self, field: &str) -> Option<u16> {
        self.get_ts_language()
            .field_id_for_name(field)
            .map(|f| f.get())
    }

    fn expando_char(&self) -> char {
        if self.needs_expando() {
            'µ'
        } else {
            self.meta_var_char()
        }
    }

    fn pre_process_pattern<'q>(&self, query: &'q str) -> Cow<'q, str> {
        if self.needs_expando() {
            pre_process_with_expando(self.expando_char(), query)
        } else {
            Cow::Borrowed(query)
        }
    }

    fn build_pattern(
        &self,
        builder: &ast_grep_core::matcher::PatternBuilder,
    ) -> Result<ast_grep_core::Pattern, ast_grep_core::PatternError> {
        builder.build(|src| StrDoc::try_new(src, *self))
    }
}

impl LanguageExt for DiffLang {
    fn get_ts_language(&self) -> TSLanguage {
        #[allow(clippy::expect_used)]
        self.0
            .grammar()
            .expect("grammar missing for enabled language")
    }
}

/// Named kinds of nodes that cover exactly the pattern slice inside `wrapped`
/// (already expando-preprocessed when needed). Also records ancestors that fully
/// contain the slice — `Pattern::contextual` wants the intended root kind.
fn kinds_covering_slice(lang: DiffLang, wrapped: &str, start: usize, end: usize) -> Vec<String> {
    let grammar = lang.get_ts_language();
    let mut parser = tree_sitter::Parser::new();
    if parser.set_language(&grammar).is_err() {
        return Vec::new();
    }
    let Some(tree) = parser.parse(wrapped, None) else {
        return Vec::new();
    };
    let mut kinds = Vec::new();
    let mut push = |k: String| {
        if !kinds.iter().any(|x| x == &k) {
            kinds.push(k);
        }
    };
    let mut stack = vec![tree.root_node()];
    while let Some(n) = stack.pop() {
        let ns = n.start_byte();
        let ne = n.end_byte();
        if ne <= start || ns >= end {
            continue;
        }
        if n.is_named() && ns == start && ne == end {
            push(n.kind().to_string());
        }
        let mut c = n.walk();
        for child in n.children(&mut c) {
            stack.push(child);
        }
    }
    kinds
}

/// Compile candidate patterns: bare `try_new`, plus Go/Rust body-context variants.
fn compile_candidates(
    lang: DiffLang,
    pattern: &str,
) -> Result<(Option<ast_grep_core::Pattern>, Vec<ast_grep_core::Pattern>), String> {
    let mut bare = None;
    let mut contextuals = Vec::new();
    let mut errs = Vec::new();
    match ast_grep_core::Pattern::try_new(pattern, lang) {
        Ok(p) => bare = Some(p),
        Err(e) => errs.push(e.to_string()),
    }
    for (prefix, suffix) in lang.body_contexts() {
        let pat_for_parse: Cow<'_, str> = if lang.needs_expando() {
            pre_process_with_expando(lang.expando_char(), pattern)
        } else {
            Cow::Borrowed(pattern)
        };
        let wrapped_for_parse = format!("{prefix}{pat_for_parse}{suffix}");
        let start = prefix.len();
        let end = start + pat_for_parse.len();
        let wrapped_for_asg = format!("{prefix}{pattern}{suffix}");
        for kind in kinds_covering_slice(lang, &wrapped_for_parse, start, end) {
            match ast_grep_core::Pattern::contextual(&wrapped_for_asg, &kind, lang) {
                Ok(p) => contextuals.push(p),
                Err(e) => errs.push(format!("contextual({kind}): {e}")),
            }
        }
        for kind in [
            "call_expression",
            "selector_expression",
            "expression_statement",
            "short_var_declaration",
            "assignment_statement",
            "scoped_identifier",
            "binary_expression",
            "unary_expression",
            "index_expression",
            "slice_expression",
            "type_assertion_expression",
            "composite_literal",
            "func_literal",
            "field_expression",
            "method_call_expression",
            "macro_invocation",
            "reference_expression",
            "await_expression",
        ] {
            if let Ok(p) = ast_grep_core::Pattern::contextual(&wrapped_for_asg, kind, lang) {
                contextuals.push(p);
            }
        }
    }
    if bare.is_none() && contextuals.is_empty() {
        Err(errs
            .into_iter()
            .next()
            .unwrap_or_else(|| "pattern refused by ast-grep".into()))
    } else {
        Ok((bare, contextuals))
    }
}

fn hits_for_pattern(
    lang: DiffLang,
    source: &str,
    pat: ast_grep_core::Pattern,
) -> Vec<NormalizedHit> {
    let grep = lang.ast_grep(source);
    let mut hits = Vec::new();
    for m in grep.root().find_all(pat) {
        let range = m.range();
        let env = m.get_env();
        let mut captures = Vec::new();
        for var in env.get_matched_variables() {
            match var {
                MetaVariable::Capture(name, _) => {
                    if let Some(node) = env.get_match(&name) {
                        let r = node.range();
                        captures.push(CaptureSpan {
                            name,
                            start: r.start,
                            end: r.end,
                            list: false,
                        });
                    }
                }
                MetaVariable::MultiCapture(name) => {
                    let nodes = env.get_multiple_matches(&name);
                    let (start, end) = if nodes.is_empty() {
                        (range.start, range.start)
                    } else {
                        (nodes[0].range().start, nodes[nodes.len() - 1].range().end)
                    };
                    captures.push(CaptureSpan {
                        name,
                        start,
                        end,
                        list: true,
                    });
                }
                MetaVariable::Dropped(_) | MetaVariable::Multiple => {}
            }
        }
        hits.push(NormalizedHit {
            start: range.start,
            end: range.end,
            captures,
        });
    }
    super::normalize::finalize_hits(hits)
}

/// Compile `pattern` and search `source` with ast-grep-core.
///
/// Returns `Err` when every candidate compilation is refused by ast-grep.
pub fn search_ast_grep(
    lang: DiffLang,
    pattern: &str,
    source: &str,
) -> Result<Vec<NormalizedHit>, String> {
    let (bare, contextuals) = compile_candidates(lang, pattern)?;
    let bare_hits = bare
        .map(|p| hits_for_pattern(lang, source, p))
        .unwrap_or_default();
    // Bare wins when it finds anything: contextual fallbacks exist only for Go/Rust
    // patterns whose bare parse is the wrong shape (e.g. `fmt.Println($X)` → type_conversion).
    if !bare_hits.is_empty() || contextuals.is_empty() {
        return Ok(bare_hits);
    }
    let score = |hits: &[NormalizedHit]| -> (usize, usize, i64) {
        // Among contextual candidates: more hits, then more captures (so
        // `call_expression` with `$X` beats a tighter `selector_expression`
        // that dropped the argument list), then tighter spans.
        let caps: usize = hits.iter().map(|h| h.captures.len()).sum();
        let span: i64 = hits.iter().map(|h| (h.end - h.start) as i64).sum();
        (hits.len(), caps, -span)
    };
    let mut best_ctx: Vec<NormalizedHit> = Vec::new();
    let mut best_ctx_score = (0usize, 0usize, i64::MIN);
    for pat in contextuals {
        let hits = hits_for_pattern(lang, source, pat);
        let s = score(&hits);
        if s > best_ctx_score {
            best_ctx_score = s;
            best_ctx = hits;
        }
    }
    Ok(best_ctx)
}
