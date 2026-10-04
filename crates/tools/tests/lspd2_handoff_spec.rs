//! LSPD2-xx: the handoff to a language server, and the fact that it is not one.
//!
//! `ISSUE-LSPD-2-COMBO-HANDOFF` asks for two things that were missing while everything they
//! depend on already existed:
//!
//! - **`ast_edit_apply` must hand over explicitly.** Its output ended with a hint to run
//!   `lsp_diagnostics`, but nothing pinned the *changed paths* that hint is about, and
//!   nothing tested that the hint is there at all. A hint nobody tests is a comment.
//! - **There must be no language-server dependency.** The engine is specified for a separate
//!   front end to build against ([`LSPD-INTEGRATION.md`](../../docs/LSPD-INTEGRATION.md)); it
//!   does not gain an LSP client, and nothing in L0-L4 changes for the integration. That is a
//!   claim about the dependency tree, so it is tested against the tree.
//!
//! What is deliberately **not** here: an end-to-end run against a real language server. This
//! machine has none, and a test that silently skips is a test that reports nothing. If one is
//! ever added it must skip by default and say so — a CI job that goes green because the thing
//! under test was absent is the failure mode this project keeps finding.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use opencrayast_core::boundary::{Boundary, BoundaryConfig};
use opencrayast_core::limits::Limits;
use opencrayast_edit::{Clock, JournalStore, PlanStore, SystemClock};
use opencrayast_tools::{
    ApplyArgs, EditTools, Mode, PreviewArgs, ToolContext, ast_edit_apply, ast_edit_preview,
};

const WS: &str = "w-00112233445566778899aabbccddeeff";

/// A clock that does not move, so `expires HH:MM` is a value rather than a race.
struct FakeClock(AtomicU64);
impl Clock for FakeClock {
    fn now_secs(&self) -> u64 {
        self.0.load(Ordering::SeqCst)
    }
}

/// A write-mode workspace: a boundary, both stores, and the capability minted the way
/// production mints it (parse a configuration that says `allow_write = true`). There is no
/// test-only bypass, by design.
struct Fx {
    _dir: tempfile::TempDir,
    root: PathBuf,
    tools: ToolContext,
    plans: PlanStore,
    journals: JournalStore,
    state: PathBuf,
}

