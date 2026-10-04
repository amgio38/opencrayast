//! CFG 1 — the operator's `[limits]` actually reaches the path checks.
//!
//! Ticket: `Y20261002/REQ-CLI-HUMAN/ISSUE-CONFIG-LIMITS-WIRING`, the CR finding against
//! SEC-FIX 5: "the container was forged and nobody poured the water". `BoundaryConfig` had a
//! `limits` field and both checkpoints read it through `Boundary::limits()`, but **nothing in
//! `crates/*/src` ever filled it in** — there was no config parser, and the one construction
//! site (`tools/src/bench.rs`) spread `..Default::default()`, so it silently measured a
//! boundary held to the wrong limits while passing the real ones to the tools beside it.
//!
//! These tests go from the text of a configuration file to a refusal at both path
//! checkpoints, with no `Default` anywhere on the route.
#![cfg(unix)]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use opencrayast_core::ErrorCode;
use opencrayast_core::boundary::Boundary;
use opencrayast_core::config::Settings;
use opencrayast_core::limits::Limits;
use opencrayast_core::walk::{WalkOptions, walk};
use std::fs;
use std::path::PathBuf;

/// A workspace with `depth` nested directories and a file in the deepest one.
fn deep_tree(depth: usize) -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("ws");
    fs::create_dir_all(&root).unwrap();
    let mut p = root.clone();
    for i in 0..depth {
        p = p.join(format!("d{i}"));
    }
    fs::create_dir_all(&p).unwrap();
    fs::write(p.join("f.txt"), b"x").unwrap();
    (dir, root)
}

fn deep_rel(depth: usize) -> String {
    (0..depth)
        .map(|i| format!("d{i}"))
        .collect::<Vec<_>>()
        .join("/")
        + "/f.txt"
}

const TIGHT: &str = "\
# a comment, then a real section
[limits]
path_max_depth = 4
path_max_bytes = 4096
";

/// CFG1-01 — the whole route: config text -> Settings -> BoundaryConfig -> `resolve_read`.
///
/// No `Default` on the route, and the value in the file is the value that binds.
#[test]
fn cfg1_01_config_file_limits_bind_at_the_resolver() {
    let (_d, root) = deep_tree(20);
    let rel = deep_rel(20);

    // Parse, exactly as a shell would.
    let settings = Settings::parse(TIGHT).unwrap();
    assert_eq!(
        settings.limits.path_max_depth, 4,
        "the configured value must survive parsing"
    );
    assert_ne!(
        settings.limits.path_max_depth,
        Limits::default().path_max_depth,
        "this fixture is only meaningful if it differs from the default"
    );

    // The ONE place that decides which limits a boundary gets.
    let boundary = Boundary::new(
        settings
            .boundary_config(&root)
            .expect("the state directory resolves on a test machine"),
    )
    .unwrap();

    let err = boundary.resolve_read(&rel).unwrap_err();
    assert_eq!(
        err.code,
        ErrorCode::LimitExceeded,
        "a 21-component path must be refused under a configured depth of 4, got {err:?}"
    );
    assert!(
        err.message.contains('4'),
        "the refusal must name the ceiling that was actually configured: {err:?}"
    );

    // And a path inside the ceiling still resolves, so this is not "refuse everything".
    fs::create_dir_all(root.join("d0/d1")).unwrap();
    fs::write(root.join("d0/d1/f.txt"), b"x").unwrap();
    assert!(
        boundary.resolve_read("d0/d1/f.txt").is_ok(),
        "a 3-component path is inside the configured ceiling of 4"
    );
}

/// CFG1-02 — the same configured value binds at the WALKER too.
///
/// The two checkpoints are separate call sites. A file-to-resolver test that never went
/// through `walk` would pass while the walker's ceiling stayed at the default.
#[test]
fn cfg1_02_config_file_limits_bind_at_the_walker() {
    let (_d, root) = deep_tree(20);
    let settings = Settings::parse(TIGHT).unwrap();
    let boundary = Boundary::new(
        settings
            .boundary_config(&root)
            .expect("the state directory resolves on a test machine"),
    )
    .unwrap();

    let start = boundary.resolve_read(".").unwrap();
    let result = walk(&boundary, &start, &WalkOptions::default()).unwrap();

    let deepest = result
        .files
        .iter()
        .map(|f| f.rel.matches('/').count())
        .max()
        .unwrap_or(0);
    assert!(
        deepest <= 4,
        "the walk returned a path {deepest} components deep under a configured ceiling of 4: {:?}",
        result.files.iter().map(|f| &f.rel).collect::<Vec<_>>()
    );
    assert!(
        result.skipped_ignored > 0,
        "the refused subtree is counted: {result:?}"
    );
}

