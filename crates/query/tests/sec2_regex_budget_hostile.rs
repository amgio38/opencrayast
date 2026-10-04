//! SEC2: hostile inputs that hit pattern/regex budgets (T-09 → PAT-03).
//!
//! PAT-03: regex constraints cannot backtrack catastrophically and respect the
//! length cap. This suite feeds inputs that actually cross those caps — not
//! merely references to the constants. Step-budget binding for many match
//! candidates is included so T-09's match budget is proven the same way.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::err_expect,
    clippy::panic
)]

use opencrayast_core::ErrorCode;
use opencrayast_lang::{Language, ParseBudget, parse};
use opencrayast_query::pattern::{
    CompiledRule, Pattern, Rule, SearchBudget, VarConstraint, search,
};
use std::time::{Duration, Instant};

/// Run `f` and require it to finish inside `budget`.
///
/// A single wall-clock comparison would make this gate's verdict a function of machine
/// speed, which is what TST-1 is about: a red light whose meaning is decided by the
/// scheduler rather than by the property. So a breach is CONFIRMED, not believed: `f` runs
/// a second time and must meet the bound on the retry.
///
/// This cannot hide a real regression. "The budget check moved after the expensive work" is
/// deterministically slow - over budget every time, usually by orders of magnitude - while
/// load is transient. A retry tells those two apart; deleting the bound would not. The
/// bound itself is NOT relaxed.
fn prompt<T>(budget: Duration, what: &str, f: impl Fn() -> T) -> T {
    let started = Instant::now();
    let out = f();
    let first = started.elapsed();
    if first < budget {
        return out;
    }
    eprintln!(
        "note: {what} took {first:?} (bound {budget:?}); re-running to tell a slow machine \
         from a regression"
    );
    let started = Instant::now();
    let out = f();
    let second = started.elapsed();
    assert!(
        second < budget,
        "{what} took {first:?} then {second:?}, both over the {budget:?} bound - that is a \
         regression, not a slow machine"
    );
    out
}

const JS: Language = Language::JavaScript;

/// Documented `where` regex length cap in `crates/query/src/pattern/rules.rs`
/// (`REGEX_MAX_BYTES`). Kept here so the test names the number it is proving;
/// if the production cap moves, this test must move with it.
const REGEX_MAX_BYTES: usize = 1024;

fn parse_budget() -> ParseBudget {
    ParseBudget {
        max_bytes: 1 << 24,
        timeout: Duration::from_secs(10),
        max_depth: 8192,
        max_nodes: 10_000_000,
    }
}

/// T-09 / PAT-03 / SEC2-05: a `where` regex longer than the length cap is refused
/// at compile time — it never reaches the matcher.
///
/// Attack shape: `a` repeated `REGEX_MAX_BYTES + 1` times as the constraint regex.

#[test]
fn sec2_05_regex_length_cap_refuses_overlong_pattern() {
    let overlong = "a".repeat(REGEX_MAX_BYTES + 1);
    assert_eq!(overlong.len(), REGEX_MAX_BYTES + 1);

    let rule = Rule {
        where_: vec![(
            "$X".into(),
            VarConstraint {
                regex: Some(overlong.clone()),
                kind: None,
            },
        )],
        ..Default::default()
    };

    let err = prompt(
        Duration::from_millis(500),
        "length-cap refusal is pre-match",
        || {
            CompiledRule::compile(JS, &rule)
                .err()
                .expect("overlong where-regex must be refused at compile")
        },
    );

    let msg = err.message.to_lowercase();
    assert!(
        msg.contains("regex") && (msg.contains("at most") || msg.contains("byte")),
        "refusal must name the regex length cap: {}",
        err.message
    );

    // Mutation self-proof: a regex at the cap still compiles. If REGEX_MAX_BYTES
    // were raised (or the check removed) so that overlong also compiled, the
    // Err assert above would go red.
    let at_cap = "a".repeat(REGEX_MAX_BYTES);
    let ok_rule = Rule {
        where_: vec![(
            "$X".into(),
            VarConstraint {
                regex: Some(at_cap),
                kind: None,
            },
        )],
        ..Default::default()
    };
    assert!(
        CompiledRule::compile(JS, &ok_rule).is_ok(),
        "mutation baseline: a regex of exactly {REGEX_MAX_BYTES} bytes must still compile"
    );
}

/// T-09 / SEC2-06: the match **step** budget binds a many-candidate search.
///
/// Attack shape: pattern `foo($X)` over 20_000 `foo(a);` statements with
/// `max_steps = 100`. Each candidate root costs steps, so the walk returns
/// `[budget_exceeded]`. The same source under a widened step budget must be
/// `Ok` — proving the tight budget is what binds, not an unconditional refuse.
///
/// (A classic ReDoS regex on a short capture does **not** exercise this budget:
/// the linear-time engine finishes in microseconds either way.)
#[test]
fn sec2_06_step_budget_stops_many_match_candidates() {
    let src = "foo(a);\n".repeat(20_000);
    let parsed = parse(JS, &src, &parse_budget()).expect("source parses");
    let pattern = Pattern::compile(JS, "foo($X)").expect("pattern compiles");

    let tight = SearchBudget {
        max_steps: 100,
        deadline: None,
        max_matches: 16,
    };

    let err = prompt(
        Duration::from_secs(2),
        "step-budget refusal must be prompt",
        || {
            search(&parsed, &src, &pattern, None, &tight)
                .err()
                .expect("tight step budget must refuse many candidates")
        },
    );

    assert_eq!(err.code, ErrorCode::BudgetExceeded);
    assert!(
        err.message.to_lowercase().contains("step"),
        "message must name steps: {}",
        err.message
    );

    // Mutation self-proof: widen steps → Ok. Raising max_steps here (or removing
    // the step check in the matcher) would turn the BudgetExceeded assert red.
    let wide = SearchBudget {
        max_steps: u64::MAX,
        deadline: None,
        max_matches: 16,
    };
    let wide_out = search(&parsed, &src, &pattern, None, &wide).expect("wide budget must succeed");
    assert!(
        !wide_out.matches.is_empty(),
        "mutation baseline: wide step budget must find matches, got {}",
        wide_out.matches.len()
    );
}
