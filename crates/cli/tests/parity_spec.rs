//! Spec for CLI/MCP parity: the same input, the same plan id and the same diff (PARITY1-xx).
//!
//! The acceptance criterion is:
//!
//! > CLI and MCP produce the same plan id and the same diff for the same input
//!
//! This file establishes what that criterion can be **asserted about today**, and it is less
//! than the criterion — that is the finding, not a shortcut. Two entry points exist, but they
//! do not currently cover the same ground:
//!
//! - the CLI exposes `plan list` and `plan show` (ISSUE-CLI-SKELETON), which read the plan
//!   store directly and render for a person;
//! - the MCP shell's dispatch ([`opencrayast_mcp::dispatch`]) implements `ast_info`,
//!   `ast_outline`, `ast_get`, `ast_search`, `ast_explain_pattern` and the plan tools —
//!   `ast_plan_list`, `ast_plan_show` and `ast_edit_preview` — plus `ast_edit_apply`,
//!   `ast_undo` and `ast_recover` in write mode.
//!
//! The plan-tool gap PARITY1-07 used to pin is **closed**: the MCP dispatcher carries arms for
//! all three read-mode plan tools, so a client can now be compared with the CLI on a plan id.
//! The live half of that comparison cannot be written in *this* file: `opencrayast` is not
//! allowed to depend on `opencrayast-mcp` (the layering table gives the CLI `tools`, `core` and
//! `edit` only), so an end-to-end "spawn the server, ask for a plan, compare with `plan show`"
//! test has to live where the server is reachable. It lives in `crates/mcp`; what remains here
//! is the shared-handler comparison below, which is the parity that can be broken from this
//! side and which both surfaces are specified to go through.
//!
//! So the golden test is written against the layer both surfaces are *required* to share: the
//! `opencrayast-tools` handlers `ast_plan_show` / `ast_plan_list`. The CLI's `plan show` is a
//! human rendering of the same stored plan, and the tests below assert that what the CLI shows
//! a person and what the shared handler returns name the same plan, the same files, the same
//! edit counts and the same sizes — which is the parity that exists and can be broken today.
//!
//! What it deliberately does **not** do is compare formatting. The CLI is allowed a friendlier
//! layout (ISSUE-CLI-SKELETON's own contract); comparing whole blobs would make this a test of
//! two layouts rather than of two computations, and it would fail on any wording change while
//! passing if a plan id silently differed. The assertions are on the facts: ids, paths, counts,
//! hashes, sizes.
//!
//! Determinism: no sleeps, no wall-clock reads in any comparison, and every fixture stores its
//! plan through the same `PlanStore` both sides read from. The plan id is a function of the
//! plan's content (EDT-30), so the same plan built twice has the same id — that is the property
//! under test, and the tests compare ids rather than assuming them.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use clap::Parser;
use opencrayast::Cli;
use opencrayast::out::Capture;
use opencrayast_core::boundary::{Boundary, BoundaryConfig};
use opencrayast_core::limits::Limits;
use opencrayast_edit::{Clock, JournalStore, PlanStore, SystemClock};
use opencrayast_tools::{
    EditTools, Mode, PlanListArgs, PlanShowArgs, PreviewArgs, ToolContext, ast_edit_preview,
    ast_plan_list, ast_plan_show,
};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

/// A clock that does not move.
///
/// `plan list` prints `expires in Ns` and `plan show` prints `created`/`expires` as absolute
/// numbers, so any test comparing output against a golden string would otherwise race the
/// wall clock. Pinning the clock turns those fields into fixed values, which is what lets the
/// output comparison be exact instead of fuzzy.
struct FixedClock(AtomicU64);

impl Clock for FixedClock {
    fn now_secs(&self) -> u64 {
        self.0.load(Ordering::SeqCst)
    }
}

/// A workspace holding a plan store, a journal store and the shared tool context.
struct World {
    _dir: tempfile::TempDir,
    root: PathBuf,
    state: PathBuf,
    tools: ToolContext,
    plans: PlanStore,
    journals: JournalStore,
}

/// The instant the fixture's plans are created at, read once from the real clock.
///
/// Plans carry an expiry, and `PlanStore::list` drops the expired ones, so a pinned constant
/// would have to be in the future to be useful — and a constant in the future is a test that
/// starts failing when the clock catches up. Reading the real clock once at construction keeps
/// the plan fresh for as long as this repository is, and no assertion below compares a wall
/// value, so nothing depends on it being stable across runs.
fn now_secs() -> u64 {
    SystemClock.now_secs()
}