/// CFG1-03 — the loaded file is the source, not a string literal.
///
/// `Settings::load` is what a shell calls. It refuses a group- or world-readable file
/// (T-18 / CFG-05): a settings file an attacker can edit is a way to turn write mode on,
/// so "private or nothing" is part of the wiring, not a separate concern.
#[test]
fn cfg1_03_load_reads_a_private_file_and_refuses_a_loose_one() {
    use std::os::unix::fs::PermissionsExt;

    let dir = tempfile::tempdir().unwrap();
    let good = dir.path().join("good.toml");
    fs::write(&good, TIGHT).unwrap();
    fs::set_permissions(&good, fs::Permissions::from_mode(0o600)).unwrap();

    let settings = Settings::load(&good).unwrap();
    assert_eq!(settings.limits.path_max_depth, 4);

    let loose = dir.path().join("loose.toml");
    fs::write(&loose, TIGHT).unwrap();
    fs::set_permissions(&loose, fs::Permissions::from_mode(0o644)).unwrap();
    let err = Settings::load(&loose).unwrap_err();
    // `config_untrusted`, not `invalid_args`: the file is fine and the machine is not
    // configured the way this program requires, which is what lets a shell give it the
    // environment exit status while a malformed file gets the user one.
    assert_eq!(err.code, ErrorCode::ConfigUntrusted, "{err:?}");
    assert!(
        err.message.contains("group") || err.message.contains("other"),
        "the refusal must say the file is too open: {err:?}"
    );
    // The refusal must not quote the path or the file's contents.
    assert!(!err.message.contains(dir.path().to_str().unwrap()));
    assert!(!err.message.contains("path_max_depth"));
}

/// CFG1-04 — a typo in a safety limit is refused, not ignored.
///
/// This is the other half of "the operator's value is the value that binds". If an unknown
/// key were ignored, `[limits] path_max_dept = 4` would leave the default in place while
/// the file reads as though it were configured — which is the same class of lie the matrix
/// ticket is about.
#[test]
fn cfg1_04_an_unknown_limit_is_refused_rather_than_ignored() {
    for src in [
        "[limits]\npath_max_dept = 4\n",                     // typo
        "[limits]\npath_max_depth = 4\npath_max_dept = 8\n", // one good, one typo
        "[limts]\npath_max_depth = 4\n",                     // typo in the section name
        "[limits]\npath_max_depth = 0\n",                    // zero is refused by Limits::validate
        "[limits]\npath_max_depth = four\n",                 // not a number
        "path_max_depth = 4\n",                              // key before any section
        "[limits]\nmax_output_bytes = 262145\n", // a resource ceiling above its hard max
        "[limits]\nplan_max_edits = 5001\n",     // ditto: above PLAN_MAX_EDITS_HARD
    ] {
        let r = Settings::parse(src);
        assert!(
            r.is_err(),
            "must be refused rather than silently defaulted: {src:?} -> {r:?}"
        );
    }
    // The control: the valid file parses.
    assert!(Settings::parse(TIGHT).is_ok());
}

