//! Hostile-input fuzz-style tests for patterns, rules and outlines.
//!
//! Three targets: a pattern that has to compile or refuse, a compiled pattern that then has to
//! search a fixed corpus inside a budget, and an outline of whatever the mutation produced.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::field_reassign_with_default
)]

mod common;

use common::fuzz::{self, Case, Ran};
use opencrayast_core::ErrorCode;
use opencrayast_lang::{Language, ParseBudget, parse};
use opencrayast_query::OutlineOptions;
use opencrayast_query::outline;
use opencrayast_query::pattern::{
    CompiledRule, Pattern, Rule, SearchBudget, VarConstraint, search,
};
use std::time::Duration;

const SEED: u64 = 0x00F0_0DE5_2026_1002;
const CASES: usize = 3000;

// -- The executed-fraction floor, one constant per target -----------------------------
//
// One constant per target, declared here rather than imported from a crate-wide value. All four
// measure 3000/3000, so each is 1.0 - the measurement, not a target.
//
// The floor used to be one shared `MIN_EXECUTED_FRACTION = 0.95`, which admitted a 3.3%, 10% or 30%
// decline without complaint while every target here executes every case it asks for. A target that
// must genuinely decline cases declares a LOWER value in this file, next to the target, with a
// reason attached - never by editing something shared and weakening the other three with it.

/// `query.pattern_compile[<language>]`: 3000/3000 for each of the six languages.
///
/// Compilation is total, and both outcomes are asserted: a compiled pattern is checked against its
/// own source and language, and a refusal is checked for a bounded message. Neither arm declines.
const QUERY_PATTERN_COMPILE_MIN_EXECUTED: f64 = 1.0;

/// `query.search`: 3000/3000. A compiled pattern searches the corpus inside its budget, and a
/// refusal for the budget is an asserted outcome rather than a declined case.
const QUERY_SEARCH_MIN_EXECUTED: f64 = 1.0;

/// `query.rules`: 3000/3000. Rules compile, refuse, or match in order and in range; all three arms
/// assert.
const QUERY_RULES_MIN_EXECUTED: f64 = 1.0;

/// `query.outline`: 3000/3000.
const QUERY_OUTLINE_MIN_EXECUTED: f64 = 1.0;

/// Seeds: fragments a caller would write, including the ones that are one character away from
/// working.
const PATTERNS: &[&str] = &[
    "",
    " ",
    "foo($$$ARGS)",
    "foo($X)",
    "$X",
    "$$$",
    "$_",
    "$x",
    "$$",
    "$$$X",
    "a + b",
    "a + ",
    "(((",
    ")))",
    "fn $NAME() { $$$B }",
    "class $C { $$$B }",
    "return $X;",
    "if ($C) { $$$B }",
    "for ($X of $Y) { $$$B }",
    "\"$X\"",
    "/* $X */",
    "package main",
    "func main() { $$$B }",
    "def $F($A):\n    $$$B\n",
    "type $T = { a: string }",
    "\\u{1b}$X",
    "console.log($X);",
    "$X.$Y",
    "\u{4e2d}\u{6587}($X)",
];

/// A small, fixed corpus to search. Small on purpose: the target is the search machinery, and a
/// big corpus would make 3000 searches slow without testing anything more.
const CORPUS_JS: &str = "\
function test_a(x) { return x + 1; }
class C { m() { return 2; } }
const k = { a: 1, b: [2, 3] };
test_a(k);
";

const CORPUS_RS: &str = "\
pub fn alpha(x: i32) -> i32 { x }
struct S { a: u8 }
impl S { fn beta(&self) -> u8 { self.a } }
";

fn parse_budget() -> ParseBudget {
    ParseBudget {
        max_bytes: 1 << 20,
        timeout: Duration::from_secs(5),
        max_depth: 1024,
        max_nodes: 1_000_000,
    }
}

fn search_budget() -> SearchBudget {
    SearchBudget {
        max_steps: 2_000_000,
        deadline: None,
        max_matches: 50,
    }
}