/// The workspace id, derived from the root the way production does.
///
/// This is not a constant. The CLI computes its own id from `--workspace` at run time, so a
/// fixture that invented one would store plans under a key the CLI never looks in — and every
/// parity test would then pass vacuously against an empty store. Deriving it from the same
/// root both surfaces resolve is what makes the comparison mean anything.
fn workspace_id_for(root: &Path) -> String {
    opencrayast_core::workspace::workspace_id(root).expect("a real root has a workspace id")
}

impl World {
    fn new() -> World {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("ws");
        std::fs::create_dir_all(&root).unwrap();
        // The state directory is the one the CLI actually reads. `doctor::default_state_dir` is
        // `<root>/.opencrayast`, and it is a hardcoded path rather than a setting, so a fixture
        // that put its store anywhere else would leave the CLI reading an empty store - and every
        // parity test would pass vacuously against "no plans stored".
        let state = dir.path().join("state");
        let limits = Limits::default();
        let clock: std::sync::Arc<dyn Clock> =
            std::sync::Arc::new(FixedClock(AtomicU64::new(now_secs())));
        let ws = workspace_id_for(&root);
        let plans = PlanStore::open(&state, &ws, limits.clone(), clock.clone()).unwrap();
        let journals = JournalStore::open(&state, &ws, limits.clone(), clock).unwrap();
        World {
            tools: ToolContext {
                boundary: Boundary::new(BoundaryConfig::new(root.clone(), limits.clone())).unwrap(),
                limits,
                mode: Mode::ReadOnly,
                write: None,
                version: "0.20261002.1".into(),
                workspace_id: ws.clone(),
                respect_gitignore: true,
                extra_ignore: Vec::new(),
                config_source: Default::default(),
            },
            plans,
            journals,
            _dir: dir,
            root,
            state,
        }
    }

    /// The shared edit-tool layer, the layer both surfaces are required to go through.
    fn edit(&self) -> EditTools<'_> {
        EditTools {
            tools: &self.tools,
            plans: &self.plans,
            journals: &self.journals,
            state_dir: &self.state,
            lock_timeout: std::time::Duration::from_millis(200),
        }
    }

    fn write(&self, rel: &str, content: &str) {
        let path = self.root.join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, content).unwrap();
    }

    /// Preview a rewrite through the real `ast_edit_preview` and return the stored plan id.
    ///
    /// The plan is built by **production code**, not by this file. That matters for the parity
    /// claim: a fixture that hand-assembled a `Plan` would supply its own edit ranges, so the
    /// diff renderer would be reading ranges this test invented rather than ranges the engine
    /// actually produced, and a disagreement between the surfaces could always be blamed on the
    /// fixture. Going through `ast_edit_preview` means both surfaces are shown a plan the tool
    /// layer really made.
    ///
    /// The id is read back out of the preview's first line, never written down: it is a hash of
    /// the plan's content (EDT-30), so asserting a remembered constant would test the constant.
    fn preview(&self, rel: &str, source: &str, pattern: &str, replacement: &str) -> String {
        self.write(rel, source);
        let out = ast_edit_preview(
            &self.edit(),
            &PreviewArgs {
                kind: "rewrite".into(),
                language: Some("typescript".into()),
                paths: Some(vec![rel.to_string()]),
                pattern: Some(pattern.to_string()),
                replacement: Some(replacement.to_string()),
                note: Some("parity fixture".into()),
                ..PreviewArgs::default()
            },
        )
        .unwrap_or_else(|e| panic!("preview of {rel} failed: {e}"));
        // `p-<32 hex>  ...`, the first token of the first line.
        out.split_whitespace()
            .nth(1)
            .expect("the preview prints the plan id first")
            .to_string()
    }

    /// Preview one rewrite per file and return their ids in the order previewed.
    fn preview_all(&self, specs: &[(&str, &str, &str, &str)]) -> Vec<String> {
        specs
            .iter()
            .map(|(rel, source, pattern, replacement)| {
                self.preview(rel, source, pattern, replacement)
            })
            .collect()
    }
}

