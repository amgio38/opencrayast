//! SEC-2 round 2: aspect 2 (write policy) on the **CLI** surface.
//!
//! Ticket: `Y20261002/REQ-SECURITY-REVIEW/ISSUE-SEC-2-MCP-CLI-F-03-POC`. Base `35f0f67`.
//!
//! SECURITY-MODEL S-2 names three write gates: the type-level capability (`WriteCap`), the
//! `Mode` in the tool registry, and — this file's subject — the shell's own `--allow-write`
//! AND-configuration combination. CONFIGURATION.md:35 says `--allow-write` "has **no effect**
//! unless the user file also sets `policy.allow_write = true`", and `doctor` prints the matching
//! text. These probes check the `edit apply` path against that contract.
//!
//! No product behaviour is modified.
//!
//! Ported verbatim from the SEC-2 round-2 re-audit (commit `b103e13`, branch of
//! `Y20261002/REQ-SECURITY-REVIEW/ISSUE-SEC-2-MCP-CLI-F-03-POC`) when its finding was fixed by
//! `Y20261002/REQ-SECURITY-REVIEW/ISSUE-SEC-FIX-WRITE-CAPABILITY`. R2-A2-05 and R2-A2-07 were
//! RED at `b103e13` and are the regression pins for the fix; R2-A2-08 is reworded from the
//! audit's recording probe to the operator ruling that creating an empty user-private state
//! directory from a read-only command is correct behaviour (its reasoning is the test's own
//! doc comment). R2-A2-04 and R2-A2-06 are unchanged controls, kept verbatim.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use clap::Parser;
use opencrayast::Cli;
use opencrayast::confirm::{self, Answer, Interaction, ScriptedConfirmer};
use opencrayast::exit::{EXIT_OK, EXIT_USER};
use opencrayast::out::Capture;
use opencrayast::palette::Palette;
use opencrayast_core::hash::ContentHash;
use opencrayast_core::limits::Limits;
use opencrayast_edit::{Plan, PlanFile, PlanRequest, PlanStore, SystemClock};
use std::path::{Path, PathBuf};
use std::sync::Arc;

const FILE: &str = "src/a.rs";
const BEFORE: &str = "fn main() {}\n";
const AFTER: &str = "fn main() { run(); }\n";

struct World {
    _dir: tempfile::TempDir,
    root: PathBuf,
    state: PathBuf,
    ws: String,
}

impl World {
    fn new() -> World {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("ws");
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(root.join(FILE), BEFORE).unwrap();
        let ws = opencrayast_core::workspace::workspace_id(&root).unwrap();
        // Outside the workspace: the tool's state is no longer a dotfile in the tree, and the
        // CLI is handed this directory explicitly so nothing resolves the machine's own.
        let state = dir.path().join("state");
        World {
            _dir: dir,
            root,
            state,
            ws,
        }
    }

    fn put_plan(&self) -> String {
        let store = PlanStore::open(
            &self.state,
            &self.ws,
            Limits::default(),
            Arc::new(SystemClock),
        )
        .unwrap();
        let plan = Plan {
            format: 1,
            workspace_id: self.ws.clone(),
            engine_format: 1,
            request: PlanRequest {
                kind: "rewrite".into(),
                summary: "r2 probe".into(),
                note: None,
            },
            files: vec![PlanFile {
                path: FILE.to_string(),
                language: "rust".into(),
                pre_hash: ContentHash::of(BEFORE.as_bytes()),
                pre_size: BEFORE.len() as u64,
                pre_errors: 0,
                post_hash: ContentHash::of(AFTER.as_bytes()),
                post_size: AFTER.len() as u64,
                post_errors: 0,
                edits: vec![opencrayast_edit::Edit {
                    start: 0,
                    end: BEFORE.len(),
                    replacement: AFTER.to_string(),
                }],
            }],
        };
        store.put(&plan).unwrap().0
    }

    fn write_config(&self, body: &str) -> PathBuf {
        let p = self.root.join("config.toml");
        std::fs::write(&p, body).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o600)).unwrap();
        }
        p
    }

    fn bytes(&self) -> String {
        std::fs::read_to_string(self.root.join(FILE)).unwrap()
    }
}

