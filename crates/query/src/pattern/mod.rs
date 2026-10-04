//! The pattern and rule language (docs/PATTERNS.md; ADR-005: a purpose-built matcher).
//!
//! A pattern is code in the target language with metavariables. [`Pattern::compile`] turns it
//! into a matcher, [`CompiledRule::compile`] adds constraints, [`search`] runs both over one
//! parsed file under a hard budget. Everything here is pure: bytes in, matches out.
//!
//! ## How a pattern becomes a tree (exact, tested)
//!
//! 1. **Metavariables are rewritten to identifiers.** Most grammars cannot parse `$X`, so before
//!    parsing, `$$$NAME` becomes `µµµNAME`, `$$$` becomes `µµµ`, `$NAME` becomes `µNAME`,
//!    `$_` becomes `µ_`, and `$$` becomes a literal `$` (U+00B5 MICRO SIGN is an identifier letter
//!    in all five languages). `NAME` is `[A-Z][A-Z0-9_]*`; anything else after a `$` is left
//!    alone (it is just code).
//! 2. **Contexts.** Contexts are tried in the language's order and the first one that parses
//!    without any `ERROR` or `MISSING` node wins. Rust tries the *function body* context first
//!    (`fn _() {` ... `}`), because that is what makes an expression or a statement parse as one:
//!    Rust reports a missing `;` after a bare expression at the top level. Then the top level
//!    (items parse there as well as inside a body). Go has three steps: (a) the **top level**
//!    (`package p` before the pattern), accepted only when the pattern root is a *declaration*
//!    (`function_declaration`, `method_declaration`, `type_declaration`, `var_declaration`,
//!    `const_declaration`, `import_declaration` or `package_clause`); (b) the function body
//!    (`package p` + `func _() {` ... `}`); (c) the top level again, accepting any root. Step (a)
//!    exists because `func f() {}` is a `function_declaration` at the top level but a
//!    `func_literal` inside a body, so a declaration pattern must be read at the top level to
//!    match declarations; step (b) comes before (c) because Go's top level ALSO reads an
//!    expression such as `fmt.Println(x)` without a syntax error (as a type conversion), which
//!    is not what a pattern author means.
//!    TypeScript, JavaScript and Python try the top level; TypeScript and JavaScript then a class
//!    body (`class _ {` ... `}`) so that member patterns work. None parses cleanly =>
//!    `invalid_pattern` with the position of the first error in the ORIGINAL pattern text.
//! 3. **Root.** The pattern root is the *innermost named node whose byte range equals the trimmed
//!    pattern text*. So `foo($X)` (no `;`) is the call expression, `foo($X);` is the whole
//!    expression statement, `return $X;` is the return statement. No such node (the text is
//!    several statements, or a fragment) → `invalid_pattern` ("Multiple AST nodes").
//! 4. **Metavariable nodes.** The OUTERMOST pattern node whose full text is exactly a
//!    metavariable token is that metavariable, whatever its kind (so a statement that contains
//!    only `$$$BODY` is the list variable). **An `ERROR` node counts too**: where a grammar does
//!    not allow a bare identifier (the body of `impl $T { $$$B }`, `struct $S { $$$F }`,
//!    `trait $T { $$$B }`), tree-sitter wraps exactly that identifier in an `ERROR` node; such an
//!    `ERROR` node whose full text is one metavariable token is accepted as that metavariable.
//!    Any other `ERROR` or `MISSING` node still makes the context invalid.
//! 5. **Error positions** point at the problem: for an invalid pattern the position is the start
//!    of the first `MISSING` node or of the innermost `ERROR` node (never just the start of an
//!    enclosing wrapper), mapped back to the ORIGINAL pattern text.
//!
//! ## What matching means (exact, tested)
//!
//! A pattern node `p` matches a source node `s` when:
//! - `p` is `$NAME` / `$_`: `s` is a NAMED node; `$NAME` binds it, and a second occurrence of the
//!   same name must be *token-equal* to the first: the two nodes have the same sequence of leaf
//!   texts (depth-first, leaves of the source tree, comments (`is_extra`) and zero-width nodes
//!   skipped). The kinds of the enclosing nodes are NOT compared, because the kind of a node
//!   depends on its role (`T` in `<T>` is a `type_parameter`, `T` in `: T` is a
//!   `type_identifier`) and code that reads the same is the same code. Whitespace and comments
//!   never matter; string, regex and template text is compared exactly because it is leaf text;
//! - `p` is a leaf (no children): same kind and same text;
//! - otherwise: same kind, and the children of `p` match the children of `s` as sequences, both
//!   with comments (`is_extra`) removed. Anonymous tokens are children and must match (so
//!   `a + b` does not match `a - b`). `$$$NAME` / `$$$` in the pattern's children absorbs zero or
//!   more consecutive source children (named or anonymous), **lazily**: the shortest absorption
//!   that lets the rest of the sequence match is chosen (backtracking, counted by the budget).
//!
//! A search visits every node of the file in document order (a parent before its children) as a
//! candidate root; every match is reported, including matches nested inside matches.