/// A pattern either compiles or refuses with `invalid_pattern`; whatever it does, it must not
/// panic and must not take the per-case ceiling with it.
#[test]
fn compiling_hostile_patterns_never_panics() {
    for (index, &language) in [
        Language::JavaScript,
        Language::TypeScript,
        Language::Tsx,
        Language::Python,
        Language::Go,
        Language::Rust,
    ]
    .iter()
    .enumerate()
    {
        fuzz::run_cases(
            &format!("query.pattern_compile[{}]", language.id()),
            // The id's length alone collides: `javascript` and `typescript` are both 10
            // characters, so those two entries ran the identical 3000 cases and the six-language
            // sweep was five distinct runs. Pair the length with the position instead.
            SEED ^ ((language.id().len() as u64) << 32) ^ (index as u64),
            PATTERNS,
            CASES,
            move |case: &Case| {
                match Pattern::compile(language, &case.input) {
                    Ok(p) => {
                        // A compiled pattern is consistent with its own source.
                        assert_eq!(p.source(), case.input);
                        assert_eq!(p.language(), language);
                    }
                    Err(e) => assert!(
                        e.message.len() < 400,
                        "{}: a refusal that quotes the whole input: {:?}",
                        language.id(),
                        e.message
                    ),
                }
                // Compilation is total and both outcomes are asserted on, so this sweep has
                // nothing to decline.
                Ran::checked()
            },
        )
        .assert_executed_fraction(QUERY_PATTERN_COMPILE_MIN_EXECUTED);
    }
}

/// A pattern that compiled has to be usable: searching a small corpus with it must finish
/// inside the search budget, whether it matches, does not match, or runs out of steps.
#[test]
fn a_compiled_hostile_pattern_searches_inside_the_budget() {
    let js = parse(Language::JavaScript, CORPUS_JS, &parse_budget()).unwrap();
    let rs = parse(Language::Rust, CORPUS_RS, &parse_budget()).unwrap();
    fuzz::run_cases("query.search", SEED, PATTERNS, CASES, move |case: &Case| {
        if let Err(e) = Pattern::compile(Language::JavaScript, &case.input) {
            assert!(
                e.message.len() < 400,
                "case {}: a refusal that quotes the whole pattern: {:?}",
                case.index,
                e.message,
            );
        }
        if let Ok(p) = Pattern::compile(Language::JavaScript, &case.input) {
            match search(&js, CORPUS_JS, &p, None, &search_budget()) {
                Ok(outcome) => {
                    // Matches are ordered and inside the corpus: a report an agent would act
                    // on has to point at real text.
                    let mut previous = 0usize;
                    for m in &outcome.matches {
                        assert!(m.start_byte >= previous, "matches are out of order");
                        assert!(m.end_byte <= CORPUS_JS.len(), "a match past the source");
                        previous = m.start_byte;
                    }
                    assert!(outcome.matches.len() <= 50);
                }
                Err(e) => assert_eq!(e.code, ErrorCode::BudgetExceeded, "{e:?}"),
            }
        }
        if let Ok(p) = Pattern::compile(Language::Rust, &case.input) {
            match search(&rs, CORPUS_RS, &p, None, &search_budget()) {
                Ok(_) => {}
                Err(e) => assert_eq!(e.code, ErrorCode::BudgetExceeded, "{e:?}"),
            }
        }
        // A pattern that does not compile cannot be searched, and refusing to compile is the
        // overwhelming majority of what this mutation stream produces. Treating that as a skip
        // meant 2435 of 3000 cases reported nothing while the harness said "3000 cases", so the
        // refusal is asserted here instead: what a refusal must not be is a panic, and its
        // message must not quote the whole input back at the caller.
        Ran::checked()
    })
    .assert_executed_fraction(QUERY_SEARCH_MIN_EXECUTED);
}