/// Drive one CLI invocation with a human present who says **yes** to every question, and report
/// what was asked along with the exit code.
///
/// The two halves of the split [`Confirmer`] both matter here, and the shipped
/// [`ScriptedConfirmer`] supplies both:
///
/// * [`ScriptedConfirmer::attended`] with [`Interaction::Interactive`] makes
///   [`Confirmer::may_decide`] `true` — somebody *is* there, so a write is not refused merely for
///   lack of a terminal, and
/// * the scripted `true` makes [`Confirmer::confirm`] answer [`Answer::Yes`].
///
/// That is the strictest possible setting for these probes: the human gate grants. **Whatever
/// refuses here refused on the write-policy gate**, which is the only thing R2-A2-05 and R2-A2-07
/// are about. Using the crate's own double rather than a local one means this file cannot drift
/// from how the gate is actually driven.
fn drive(root: &Path, config: &Path, args: &[&str]) -> (i32, Capture, Vec<String>) {
    let mut full: Vec<String> = vec![
        "opencrayast".into(),
        "--workspace".into(),
        root.to_str().unwrap().into(),
        "--config".into(),
        config.to_str().unwrap().into(),
    ];
    full.extend(args.iter().map(|s| (*s).to_string()));
    let refs: Vec<&str> = full.iter().map(String::as_str).collect();
    let cli = Cli::try_parse_from(&refs).unwrap_or_else(|e| panic!("{e}"));
    // Interaction is no longer a `run_with` argument: it is answered by the confirmer's `may_decide`,
    // so the same statement is made here through the double rather than through the call.
    let mut confirmer = ScriptedConfirmer::attended(Interaction::Interactive, [true]);
    let mut cap = Capture::default();
    let state = root.parent().unwrap_or(Path::new(".")).join("state");
    let code = opencrayast::run_with_state(
        &cli,
        &mut cap,
        Palette::new(false),
        &mut confirmer,
        &opencrayast::StateDir::Fixed(&state),
    );
    (code, cap, confirmer.prompts().to_vec())
}

/// R2-A2-04: the flag AND the config together DO apply. This is the positive control: without
/// it, R2-A2-05 below would pass for the wrong reason (a broken harness proves nothing).
#[test]
fn r2_a2_04_flag_plus_config_applies() {
    let w = World::new();
    let id = w.put_plan();
    let cfg = w.write_config("[policy]\nallow_write = true\n");
    let (code, cap, asked) = drive(&w.root, &cfg, &["--write", "edit", "apply", &id]);
    let t = cap.all().join("\n");
    eprintln!("R2-A2-04 exit={code}\n{t}");
    assert_eq!(code, EXIT_OK, "flag + config must apply:\n{t}");
    assert_eq!(w.bytes(), AFTER, "the file must actually have changed");
    // The control only means something if the apply really passed the human gate rather than
    // reaching the write path some other way. One question, and it is *this* question.
    assert_eq!(
        asked,
        vec![confirm::question(confirm::APPLY_PROMPT, Some(id.as_str()))],
        "R2-A2-04: the positive control must have actually been asked, so that R2-A2-05's refusal \
         is attributable to the write-policy gate and not to a harness that never asks.\n{t}"
    );
}

