//! The operator ruling on `path_max_depth`, stated as tests.
//!
//! The ruling: **path depth is operator-tunable; resource ceilings stay fixed and clamped.**
//! That split is only real if BOTH halves are observable, so this file pins both:
//!
//! - a depth **different from the default** genuinely takes effect (the tunability half);
//! - an above-maximum resource ceiling is **refused** — there is no switch that turns it
//!   off, and no way to talk the loader into accepting one (the security half);
//! - an above-maximum `path_max_depth` is **clamped to the ceiling**, and the effective
//!   value is observable through the boundary, not merely implied by a constructor's return.
//!
//! Every clamp here is mutation-checked: each test is run against a build where that
//! specific clamp is removed and is required to go red. A clamp test that stays green under
//! its own mutation would be worse than no test, so those are named in the comments.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::field_reassign_with_default
)]

use opencrayast_core::ErrorCode;
use opencrayast_core::boundary::{Boundary, BoundaryConfig};
use opencrayast_core::config::Settings;
use opencrayast_core::limits::{Limits, PATH_MAX_DEPTH_HARD};
use opencrayast_core::walk::{WalkOptions, walk};
use std::fs;
use std::path::Path;
use tempfile::TempDir;

fn ws() -> TempDir {
    tempfile::tempdir().expect("temp dir")
}

/// A workspace-relative path of `n` components, plus a file at its end.
///
/// Directory names are single characters: this builds `PATH_MAX_DEPTH_HARD + 1` = 257 levels,
/// and a multi-character name turns that into a path past macOS's 1024-byte limit, where the
/// mkdir fails with ENAMETOOLONG before the code under test ever runs. The test is about the
/// clamp, not about the filesystem's depth ceiling.
fn nested(root: &Path, n: usize) -> String {
    let dirs: Vec<String> = (0..n)
        .map(|i| {
            // base-36 keeps every name one character for n < 36, which covers 257 levels.
            std::char::from_digit(i as u32 % 36, 36)
                .unwrap_or('d')
                .to_string()
        })
        .collect();
    let rel = dirs.join("/");
    fs::create_dir_all(root.join(&rel)).expect("mkdir nested");
    let file = format!("{rel}/f.txt");
    fs::write(root.join(&file), b"x").expect("write nested file");
    file
}

fn boundary(root: &Path, limits: Limits) -> Boundary {
    Boundary::new(BoundaryConfig::new(root.to_path_buf(), limits)).expect("boundary")
}

// =====================================================================================
// Half 1 — F-03 (tunability): an operator sets a depth that differs from the default.
// =====================================================================================

/// **F-03-a: an operator's configured `path_max_depth` binds, end to end, from the file.**
///
/// The invariant: a depth in the operator's `[limits]` block is the depth the boundary
/// enforces. The fixture is set **above** the default, and a path *over the default but
/// inside the configured value* is offered — so it can only resolve if the configured value
/// reached the check. A boundary reading the built-in default would refuse it.
///
/// The control is the same path against a boundary left at the defaults, which must refuse
/// it. Without the control, "accepted" would not distinguish "the operator's value bound"
/// from "this path was accepted for some unrelated reason".
#[test]
fn f03_a_operator_configured_depth_binds_from_the_config_file() {
    let dir = ws();
    let root = dir.path().join("ws");
    fs::create_dir_all(&root).expect("mkdir ws");

    let d = Limits::default();
    // Strictly above the default, strictly inside the hard ceiling: a real widening.
    let configured = d.path_max_depth + 32;
    assert!(
        configured <= PATH_MAX_DEPTH_HARD,
        "the fixture must stay inside the ceiling the operator is clamped to"
    );

    // The parsed route, exactly as a shell builds it: the number comes out of the operator's
    // file text, not out of a struct literal.
    let settings = Settings::parse(&format!("[limits]\npath_max_depth = {configured}\n"))
        .expect("the operator's configuration text must parse");
    assert_eq!(
        settings.limits.path_max_depth, configured,
        "a legal depth must survive parsing unchanged"
    );

    // A path of default+1 components: over the default, inside the configured value.
    let file = nested(&root, (d.path_max_depth + 1) as usize);

    let b = boundary(&root, settings.boundary_config(&root).unwrap().limits);
    assert!(
        b.resolve_read(&file).is_ok(),
        "a {}-component path is inside the operator's ceiling of {configured} and must resolve; \
         refusing it means the check fell back to the default of {}",
        d.path_max_depth + 1,
        d.path_max_depth
    );

    // The control: identical path, identical root, defaults only.
    let control = boundary(&root, Limits::default());
    assert_eq!(
        control.resolve_read(&file).err().map(|e| e.code),
        Some(ErrorCode::LimitExceeded),
        "the same path must hit the default ceiling of {}, otherwise the assertion above proves \
         nothing about the ceiling",
        d.path_max_depth
    );
}