/// Rules go through the same mutation stream, and the regex in a `where` must not be able to
/// backtrack: the linear-time engine either compiles the expression or refuses it.
#[test]
fn hostile_rules_compile_refuse_and_stay_linear() {
    fuzz::run_cases("query.rules", SEED, PATTERNS, CASES, |case: &Case| {
        // Half the cases use a well-formed key and half a malformed one, so both the
        // refusal path and the accepted path are exercised.
        let name = if case.index.is_multiple_of(2) {
            "$X"
        } else {
            "X"
        };
        let rule = Rule {
            kind: Some(case.input.clone()),
            where_: vec![(
                name.to_string(),
                VarConstraint {
                    regex: Some(case.input.clone()),
                    kind: None,
                },
            )],
            ..Default::default()
        };
        // Any refusal is fine, as long as it is a refusal and not a panic: the point of this
        // target is totality, not a particular wording.
        match CompiledRule::compile(Language::JavaScript, &rule) {
            Ok(compiled) => {
                // A rule that did compile still has to be cheap to run: this is where a
                // catastrophic pattern would show up.
                let src = parse(Language::JavaScript, CORPUS_JS, &parse_budget()).unwrap();
                let p = Pattern::compile(Language::JavaScript, "test_a($X)").unwrap();
                let outcome = search(&src, CORPUS_JS, &p, Some(&compiled), &search_budget());
                // The result used to be discarded into `let _ = ...`, which made this target assert
                // nothing at all beyond "did not panic": a rule that matched every node, or a search
                // that failed for a reason unrelated to the budget, both passed silently.
                //
                // What is legitimate here: Ok, or a refusal that NAMES itself. What is not: a panic,
                // and an `io_error` or `outside_workspace` from a fixed in-memory corpus, which would
                // mean the rule did something other than evaluate.
                match outcome {
                    Ok(_) => {}
                    Err(e) => assert!(
                        matches!(
                            e.code,
                            ErrorCode::BudgetExceeded
                                | ErrorCode::Timeout
                                | ErrorCode::InvalidPattern
                        ),
                        "case {} ({}) produced an unexpected refusal: {:?}\nrule: {rule:?}",
                        case.index,
                        case.repro(),
                        e,
                    ),
                }
            }
            // A refusal is a result to check, not a reason to bow out. The comment above this target
            // says the point is totality rather than a particular wording, and it is: what a refusal
            // must NOT be is a panic, and what a compile must NOT do is accept something that would
            // then fail for an unrelated reason. Before this arm existed the refusal path was dropped
            // on the floor, so 2887 of 3000 cases asserted nothing at all while the harness reported
            // "3000 cases".
            Err(e) => {
                assert!(
                    e.message.len() < 400,
                    "case {}: a refusal that quotes the whole rule: {:?}\n{}",
                    case.index,
                    e.message,
                    case.repro(),
                );
            }
        }
        Ran::checked()
    })
    .assert_executed_fraction(QUERY_RULES_MIN_EXECUTED);
}