/// CFG1-05 — `BoundaryConfig` has no `Default`, and there is one decision point.
///
/// The enforcement here is the COMPILER, not this test: `#[derive(Default)]` was removed
/// from `BoundaryConfig`, and 26 files that used to spread `..Default::default()` into a
/// boundary now name their limits. Re-adding the derive would break all of them, which is a
/// stronger property than any assertion in this file.
///
/// So this test checks only what the compiler cannot: that the single decision point exists
/// and is reachable, and that the tree still contains no spread that could quietly restore
/// the old shape. Two earlier versions of this test tried to brace-match each literal and
/// flagged unrelated structs (`OutlineArgs`) and even a mention inside a comment - a rule
/// wider than the thing it guards, which is the shape this whole ticket exists to remove.
#[test]
fn cfg1_05_there_is_one_decision_point_and_no_silent_spread() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");

    // The decision point exists and is the documented one.
    let cfg_src = fs::read_to_string(root.join("crates/core/src/config.rs")).unwrap();
    assert!(
        cfg_src.contains("pub fn boundary_config"),
        "Settings::boundary_config is the single place that decides a boundary's limits"
    );

    // No BOUNDARY construction site spreads a Default. The check has to know which struct
    // the spread belongs to: a file-level ban flagged `OutlineArgs` in bench.rs, which is a
    // different struct entirely and spreads its own defaults harmlessly.
    let mut literals = 0;
    for path in walk_rs(&root.join("crates")) {
        let src = fs::read_to_string(&path).unwrap();
        if !src.contains("BoundaryConfig") {
            continue;
        }
        for (_start, open, close) in boundary_literals(&src) {
            let lit = &src[open..=close];
            assert!(
                !lit.contains("..Default::default()"),
                "{}: a BoundaryConfig literal spreads Default, so its limits are whatever the \
                 default says rather than what the caller chose",
                path.display()
            );
            assert!(
                lit.contains("limits"),
                "{}: a BoundaryConfig literal does not name `limits`: {lit:?}",
                path.display()
            );
            literals += 1;
        }
        literals += src.matches("BoundaryConfig::new(").count();
    }
    assert!(
        literals > 20,
        "expected many boundary construction sites now naming their limits, saw {literals}"
    );
}

/// Every `.rs` under a directory.
fn walk_rs(dir: &std::path::Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        let Ok(rd) = fs::read_dir(&d) else { continue };
        for e in rd.flatten() {
            let p = e.path();
            if p.is_dir() {
                if p.file_name().and_then(|n| n.to_str()) == Some("target") {
                    continue;
                }
                stack.push(p);
            } else if p.extension().and_then(|n| n.to_str()) == Some("rs") {
                out.push(p);
            }
        }
    }
    out
}

/// Every `BoundaryConfig { ... }` literal in `src`, as byte ranges of its braces.
///
/// Skips mentions that are not literals: one inside a comment, and one reached through a
/// path such as `use crate::boundary::BoundaryConfig`. Both were real false positives while
/// this function was being written.
fn boundary_literals(src: &str) -> Vec<(usize, usize, usize)> {
    let bytes = src.as_bytes();
    let mut out = Vec::new();
    let mut at = 0usize;
    while let Some(found) = src[at..].find("BoundaryConfig") {
        let start = at + found;
        at = start + 1;
        let line_start = src[..start].rfind('\n').map_or(0, |i| i + 1);
        if src[line_start..start].trim_start().starts_with("//") {
            continue;
        }
        let Some(rel) = src[start + "BoundaryConfig".len()..].find('{') else {
            continue;
        };
        let open = start + "BoundaryConfig".len() + rel;
        if !src[start + "BoundaryConfig".len()..open].trim().is_empty() {
            continue;
        }
        let mut depth = 0i32;
        let mut close = open;
        for (i, b) in bytes[open..].iter().enumerate() {
            match b {
                b'{' => depth += 1,
                b'}' => {
                    depth -= 1;
                    if depth == 0 {
                        close = open + i;
                        break;
                    }
                }
                _ => {}
            }
        }
        out.push((start, open, close.min(src.len() - 1)));
    }
    out
}

/// CFG1-R2-06: the parser's key names and `Limits::table()`'s names are the same list.
///
/// They were two independently maintained lists: `table()` is exhaustive over the *fields* (the
/// compiler makes sure of that) but nothing tied its *name strings* to the strings `set_limit`
/// matches on, so adding a field could leave the parser rejecting the name the table advertises.
/// Every name in the table must therefore be accepted, with the value landing.
#[test]
fn every_name_in_limits_table_is_accepted_by_the_parser() {
    let table = Limits::default().table();
    assert!(!table.is_empty(), "the table must not be empty");

    let mut text = String::from("[limits]\n");
    for (name, _default, hard) in table.iter() {
        // 1 is below every hard maximum (they are all >= 1) and is accepted everywhere.
        assert!(
            *hard >= 1,
            "{name}: hard maximum {hard} is below the minimum 1"
        );
        text.push_str(&format!("{name} = 1\n"));
    }
    Settings::parse(&text)
        .unwrap_or_else(|e| panic!("the parser rejected a name its own table advertises: {e:?}"));

    // One of them is read back by name, so this is not only "the parser did not complain".
    let probe = Settings::parse("[limits]\npath_max_depth = 1\n").unwrap();
    assert_eq!(
        probe.limits.path_max_depth, 1,
        "path_max_depth = 1 in the file must land in Settings"
    );

    // And an invented name is still refused, so the loop above was not vacuous.
    assert!(
        Settings::parse("[limits]\nnot_a_limit = 1\n").is_err(),
        "an unknown [limits] key must be refused"
    );
}

