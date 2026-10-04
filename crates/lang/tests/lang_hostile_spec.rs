//! Hostile-input fuzz-style tests for the parser.
//!
//! A parse gets whatever the mutation stream produced, in all six languages, and must answer
//! either with a tree or with a budget refusal - never with a panic and never with a budget it
//! did not enforce.
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
use std::time::Duration;

const SEED: u64 = 0x00F0_0DE5_2026_1002;
const CASES: usize = 3000;

// -- The executed-fraction floor, one constant per target -----------------------------
//
// Declared here, next to the target it governs, rather than imported from a crate-wide value. All
// six languages execute 3000/3000, so this is 1.0: the measurement, not a target.
//
// The floor used to be one shared `MIN_EXECUTED_FRACTION = 0.95`, which had no force against this
// tree - a 3.3%, 10% or 30% decline all passed it while every one of these targets runs every
// case it asks for. If a language ever needs to decline cases (say, a refusal the sweep cannot
// assert on), it declares a lower value HERE, in view, with a reason; it never lowers something
// shared, which would weaken the other five languages at the same time.

/// `lang.parse[<language>]`: 3000/3000 for each of the six languages.
///
/// A parse is total: it either yields a tree whose root stays inside the source, or it refuses with
/// `budget_exceeded` or `timeout`. Both arms assert, so there is nothing for the sweep to decline
/// and no honest reason for any case to be skipped.
const LANG_PARSE_MIN_EXECUTED: f64 = 1.0;

/// Seeds: the shapes that make a parser work - deep nesting, long lines, unbalanced delimiters,
/// bytes that are not text at all.
const SOURCES: &[&str] = &[
    "",
    " ",
    "\n\n\n",
    "\0",
    "\u{feff}",
    "fn a() {}",
    "fn a() { let x = ((((1))))",
    "class A { m() { return {a: 1, b: [2, 3]} } }",
    "package main\n\nfunc main() { fmt.Println(\"x\") }",
    "def f(x):\n    return x\n",
    "type T = { a: string; b?: number };",
    "impl $T { $$$B }",
    "if (a) { b; } else { c; }",
    "a + b * c - d / e % f",
    "\u{4e2d}\u{6587}\u{5b57}\u{7b26}\u{4e32}",
    "/* comment */ // line\n",
    "\"unterminated",
    "'unterminated",
    "`unterminated",
    "(((((((((((((((((((((",
    "}}}}}}}}}}}}}}}}}}",
    "[]",
    "struct S { $$$F }",
    "trait T { $$$B }",
    "let x: Vec<Option<Result<String, Error>>> = todo!();",
    LONE_LONG_TOKEN,
];

/// A single very long token, which is where a lexer's worst case lives. A `const` because the
/// seed list is one: a long string built at runtime would be a different case every run.
const LONE_LONG_TOKEN: &str = "xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx";

/// A budget small enough that a pathological input is refused rather than allowed to run: the
/// point of the fuzz target is that the refusal happens.
fn budget() -> ParseBudget {
    ParseBudget {
        max_bytes: 64 * 1024,
        timeout: Duration::from_millis(500),
        max_depth: 256,
        max_nodes: 200_000,
    }
}

#[test]
fn parsing_hostile_source_never_panics_in_any_language() {
    for (index, language) in Language::all().iter().enumerate() {
        let language = *language;
        let index = index as u64;
        // A case is charged against ONE language per run, so the six languages each get their
        // own 3000 cases rather than sharing one stream.
        //
        // The size budget is per-run tight as well. At the 64 KiB of `budget()` the generator's
        // largest case is 568 bytes, so the `BudgetExceeded` arm below could never be entered and
        // the assertion on it was dead - the fuzz loop constrained what a refusal could say but
        // never proved the parser could produce one. A cap of 64 bytes sits inside the generated
        // distribution (490 of 3000 cases exceed it) and just under the largest case (568), so
        // both arms of the match are genuinely exercised and the refusal path is reached rather
        // than merely permitted. The metric and depth budgets stay at `budget()`'s values.
        let tight = ParseBudget {
            max_bytes: 64,
            ..budget()
        };
        fuzz::run_cases(
            &format!("lang.parse[{}]", language.id()),
            // Pair the id's length with its index in the language list. XOR-ing with the length
            // alone made `javascript` and `typescript` collide - both are 10 characters, so both
            // got the same seed and ran the identical 3000 cases, and the six-language sweep was
            // five distinct runs. The index breaks the tie; the length keeps the runs separated
            // across releases.
            SEED ^ ((language.id().len() as u64) << 32) ^ index,
            SOURCES,
            CASES,
            move |case: &Case| {
                match parse(language, &case.input, &tight) {
                    Ok(parsed) => {
                        // A parsed file is internally consistent: the root covers the text.
                        assert_eq!(parsed.language, language);
                        assert!(
                            parsed.tree.root_node().end_byte() <= case.input.len(),
                            "{}: the root runs past the source",
                            language.id()
                        );
                    }
                    Err(e) => {
                        // Both refusals the tight budget can produce, named rather than waved
                        // through: a refusal for any other reason is a defect this sweep exists
                        // to find. `FileTooLarge` is what the 64-byte cap produces, and it is
                        // here because the cap is crossed by real generated cases.
                        assert!(
                            matches!(
                                e.code,
                                ErrorCode::BudgetExceeded
                                    | ErrorCode::Timeout
                                    | ErrorCode::FileTooLarge
                            ),
                            "{}: a parse refused for an unexpected reason: {e:?}",
                            language.id()
                        );
                    }
                }
                // Both arms assert on what `parse` decided, so there is nothing to decline.
                Ran::checked()
            },
        )
        .assert_executed_fraction(LANG_PARSE_MIN_EXECUTED)
        .assert_property_fraction(LANG_PARSE_MIN_EXECUTED);
    }
}