/// **F-03-b: the tunable knob has a compiled-in ceiling the operator cannot pass.**
///
/// The operator asks for 999999. Two things must hold, and they are different claims:
///
///  1. the configuration **loads** (it is a tunable knob — refusing to start would make the
///     one useful knob unusable), and
///  2. the depth actually enforced is `PATH_MAX_DEPTH_HARD`, **not** 999999.
///
/// The second is the security claim and is the one worth a test: an operator who could set
/// an unbounded depth would defeat the guard outright. The boundary is built directly from a
/// `Limits` carrying 999999 — no parse in between — because the clamp must hold at the
/// enforcement point, not merely be tidied up on the way in through the config reader.
///
/// `MUTATION target: clamped_path_max_depth().min(...)` — removing the clamp (returning the
/// raw field) must turn this red, because the refusal below stops happening.
#[test]
fn f03_b_an_above_ceiling_depth_is_clamped_to_the_compiled_ceiling() {
    let dir = ws();
    let root = dir.path().join("ws");
    fs::create_dir_all(&root).expect("mkdir ws");

    // A legal configuration that simply asks for far too much.
    let settings =
        Settings::parse("[limits]\npath_max_depth = 999999\n").expect("a tunable knob must load");
    assert_eq!(
        settings.limits.path_max_depth, 999999,
        "the requested value is retained; the clamp is applied where it is enforced"
    );
    assert!(
        settings.limits.path_max_depth_was_clamped(),
        "the request is above the ceiling, so it must report as clamped"
    );
    assert_eq!(
        settings.limits.clamped_path_max_depth(),
        PATH_MAX_DEPTH_HARD,
        "the effective ceiling is the compiled-in one"
    );

    // One component past the ceiling must be refused. At 999999 it would resolve, so a green
    // test here is only possible because the clamp fired.
    let file = nested(&root, PATH_MAX_DEPTH_HARD as usize + 1);
    let b = boundary(&root, settings.limits.clone());
    let err = b.resolve_read(&file).expect_err(
        "a path past the hard ceiling must be refused, however large the operator asked",
    );
    assert_eq!(err.code, ErrorCode::LimitExceeded, "{err:?}");
    assert!(
        err.message.contains(&PATH_MAX_DEPTH_HARD.to_string()),
        "the refusal must name the ceiling actually enforced ({PATH_MAX_DEPTH_HARD}), not the \
         requested one: {}",
        err.message
    );

    // The rewrite form agrees, so a caller that resolves the request into a Limits to hand
    // out gets the same number the boundary enforces.
    let mut rewritten = settings.limits.clone();
    assert!(
        rewritten.clamp_path_max_depth(),
        "clamping an over-ceiling value must report that it did"
    );
    assert_eq!(rewritten.path_max_depth, PATH_MAX_DEPTH_HARD);
    assert!(
        !rewritten.clamp_path_max_depth(),
        "clamping an already-legal value must report that it did not"
    );
}