// Skeleton: the compile/matcher/rules tickets remove this allow once their parts are used.
#![allow(dead_code)]

mod compile;
mod matcher;
mod rules;

use opencrayast_core::error::{ErrorCode, ToolError};
use opencrayast_lang::{Language, ParsedFile};
use std::time::Instant;

/// A pattern that could not be compiled, with where and how to fix it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PatternError {
    /// What is wrong, in one sentence, without quoting more than a short snippet.
    pub message: String,
    /// Byte offset in the pattern text of the first problem, if there is one.
    pub position: Option<usize>,
    /// What to do about it (wrap it in a block, give a complete statement, ...).
    pub suggestion: String,
}

impl From<PatternError> for ToolError {
    /// `invalid_pattern`; the message gains ` (at byte N of the pattern)` when a position is known.
    fn from(e: PatternError) -> ToolError {
        let message = match e.position {
            Some(p) => format!("{} (at byte {} of the pattern)", e.message, p),
            None => e.message,
        };
        ToolError::new(ErrorCode::InvalidPattern, message, e.suggestion)
    }
}

/// Whether a metavariable stands for one node or a list.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CaptureKind {
    /// `$NAME`
    One,
    /// `$$$NAME`
    List,
}

/// A metavariable declared by a pattern (anonymous `$_` and `$$$` are not listed).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MetaVar {
    /// The name without the `$` signs, e.g. `ARGS`.
    pub name: String,
    /// One or list.
    pub kind: CaptureKind,
}

/// A compiled pattern.
#[derive(Debug, Clone)]
pub struct Pattern {
    /// The language it was compiled for.
    pub(crate) language: Language,
    /// The pattern text as the caller wrote it.
    pub(crate) source: String,
    /// Compiler-private state (the parsed tree and the matcher program).
    pub(crate) program: compile::Program,
}

impl Pattern {
    /// Compile `source` for `language`. See the module documentation for the exact rules.
    /// Errors (`PatternError`): empty or whitespace-only pattern; no context parses cleanly (the
    /// position is the first `ERROR`/`MISSING` node of the best context); more than one root node;
    /// a root that is only `$$$NAME` (a list cannot be a root); more than 64 metavariables or a
    /// pattern longer than 16 KiB; a grammar that is not built in.
    pub fn compile(language: Language, source: &str) -> Result<Pattern, PatternError> {
        compile::compile(language, source)
    }

    /// The language this pattern was compiled for.
    pub fn language(&self) -> Language {
        self.language
    }

    /// The pattern text.
    pub fn source(&self) -> &str {
        &self.source
    }

    /// Named metavariables in order of first appearance.
    pub fn metavars(&self) -> Vec<MetaVar> {
        compile::metavars(&self.program)
    }

    /// The parsed pattern as an indented tree for `ast_explain_pattern`: two spaces per depth;
    /// a named node is its kind (a leaf adds ` "text"`); an anonymous token is its text in double
    /// quotes; `$NAME (one)` / `$$$NAME (list)` / `$_ (one)` / `$$$ (list)` for metavariables.
    /// Field names are not shown. A first line `warning: <text>` is added for each warning
    /// (currently: the pattern was only parseable inside a context, naming it).
    pub fn explain(&self) -> String {
        compile::explain(&self.program)
    }
}

/// A value constraint on a captured metavariable (`where` in a rule).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct VarConstraint {
    /// The capture's whole text must match this regular expression (linear-time engine, at most
    /// 1024 bytes of pattern, unanchored unless the expression anchors itself).
    pub regex: Option<String>,
    /// For a `$NAME` capture: the node kind must equal this.
    pub kind: Option<String>,
}

/// Either a code pattern or a nested rule (the operand of `inside`, `has`, `not`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RuleOperand {
    /// Code, compiled like the main pattern.
    Pattern(String),
    /// A nested rule.
    Rule(Box<Rule>),
}