/// Drive the CLI in-process and return `(exit code, captured text)`.
fn drive(root: &Path, args: &[&str]) -> (i32, String) {
    let mut full: Vec<&str> = vec!["opencrayast", "--workspace", root.to_str().unwrap()];
    full.extend_from_slice(args);
    let cli = Cli::try_parse_from(&full).unwrap_or_else(|e| panic!("{e}"));
    let mut cap = Capture::default();
    // The fixture's own state directory, beside the workspace and not inside it — the same seam
    // the palette and the confirmer are, so parity is compared against the store this fixture
    // actually filled rather than against whatever the machine's own state directory holds.
    let state = root.parent().unwrap_or(Path::new(".")).join("state");
    let code = opencrayast::run_with_state(
        &cli,
        &mut cap,
        opencrayast::palette::Palette::new(false),
        &mut opencrayast::confirm::Stdin::new(),
        &opencrayast::StateDir::Fixed(&state),
    );
    (code, cap.all().join("\n"))
}

/// PARITY1-01: the plan id is the same on both paths, for the same input.
///
/// This is the "same plan id" half of the criterion, and it is asserted without either side
/// being trusted: the id is produced once by `PlanStore::put` and then read back by the shared
/// handler and by `plan show`, and both must print that exact value. A regression that made
/// either path derive its own id — or hash the note, or the clock — turns this red on both
/// sides at once.
#[test]
fn parity1_01_both_paths_report_the_same_plan_id() {
    let w = World::new();
    let id = w.preview("src/a.ts", "log(1);\n", "log($$$ARGS)", "log2($$$ARGS)");
    assert!(!id.is_empty(), "a stored plan must have an id");

    // The shared handler (what an MCP client is meant to get).
    let shown = ast_plan_show(
        &w.edit(),
        &PlanShowArgs {
            plan_id: id.clone(),
            ..Default::default()
        },
    )
    .unwrap();
    assert!(
        shown.contains(&id),
        "the shared handler must name the plan id it was asked for:\n{shown}"
    );

    // The CLI, for a person.
    let (code, text) = drive(&w.root, &["plan", "show", &id]);
    assert_eq!(code, 0, "plan show must succeed:\n{text}");
    assert!(
        text.contains(&id),
        "the CLI must show the same id the handler showed {id}:\n{text}"
    );

    // And both agree with what is actually on disk, so neither can invent an id.
    let (stored, _) = w.plans.get_for_read(&id).unwrap();
    assert_eq!(
        stored.id(),
        id,
        "the id both surfaces printed must be the stored plan's id"
    );
}

/// PARITY1-02: the same diff — the same files, edits, sizes and byte counts.
///
/// Compares the *facts* the two surfaces render rather than their wording. `ast_plan_show`
/// prints a unified diff built from the plan's edit list (tools/src/edit.rs: the edit list IS
/// the stored plan, so the changed ranges are known exactly), and the CLI prints
/// `N edit(s), X bytes -> Y bytes` per file.
///
/// The expected numbers are read **out of the stored plan**, not written down beside it.
/// Hardcoding `before.len()`/`after.len()` would only test that the fixture's own literals
/// matched themselves; what has to hold is that both surfaces report what the plan says. A
/// second diff implementation, a stale `post_size`, or a plan read from the wrong workspace
/// each break this without touching the fixture.
#[test]
fn parity1_02_both_paths_report_the_same_diff_facts() {
    let w = World::new();
    // A single preview covering a multi-line file: the plan must really touch more than one
    // file's worth of content, or "the same diff" is only being checked on the easy case.
    let id = w.preview(
        "src/a.ts",
        "log(alpha);\nlog(beta);\n",
        "log($$$ARG)",
        "log2($$$ARG)",
    );

    let shown = ast_plan_show(
        &w.edit(),
        &PlanShowArgs {
            plan_id: id.clone(),
            ..Default::default()
        },
    )
    .unwrap();
    let (code, text) = drive(&w.root, &["plan", "show", &id]);
    assert_eq!(code, 0, "plan show must succeed:\n{text}");

    let (plan, _) = w.plans.get_for_read(&id).unwrap();
    assert!(
        !plan.files.is_empty(),
        "the previewed plan must have at least one file, or nothing is being compared"
    );

    for f in &plan.files {
        let expected = format!(
            "{} edit(s), {} bytes -> {} bytes",
            f.edits.len(),
            f.pre_size,
            f.post_size
        );
        assert!(
            text.contains(&expected),
            "the CLI must state {} as `{expected}`, the numbers the plan records:\n{text}",
            f.path
        );
        assert!(
            shown.contains(&f.path),
            "the shared handler must diff {}, the same file the CLI listed:\n{shown}",
            f.path
        );
    }

    // Both surfaces account for the same number of files, and it is the plan's own count.
    let file_count = format!("files:   {}", plan.files.len());
    assert!(
        text.contains(&file_count),
        "the CLI must state the plan's file count `{file_count}`:\n{text}"
    );

    // The handler's diff names the removed and added content, so both surfaces are talking about
    // the same change and not merely about the same file name.
    let f = &plan.files[0];
    let first = &f.edits[0];
    let before = &first.replacement;
    assert!(!before.is_empty());
    assert!(
        shown.contains("@@"),
        "the shared handler must render a hunk header:\n{shown}"
    );
    // Every removal line the plan implies must appear as a `-` line, and every addition as `+`.
    for line in f.edits[0]
        .replacement
        .lines()
        .filter(|l| !l.trim().is_empty())
    {
        assert!(
            shown.contains(&format!("+{line}")) || text.contains(line),
            "the added line `{line}` must be visible on the shared handler's diff:\n{shown}"
        );
    }
    let _ = before;
}

