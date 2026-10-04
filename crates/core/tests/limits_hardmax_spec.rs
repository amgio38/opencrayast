//! Table-driven coverage for CFG-04: every field of `Limits` has a compiled-in hard
//! maximum, exactly the maximum is accepted, one above is refused by name, and zero is
//! refused. Also pins the default of every field against its documented hard maximum.
#![allow(clippy::field_reassign_with_default)]
use opencrayast_core::ErrorCode;
use opencrayast_core::limits::{
    CALL_TIMEOUT_MS_HARD, Limits, MAX_FILE_BYTES_HARD, MAX_OUTPUT_BYTES_HARD, MAX_RESULTS_HARD,
    MAX_SCAN_FILES_HARD, NOTE_MAX_BYTES_HARD, PARSE_MAX_DEPTH_HARD, PARSE_MAX_NODES_HARD,
    PARSE_TIMEOUT_MS_HARD, PATH_MAX_BYTES_HARD, PATH_MAX_DEPTH_HARD, PLAN_MAX_CHANGED_BYTES_HARD,
    PLAN_MAX_EDITS_HARD, PLAN_MAX_FILES_HARD, PLAN_MAX_PLANS_HARD, PLAN_MAX_PLANS_PER_PROCESS_HARD,
    PLAN_MAX_STORE_MIB_HARD, PLAN_TTL_MINUTES_HARD,
};
use opencrayast_core::limits::{
    JOURNAL_MAX_PLAN_MIB_HARD, JOURNAL_MAX_TOTAL_MIB_HARD, JOURNAL_RETENTION_DAYS_HARD,
};