/// Six languages must mean six runs, not five.
///
/// The sweep above derived each run's seed from the language id's length alone. `javascript` and
/// `typescript` are both 10 characters, so both derived the same seed and ran the identical 3000
/// cases - a whole language's cases were generated twice and one language was never tested at all,
/// while the sweep still printed six healthy lines. This pins the property directly: the derived
/// seeds are pairwise distinct, and so are the cases they generate.
#[test]
fn each_language_gets_its_own_seed_and_its_own_cases() {
    const BASE: u64 = 0x00F0_0DE5_2026_1002;
    let derived: Vec<(String, u64)> = Language::all()
        .iter()
        .enumerate()
        .map(|(index, &language)| {
            let seed = BASE ^ ((language.id().len() as u64) << 32) ^ (index as u64);
            (language.id().to_string(), seed)
        })
        .collect();

    for (i, (id_a, seed_a)) in derived.iter().enumerate() {
        for (id_b, seed_b) in derived.iter().skip(i + 1) {
            assert_ne!(
                seed_a, seed_b,
                "{id_a} and {id_b} derived the same seed, so their runs were the same run"
            );
        }
    }

    // Seeds differing is necessary but not sufficient: confirm the generated cases differ too,
    // since a seed collision is only harmful to the extent it reproduces the same inputs.
    let first_case = |seed: u64| fuzz::case_at(0, seed, SOURCES).input;
    for (i, (id_a, seed_a)) in derived.iter().enumerate() {
        for (id_b, seed_b) in derived.iter().skip(i + 1) {
            assert_ne!(
                first_case(*seed_a),
                first_case(*seed_b),
                "{id_a} and {id_b} share a seed-derived case stream"
            );
        }
    }
}

/// Source with a size limit and a timeout is refused by SIZE, not by being read into memory: the
/// check has to come before the parse, which is what makes an oversized file cheap.
#[test]
fn an_oversized_source_is_refused_before_parsing() {
    let big = "x".repeat(200_000);
    let Err(error) = parse(Language::Rust, &big, &budget()) else {
        panic!("a source over the byte budget must be refused");
    };
    assert_eq!(error.code, ErrorCode::FileTooLarge, "{error:?}");
    assert!(error.message.contains("bytes"), "{error}");
}

/// Deep nesting must hit the depth budget rather than the stack. This is the case that would
/// take the process down if the walk after parsing recursed.
#[test]
fn deep_nesting_is_refused_by_the_depth_budget() {
    let deep = format!("{}1{}", "(".repeat(2000), ")".repeat(2000));
    let Err(error) = parse(Language::JavaScript, &deep, &budget()) else {
        panic!("a source over the depth budget must be refused");
    };
    assert_eq!(error.code, ErrorCode::BudgetExceeded, "{error:?}");
    assert!(error.message.contains("depth"), "{error}");
}