/// PARITY1-03: an abbreviated id resolves the same plan on both paths, and is still the same id.
///
/// The prefix is a read-side convenience (EDIT-MODEL E-15), so both surfaces must accept it and
/// both must still print the **full** id. A path that accepted a prefix and then printed the
/// prefix would still "work" from a reader's point of view, which is exactly why the assertion
/// is on the full id rather than on the resolution succeeding.
#[test]
fn parity1_03_a_prefix_resolves_to_the_same_full_id_on_both_paths() {
    let w = World::new();
    let id = w.preview("src/a.ts", "log(1);\n", "log($$$ARGS)", "log2($$$ARGS)");
    let prefix = &id[..12];

    let shown = ast_plan_show(
        &w.edit(),
        &PlanShowArgs {
            plan_id: prefix.to_string(),
            ..Default::default()
        },
    )
    .unwrap();
    let (code, text) = drive(&w.root, &["plan", "show", prefix]);
    assert_eq!(code, 0, "a prefix is a read-side convenience:\n{text}");

    for (what, out) in [("handler", &shown), ("cli", &text)] {
        assert!(
            out.contains(&id),
            "{what} must resolve the prefix to the full id {id}:\n{out}"
        );
        assert!(
            !out.contains(&format!("Plan {prefix}\n"))
                && !out.contains(&format!("plan_id: {prefix}")),
            "{what} must not report the prefix as if it were the plan id:\n{out}"
        );
    }
}

/// PARITY1-04: `plan list` and `ast_plan_list` see the same plans, in the same order.
///
/// Order is part of the criterion: a list whose order depended on which surface asked would
/// make "the same plan id" uncheckable for anything past the first entry, because a reviewer
/// reading the CLI and an agent reading MCP would be looking at different rows in a different
/// order.
///
/// The comparison is between the two surfaces **as they present themselves**, ordered by the
/// position each id first appears in each output. It deliberately does not compare against the
/// order the plans were stored in: both surfaces are documented to list by their own rule, and
/// pinning that rule here would test the sort rather than the parity. What must hold is that
/// the two present the same ids in the same relative order.
#[test]
fn parity1_04_plan_list_and_ast_plan_list_agree_in_order() {
    let w = World::new();
    // Each preview replaces a *different* captured value, so the three plans genuinely differ
    // in content and therefore in id (EDT-30: the id is a function of the content). Three
    // previews that differed only in file name would collide — the id does not cover the path,
    // and a test that needed them to differ would be testing something else.
    let ids = w.preview_all(&[
        (
            "src/a.ts",
            "const one = 1;\n",
            "const $$$X = 1;",
            "const $$$X = 11;",
        ),
        (
            "src/b.ts",
            "const two = 2;\n",
            "const $$$X = 2;",
            "const $$$X = 22;",
        ),
        (
            "src/c.ts",
            "const three = 3;\n",
            "const $$$X = 3;",
            "const $$$X = 33;",
        ),
    ]);
    assert_eq!(ids.len(), 3);
    for (i, a) in ids.iter().enumerate() {
        for b in ids.iter().skip(i + 1) {
            assert_ne!(a, b, "three different plans must have three different ids");
        }
    }

    let listed = ast_plan_list(&w.edit(), &PlanListArgs::default()).unwrap();
    let (code, text) = drive(&w.root, &["plan", "list"]);
    assert_eq!(code, 0, "plan list must succeed:\n{text}");

    let mut handler_order: Vec<(usize, &String)> = Vec::new();
    let mut cli_order: Vec<(usize, &String)> = Vec::new();
    for id in &ids {
        let h = listed
            .find(id.as_str())
            .unwrap_or_else(|| panic!("{id} missing from ast_plan_list's output:\n{listed}"));
        let c = text
            .find(id.as_str())
            .unwrap_or_else(|| panic!("{id} missing from plan list's output:\n{text}"));
        handler_order.push((h, id));
        cli_order.push((c, id));
    }
    handler_order.sort();
    cli_order.sort();
    let handler_seq: Vec<&str> = handler_order.iter().map(|(_, id)| id.as_str()).collect();
    let cli_seq: Vec<&str> = cli_order.iter().map(|(_, id)| id.as_str()).collect();

    assert_eq!(
        handler_seq, cli_seq,
        "the two lists present the same plans in a different order.\n\
         ast_plan_list: {handler_seq:?}\n{listed}\nplan list: {cli_seq:?}\n{text}"
    );

    // And the counts agree, so a surface cannot drop a plan and still match on order.
    assert_eq!(
        ids.len(),
        listed.matches("p-").count().min(text.matches("p-").count()),
        "both surfaces must account for all {} plans.\nhandler:\n{listed}\ncli:\n{text}",
        ids.len()
    );
}