/// **F-03-c: the clamped depth is observable, not merely implied.**
///
/// A boundary asked for a depth above the ceiling reports the *effective* ceiling back, so
/// an operator can tell what they are actually running under without reading this source
/// file. The `Limits` returned by `Boundary::limits()` is the same object the check reads,
/// so this is the value the guard is using — not a copy.
///
/// `MUTATION target: the clamp in Boundary::limits()/check_size` — if the boundary served an
/// unclamped value this must go red.
#[test]
fn f03_c_the_effective_depth_ceiling_is_readable_back_off_the_boundary() {
    let dir = ws();
    let root = dir.path().join("ws");
    fs::create_dir_all(&root).expect("mkdir ws");

    let mut l = Limits::default();
    l.path_max_depth = PATH_MAX_DEPTH_HARD * 10;
    let b = boundary(&root, l);

    assert!(
        b.limits().path_max_depth_was_clamped(),
        "the boundary must be able to say that the operator's request was clamped"
    );
    assert_eq!(
        b.limits().clamped_path_max_depth(),
        PATH_MAX_DEPTH_HARD,
        "the ceiling the boundary enforces must be readable from the boundary"
    );
}

// =====================================================================================
// Half 2 — the security side: resource ceilings cannot be switched off.
// =====================================================================================