/// BND-15 through the **production** route: `Settings::boundary_config` sets `state_dir`.
///
/// This is the test the whole relocation turns on, and it exists because a guard nothing sets is
/// not a guard. `Settings::boundary_config` is the one place every shell builds a
/// `BoundaryConfig`, and `BoundaryConfig::new` leaves `state_dir: None`. So for as long as both
/// were true, `Boundary::resolve_write`'s "never a write target inside the state directory" check
/// compared against `None` and never fired — in every production binary. The 26-agent review
/// demonstrated the consequence end to end: `preview` -> `apply` wrote into the tool's own
/// journal, and `ast_undo` restored it as if legitimate.
///
/// Two things are asserted, and the first is the one that goes red on the mutation:
///
/// 1. `state_dir` is `Some`, and it is outside the workspace — so the field is *armed*.
/// 2. A write into that directory is refused. **Not** refused with `protected_path`
///    specifically: with the state directory now outside the root, `resolve_path` refuses it as
///    `outside_workspace` one step earlier, and it is the fact of refusal that is the property.
///    Asserting the specific code here would pin an accident of check ordering rather than the
///    guarantee, and would break for the wrong reason if the checks were reordered.
///
/// **Mutation self-proof: set `state_dir` back to `None` in `boundary_config` and this goes red.**
#[test]
fn boundary_config_makes_the_state_dir_guard_reachable() {
    let ws = tempfile::tempdir().unwrap();
    let root = ws.path().to_path_buf();
    let cfg = Settings::default()
        .boundary_config(&root)
        .expect("the state directory resolves on a test machine");
    let state = cfg
        .state_dir
        .clone()
        .expect("Settings::boundary_config must set state_dir, or BND-15 is unreachable");
    assert!(
        !state.starts_with(&root),
        "the resolved state directory must be outside the workspace: {} is under {}",
        state.display(),
        root.display()
    );

    // The directory really exists and really is where the stores would put their files.
    let inside = state.join("ws-w-test/plans/p.json");
    fs::create_dir_all(inside.parent().unwrap()).unwrap();
    fs::write(&inside, "{}").unwrap();
    fs::write(root.join("ordinary.rs"), "fn a() {}\n").unwrap();

    let b = Boundary::new(cfg).expect("a real workspace root builds");
    assert!(
        b.resolve_write(inside.to_str().unwrap()).is_err(),
        "a write into the tool's own state directory must be refused"
    );
    assert!(
        b.resolve_write("ordinary.rs").is_ok(),
        "the guard must refuse the state directory and nothing else"
    );
}

/// And the other half of the same defect: an **undeterminable** base is a refusal, not a silent
/// "carry on with no state directory". There is no branch in `boundary_config` that drops the
/// guard, because that branch would recreate exactly the unguarded state above.
///
/// Run in a child process with the environment cleared, for the same reason the resolver tests
/// are: `set_var` is `unsafe` and this binary runs its tests in parallel.
#[test]
fn boundary_config_refuses_rather_than_dropping_the_guard() {
    if std::env::var_os(HELPER_ENV).is_some() {
        // The helper: resolve with no XDG_STATE_HOME and no HOME at all.
        match Settings::default().boundary_config(std::path::Path::new("/tmp")) {
            Ok(cfg) => println!("HELPER-ACCEPTED {}", cfg.state_dir.is_some()),
            Err(e) => println!("HELPER-REFUSED {}", e.code.as_str()),
        }
        return;
    }

    let exe = std::env::current_exe().unwrap();
    let out = std::process::Command::new(exe)
        .args([
            "--exact",
            "boundary_config_refuses_rather_than_dropping_the_guard",
            "--nocapture",
        ])
        .env_remove("XDG_STATE_HOME")
        .env_remove("HOME")
        .env(HELPER_ENV, "1")
        .output()
        .unwrap();
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(
        text.contains("HELPER-REFUSED io_error"),
        "with no state base at all, boundary_config must refuse; it printed:\n{text}"
    );
    assert!(
        !text.contains("HELPER-ACCEPTED"),
        "a boundary with no state_dir is the unguarded state this ticket exists to remove"
    );
}

const HELPER_ENV: &str = "OPENCRAYAST_BND_STATE_HELPER";