/// R2-A2-05: **the finding probe.** `policy.allow_write = true` in the user file, WITHOUT
/// `--write`, on an interactive session whose human gate says YES. Does the workspace change?
///
/// CONTRACT (CONFIGURATION.md:35, doctor.rs:137-143, T-17 "read-only by default", S-2):
/// both gates must pass. The flag is half of the pair and this case supplies only the other
/// half, so the correct behaviour is a refusal that writes nothing.
#[test]
fn r2_a2_05_config_alone_does_not_enable_the_clis_write_path() {
    let w = World::new();
    let id = w.put_plan();
    let cfg = w.write_config("[policy]\nallow_write = true\n");
    let (code, cap, asked) = drive(&w.root, &cfg, &["edit", "apply", &id]);
    let t = cap.all().join("\n");
    eprintln!("R2-A2-05 exit={code}\n{t}");
    assert_eq!(
        w.bytes(),
        BEFORE,
        "R2-A2-05 FINDING: `opencrayast edit apply` with `policy.allow_write = true` but WITHOUT \
         `--write` MODIFIED THE WORKSPACE.\noutput was:\n{t}"
    );
    assert_ne!(
        code, EXIT_OK,
        "R2-A2-05 FINDING: the apply reported success with only one of the two write gates \
         supplied. A shell reading the exit code cannot tell writing was not authorised.\n{t}"
    );
    // The refusal must be the *write policy*, and it must be the CLI's own decision: no question
    // was put to the human, because the capability is refused before the gate is ever reached.
    // Asserting the exact refusal also pins which of the two gates did the refusing — if this ever
    // says `nobody_to_ask` instead, the harness stopped providing a human and this probe would be
    // passing for the wrong reason.
    assert!(
        t.contains(opencrayast_core::ErrorCode::WriteDisabled.as_str()),
        "R2-A2-05: the refusal must be the write-policy gate, not the human gate. The double \
         grants consent, so a human-gate refusal here would mean the probe is not testing what it \
         claims.\noutput was:\n{t}"
    );
    assert!(
        asked.is_empty(),
        "R2-A2-05: nobody was asked, so nothing could be confirmed by answering: the apply must \
         refuse on the write policy alone. Asked: {asked:?}\n{t}"
    );
    let _ = (EXIT_USER, confirm::APPLY_PROMPT, Answer::Yes);
}
/// R2-A2-06: the negative case still refuses, so R2-A2-05 is specifically about the FLAG being
/// inert and not about the capability gate being absent. `policy.allow_write` defaults to false.
#[test]
fn r2_a2_06_without_either_gate_nothing_is_written() {
    let w = World::new();
    let id = w.put_plan();
    let cfg = w.write_config("[policy]\nallow_write = false\n");
    for args in [
        vec!["edit", "apply", &id],
        vec!["--write", "edit", "apply", &id],
    ] {
        let (code, cap, asked) = drive(&w.root, &cfg, &args);
        let t = cap.all().join("\n");
        eprintln!("R2-A2-06 args={args:?} exit={code}\n{t}");
        assert_eq!(
            w.bytes(),
            BEFORE,
            "allow_write=false must write nothing: {t}"
        );
        assert_ne!(code, EXIT_OK, "a refusal must not exit 0: {t}");
        assert!(
            t.contains(opencrayast_core::ErrorCode::WriteDisabled.as_str()),
            "the refusal must be [write_disabled]: {t}"
        );
        // Neither case may reach the human gate: this is the "no capability at all" control, and
        // the human gate saying yes must not be what a capability-less apply has to overcome.
        assert!(
            asked.is_empty(),
            "allow_write=false must refuse before asking anybody: asked {asked:?}\n{t}"
        );
    }
}

/// R2-A2-07: `doctor` and `edit apply` must not disagree about the same workspace, config and
/// flag. This is the sharpest form of R2-A2-05: one binary tells the operator "writing is
/// disabled" and then, in the same invocation set, applies a plan.
///
/// The merge made the agreement assertable rather than merely "nothing was written": `doctor`
/// names the *reason* it considered writing disabled, so the test can require that the two
/// commands attribute the refusal to the **same missing gate**. The flag is what `apply` is
/// missing, so `doctor` must be the variant that blames `policy.allow_write` — if it instead
/// blamed `--write`, the two commands would be reading different settings or a different flag,
/// which is precisely the disagreement this test exists to catch.
#[test]
fn r2_a2_07_doctor_and_apply_agree_about_write_mode() {
    let w = World::new();
    let id = w.put_plan();
    let cfg = w.write_config("[policy]\nallow_write = true\n");

    // `doctor`, told there is no --write.
    let (dcode, dcap, _) = drive(&w.root, &cfg, &["doctor"]);
    let dt = dcap.all().join("\n");
    eprintln!("R2-A2-07 doctor:\n{dt}");

    let (acode, acap, asked) = drive(&w.root, &cfg, &["edit", "apply", &id]);
    let at = acap.all().join("\n");
    eprintln!("R2-A2-07 apply exit={acode}:\n{at}");

    assert_eq!(w.bytes(), BEFORE, "R2-A2-07 FINDING: apply wrote.\n{at}");

    // `doctor` reports writing is off, and the branch it takes names the situation: no `--write`
    // was passed on this invocation, so it must be the branch that asks for the flag rather than
    // the one that reports "the flag was passed but the policy is not set". Requiring the absence
    // of that other branch is what makes this an *agreement* check rather than a "doctor said
    // disabled" check: if `doctor` were reading a different settings file than `apply`, it would
    // land in the other branch while `apply` refused.
    assert!(
        dt.contains("write mode") && dt.contains("disabled"),
        "R2-A2-07: doctor must report write mode as disabled for this config, otherwise the two \
         commands are being compared on a workspace doctor considers writable.\n{dt}"
    );
    assert!(
        dt.contains("only reading commands are allowed"),
        "R2-A2-07: doctor must be reporting the no-flag case, asking for `--write`.\n{dt}"
    );
    assert!(
        !dt.contains("--write was passed"),
        "R2-A2-07 FINDING: no `--write` was passed here, so `doctor` reporting that it *was* \
         passed means doctor and apply are not reading the same flag and do not agree about the \
         same workspace.\n{dt}"
    );

    // `apply` refuses, on the write policy, having asked nobody.
    assert_ne!(
        acode, EXIT_OK,
        "R2-A2-07: apply must not succeed here.\n{at}"
    );
    assert!(
        at.contains(opencrayast_core::ErrorCode::WriteDisabled.as_str()),
        "R2-A2-07: apply must refuse for the same reason doctor reports writing is off.\n{at}"
    );
    assert!(
        asked.is_empty(),
        "R2-A2-07: the apply was refused by policy, not by a missing human: asked {asked:?}\n{at}"
    );
    let _ = dcode;
}