/// The fuzz loop above is a NEGATIVE test: it constrains what a refusal may say, and lets
/// every success through. That is the right shape for "never panics, never refuses for a
/// silly reason", and on its own it has no teeth about the budgets themselves — disabling the
/// node budget leaves it green, because a parse that should have been refused simply returns
/// `Ok` and the loop waves it through. Its own `budget()` docstring says "the point of the
/// fuzz target is that the refusal happens", so the refusal is what had to be pinned.
///
/// These two pin it. Each names a budget, crosses it on purpose, and requires the refusal.
/// A node budget that is crossed is refused, and named.
///
/// Mutation self-proof: `if node_count as u64 > budget.max_nodes` in `parse.rs` becomes
/// `if false && ...` and this goes red. Before it existed, `lang_hostile_spec` was 3/3 green
/// with the node budget switched off.
#[test]
fn a_crossed_node_budget_is_refused_and_named() {
    // One node is crossed by any source with a declaration in it. Deliberately absurd, so the
    // refusal cannot be an accident of the input being marginal.
    let tight = ParseBudget {
        max_nodes: 1,
        ..budget()
    };
    let Err(error) = parse(Language::Rust, "fn a() {}", &tight) else {
        panic!("a source over a one-node budget must be refused, not parsed");
    };
    assert_eq!(error.code, ErrorCode::BudgetExceeded, "{error:?}");
    assert!(
        error.message.to_lowercase().contains("node"),
        "the refusal must name the budget that was crossed, or an operator cannot tell \\
         which knob to turn: {error}"
    );

    // And the control: the same source under a budget it fits is parsed. Without this the
    // test would also pass if the parser refused everything.
    assert!(
        parse(Language::Rust, "fn a() {}", &budget()).is_ok(),
        "the same source must parse under a budget it fits"
    );
}

/// A wall-clock budget that is crossed is refused, and named.
///
/// `Duration::ZERO` is the deterministic injection: the progress callback's first tick already
/// satisfies `elapsed >= ZERO`, so this does not wait for wall-clock expiry and load cannot
/// flip it. Mutation self-proof: the `started.elapsed() >= timeout` arm of the progress
/// callback in `parse.rs` becomes unreachable and this goes red.
#[test]
fn a_crossed_timeout_budget_is_refused_and_named() {
    // Large enough that tree-sitter invokes the progress callback, small enough to stay
    // under `budget().max_bytes` (64 KiB): a source over the byte budget is refused as
    // `file_too_large` before the parse starts, which would not exercise the timeout at all.
    let src = "fn f() { let a = 1; }\n".repeat(1_500);
    assert!(src.len() < budget().max_bytes as usize);
    let zero = ParseBudget {
        timeout: Duration::ZERO,
        ..budget()
    };
    let Err(error) = parse(Language::Rust, &src, &zero) else {
        panic!("a source parsed under a zero wall-clock budget must be refused");
    };
    assert_eq!(error.code, ErrorCode::Timeout, "{error:?}");

    // Control: a generous timeout accepts the same source.
    let generous = ParseBudget {
        timeout: Duration::from_secs(30),
        ..budget()
    };
    assert!(
        parse(Language::Rust, &src, &generous).is_ok(),
        "the same source must parse under a timeout it fits"
    );
}

/// The loop's own budget really is tight enough to be crossed — otherwise the two tests above
/// would be pinning a budget nothing in the corpus ever reaches, and "the refusal happens"
/// would be true only of the two hand-written cases.
#[test]
fn the_corpus_budget_is_tight_against_this_corpus() {
    // Deep nesting well past `budget().max_depth` (256): the corpus's own nesting seed.
    let deep = format!("{}1{}", "(".repeat(2000), ")".repeat(2000));
    assert!(
        parse(Language::JavaScript, &deep, &budget()).is_err(),
        "the depth seed must cross the loop's own budget, or the loop never sees a refusal"
    );
}

/// The tightened budget must actually refuse cases, or it has merely been made tighter.
///
/// A budget cap that never fires is the dead-assertion failure in a different costume: the sweep
/// would constrain what a refusal may say while never producing one. This pins the count so a
/// future edit that widens `max_bytes` back to 64 KiB - or narrows it past the whole generated
/// distribution - goes red instead of silently restoring an unexercised arm.
#[test]
fn the_tight_budget_refuses_a_real_share_of_the_generated_cases() {
    const BASE: u64 = 0x00F0_0DE5_2026_1002;
    let tight = ParseBudget {
        max_bytes: 64,
        ..budget()
    };
    let mut refused = 0usize;
    let mut parsed = 0usize;
    for index in 0..CASES {
        let seed = BASE ^ ((Language::Rust.id().len() as u64) << 32);
        let case = fuzz::case_at(index, seed, SOURCES);
        match parse(Language::Rust, &case.input, &tight) {
            Ok(_) => parsed += 1,
            Err(_) => refused += 1,
        }
    }
    assert_eq!(
        refused + parsed,
        CASES,
        "every generated case took exactly one branch"
    );
    assert!(
        refused > 0,
        "no case crossed the 64-byte cap, so the refusal arm of the sweep is unreachable"
    );
    assert!(
        parsed > 0,
        "every case was refused, so the success arm of the sweep is unreachable"
    );
}