/// Field name, exported hard maximum, the default, and a setter that installs a value.
type Case = (&'static str, u64, u64, fn(&mut Limits, u64));

const CASES: [Case; 21] = [
    (
        "max_file_bytes",
        MAX_FILE_BYTES_HARD,
        4 * 1024 * 1024,
        |l, v| l.max_file_bytes = v,
    ),
    (
        "max_output_bytes",
        MAX_OUTPUT_BYTES_HARD,
        64 * 1024,
        |l, v| l.max_output_bytes = v,
    ),
    ("max_results", MAX_RESULTS_HARD, 200, |l, v| {
        l.max_results = v
    }),
    ("max_scan_files", MAX_SCAN_FILES_HARD, 5000, |l, v| {
        l.max_scan_files = v
    }),
    ("parse_timeout_ms", PARSE_TIMEOUT_MS_HARD, 2000, |l, v| {
        l.parse_timeout_ms = v
    }),
    ("parse_max_depth", PARSE_MAX_DEPTH_HARD, 512, |l, v| {
        l.parse_max_depth = v
    }),
    (
        "parse_max_nodes",
        PARSE_MAX_NODES_HARD,
        2_000_000,
        |l, v| l.parse_max_nodes = v,
    ),
    ("call_timeout_ms", CALL_TIMEOUT_MS_HARD, 10_000, |l, v| {
        l.call_timeout_ms = v
    }),
    ("plan_ttl_minutes", PLAN_TTL_MINUTES_HARD, 15, |l, v| {
        l.plan_ttl_minutes = v
    }),
    ("plan_max_files", PLAN_MAX_FILES_HARD, 50, |l, v| {
        l.plan_max_files = v
    }),
    ("plan_max_edits", PLAN_MAX_EDITS_HARD, 500, |l, v| {
        l.plan_max_edits = v
    }),
    (
        "plan_max_changed_bytes",
        PLAN_MAX_CHANGED_BYTES_HARD,
        1024 * 1024,
        |l, v| l.plan_max_changed_bytes = v,
    ),
    ("plan_max_store_mib", PLAN_MAX_STORE_MIB_HARD, 64, |l, v| {
        l.plan_max_store_mib = v
    }),
    ("plan_max_plans", PLAN_MAX_PLANS_HARD, 100, |l, v| {
        l.plan_max_plans = v
    }),
    (
        "plan_max_plans_per_process",
        PLAN_MAX_PLANS_PER_PROCESS_HARD,
        25,
        |l, v| l.plan_max_plans_per_process = v,
    ),
    (
        "journal_max_plan_mib",
        JOURNAL_MAX_PLAN_MIB_HARD,
        64,
        |l, v| l.journal_max_plan_mib = v,
    ),
    (
        "journal_retention_days",
        JOURNAL_RETENTION_DAYS_HARD,
        7,
        |l, v| l.journal_retention_days = v,
    ),
    (
        "journal_max_total_mib",
        JOURNAL_MAX_TOTAL_MIB_HARD,
        256,
        |l, v| l.journal_max_total_mib = v,
    ),
    ("note_max_bytes", NOTE_MAX_BYTES_HARD, 1024, |l, v| {
        l.note_max_bytes = v
    }),
    ("path_max_bytes", PATH_MAX_BYTES_HARD, 4096, |l, v| {
        l.path_max_bytes = v
    }),
    ("path_max_depth", PATH_MAX_DEPTH_HARD, 64, |l, v| {
        l.path_max_depth = v
    }),
];

#[test]
fn every_field_has_a_tested_hard_maximum() {
    for (name, hard, default, set) in CASES {
        // Exactly the hard maximum is accepted: no silent clamp, no off-by-one.
        let mut l = Limits::default();
        set(&mut l, hard);
        assert!(l.validate().is_ok(), "{name} at its hard max {hard}");

        // One above the maximum is refused, and the message names the field and the cap.
        //
        // `path_max_depth` is the operator-ruling EXCEPTION: it is tunable, so it is clamped
        // rather than refused. It is covered separately below, and by
        // `crates/core/tests/path_depth_tunable_spec.rs`.
        if name == "path_max_depth" {
            set(&mut l, hard + 1);
            assert!(
                l.validate().is_ok(),
                "path_max_depth is tunable, so an above-ceiling value must not be refused"
            );
            assert_eq!(
                l.clamped_path_max_depth(),
                hard,
                "path_max_depth must resolve to the ceiling the operator cannot pass"
            );
        } else {
            set(&mut l, hard + 1);
            let e = l.validate().expect_err("above hard max must be refused");
            assert_eq!(e.code, ErrorCode::InvalidArgs, "{name}");
            assert!(e.message.contains(name), "{name}: {}", e.message);
            assert!(
                e.message.contains(&hard.to_string()),
                "{name}: {}",
                e.message
            );
            assert!(!e.next.is_empty(), "{name} must say how to fix it");
        }

        // Zero is refused with the lower-bound wording.
        set(&mut l, 0);
        let e = l.validate().expect_err("zero must be refused");
        assert_eq!(e.code, ErrorCode::InvalidArgs, "{name}");
        assert!(
            e.message.contains(name) && e.message.contains("must be at least 1"),
            "{name}: {}",
            e.message
        );

        // The documented default sits inside the allowed range.
        assert!(default >= 1 && default <= hard, "{name} default {default}");

        // The table `validate` walks agrees with the case list, in declaration order.
        let t = Limits::default().table();
        let pos = t.iter().position(|(n, _, _)| *n == name).expect(name);
        assert_eq!(t[pos].2, hard, "{name}");
        assert_eq!(t[pos].1, default, "{name}");
    }
}

/// Every field of `Limits` appears in `table()` and in `CASES` exactly once.
///
/// Exhaustive destructure (no `..`): adding a field to `Limits` without naming it
/// here fails to compile — the same gate as `Limits::table`. Comparing only
/// `t.len() == CASES.len()` is **not** enough: both sides can be a fixed 21 while
/// a 22nd struct field goes unvalidated.
#[test]
fn table_covers_every_field_exactly_once() {
    let limits = Limits::default();
    let Limits {
        max_file_bytes: _,
        max_output_bytes: _,
        max_results: _,
        max_scan_files: _,
        parse_timeout_ms: _,
        parse_max_depth: _,
        parse_max_nodes: _,
        call_timeout_ms: _,
        plan_ttl_minutes: _,
        plan_max_files: _,
        plan_max_edits: _,
        plan_max_changed_bytes: _,
        plan_max_store_mib: _,
        plan_max_plans: _,
        plan_max_plans_per_process: _,
        journal_max_plan_mib: _,
        journal_retention_days: _,
        journal_max_total_mib: _,
        note_max_bytes: _,
        path_max_bytes: _,
        path_max_depth: _,
    } = &limits;

    let t = limits.table();
    assert_eq!(t.len(), CASES.len());
    for (i, ((cname, chard, cdefault, _), (tname, tval, thard))) in
        CASES.iter().zip(t.iter()).enumerate()
    {
        assert_eq!(cname, tname, "declaration order mismatch at index {i}");
        assert_eq!(chard, thard, "{cname}");
        assert_eq!(cdefault, tval, "{cname}");
    }
    let mut names: Vec<&str> = t.iter().map(|(n, _, _)| *n).collect();
    let before = names.len();
    names.sort_unstable();
    names.dedup();
    assert_eq!(names.len(), before, "duplicate field in the limits table");
}

/// Report order is stable: the first offending field in declaration order wins, so the
/// same bad configuration always produces the same message.
#[test]
fn first_violation_in_declaration_order_wins() {
    let mut l = Limits::default();
    l.max_results = 0;
    l.parse_max_nodes = PARSE_MAX_NODES_HARD + 1;
    let e = l.validate().expect_err("max_results is declared first");
    assert!(e.message.contains("max_results"), "{}", e.message);
}