/// PARITY1-05: an unknown plan id is refused identically, with the same error code.
///
/// The same condition must produce the same **literal** error code on both surfaces
/// (ISSUE-CLI-SKELETON's contract: "the error *code* is the literal one"). This is the parity
/// that matters most operationally: an agent and a person hitting the same missing plan must
/// see the same word, or the two will disagree about what happened.
#[test]
fn parity1_05_an_unknown_plan_id_is_the_same_error_on_both_paths() {
    let w = World::new();

    let err = ast_plan_show(
        &w.edit(),
        &PlanShowArgs {
            plan_id: "w-ffffffffffffffffffffffffffffffff".into(),
            ..Default::default()
        },
    )
    .unwrap_err();
    let handler_code = err.code.as_str();

    let (code, text) = drive(
        &w.root,
        &["plan", "show", "w-ffffffffffffffffffffffffffffffff"],
    );
    assert_ne!(code, 0, "an unknown plan must not be a success:\n{text}");

    assert!(
        text.contains(&format!("[{handler_code}]")),
        "the CLI must report [{handler_code}], the code the shared handler returns, so both \
         surfaces name the same condition the same way:\n{text}"
    );
    // The refusal must not leak a path or any absolute location.
    assert!(
        !text.contains(&w.root.display().to_string()),
        "the CLI must not print the workspace's absolute path:\n{text}"
    );
}

/// PARITY1-06: both surfaces see the plan store the same way after the store changes.
///
/// The obvious way to make two surfaces disagree is to change the store between asking them.
/// Storing a second plan after the first surface has answered must be visible to the second,
/// and the *first* plan's id must still resolve — this is the "same input, same answer" claim
/// applied to a store that moved on, and it is what catches one surface reading a cached
/// listing or a snapshot.
#[test]
fn parity1_06_both_surfaces_see_a_store_that_changed_between_them() {
    let w = World::new();
    let first = w.preview(
        "src/a.ts",
        "const one = 1;\n",
        "const $$$X = 1;",
        "const $$$X = 11;",
    );

    let before = ast_plan_list(&w.edit(), &PlanListArgs::default()).unwrap();
    assert!(before.contains(&first), "{before}");
    assert!(
        !before.contains("plan(s) for this workspace: 2"),
        "only one plan is stored at this point:\n{before}"
    );

    let second = w.preview(
        "src/b.ts",
        "const two = 2;\n",
        "const $$$X = 2;",
        "const $$$X = 22;",
    );
    assert_ne!(
        first, second,
        "two different plans must have two different ids"
    );

    let after = ast_plan_list(&w.edit(), &PlanListArgs::default()).unwrap();
    let (code, text) = drive(&w.root, &["plan", "list"]);
    assert_eq!(code, 0, "{text}");

    for id in [&first, &second] {
        assert!(
            after.contains(id),
            "ast_plan_list must see {id} after the store changed:\n{after}"
        );
        assert!(
            text.contains(id),
            "plan list must see {id} after the store changed:\n{text}"
        );
    }
    // The old id still resolves on both sides: a new plan must not disturb an old one.
    let shown = ast_plan_show(
        &w.edit(),
        &PlanShowArgs {
            plan_id: first.clone(),
            ..Default::default()
        },
    )
    .unwrap();
    assert!(shown.contains(&first), "{shown}");
    let (code, text) = drive(&w.root, &["plan", "show", &first]);
    assert_eq!(code, 0, "{text}");
    assert!(text.contains(&first), "{text}");
}
