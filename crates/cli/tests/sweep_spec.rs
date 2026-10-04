//! Spec for the `plan gc` maintenance entry point, and for `doctor`'s report of what it would
//! remove.
//!
//! These exist because the reclamation code was **reachable from nowhere**. `PlanStore::sweep` and
//! `JournalStore::evict` had callers only in tests — `grep -rn "\.sweep(" crates/` gave 14 hits,
//! every one under `tests/` — so there was no server-start hook, no per-call hook and no CLI verb.
//! Executed: 150 long-expired plans plus three `doctor` runs left 300 files and 1.3 MB
//! unchanged. Nothing was wrong with the policies; they simply could not be reached.
//!
//! What is pinned here:
//!
//! 1. `plan gc` reclaims, and reports what it removed.
//! 2. `doctor` reports what **would** be removed, without removing it, and names the verb.
//! 3. Removing the verb makes (1) unreachable — the mutation self-proof.
//!
//! **Mutation self-proof: delete the `PlanCmd::Gc` arm in `crates/cli/src/lib.rs` and the
//! first two tests go red** (`plan gc` becomes an unknown subcommand). Remove the `reclaimable`
//! call in `doctor::run` and the second goes red on its count.

// Unix-only: `set_private` below sets mode bits through `std::os::unix`, and the retention
// behaviour it pins is a unix permission model. Without this the file does not compile on
// Windows, and CI builds a Windows leg — a test that cannot compile is not a passing test.
#![cfg(unix)]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use clap::Parser;
use opencrayast::Cli;
use opencrayast::StateDir;
use opencrayast::exit::{EXIT_ENV, EXIT_OK};
use opencrayast::out::Sink;
use opencrayast_core::limits::Limits;
use opencrayast_core::workspace::workspace_id;
use opencrayast_edit::{Clock, JournalState, JournalStore, PlanStore, SystemClock};
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// Everything printed to stdout, as one string.
#[derive(Default)]
struct Capture(Vec<String>);

impl Sink for Capture {
    fn line(&mut self, s: &str) {
        self.0.push(s.to_string());
    }
    fn diag(&mut self, s: &str) {
        self.0.push(s.to_string());
    }
}

impl Capture {
    fn all(&self) -> String {
        self.0.join("\n")
    }
}

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
        std::fs::write(root.join("src/a.rs"), "fn main() {}\n").unwrap();
        let ws = workspace_id(&root).unwrap();
        World {
            // Outside the workspace: that is the whole point of the relocation.
            state: dir.path().join("state"),
            _dir: dir,
            root,
            ws,
        }
    }
}

/// Drive one invocation against this fixture's state directory.
fn drive(w: &World, args: &[&str]) -> (i32, String) {
    // No `--config`: the fixture has none, so `load_or_default` takes the documented "no file"
    // path, which is the default a real operator runs under.
    let mut full: Vec<&str> = vec!["opencrayast", "--workspace", w.root.to_str().unwrap()];
    full.extend_from_slice(args);
    let cli = Cli::try_parse_from(&full).unwrap_or_else(|e| panic!("{args:?}: {e}"));
    let mut cap = Capture::default();
    let code = opencrayast::run_with_state(
        &cli,
        &mut cap,
        opencrayast::palette::Palette::new(false),
        &mut opencrayast::confirm::Stdin::new(),
        &StateDir::Fixed(&w.state),
    );
    (code, cap.all())
}

/// Store `n` plans that are **already expired**.
///
/// `plan_ttl_minutes = 1` and a clock one day in the future would not work: the envelope's
/// `expires_at` is written by `put` from the same clock, so there is no gap to exploit. Instead
/// the plans are stored normally and the *store is then opened with a clock far ahead of them*,
/// which is exactly the "left alone overnight" situation.
fn store_expired_plans(w: &World, n: usize) -> Vec<String> {
    let clock = Arc::new(SystemClock);
    let store = PlanStore::open(&w.state, &w.ws, Limits::default(), clock).unwrap();
    let mut ids = Vec::new();
    for i in 0..n {
        let bytes = format!("{{\"plan\":{i}}}");
        let id = format!("p-{:0>26}", base32_of(i));
        let path = plans_dir(w, &w.ws).join(format!("{id}.json"));
        std::fs::write(&path, &bytes).unwrap();
        set_private(&path);
        let meta = std::fs::write(
            plans_dir(w, &w.ws).join(format!("{id}.meta.json")),
            br#"{"created_at":1,"expires_at":2,"producer_version":"x"}"#,
        );
        assert!(meta.is_ok());
        set_private(&plans_dir(w, &w.ws).join(format!("{id}.meta.json")));
        ids.push(id);
    }
    store.list().expect("the fixture store lists");
    ids
}

/// A plan id that is a valid full id, distinct per `i`.
fn base32_of(i: usize) -> String {
    const ALPHABET: &[u8] = b"abcdefghijklmnopqrstuvwxyz234567";
    let mut n = i as u64 + 1;
    let mut out = String::new();
    for _ in 0..26 {
        out.push(ALPHABET[(n % 32) as usize] as char);
        n /= 32;
    }
    out
}

