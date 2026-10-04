//! Known divergences between our matcher and ast-grep-core (PAT-05).
//!
//! Each row keeps a minimal repro. `kind` classifies the row so a disappearing
//! divergence (test failure) can be re-triaged instead of silently "fixed".
//!
//! `why` always records: pattern, source, both sides' result summary, and for
//! `Intentional` a pointer into `docs/PATTERNS.md` or `pattern/mod.rs`.

/// How we classify a recorded divergence.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// Behaviour we believe is wrong on our side (matcher / nodes_equal / …).
    OursBug,
    /// ast-grep-core behaviour that disagrees with PATTERNS.md (verified on
    /// upstream `ast-grep-language`, not an adapter miss).
    AstGrepDiff,
    /// Intentional semantic difference documented in PATTERNS.md / mod.rs.
    Intentional,
}

/// One documented divergence.
#[derive(Debug, Clone, Copy)]
pub struct KnownDivergence {
    pub id: &'static str,
    pub kind: Kind,
    pub why: &'static str,
    pub lang: opencrayast_lang::Language,
    pub pattern: &'static str,
    pub source: &'static str,
}

/// Second edition after fixing LanguageExt expando + Go/Rust pattern context (2026-10-02).
pub const KNOWN_DIVERGENCES: &[KnownDivergence] = &[
    KnownDivergence {
        id: "repeat-metavar-capture-span",
        kind: Kind::Intentional,
        why: "pattern `f($A, $A)` source `f(1, 1);`: both hit [0,7); ours captures A=[2,3) (first binding), asg captures A=[5,6) (last). PATTERNS.md «same name ⇒ structurally identical»; pattern/mod.rs «second occurrence equal to the first»; Capture reported at first-appearance span (matcher Bindings).",
        lang: opencrayast_lang::Language::JavaScript,
        pattern: "f($A, $A)",
        source: "f(1, 1);",
    },
    KnownDivergence {
        id: "ts-generic-repeated-type-capture-span",
        kind: Kind::Intentional,
        why: "pattern `function $F<$T>($A: $T) { $$$B }` source `function f<T>(a: T) { return a; }`: BOTH sides now hit the function [0,33) - FIX-1 made the tokens equal. What remains is WHICH occurrence of `$T` is reported: ours captures T=[11,12) (the type parameter, first binding), asg captures T=[17,18) (the annotation, last). Same shape as repeat-metavar-capture-span; Capture is reported at the first-appearance span (matcher Bindings).",
        lang: opencrayast_lang::Language::TypeScript,
        pattern: "function $F<$T>($A: $T) { $$$B }",
        source: "function f<T>(a: T) { return a; }",
    },
    KnownDivergence {
        id: "error-node-as-match",
        kind: Kind::Intentional,
        why: "pattern `f($X)` source `f(1;\\ngood(2);`: asg hits ERROR-rooted [0,3) with X=[2,3); ours hits []. pattern/mod.rs search: «an ERROR node itself never matches a non-metavariable pattern root».",
        lang: opencrayast_lang::Language::JavaScript,
        pattern: "f($X)",
        source: "f(1;\ngood(2);",
    },
    KnownDivergence {
        id: "asg-miss-list-ends",
        kind: Kind::AstGrepDiff,
        why: "pattern `f($A, $$$M, $Z)` source `f(1, 2, 3, 4);`: ours hits [0,13) A=1,M=[2,3],Z=4; asg hits [] (also [] with upstream ast-grep-language 0.45.3 JavaScript — not an adapter miss). asg does match the 2-arg form `f(1, 2);` (M empty).",
        lang: opencrayast_lang::Language::JavaScript,
        pattern: "f($A, $$$M, $Z)",
        source: "f(1, 2, 3, 4);",
    },
    KnownDivergence {
        id: "asg-miss-literal-dollar",
        kind: Kind::AstGrepDiff,
        why: "pattern `f($$)` source `f($);`: ours hits [0,4) (no captures); asg hits []. PATTERNS.md «`$$` = a literal `$` in the pattern»; upstream JavaScript also returns 0.",
        lang: opencrayast_lang::Language::JavaScript,
        pattern: "f($$)",
        source: "f($);",
    },
    KnownDivergence {
        id: "rust-error-broken",
        kind: Kind::Intentional,
        why: "pattern `f($X)` source `fn g() { f(1\\n}`: asg hits ERROR-rooted [9,12); ours hits []. Same rule as error-node-as-match (pattern/mod.rs: ERROR never as non-meta root).",
        lang: opencrayast_lang::Language::Rust,
        pattern: "f($X)",
        source: "fn g() { f(1\n}",
    },
    KnownDivergence {
        id: "go-error-broken",
        kind: Kind::Intentional,
        why: "pattern `f($X)` source `package p\\nfunc g() { f(1\\n}`: asg hits [21,24); ours hits []. Same ERROR-root refusal (pattern/mod.rs).",
        lang: opencrayast_lang::Language::Go,
        pattern: "f($X)",
        source: "package p\nfunc g() { f(1\n}",
    },
];
