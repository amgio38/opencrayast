//! SEC2: hostile inputs that actually hit parse budgets (PRS-01..04).
//!
//! These are not "the field is referenced" checks. Each case builds an input that
//! crosses one budget and asserts the documented error code. Mutation self-proof
//! is inlined: the same source under a widened budget must succeed, so widening
//! the tight budget under test would turn the refusal assertion red.
//!
//! Timeout uses `Duration::ZERO` (cancel on the first tree-sitter progress
//! callback). That is deterministic and load-independent: it does not wait for
//! wall-clock expiry.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::err_expect,
    clippy::panic
)]

use opencrayast_core::ErrorCode;
use opencrayast_lang::{Language, ParseBudget, parse};
use std::time::{Duration, Instant};

/// Run `f` and require it to finish inside `budget`.
///
/// A single wall-clock comparison would make this gate's verdict a function of machine
/// speed, which is exactly what TST-1 is about: a red light whose meaning is decided by the
/// scheduler rather than by the property. So a breach is CONFIRMED, not believed: `f` runs
/// a second time and must meet the bound on the retry.
///
/// This cannot hide a real regression. "The budget check moved after the expensive work" is
/// deterministically slow - over budget every time, usually by orders of magnitude - while
/// load is transient. A retry tells those two apart; deleting the bound would not.
fn prompt<T>(budget: Duration, what: &str, f: impl Fn() -> T) -> T {
    let started = Instant::now();
    let out = f();
    let first = started.elapsed();
    if first < budget {
        return out;
    }
    eprintln!(
        "note: {what} took {first:?} (bound {budget:?}); re-running to tell a slow machine from a regression"
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

fn base_budget() -> ParseBudget {
    ParseBudget {
        max_bytes: 16 * 1024 * 1024,
        timeout: Duration::from_secs(30),
        max_depth: 512,
        max_nodes: 2_000_000,
    }
}

/// PRS-01 / SEC2-01: deep nesting is refused by the depth budget, not by stack overflow.
///
/// Attack shape: `fn f() { let x = ((((…1…)))); }` with thousands of '(' / ')'.
/// Run on a deliberately small stack so a recursive walk would blow up first;
/// the iterative walk returns `[budget_exceeded]` naming depth instead.
#[test]
fn sec2_01_depth_budget_stops_pathological_nesting() {
    let n = 5_000;
    let src = format!(
        "fn f() {{ let x = {}1{}; }}\n",
        "(".repeat(n),
        ")".repeat(n)
    );
    let mut tight = base_budget();
    tight.max_depth = 64;

    let depth_probe = || {
        let src_for_thread = src.clone();
        let tight_for_thread = tight.clone();
        let handle = std::thread::Builder::new()
            .name("sec2-01-depth".into())
            .stack_size(128 * 1024)
            .spawn(move || parse(Language::Rust, &src_for_thread, &tight_for_thread))
            .expect("spawn depth probe");
        handle
            .join()
            .expect("depth walk must not overflow a 128 KiB stack")
            .err()
            .expect("tight depth budget must refuse")
    };
    let err = prompt(
        Duration::from_secs(5),
        "depth refusal must be prompt",
        depth_probe,
    );

    assert_eq!(err.code, ErrorCode::BudgetExceeded);
    assert!(
        err.message.to_lowercase().contains("depth"),
        "message must name depth: {}",
        err.message
    );

    // Mutation self-proof: widen depth → same input is accepted. If the depth
    // check were removed (or this test's max_depth raised to u64::MAX), the
    // refusal assert above would go red.
    let mut wide = tight;
    wide.max_depth = u64::MAX;
    assert!(
        parse(Language::Rust, &src, &wide).is_ok(),
        "mutation baseline: wide depth budget must accept this source"
    );
}

/// PRS-02 / SEC2-02: a huge token / oversized source hits the size budget with
/// `[file_too_large]` before any parse work starts.
///
/// Attack shape: one enormous string literal whose byte length exceeds `max_bytes`.
#[test]
fn sec2_02_size_budget_refuses_oversized_source() {
    let payload = "x".repeat(64 * 1024);
    let src = format!("let s = \"{payload}\";\n");
    assert!(src.len() > 32 * 1024);

    let mut tight = base_budget();
    tight.max_bytes = 32 * 1024;

    let err = prompt(
        Duration::from_millis(200),
        "size check is pre-parse",
        || {
            parse(Language::Rust, &src, &tight)
                .err()
                .expect("oversize must be refused")
        },
    );

    assert_eq!(err.code, ErrorCode::FileTooLarge);

    // Mutation self-proof: raise max_bytes to fit → parse succeeds (or at least
    // is not file_too_large). Widening this test's max_bytes would turn the
    // FileTooLarge assert red.
    let mut wide = tight;
    wide.max_bytes = src.len() as u64;
    wide.timeout = Duration::from_secs(30);
    let wide_result = parse(Language::Rust, &src, &wide);
    assert!(
        wide_result.is_ok()
            || wide_result
                .as_ref()
                .err()
                .is_some_and(|e| e.code != ErrorCode::FileTooLarge),
        "mutation baseline: wide size budget must not return file_too_large"
    );
}

/// PRS-03 / SEC2-03: many small nodes hit the node budget with `[budget_exceeded]`.
///
/// Attack shape: a long JS program of `a;\n` repeated — each statement is a handful
/// of AST nodes, so node count grows with length while depth stays shallow.
#[test]
fn sec2_03_node_budget_stops_many_small_nodes() {
    let src = "a;\n".repeat(50_000);
    let mut tight = base_budget();
    tight.max_nodes = 2_000;

    let err = prompt(
        Duration::from_secs(5),
        "node walk must stop at first breach",
        || {
            parse(Language::JavaScript, &src, &tight)
                .err()
                .expect("tight node budget must refuse")
        },
    );

    assert_eq!(err.code, ErrorCode::BudgetExceeded);
    assert!(
        err.message.to_lowercase().contains("node"),
        "message must name node: {}",
        err.message
    );

    // Mutation self-proof: widen nodes → accepted. Raising max_nodes here to
    // u64::MAX would turn the refusal assert red.
    let mut wide = tight;
    wide.max_nodes = u64::MAX;
    assert!(
        parse(Language::JavaScript, &src, &wide).is_ok(),
        "mutation baseline: wide node budget must accept this source"
    );
}

/// PRS-04 / SEC2-04: the wall-clock budget cancels with `[timeout]`.
///
/// Deterministic injection: `timeout = Duration::ZERO` makes the progress callback
/// cancel on its first tick (`elapsed >= ZERO` is always true). No sleep, no
/// "wait and see" — load cannot flip this green/red.
///
/// Attack shape: a multi-kilobyte Rust source large enough that tree-sitter invokes
/// the progress callback at least once.
#[test]
fn sec2_04_timeout_cancels_via_zero_deadline() {
    let src = "fn f() { let a = 1; }\n".repeat(8_000);
    let mut tight = base_budget();
    tight.timeout = Duration::ZERO;

    let err = prompt(
        Duration::from_secs(2),
        "zero-deadline cancel must be prompt",
        || {
            parse(Language::Rust, &src, &tight)
                .err()
                .expect("zero timeout must cancel")
        },
    );

    assert_eq!(err.code, ErrorCode::Timeout);

    // Mutation self-proof: a generous timeout parses the same source. If this
    // test's timeout were raised to many seconds (or the cancel check removed),
    // the Timeout assert above would go red.
    let mut wide = tight;
    wide.timeout = Duration::from_secs(30);
    assert!(
        parse(Language::Rust, &src, &wide).is_ok(),
        "mutation baseline: generous timeout must accept this source"
    );
}