fn plans_dir(w: &World, ws: &str) -> PathBuf {
    w.state.join(format!("ws-{ws}")).join("plans")
}

fn set_private(p: &Path) {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(p, std::fs::Permissions::from_mode(0o600)).unwrap();
}

fn count_files(dir: &Path) -> usize {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return 0;
    };
    rd.flatten().count()
}

/// GC-01: `plan gc` reclaims expired plans and says what it removed.
///
/// Mutation self-proof: delete the `PlanCmd::Gc` arm and this fails at argument parsing — the
/// subcommand stops existing, which is precisely the state the code was in.
#[test]
fn gc_removes_expired_plans_and_reports_it() {
    let w = World::new();
    let ids = store_expired_plans(&w, 12);
    assert_eq!(
        count_files(&plans_dir(&w, &w.ws)),
        ids.len() * 2,
        "the fixture wrote {ids:?} and nothing else"
    );

    let (code, text) = drive(&w, &["plan", "gc"]);
    assert_eq!(code, EXIT_OK, "{text}");
    assert!(
        text.contains("Removed 12 plan"),
        "gc must say how many plans it removed:\n{text}"
    );
    assert_eq!(
        count_files(&plans_dir(&w, &w.ws)),
        0,
        "every expired plan is gone after gc"
    );
}

/// GC-02: an idle store is a no-op, not an error. A verb that fails when there is nothing to do is
/// a verb people stop running.
#[test]
fn gc_on_an_empty_store_is_a_no_op() {
    let w = World::new();
    let (code, text) = drive(&w, &["plan", "gc"]);
    assert_eq!(code, EXIT_OK, "{text}");
    assert!(
        text.contains("Removed 0 plan"),
        "an empty store reports nothing removed, not an error:\n{text}"
    );
}

/// GC-03: `doctor` reports what would be reclaimed, **without removing it**, and names the verb.
///
/// Before the verb existed this line was unanswerable: nothing asked the stores what they would
/// do. That is the operator-facing half of the fix.
#[test]
fn doctor_reports_reclaimable_without_removing_it() {
    let w = World::new();
    let ids = store_expired_plans(&w, 7);
    let before = count_files(&plans_dir(&w, &w.ws));
    assert_eq!(before, ids.len() * 2);

    let (code, text) = drive(&w, &["doctor"]);
    assert_eq!(code, EXIT_OK, "doctor must still pass: {text}");
    assert!(
        text.contains("reclaimable"),
        "doctor must report a reclaimable line:\n{text}"
    );
    // Seven plans. A plan is two files (`.json` + `.meta.json`) but **one entry**, and the
    // count must be of entries — asserting on files doubled it and let a real off-by-two ship.
    assert!(
        text.contains("7 plan"),
        "doctor must report the count gc would remove:\n{text}"
    );
    assert!(
        text.contains("plan gc"),
        "doctor must name the verb that does it:\n{text}"
    );
    assert_eq!(
        count_files(&plans_dir(&w, &w.ws)),
        before,
        "doctor is a diagnostic: it must not have removed anything"
    );
}

/// And after `gc`, the same `doctor` says there is nothing left — so the report is a real
/// measurement of the current state, not a fixed string.
#[test]
fn doctor_reports_nothing_reclaimable_once_gc_has_run() {
    let w = World::new();
    store_expired_plans(&w, 5);
    let (code, gc_text) = drive(&w, &["plan", "gc"]);
    assert_eq!(code, EXIT_OK, "{gc_text}");
    assert!(
        gc_text.contains("Removed 5 plan"),
        "gc removed a different number than doctor reported:\n{gc_text}"
    );

    let (code, text) = drive(&w, &["doctor"]);
    assert_eq!(code, EXIT_OK, "{text}");
    assert!(
        text.contains("nothing: no plan or journal is past its retention"),
        "after gc there is nothing to report:\n{text}"
    );
}