/// A rule object (docs/PATTERNS.md "Rules"). All keys optional, combined with AND.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Rule {
    /// The matched node's grammar kind must equal this.
    pub kind: Option<String>,
    /// Some proper ancestor of the matched node satisfies the operand.
    pub inside: Option<Box<RuleOperand>>,
    /// Some proper descendant of the matched node satisfies the operand.
    pub has: Option<Box<RuleOperand>>,
    /// The matched node must NOT satisfy the operand.
    pub not: Option<Box<RuleOperand>>,
    /// Every rule must hold.
    pub all: Vec<Rule>,
    /// At least one rule must hold (an empty list is "no constraint").
    pub any: Vec<Rule>,
    /// Constraints on the main pattern's captures, keyed by `$NAME`.
    pub where_: Vec<(String, VarConstraint)>,
}

/// A rule validated and compiled for one language.
#[derive(Debug, Clone)]
pub struct CompiledRule {
    #[allow(dead_code)]
    pub(crate) program: rules::RuleProgram,
}

impl CompiledRule {
    /// Validate and compile `rule`: every `Pattern` operand compiles for `language`; every `kind`
    /// name exists in the grammar (an unknown name is an error whose suggestion lists up to five
    /// close names); regexes compile within their limits; `where` keys are `$NAME` forms; nesting
    /// is at most 8 deep and the whole rule has at most 64 nodes.
    pub fn compile(language: Language, rule: &Rule) -> Result<CompiledRule, PatternError> {
        rules::compile(language, rule)
    }
}

/// One captured metavariable of one match.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Capture {
    /// Name without `$` signs.
    pub name: String,
    /// One or list.
    pub kind: CaptureKind,
    /// Start byte of the captured text (for an empty list: the position where it would be).
    pub start_byte: usize,
    /// End byte, exclusive.
    pub end_byte: usize,
    /// The source text from `start_byte` to `end_byte` (a list's text includes the separators
    /// between its nodes, e.g. `"start", id`).
    pub text: String,
}

/// One match.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Match {
    /// Start byte of the matched node.
    pub start_byte: usize,
    /// End byte, exclusive.
    pub end_byte: usize,
    /// 1-based line of the start.
    pub start_line: usize,
    /// 1-based column of the start, in bytes.
    pub start_col: usize,
    /// 1-based line of the end position.
    pub end_line: usize,
    /// 1-based column of the end position (one past the last byte), in bytes.
    pub end_col: usize,
    /// The matched source text.
    pub text: String,
    /// The captures in order of first appearance in the pattern.
    pub captures: Vec<Capture>,
}

/// Hard limits of one search.
#[derive(Debug, Clone)]
pub struct SearchBudget {
    /// Maximum comparison steps (one step per pattern-node/source-node comparison attempt, per
    /// rule evaluation step). Exceeded => `budget_exceeded` naming "steps".
    pub max_steps: u64,
    /// Wall-clock deadline, checked at least every 1024 steps and between candidate roots.
    /// Passed => `timeout`.
    pub deadline: Option<Instant>,
    /// Stop after this many matches; the outcome says so.
    pub max_matches: usize,
}

impl Default for SearchBudget {
    fn default() -> Self {
        SearchBudget {
            max_steps: 50_000_000,
            deadline: None,
            max_matches: 1000,
        }
    }
}

/// The result of a search.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchOutcome {
    /// Matches in document order (a parent before its children, ordered by start byte, outer
    /// before inner for equal starts).
    pub matches: Vec<Match>,
    /// True if `max_matches` stopped the search.
    pub truncated: bool,
    /// Steps consumed (for diagnostics and tests).
    pub steps_used: u64,
}

/// Run `pattern` (and the optional `rule`) over one parsed file.
///
/// `source` must be the text `parsed` was produced from, and `pattern`/`rule` must have been
/// compiled for `parsed.language` (otherwise `invalid_args`). A file with syntax errors is
/// searched like any other: nodes under `ERROR` nodes can match, an `ERROR` node itself never
/// matches a non-metavariable pattern root. Never panics; no recursion deeper than a constant
/// (the walk and the matcher are iterative or bounded by the pattern's own depth, which
/// compilation limits to 64).
pub fn search(
    parsed: &ParsedFile,
    source: &str,
    pattern: &Pattern,
    rule: Option<&CompiledRule>,
    budget: &SearchBudget,
) -> Result<SearchOutcome, ToolError> {
    matcher::search(parsed, source, pattern, rule, budget)
}