impl Fx {
    fn new() -> Fx {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("ws");
        fs::create_dir_all(root.join("src")).unwrap();
        let state = dir.path().join("state");
        let limits = Limits::default();
        let clock: Arc<dyn Clock> = Arc::new(FakeClock(AtomicU64::new(1_000_000)));
        let plans = PlanStore::open(&state, WS, limits.clone(), clock.clone()).unwrap();
        let journals = JournalStore::open(&state, WS, limits.clone(), clock).unwrap();
        let write = opencrayast_core::config::Settings::parse("[policy]\nallow_write = true\n")
            .expect("the fixture's own configuration parses")
            .write_permission()
            .map(opencrayast_tools::WriteCap::mint);
        Fx {
            tools: ToolContext {
                boundary: Boundary::new(BoundaryConfig::new(root.clone(), limits.clone())).unwrap(),
                limits,
                mode: Mode::Write,
                write,
                version: "0.20261002.1".into(),
                workspace_id: WS.into(),
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
        let p = self.root.join(rel);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(p, content).unwrap();
    }

    /// Preview a rewrite over one file and return the stored plan id.
    fn preview(&self, rel: &str, source: &str) -> String {
        self.write(rel, source);
        let out = ast_edit_preview(
            &self.edit(),
            &PreviewArgs {
                kind: "rewrite".into(),
                language: Some("typescript".into()),
                paths: Some(vec![rel.into()]),
                pattern: Some("log($$$ARGS)".into()),
                replacement: Some("log2($$$ARGS)".into()),
                note: None,
                ..PreviewArgs::default()
            },
        )
        .unwrap();
        out.split_whitespace().nth(1).unwrap().to_string()
    }

    fn apply(&self, id: &str) -> String {
        ast_edit_apply(
            &self.edit(),
            &ApplyArgs {
                plan_id: id.to_string(),
            },
        )
        .unwrap()
    }
}

/// LSPD2-01: a successful apply **names every changed file**, so the next step has a subject.
///
/// Without the list, "run diagnostics on the changed files" is an instruction with no object —
/// the reader has to diff the plan themselves to know what to check, and the obvious failure
/// is checking a superset or forgetting the nested one.
#[test]
fn lspd2_01_apply_names_every_changed_file() {
    let fx = Fx::new();
    let id = fx.preview("src/a.ts", "log(1);\n");
    let out = fx.apply(&id);

    assert!(
        out.contains("1 files changed") || out.contains("1 file changed"),
        "the apply must report the change count: {out}"
    );
    assert!(
        out.contains("src/a.ts"),
        "the apply must name the file it changed — the next step refers to 'the changed files': {out}"
    );
    // And it is the relative path, never the absolute one: an absolute path here would put a
    // machine path in a document an agent reads and copies.
    assert!(
        !out.contains(&fx.root.to_string_lossy().into_owned()),
        "no absolute workspace path may appear in the output: {out}"
    );
}

/// LSPD2-02: the handoff instruction is present and points at diagnostics.
///
/// This is the whole point of the ticket in one assertion. The line existed but nothing
/// tested it, so deleting it — or replacing it with a vaguer sentence — was free.
#[test]
fn lspd2_02_apply_points_at_language_server_diagnostics() {
    let fx = Fx::new();
    let id = fx.preview("src/a.ts", "log(1);\n");
    let out = fx.apply(&id);

    assert!(
        out.contains("lsp_diagnostics"),
        "a successful apply must tell the reader to verify semantics with lsp_diagnostics: {out}"
    );
    // The reason matters as much as the tool: a reader who skips it because it looks
    // decorative is exactly the failure the line exists to prevent.
    assert!(
        out.contains("verify the semantics") || out.contains("semantic"),
        "the instruction must say WHY, not only which tool: {out}"
    );
    // It must also say what it is not, or "syntax errors 0 → 0" reads like a clean bill.
    assert!(
        out.contains("syntax errors"),
        "the per-file syntax counts must be present, so the next step has something to \
         distinguish itself from: {out}"
    );
}

/// LSPD2-03: the syntax counts are honestly framed — they are the gate, not diagnostics.
///
/// A file can go `0 → 0` and still be wrong. The output must not invite the reader to treat
/// that row as a verification.
#[test]
fn lspd2_03_syntax_counts_are_not_presented_as_verification() {
    let fx = Fx::new();
    let id = fx.preview("src/a.ts", "log(1);\n");
    let out = fx.apply(&id);

    assert!(
        out.contains("syntax errors 0 → 0"),
        "the gate counts must be reported as the gate measures them: {out}"
    );
    // The word "verified" must not appear next to those counts.
    for line in out.lines().filter(|l| l.contains("syntax errors")) {
        assert!(
            !line.to_lowercase().contains("verified"),
            "a syntax-gate row must not claim verification: {line}"
        );
    }
}

/// LSPD2-04: **the dependency tree has no language server in it.**
///
/// The integration is a specification for another repository to build against. If an `lspd`
/// crate ever entered this workspace, the engine would inherit a socket, a startup cost and a
/// version-skew failure mode — and the security story (one reviewed path policy, no network)
/// would quietly acquire an exception. This is a claim about `Cargo.lock`, so it is checked
/// against `Cargo.lock`.
///
/// **Read at run time, not through `include_str!`.** An `include_str!` snapshot is taken when
/// the test binary is *compiled*, and `cargo test` is free to rewrite `Cargo.lock` while
/// building — so a snapshot can describe a lockfile that no longer exists. That is not
/// hypothetical: while writing this test, a mutation that injected `tower-lsp` into
/// `Cargo.lock` passed, because cargo had already rewritten the file before the test ran. A
/// check whose subject can change between compile and run has to read the subject at run time.
#[test]
fn lspd2_04_the_dependency_tree_contains_no_language_server() {
    let lock_path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../Cargo.lock");
    let lock = fs::read_to_string(&lock_path).unwrap_or_else(|e| {
        panic!("Cargo.lock must be readable for this claim to be checkable: {e}");
    });
    assert!(
        lock.contains("[[package]]"),
        "the lockfile looks wrong-shaped; a scan that finds no package blocks would pass vacuously"
    );

    // The names that would mean a client had been pulled in. Matched case-insensitively on
    // the package-name line, so a description or a comment elsewhere cannot trip it.
    for forbidden in ["lspd", "tower-lsp", "lsp-types", "lsp-server"] {
        let hit = lock.lines().any(|l| {
            let t = l.trim_start_matches('-').trim();
            t.starts_with("name") && t.to_lowercase().contains(forbidden)
        });
        assert!(
            !hit,
            "{} gained a `{forbidden}` package: this engine is specified for another \
             repository to build against and must not gain an LSP client dependency",
            lock_path.display()
        );
    }
}

/// LSPD2-05: the same claim, checked on the **manifests** rather than the lockfile.
///
/// The lockfile alone would miss a mutable ref resolved at build time — the same trap
/// `check-grammar-provenance.sh` is built to catch: cargo writes a branch's resolved commit
/// into the lockfile, so the lockfile alone cannot see that it was ever a float. A workspace
/// manifest is the other half.
#[test]
fn lspd2_05_no_manifest_asks_for_a_language_server() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let mut manifests: Vec<PathBuf> = Vec::new();
    for entry in std::fs::read_dir(root.join("crates")).expect("crates/ exists") {
        let dir = entry.expect("readable entry").path();
        let m = dir.join("Cargo.toml");
        if m.is_file() {
            manifests.push(m);
        }
    }
    manifests.push(root.join("Cargo.toml"));
    assert!(
        !manifests.is_empty(),
        "no manifests found — a scan that read nothing would pass vacuously"
    );

    for m in &manifests {
        let text = fs::read_to_string(m).unwrap();
        for forbidden in ["lspd", "tower-lsp", "lsp-types", "lsp-server"] {
            assert!(
                !text.to_lowercase().contains(forbidden),
                "{} now mentions `{forbidden}`; the engine must not depend on a language server",
                m.display()
            );
        }
    }
}

/// LSPD2-06: a real clock still works — the handoff is not an artefact of `FakeClock`.
///
/// The fixture freezes time so `expires HH:MM` is deterministic. This case uses the system
/// clock, so the test would fail if anything about the apply path depended on the frozen one
/// in a way the shipping binary does not have.
#[test]
fn lspd2_06_the_handoff_survives_the_real_clock() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("ws");
    fs::create_dir_all(root.join("src")).unwrap();
    let state = dir.path().join("state");
    let limits = Limits::default();
    let clock: Arc<dyn Clock> = Arc::new(SystemClock);
    let plans = PlanStore::open(&state, WS, limits.clone(), clock.clone()).unwrap();
    let journals = JournalStore::open(&state, WS, limits.clone(), clock).unwrap();
    let write = opencrayast_core::config::Settings::parse("[policy]\nallow_write = true\n")
        .unwrap()
        .write_permission()
        .map(opencrayast_tools::WriteCap::mint);
    let tools = ToolContext {
        boundary: Boundary::new(BoundaryConfig::new(root.clone(), limits.clone())).unwrap(),
        limits,
        mode: Mode::Write,
        write,
        version: "0.20261002.1".into(),
        workspace_id: WS.into(),
        respect_gitignore: true,
        extra_ignore: Vec::new(),
        config_source: Default::default(),
    };
    fs::write(root.join("src/a.ts"), "log(1);\n").unwrap();
    let edit = EditTools {
        tools: &tools,
        plans: &plans,
        journals: &journals,
        state_dir: &state,
        lock_timeout: std::time::Duration::from_millis(200),
    };
    let previewed = ast_edit_preview(
        &edit,
        &PreviewArgs {
            kind: "rewrite".into(),
            language: Some("typescript".into()),
            paths: Some(vec!["src/a.ts".into()]),
            pattern: Some("log($$$ARGS)".into()),
            replacement: Some("log2($$$ARGS)".into()),
            note: None,
            ..PreviewArgs::default()
        },
    )
    .unwrap();
    let id = previewed.split_whitespace().nth(1).unwrap().to_string();
    let out = ast_edit_apply(&edit, &ApplyArgs { plan_id: id }).unwrap();
    assert!(
        out.contains("lsp_diagnostics") && out.contains("src/a.ts"),
        "the handoff must be identical under the real clock: {out}"
    );
}