/// A regex that the engine accepts must not blow up on a long input. The classic catastrophic
/// patterns are the ones to try, and the ceiling per case is what proves it.
#[test]
fn a_catastrophic_regex_is_either_refused_or_linear() {
    for regex in ["(a+)+$", "(a*)*b", "(a|a)*$", "^(a+)*b", "(x+x+)+y"] {
        let rule = Rule {
            where_: vec![(
                "$X".to_string(),
                VarConstraint {
                    regex: Some(regex.to_string()),
                    kind: None,
                },
            )],
            ..Default::default()
        };
        let compiled = match CompiledRule::compile(Language::JavaScript, &rule) {
            Ok(c) => c,
            // Refused at compile time is the best possible answer.
            Err(_) => continue,
        };

        // The regex has to be MEASURED against the input that would hurt it. A 5-line corpus is
        // the worst possible probe for `(a+)+$`: the capture it runs against is a few characters
        // long, so the pattern is linear over it and proves nothing about a long run.
        //
        // Two measurements, both against a long input, because they fail differently:
        //
        // 1. the regex itself, over a 4000-character `aaaa...b` — this is the one that would hang
        //    a backtracking engine;
        // 2. the same rule through the real `search` path, over a source with a genuinely long
        //    capture.
        //
        // The source stays UNDER the parse budget (1 MiB): an earlier version repeated the line
        // 500 times and the parse refused it as `file_too_large`, so the assertion it was meant
        // to make never ran. `REPEATS` is chosen to make the capture long without tripping it.
        const REPEATS: usize = 40;
        let long = format!("{}b", "a".repeat(4000));
        let hostile_source = format!(
            "function test_a(x) {{ return x; }}\n{}\n",
            format!("// {}\n", long).repeat(REPEATS)
        );
        let src = parse(Language::JavaScript, &hostile_source, &parse_budget())
            .unwrap_or_else(|e| panic!("the hostile source must parse ({REPEATS} repeats): {e:?}"));

        // The engine used by `where` regexes: the linear-time one. Exercising it directly is what
        // makes this a measurement of the REGEX, rather than of whatever the search happened to do
        // with a capture elsewhere. (The query crate sets explicit size limits on top; those only
        // make refusal more likely, so the default builder is the weaker thing to measure.)
        match regex::RegexBuilder::new(regex).build() {
            // Refused to compile: the best possible answer, and the reason the query crate uses
            // this engine in the first place.
            Err(_) => continue,
            Ok(re) => {
                let started = std::time::Instant::now();
                let m = re.is_match(&long);
                let took = started.elapsed();
                assert!(
                    took < Duration::from_secs(2),
                    "{regex} took {took:?} on a {}-byte input (match={m})",
                    long.len()
                );
                // And the same pattern against the same input through the real query path, so the
                // guarantee is not only about the regex crate in isolation.
                let p = Pattern::compile(Language::JavaScript, "test_a($X)").unwrap();
                let started = std::time::Instant::now();
                let outcome = search(&src, &hostile_source, &p, Some(&compiled), &search_budget());
                assert!(
                    started.elapsed() < Duration::from_secs(2),
                    "{regex} through search took {:?}",
                    started.elapsed()
                );
                if let Err(e) = outcome {
                    assert_eq!(e.code, ErrorCode::BudgetExceeded, "{regex}: {e:?}");
                }
            }
        }
    }
}

/// An outline of whatever the stream produced: whatever comes back, no panic and no name that
/// came from nowhere.
#[test]
fn outlining_hostile_source_never_panics() {
    fuzz::run_cases("query.outline", SEED, PATTERNS, CASES, |case: &Case| {
        let mut skipped_languages = 0usize;
        for language in [
            Language::JavaScript,
            Language::Rust,
            Language::Python,
            Language::Go,
        ] {
            let Ok(parsed) = parse(language, &case.input, &parse_budget()) else {
                // A language that cannot parse the mutated text has no outline to check. The
                // loop keeps going so the other languages still run.
                skipped_languages += 1;
                continue;
            };
            let symbols = outline(
                &parsed,
                &case.input,
                &OutlineOptions {
                    max_depth: 3,
                    include_docs: true,
                    ..OutlineOptions::default()
                },
            );
            for symbol in &symbols {
                assert!(!symbol.name.is_empty(), "{}: an empty name", language.id());
                assert!(
                    symbol.start_line >= 1 && symbol.start_line <= symbol.end_line,
                    "{}: impossible extent {}..{}",
                    language.id(),
                    symbol.start_line,
                    symbol.end_line
                );
                assert!(
                    symbol.end_byte <= case.input.len(),
                    "{}: an extent past the source",
                    language.id()
                );
            }
        }
        // Four languages, and one case is only worth a skip when none of them parsed the input.
        if skipped_languages == 4 {
            return Ran::skip("none of the four languages parsed the mutated text");
        }
        Ran::checked()
    })
    .assert_executed_fraction(QUERY_OUTLINE_MIN_EXECUTED);
}