/// GC-04: `gc` never removes a journal that recovery needs. `Prepared`, `Writing` and `Undoing`
/// are not evictable at any age, so gc must leave them alone however old they are.
#[test]
fn gc_never_removes_a_journal_recovery_needs() {
    let w = World::new();
    let clock = Arc::new(SystemClock);
    let journals = JournalStore::open(&w.state, &w.ws, Limits::default(), clock.clone()).unwrap();

    // Three journals in the states that are never evicted.
    let mut kept = Vec::new();
    for (i, state) in [
        JournalState::Prepared,
        JournalState::Writing,
        JournalState::Undoing,
    ]
    .into_iter()
    .enumerate()
    {
        // Distinct originals per journal: a plan's id is its content hash, so three identical
        // plans would be three attempts to journal the *same* plan and the second would be
        // refused as `already_applied` (E-9).
        let o = vec![format!("original number {i}\n").into_bytes()];
        let plan = plan_over(&w.ws, &o);
        let m = journals.create(&plan, &o).unwrap();
        // Each state is reached through the journal state machine, not assigned: `prepared` has
        // no direct edge to `undoing`, so an illegal jump is refused and would be the store
        // catching a fixture bug rather than this test testing retention.
        match state {
            JournalState::Prepared => {}
            JournalState::Writing => {
                journals
                    .set_state(&m.plan_id, JournalState::Writing, 0, &plan)
                    .unwrap();
            }
            JournalState::Undoing => {
                journals
                    .set_state(&m.plan_id, JournalState::Writing, 0, &plan)
                    .unwrap();
                journals
                    .set_state(&m.plan_id, JournalState::Applied, 1, &plan)
                    .unwrap();
                journals
                    .set_state(&m.plan_id, JournalState::Undoing, 0, &plan)
                    .unwrap();
            }
            _ => unreachable!("only the three never-evictable states are exercised"),
        }
        kept.push(m.plan_id);
    }
    // Age them by well past the retention window using a clock far in the future.
    let future: Arc<dyn Clock> = Arc::new(FixedClock(u64::MAX / 4));
    let aged = JournalStore::open(
        &w.state,
        &w.ws,
        Limits {
            journal_retention_days: 7,
            ..Limits::default()
        },
        future,
    )
    .unwrap();
    assert!(
        aged.evict().unwrap().is_empty(),
        "a journal recovery needs is never evicted, whatever its age"
    );
    for id in &kept {
        journals.load(id).expect("and it is still there");
    }
}

/// A clock parked in the far future, so "aged past retention" is expressible.
struct FixedClock(u64);
impl Clock for FixedClock {
    fn now_secs(&self) -> u64 {
        self.0
    }
}

/// A minimal valid plan whose pre-image matches `originals`.
fn plan_over(ws: &str, originals: &[Vec<u8>]) -> opencrayast_edit::Plan {
    use opencrayast_core::hash::ContentHash;
    use opencrayast_edit::{Edit, PlanFile, PlanRequest};
    opencrayast_edit::Plan {
        format: 1,
        workspace_id: ws.into(),
        engine_format: 1,
        request: PlanRequest {
            kind: "rewrite".into(),
            summary: "gc fixture".into(),
            note: None,
        },
        files: originals
            .iter()
            .enumerate()
            .map(|(i, o)| PlanFile {
                path: format!("f{i}.rs"),
                language: "rust".into(),
                pre_hash: ContentHash::of(o),
                pre_size: o.len() as u64,
                pre_errors: 0,
                post_hash: ContentHash::of(b"post"),
                post_size: o.len() as u64 + 1,
                post_errors: 0,
                edits: vec![Edit {
                    start: 0,
                    end: 0,
                    replacement: "x".into(),
                }],
            })
            .collect(),
    }
}

/// GC-05: the state directory gc reports is the one it was given, and gc never reaches into the
/// workspace. The removal must be of *state*, and a maintenance command is exactly the kind of
/// thing that would be dangerous if it could also delete workspace files.
#[test]
fn gc_never_touches_the_workspace() {
    let w = World::new();
    store_expired_plans(&w, 3);
    let before = std::fs::read(w.root.join("src/a.rs")).unwrap();

    let (code, text) = drive(&w, &["plan", "gc"]);
    assert_eq!(code, EXIT_OK, "{text}");
    assert_eq!(
        std::fs::read(w.root.join("src/a.rs")).unwrap(),
        before,
        "gc must not modify workspace files"
    );
    assert!(
        !w.root.join(".opencrayast").exists(),
        "and the workspace must not have gained a state directory"
    );
    assert!(
        text.contains("opencrayast"),
        "gc must tell the operator where state lives so they can delete it:\n{text}"
    );
}

/// GC-06: an unusable state directory is an environment error, not a silent success. `gc` that
/// cannot open the store has done nothing, and saying so is the difference between a maintenance
/// command and a misleading one.
#[test]
fn gc_on_an_unusable_state_directory_is_an_environment_error() {
    let w = World::new();
    std::fs::write(&w.state, b"not a directory").unwrap();
    let (code, text) = drive(&w, &["plan", "gc"]);
    assert_eq!(code, EXIT_ENV, "{text}");
    assert!(
        text.contains("io_error"),
        "the refusal must carry its code:\n{text}"
    );
    assert!(
        !text.contains("Removed"),
        "nothing was removed, and it must not say otherwise:\n{text}"
    );
}

/// GC-07: `plan gc` is listed in the help, so the entry point is discoverable. An entry point
/// nobody can find has the same reachability problem as one that does not exist.
#[test]
fn gc_is_discoverable_in_the_help() {
    let help = std::process::Command::new(cli_binary())
        .args(["plan", "--help"])
        .output()
        .expect("running the CLI");
    let text = String::from_utf8_lossy(&help.stdout);
    assert!(
        text.contains("gc"),
        "`plan gc` must appear in `plan --help`, so an operator can find it:\n{text}"
    );
    assert!(text.contains("retention"), "and say what it does:\n{text}");
}

/// The CLI binary, two levels up from the test binary.
fn cli_binary() -> PathBuf {
    let mut p = std::env::current_exe().expect("test binary path");
    p.pop();
    if p.ends_with("deps") {
        p.pop();
    }
    p.join("opencrayast")
}