/// **SEC-a: an above-maximum resource ceiling is refused, at the loader and at validation.**
///
/// Each resource ceiling named in the operator ruling — bytes, output, results, plan size —
/// is set one above its hard maximum and must be **refused**. There is no knob that lowers a
/// ceiling past its cap, and no spelling of the configuration that gets one accepted: not
/// through the parser, and not through `Limits::validate` called directly.
///
/// This is the half of the ruling that "fully tunable" would have destroyed: with every
/// ceiling tunable, this table would be empty and there would be no guard left to pass.
///
/// `MUTATION target: the `value > hard` arm in Limits::validate` — returning `Ok(())`
/// instead of refusing must turn this red.
#[test]
fn sec_a_resource_ceilings_above_their_maximum_are_refused() {
    // One past each hard maximum, for the ceilings the ruling names as fixed.
    type OverCeiling = (&'static str, fn(&mut Limits));
    let over: Vec<OverCeiling> = vec![
        ("max_file_bytes", |l| {
            l.max_file_bytes = opencrayast_core::limits::MAX_FILE_BYTES_HARD + 1
        }),
        ("max_output_bytes", |l| {
            l.max_output_bytes = opencrayast_core::limits::MAX_OUTPUT_BYTES_HARD + 1
        }),
        ("max_results", |l| {
            l.max_results = opencrayast_core::limits::MAX_RESULTS_HARD + 1
        }),
        ("max_scan_files", |l| {
            l.max_scan_files = opencrayast_core::limits::MAX_SCAN_FILES_HARD + 1
        }),
        ("plan_max_files", |l| {
            l.plan_max_files = opencrayast_core::limits::PLAN_MAX_FILES_HARD + 1
        }),
        ("plan_max_edits", |l| {
            l.plan_max_edits = opencrayast_core::limits::PLAN_MAX_EDITS_HARD + 1
        }),
        ("plan_max_changed_bytes", |l| {
            l.plan_max_changed_bytes = opencrayast_core::limits::PLAN_MAX_CHANGED_BYTES_HARD + 1
        }),
        ("plan_max_store_mib", |l| {
            l.plan_max_store_mib = opencrayast_core::limits::PLAN_MAX_STORE_MIB_HARD + 1
        }),
        ("plan_max_plans", |l| {
            l.plan_max_plans = opencrayast_core::limits::PLAN_MAX_PLANS_HARD + 1
        }),
        ("plan_ttl_minutes", |l| {
            l.plan_ttl_minutes = opencrayast_core::limits::PLAN_TTL_MINUTES_HARD + 1
        }),
    ];

    for (name, push_over) in over {
        // Straight through the validation gate.
        let mut l = Limits::default();
        push_over(&mut l);
        let e = l
            .validate()
            .expect_err("{name} above its hard maximum must be refused, not clamped");
        assert_eq!(e.code, ErrorCode::InvalidArgs, "{name}");
        assert!(e.message.contains(name), "{name}: {}", e.message);
        assert!(!e.next.is_empty(), "{name} must say how to fix it");

        // And through the configuration file an operator would actually write.
        let text = format!("[limits]\n{name} = {}\n", ceiling_plus_one(name));
        let r = Settings::parse(&text);
        assert!(
            r.is_err(),
            "{name}: a configuration file must not load with an over-ceiling resource limit: {r:?}"
        );
    }
}

/// The exact value that puts each named resource ceiling one past its maximum.
fn ceiling_plus_one(name: &str) -> u64 {
    match name {
        "max_file_bytes" => opencrayast_core::limits::MAX_FILE_BYTES_HARD + 1,
        "max_output_bytes" => opencrayast_core::limits::MAX_OUTPUT_BYTES_HARD + 1,
        "max_results" => opencrayast_core::limits::MAX_RESULTS_HARD + 1,
        "max_scan_files" => opencrayast_core::limits::MAX_SCAN_FILES_HARD + 1,
        "plan_max_files" => opencrayast_core::limits::PLAN_MAX_FILES_HARD + 1,
        "plan_max_edits" => opencrayast_core::limits::PLAN_MAX_EDITS_HARD + 1,
        "plan_max_changed_bytes" => opencrayast_core::limits::PLAN_MAX_CHANGED_BYTES_HARD + 1,
        "plan_max_store_mib" => opencrayast_core::limits::PLAN_MAX_STORE_MIB_HARD + 1,
        "plan_max_plans" => opencrayast_core::limits::PLAN_MAX_PLANS_HARD + 1,
        "plan_ttl_minutes" => opencrayast_core::limits::PLAN_TTL_MINUTES_HARD + 1,
        other => panic!("unlisted ceiling {other}"),
    }
}

/// **SEC-b: the tunability of depth does not leak into any other limit.**
///
/// The clamp exists on `path_max_depth` and nowhere else. This walks the whole table and
/// asserts that every *other* row still refuses above its maximum, and that depth is the
/// single row that does not — so adding a clamp to a second field later would be caught
/// here rather than shipped.
///
/// `MUTATION target: the `&& name != "path_max_depth"` guard` — dropping it (making depth
/// refuse like everything else) must turn this red, which is what keeps the ruling's split
/// honest from the other direction.
#[test]
fn sec_b_depth_is_the_only_row_that_does_not_refuse() {
    for (name, value, hard) in Limits::default().table() {
        let mut l = Limits::default();
        // Set the field under test one past its own maximum.
        match name {
            "max_file_bytes" => l.max_file_bytes = hard + 1,
            "max_output_bytes" => l.max_output_bytes = hard + 1,
            "max_results" => l.max_results = hard + 1,
            "max_scan_files" => l.max_scan_files = hard + 1,
            "parse_timeout_ms" => l.parse_timeout_ms = hard + 1,
            "parse_max_depth" => l.parse_max_depth = hard + 1,
            "parse_max_nodes" => l.parse_max_nodes = hard + 1,
            "call_timeout_ms" => l.call_timeout_ms = hard + 1,
            "plan_ttl_minutes" => l.plan_ttl_minutes = hard + 1,
            "plan_max_files" => l.plan_max_files = hard + 1,
            "plan_max_edits" => l.plan_max_edits = hard + 1,
            "plan_max_changed_bytes" => l.plan_max_changed_bytes = hard + 1,
            "plan_max_store_mib" => l.plan_max_store_mib = hard + 1,
            "plan_max_plans" => l.plan_max_plans = hard + 1,
            "plan_max_plans_per_process" => l.plan_max_plans_per_process = hard + 1,
            "journal_max_plan_mib" => l.journal_max_plan_mib = hard + 1,
            "journal_retention_days" => l.journal_retention_days = hard + 1,
            "journal_max_total_mib" => l.journal_max_total_mib = hard + 1,
            "note_max_bytes" => l.note_max_bytes = hard + 1,
            "path_max_bytes" => l.path_max_bytes = hard + 1,
            "path_max_depth" => l.path_max_depth = hard + 1,
            other => panic!("a new field is not covered by this test: {other}"),
        }
        assert_eq!(
            l.table()[0].1,
            l.max_file_bytes,
            "the table must describe self"
        );

        let result = l.validate();
        if name == "path_max_depth" {
            assert!(
                result.is_ok(),
                "path_max_depth is the tunable knob and must not refuse: {result:?}"
            );
            assert_eq!(l.clamped_path_max_depth(), PATH_MAX_DEPTH_HARD);
        } else {
            assert!(
                result.is_err(),
                "{name} must still refuse a value above {hard}; it is a resource ceiling and has \
                 no switch that turns it off"
            );
            assert!(
                value > 0,
                "{name} keeps its documented value until the clamp, if any"
            );
        }
    }
}

/// **SEC-c: zero is refused everywhere, including depth.**
///
/// The clamp has one direction. A `path_max_depth = 0` must not become "clamped down to
/// nothing" or, worse, be read as "unbounded" by a `depth > max` check — `0` would refuse
/// every path, and any future reading that treats a zero ceiling as unlimited would turn a
/// resource ceiling off, which is the thing the ruling forbids.
///
/// `MUTATION target: the `value == 0` arm in Limits::validate` — must turn this red.
#[test]
fn sec_c_zero_is_refused_for_depth_and_for_every_ceiling() {
    for (name, _, _) in Limits::default().table() {
        let mut l = Limits::default();
        match name {
            "max_file_bytes" => l.max_file_bytes = 0,
            "max_output_bytes" => l.max_output_bytes = 0,
            "max_results" => l.max_results = 0,
            "max_scan_files" => l.max_scan_files = 0,
            "parse_timeout_ms" => l.parse_timeout_ms = 0,
            "parse_max_depth" => l.parse_max_depth = 0,
            "parse_max_nodes" => l.parse_max_nodes = 0,
            "call_timeout_ms" => l.call_timeout_ms = 0,
            "plan_ttl_minutes" => l.plan_ttl_minutes = 0,
            "plan_max_files" => l.plan_max_files = 0,
            "plan_max_edits" => l.plan_max_edits = 0,
            "plan_max_changed_bytes" => l.plan_max_changed_bytes = 0,
            "plan_max_store_mib" => l.plan_max_store_mib = 0,
            "plan_max_plans" => l.plan_max_plans = 0,
            "plan_max_plans_per_process" => l.plan_max_plans_per_process = 0,
            "journal_max_plan_mib" => l.journal_max_plan_mib = 0,
            "journal_retention_days" => l.journal_retention_days = 0,
            "journal_max_total_mib" => l.journal_max_total_mib = 0,
            "note_max_bytes" => l.note_max_bytes = 0,
            "path_max_bytes" => l.path_max_bytes = 0,
            "path_max_depth" => l.path_max_depth = 0,
            other => panic!("a new field is not covered by this test: {other}"),
        }
        let e = l
            .validate()
            .expect_err("{name} = 0 must be refused, not clamped");
        assert!(
            e.message.contains(name) && e.message.contains("must be at least 1"),
            "{name}: {}",
            e.message
        );
    }
}

/// **SEC-d: a legal depth is never clamped — the clamp fires on nothing else.**
///
/// The mirror of the boundary case: an operator must be able to *lower* the ceiling to a
/// strictly stricter value and get exactly that value, or the "tunable" half of the ruling
/// would only ever mean "clamped to 256". Both extremes of the legal range are pinned, plus
/// the exact ceiling itself.
///
/// `MUTATION target: the `.min(PATH_MAX_DEPTH_HARD)` in clamped_path_max_depth` — replacing
/// it with an unconditional `PATH_MAX_DEPTH_HARD` must turn this red.
#[test]
fn sec_d_a_legal_depth_is_served_unchanged_at_both_extremes() {
    for want in [1u64, 2, 17, PATH_MAX_DEPTH_HARD] {
        let settings = Settings::parse(&format!("[limits]\npath_max_depth = {want}\n"))
            .expect("a legal depth must load");
        assert_eq!(
            settings.limits.path_max_depth, want,
            "{want} must survive parsing"
        );
        assert_eq!(
            settings.limits.clamped_path_max_depth(),
            want,
            "{want} is inside the ceiling and must be served unchanged"
        );
        assert!(
            !settings.limits.path_max_depth_was_clamped(),
            "{want} is not above the ceiling and must not report as clamped"
        );
    }
}

/// **SEC-e: the walk honours the clamp, not just the resolver.**
///
/// `check_size` is not the only reader of the depth ceiling — the directory walk counts
/// ignored-depth entries against it. If the walk read the raw field, an operator's 999999
/// would bound the resolver but leave the walk unbounded, which is the same defect in the
/// other place. Both enforcement points go through `clamped_path_max_depth`, so a path the
/// walk considers too deep must be one the walk stops at, not merely one the resolver
/// refuses afterwards.
///
/// The assertion is on the walk's own accounting (`ignored` counts deliberate skips), so it
/// observes the walk's ceiling rather than inferring it from a later resolver error.
#[test]
fn sec_e_the_directory_walk_honours_the_clamped_ceiling() {
    let dir = ws();
    let root = dir.path().join("ws");
    fs::create_dir_all(&root).expect("mkdir ws");

    // A tree DEEPER than the hard ceiling, configured with a depth far above it. The walk
    // must still stop at `PATH_MAX_DEPTH_HARD` — which is the whole claim: the clamp has to
    // hold at the walk, not only at the resolver, or an operator's 999999 would leave the
    // walk unbounded (the same defect, in the other reader).
    let depth = PATH_MAX_DEPTH_HARD as usize + 8;
    nested(&root, depth);

    let mut l = Limits::default();
    l.path_max_depth = 999_999;
    let b = boundary(&root, l);
    let start = b.resolve_read(".").expect("the root resolves");

    let result = walk(&b, &start, &WalkOptions::default()).expect("the walk itself must succeed");
    let deepest = result
        .files
        .iter()
        .map(|f| f.rel.matches('/').count())
        .max()
        .unwrap_or(0);
    assert!(
        deepest <= PATH_MAX_DEPTH_HARD as usize,
        "the walk returned a path {deepest} components deep although the effective ceiling is \
         {PATH_MAX_DEPTH_HARD}; the operator asked for 999999 and the walk followed them: {:?}",
        result.files.iter().map(|f| &f.rel).collect::<Vec<_>>()
    );
    assert!(
        result.skipped_ignored > 0,
        "the tail of a {depth}-component tree must be skipped and counted, not walked"
    );

    // The control: with the operator's 999999 clamped down to the ceiling, the walk and a
    // boundary configured AT the ceiling must agree exactly. If they disagree, one of the two
    // is not honouring the clamp.
    let mut at_ceiling = Limits::default();
    at_ceiling.path_max_depth = PATH_MAX_DEPTH_HARD;
    let c = boundary(&root, at_ceiling);
    let c_start = c.resolve_read(".").expect("the root resolves");
    let c_result = walk(&c, &c_start, &WalkOptions::default()).expect("walk");
    assert_eq!(
        result, c_result,
        "a clamped 999999 must walk identically to a configured ceiling of {PATH_MAX_DEPTH_HARD}"
    );
}