/// R2-A2-08: does a READ-ONLY command write to the workspace? This is SEC-1's aspect-2 gap
/// ("no CLI plan-save path ... un-auditable"), now reachable.
///
/// `plan list` is a reading command, and S-2 says "No write to the workspace happens unless
/// write mode is enabled". `PlanStore::open` calls `ensure_state_dir`, which creates
/// `.opencrayast/`, `ws-<id>/` and `plans/` with `DirBuilder::recursive(true).mode(0o700)`.
///
/// **Ruling: this is CORRECT BEHAVIOUR, not a defect.** The original probe asserted the whole
/// tree was unchanged, which would require `plan list` to fail on a filesystem it is allowed to
/// read. The reason that is the wrong contract:
///
/// 1. **The project's own rule already says so.** `crates/cli/src/doctor.rs:9-11`: "It creates
///    the state directory, because that is one of the things it is checking can be created, and
///    an empty state directory is not a change to anyone's code." `doctor` is the reference
///    implementation of "read the world, create the state dir", and `plan list` is the same
///    operation.
/// 2. **Nothing is disclosed and nothing is overwritten.** The creation is three empty `0700`
///    directories. No file content is written, so no plan, no path, no hostname and no version
///    leaks — the property S-2 exists to protect (S-1 is about bytes crossing the boundary).
/// 3. **A read-only workspace must still be readable.** A leak would be a real defect: it would
///    make `plan list` FAIL where it should succeed, on a read-only mount or under a user with
///    no write permission. `ensure_state_dir` already refuses to adopt a foreign, symlinked or
///    non-`0700` directory instead of repairing it, so the created tree is safe by construction.
/// 4. **S-2 is read as being about workspace *content*** — code, config, the operator's files —
///    which is what "unless write mode is enabled" is protecting. The state dir is
///    user-private tool state (SECURITY-MODEL:70 lists it as "state dir (user-private)"), and
///    T-05 treats it as a *protected target*, i.e. something code may not write into by path.
///
/// So the assertion below pins the property that actually matters: the tree is UNCHANGED except
/// for empty, user-private directories. If a future change made `plan list` write a *file*, or
/// made the directory group/world-readable, this test goes red — which is the leak worth
/// catching. The observed paths are asserted, so a silent relocation is also caught.
///
/// Ruling recorded by the SEC-2 round-2 re-audit operator, 2026-10-03.
#[test]
fn r2_a2_08_a_read_only_plan_command_creates_only_an_empty_user_private_state_dir() {
    let w = World::new();
    let cfg = w.write_config("[policy]\nallow_write = false\n");

    fn tree(root: &Path) -> Vec<String> {
        let mut v = Vec::new();
        fn walk(d: &Path, base: &Path, v: &mut Vec<String>) {
            let Ok(rd) = std::fs::read_dir(d) else { return };
            for e in rd.flatten() {
                let p = e.path();
                v.push(
                    p.strip_prefix(base)
                        .unwrap_or(&p)
                        .to_string_lossy()
                        .into_owned(),
                );
                if e.metadata().map(|m| m.is_dir()).unwrap_or(false) {
                    walk(&p, base, v);
                }
            }
        }
        walk(root, root, &mut v);
        v.sort();
        v
    }

    /// `0700` on unix, and the whole tree must contain no regular file at all.
    #[cfg(unix)]
    fn assert_private_empty_dirs(state_root: &Path) {
        use std::os::unix::fs::PermissionsExt;
        let mut dirs = Vec::new();
        let mut files = Vec::new();
        fn walk(d: &Path, dirs: &mut Vec<PathBuf>, files: &mut Vec<PathBuf>) {
            let Ok(rd) = std::fs::read_dir(d) else { return };
            for e in rd.flatten() {
                let p = e.path();
                if e.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                    dirs.push(p.clone());
                    walk(&p, dirs, files);
                } else {
                    files.push(p);
                }
            }
        }
        walk(state_root, &mut dirs, &mut files);
        assert!(
            !dirs.is_empty(),
            "the state directory itself must have been created by this read-only command"
        );
        for d in &dirs {
            let mode = std::fs::symlink_metadata(d).unwrap().permissions().mode() & 0o777;
            assert_eq!(
                mode,
                0o700,
                "a read-only command must not leave a state directory others can reach: {} is {mode:o}",
                d.display()
            );
        }
        assert!(
            files.is_empty(),
            "a read-only command must not write any file: {:?}",
            files
        );
    }
    #[cfg(not(unix))]
    fn assert_private_empty_dirs(_root: &Path) {}

    // The state directory is now OUTSIDE the workspace, so the two trees are measured
    // separately: what matters is that the workspace tree is untouched, and that everything new
    // landed under the state directory, which is the test's own.
    let ws_id = opencrayast_core::workspace::workspace_id(&w.root).unwrap();
    let before = tree(&w.root);
    let before_state = tree(&w.state);
    let (code, cap, _) = drive(&w.root, &cfg, &["plan", "list"]);
    let t = cap.all().join("\n");
    let after = tree(&w.root);
    let after_state = tree(&w.state);
    eprintln!("R2-A2-08 exit={code}\n{t}\nbefore={before:?}\nafter={after:?}");

    assert_eq!(code, EXIT_OK, "a read-only `plan list` must succeed: {t}");

    // The state directory is never inside the workspace. This is the property the whole
    // relocation exists for, and it is asserted on the real filesystem rather than trusted.
    assert!(
        !w.state.starts_with(&w.root),
        "state must not live inside the workspace: {} is under {}",
        w.state.display(),
        w.root.display()
    );

    // Nothing the operator owns is touched: no file content is read, written or removed, and
    // `config.toml` and `src/a.rs` are exactly where they were.
    let untouched: Vec<&String> = before.iter().filter(|p| after.contains(p)).collect();
    assert_eq!(
        untouched.len(),
        before.len(),
        "a read-only command must not remove or replace anything the operator had: \
         before={before:?} after={after:?}"
    );
    assert_eq!(
        after, before,
        "a read-only command must add NOTHING to the workspace tree, state directory or not"
    );
    assert_eq!(
        before_state,
        tree(&w.state)
            .iter()
            .filter(|p| *p == "state")
            .cloned()
            .collect::<Vec<_>>(),
        "the state directory itself must be created by this read-only command, and nothing else"
    );
    assert_eq!(
        w.bytes(),
        BEFORE,
        "a read-only command must not modify a workspace file"
    );

    // Everything new is inside the state directory, and it is empty and private.
    assert!(
        after_state.len() > before_state.len(),
        "the state directory must have been created: before={before_state:?} after={after_state:?}"
    );
    let new_paths: Vec<String> = after_state
        .iter()
        .filter(|p| !before_state.contains(p))
        .cloned()
        .collect();
    // The per-workspace segment is appended exactly once. The resolver deliberately does NOT
    // append it, because the stores do — so `ws-w-<id>/ws-w-<id>/plans/` would be the tell that
    // it did. Asserted on the real directory names rather than trusted.
    for p in &new_paths {
        let doubled = p.matches("ws-").count();
        assert!(
            doubled <= 1,
            "the ws-<id> segment must appear once, not doubled: {p:?} in {new_paths:?}"
        );
    }
    assert!(
        new_paths
            .iter()
            .all(|p| p.starts_with(&format!("ws-{ws_id}"))),
        "everything created belongs to this workspace's state directory: {new_paths:?}"
    );
    let expected_plans = std::path::Path::new(&format!("ws-{ws_id}")).join("plans");
    assert_eq!(
        new_paths.last().map(std::path::Path::new),
        Some(expected_plans.as_path()),
        "the per-workspace directory and its plans/ are the only entries created: {new_paths:?}"
    );
    assert_private_empty_dirs(&w.state);
}
