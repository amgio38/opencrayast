//! Fuzz-style hostile-input tests for the on-disk state: the plan store and the journal store
//! (EDT-20; FUZZ-2). Unix only, by construction: both stores verify a file's owner and mode bits
//! and `ensure_state_dir` answers `unsupported_target` elsewhere, exactly as in `store_spec.rs`.
//! The portable half of FUZZ-2 - `Plan::parse`, `Plan::parse_named`, `Manifest::parse` - lives in
//! `edit_parse_hostile_spec.rs`, which has no `cfg` and runs on Windows CI too.
//!
//! The invariant under attack is **what comes out is what went in**: whatever an attacker left
//! lying in the store directory, the plan that comes back is the plan that was put, and nothing
//! is ever written outside the store.
//!
//! ## Why the store target is sharded
//!
//! Every case here does real `fsync`-backed writes, which no amount of in-test parallelism can
//! hide: `run_cases` runs cases one at a time so that a single case that does not RETURN is
//! caught by the per-case ceiling. Three thousand fsync-ing cases on a shared machine measured
//! anywhere from 13 to 40 seconds, which is over this file's budget and, worse, unpredictable.
//! So the cases are split into [`STORE_SHARDS`] independent tests the runner puts on separate
//! threads, each shard with its own seed: 3000 cases in total, the set still deterministic, and a
//! printed seed replays exactly (the seed names the shard).
#![cfg(unix)]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use common::fuzz::{self, Case, Ran};
use opencrayast_core::ErrorCode;
use opencrayast_core::hash::ContentHash;
use opencrayast_core::limits::Limits;
use opencrayast_edit::{Clock, Edit, Plan, PlanFile, PlanRequest, PlanStore};
use std::fs;
use std::os::unix::fs::symlink;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

const SEED: u64 = 0x00F0_0DE5_2026_1003;
const CASES: usize = 3000;

/// The store target is the only one that touches the filesystem, and every case does real
/// `fsync`-backed writes, so it does not parallelise within a test the way the pure targets do.
/// It is therefore split into [`STORE_SHARDS`] independent tests that the test runner runs on
/// separate threads: 3000 cases in total, each shard seeded differently so the whole set stays
/// deterministic and a failure replays exactly (the seed printed names the shard).
const STORE_SHARDS: usize = 6;
const STORE_CASES_PER_SHARD: usize = CASES / STORE_SHARDS;
const WS: &str = "w-00112233445566778899aabbccddeeff";

// -- The executed-fraction floor, one constant per target -----------------------------
//
// Declared here rather than imported, so a target that ever needs a lower value carries that
// exception in view, in its own file. Measured: the six store shards execute 500/500 each (3000 in
// total across the shards), and the journal target is compiled out - see its constant below.

/// `edit_state.plan_store[<shard>]`: 500/500 per shard, 3000/3000 across all six.
///
/// Every case does real `fsync`-backed writes and then damages exactly one file, so there is no
/// cheap path on which a case could be declined; both the store's answer and its error code are
/// asserted. 1.0 is the measurement.
///
/// The shards share this one constant because they are one target run six times - one generator,
/// one seed per shard. If a shard ever needs to decline for a reason of its own, that is a change
/// in what the shard is testing, and it belongs in a per-shard value next to the shard, visible.
const EDIT_STATE_PLAN_STORE_MIN_EXECUTED: f64 = 1.0;

/// `edit_state.journal_store`: 1.0, for the same reason, though it does not run yet.
///
/// The target below is `#[cfg(any())]`, switched off until `JournalStore` lands (EDIT-5), so this
/// constant is currently referenced by nothing that executes. It is kept, and kept at 1.0, so that
/// enabling the target after EDIT-5 does not inherit a floor that was quietly chosen for a target
/// nobody had ever measured. Whoever enables it must run it and confirm 3000/3000 before trusting
/// the 1.0; if it does not reach that, the honest move is to report the rate, not to lower this
/// number in advance.
#[allow(
    dead_code,
    reason = "the target that uses it is #[cfg(any())] until EDIT-5 lands"
)]
const EDIT_STATE_JOURNAL_STORE_MIN_EXECUTED: f64 = 1.0;

fn lim() -> Limits {
    Limits::default()
}

/// A clock that does not move, so an expiry can never turn a case flaky.
struct FakeClock(AtomicU64);
impl Clock for FakeClock {
    fn now_secs(&self) -> u64 {
        self.0.load(Ordering::SeqCst)
    }
}

fn clock() -> Arc<FakeClock> {
    Arc::new(FakeClock(AtomicU64::new(1_000_000)))
}

/// A valid plan, varied a little per index so the seeds are not all the same document.
fn plan(n: usize) -> Plan {
    let paths = ["src/a.ts", "src/b.ts", "a.rs"];
    Plan {
        format: 1,
        workspace_id: WS.into(),
        engine_format: 1,
        request: PlanRequest {
            kind: if n.is_multiple_of(2) {
                "rewrite".into()
            } else {
                "symbol".into()
            },
            summary: format!("case {n}"),
            note: (n.is_multiple_of(3)).then(|| format!("note {n}")),
        },
        files: vec![PlanFile {
            path: paths[n % paths.len()].into(),
            language: "typescript".into(),
            pre_hash: ContentHash::of(format!("before{n}").as_bytes()),
            pre_size: 20,
            pre_errors: 0,
            post_hash: ContentHash::of(format!("after{n}").as_bytes()),
            // `post_size` is not free: the checker demands exactly
            // `pre_size - removed + inserted` (E-11), so it is computed, not guessed.
            post_size: (20 - 7 + format!("logger.debug({n})").len()) as u64,
            post_errors: 0,
            edits: vec![Edit {
                start: 5,
                end: 12,
                replacement: format!("logger.debug({n})"),
            }],
        }],
    }
}

fn snapshot(root: &Path) -> Vec<(String, [u8; 32])> {
    let mut out = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let rel = path
                .strip_prefix(root)
                .unwrap_or(&path)
                .to_string_lossy()
                .into_owned();
            let meta = entry.metadata().ok();
            if meta.as_ref().is_some_and(|m| m.is_dir()) {
                out.push((rel.clone(), [0u8; 32]));
                stack.push(path);
            } else {
                out.push((
                    rel.clone(),
                    ContentHash::of(&fs::read(&path).unwrap_or_default()).0,
                ));
            }
        }
    }
    out.sort();
    out
}

/// A plan store in a temporary state directory, plus a sentinel directory beside it that must
/// never be written to.
///
/// One `TempDir` for the whole target, wiped and rebuilt per case: creating three thousand
/// temporary directories costs more than everything else in this file put together, and the
/// property under test (the store's own directory is attacker-controlled) needs the directory
/// to be *reset*, not to be new.
struct Fixture {
    /// Held so the temporary directory lives as long as the fixture.
    _dir: tempfile::TempDir,
    state: PathBuf,
    sentinel: PathBuf,
    clock: Arc<FakeClock>,
}

impl Fixture {
    fn new() -> Fixture {
        let dir = tempfile::tempdir().unwrap();
        let fixture = Fixture {
            state: dir.path().join("state"),
            sentinel: dir.path().join("sentinel"),
            clock: clock(),
            _dir: dir,
        };
        fs::create_dir_all(&fixture.sentinel).unwrap();
        fixture
    }
    /// Drop everything the previous case left behind, so each case starts from the same
    /// attacker-reachable-but-empty directory.
    fn reset(&self) {
        let _ = fs::remove_dir_all(&self.state);
        let _ = fs::remove_dir_all(&self.sentinel);
        fs::create_dir_all(&self.sentinel).unwrap();
    }
    fn open(&self) -> PlanStore {
        PlanStore::open(&self.state, WS, lim(), self.clock.clone()).unwrap()
    }
    fn plans_dir(&self) -> PathBuf {
        self.state.join(format!("ws-{WS}")).join("plans")
    }
}

/// Every error code a store operation is allowed to answer with. Anything else is a bug in the
/// store's decision table, not a hostile input.
const STORE_CODES: &[ErrorCode] = &[
    ErrorCode::PlanCorrupt,
    ErrorCode::PlanNotFound,
    ErrorCode::PlanExpired,
    ErrorCode::WrongWorkspace,
    ErrorCode::InvalidArgs,
    ErrorCode::IoError,
    ErrorCode::LimitExceeded,
    ErrorCode::Busy,
];

fn assert_store_error(e: &opencrayast_core::ToolError, what: &str, case: &Case) {
    assert!(
        STORE_CODES.contains(&e.code),
        "{what} answered {:?} for {:?}, which is not in its decision table",
        e.code,
        case.input
    );
}

/// The store is the one place that decides which plans exist, and its directory is
/// attacker-reachable state. This target attacks the directory: a plan file that is truncated,
/// rewritten, replaced by a symlink, or paired with a meta file that lies.
fn store_shard(shard: usize) {
    let fixture = Arc::new(Fixture::new());
    let case_fixture = Arc::clone(&fixture);
    fuzz::run_cases(
        &format!("edit_state.plan_store[{shard}]"),
        SEED ^ shard as u64,
        &["plan", "meta", "truncated", "symlink", "rewritten"],
        STORE_CASES_PER_SHARD,
        move |case: &Case| {
            let fixture = Arc::clone(&case_fixture);
            fixture.reset();
            let sentinel_before = snapshot(&fixture.sentinel);
            let store = fixture.open();
            let stored = plan(case.index);
            let id = store.put(&stored).expect("a valid plan is stored").0;

            // Whatever the mutation stream says, damage exactly one of the two files (or link
            // one of them elsewhere), the way an attacker with write access would.
            let plan_path = fixture.plans_dir().join(format!("{id}.json"));
            let meta_path = fixture.plans_dir().join(format!("{id}.meta.json"));
            let good_plan = fs::read(&plan_path).unwrap();
            let good_meta = fs::read(&meta_path).unwrap();
            match case.index % 6 {
                0 => {
                    // Truncated plan bytes: a half-written document.
                    let keep = case.input.len().min(good_plan.len());
                    fs::write(&plan_path, &good_plan[..keep]).unwrap();
                }
                1 => {
                    // Rewritten plan bytes: still valid JSON, different content.
                    let other = plan(case.index + 1).canonical_bytes();
                    fs::write(&plan_path, other).unwrap();
                }
                2 => {
                    // A meta file that claims a different expiry.
                    let mut meta: serde_json::Value = serde_json::from_slice(&good_meta).unwrap();
                    meta["expires_at"] = serde_json::json!(u64::MAX);
                    fs::write(&meta_path, serde_json::to_vec(&meta).unwrap()).unwrap();
                }
                3 => {
                    // Truncated meta bytes.
                    fs::write(&meta_path, &good_meta[..good_meta.len() / 2]).unwrap();
                }
                4 => {
                    // The plan file replaced by a symlink pointing out of the store.
                    fs::remove_file(&plan_path).unwrap();
                    symlink("/etc/hostname", &plan_path).unwrap();
                }
                _ => {
                    // The plan file is gone but its meta remains: an orphan.
                    fs::remove_file(&plan_path).unwrap();
                }
            }

            // Every read path must answer, and an answer must be *this* plan or a refusal.
            for (what, result) in [
                ("get_for_write", store.get_for_write(&id)),
                ("get_for_read", store.get_for_read(&id)),
                (
                    "get_for_read(prefix)",
                    store.get_for_read(&id[..12.min(id.len())]),
                ),
            ] {
                match result {
                    Ok((p, _meta)) => assert_eq!(
                        p, stored,
                        "{what} returned a plan that is not the one that was stored"
                    ),
                    Err(e) => assert_store_error(&e, what, case),
                }
            }
            match store.list() {
                Ok((good, _bad)) => {
                    for summary in &good {
                        assert!(summary.id.len() > 2, "a summary without an id");
                    }
                    assert!(
                        good.windows(2).all(|w| w[0].id < w[1].id),
                        "list is unsorted"
                    );
                }
                Err(e) => assert_store_error(&e, "list", case),
            }
            match store.sweep() {
                Ok(n) => assert!(n <= 1, "sweep claimed to delete {n} plans in one call"),
                Err(e) => assert_store_error(&e, "sweep", case),
            }

            // Nothing outside the store directory, ever.
            assert_eq!(
                snapshot(&fixture.sentinel),
                sentinel_before,
                "the store wrote outside its own directory"
            );
            // And no stray files outside the plans directory inside the state dir.
            let state_entries: Vec<String> = fs::read_dir(&fixture.state)
                .unwrap()
                .flatten()
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .collect();
            assert!(
                state_entries.iter().all(|n| n == &format!("ws-{WS}")),
                "the store created something beside its workspace directory: {state_entries:?}"
            );
            // Every case asserts on the store's aftermath; none of them is conditional.
            Ran::checked()
        },
    )
    .assert_executed_fraction(EDIT_STATE_PLAN_STORE_MIN_EXECUTED);
}

macro_rules! store_shard_tests {
    ($($name:ident => $shard:expr),* $(,)?) => {
        $(
            #[test]
            fn $name() {
                store_shard($shard);
            }
        )*
    };
}

store_shard_tests! {
    a_hostile_store_directory_shard_0_never_yields_another_plan => 0,
    a_hostile_store_directory_shard_1_never_yields_another_plan => 1,
    a_hostile_store_directory_shard_2_never_yields_another_plan => 2,
    a_hostile_store_directory_shard_3_never_yields_another_plan => 3,
    a_hostile_store_directory_shard_4_never_yields_another_plan => 4,
    a_hostile_store_directory_shard_5_never_yields_another_plan => 5,
}

/// A plan id is a path component when it is a file name. Whatever an attacker writes into the
/// store directory, an id that is not a full plan id is refused before it can name a path.
/// The journal store target for FUZZ-2, written against the API but switched off until
/// `JournalStore` lands (EDIT-5): `#[cfg(any())]` is always false, so this compiles to nothing
/// and the test below never runs. It is kept here, complete, so that enabling it after EDIT-5 is
/// a one-token edit rather than a rewrite.
///
/// Enable by changing `#[cfg(any())]` to `#[cfg(unix)]` - the surrounding file is already
/// `#![cfg(unix)]`, which is right: the journal checks owner and mode bits.
#[cfg(any())]
#[test]
fn a_hostile_journal_directory_never_yields_another_original() {
    use opencrayast_edit::JournalStore;

    use opencrayast_edit::JournalStore;

    fuzz::run_cases(
        "edit_state.journal_store",
        SEED,
        &["manifest", "orig", "truncated", "symlink", "rewritten"],
        CASES,
        |case: &Case| {
            let fixture = Fixture::new();
            fixture.reset();
            let store =
                JournalStore::open(&fixture.state, WS, lim(), fixture.clock.clone()).unwrap();
            let p = plan(case.index);
            let original = format!("before{}", case.index).into_bytes();
            let manifest = store.create(&p, std::slice::from_ref(&original)).unwrap();
            let journal_dir = fixture
                .state
                .join(format!("ws-{WS}"))
                .join("journal")
                .join(&manifest.plan_id);

            // Damage the journal the way an attacker with write access would.
            let manifest_path = journal_dir.join("manifest.json");
            let orig_path = journal_dir.join("orig/0");
            match case.index % 4 {
                0 => fs::write(&manifest_path, b"{").unwrap(),
                1 => fs::write(&orig_path, b"tampered").unwrap(),
                2 => {
                    fs::remove_file(&orig_path).unwrap();
                    symlink("/etc/hostname", &orig_path).unwrap();
                }
                _ => fs::remove_dir_all(journal_dir.join("orig")).unwrap(),
            }

            match store.load(&manifest.plan_id) {
                Ok(m) => assert_eq!(m, manifest, "load returned a different manifest"),
                Err(e) => assert_store_error(&e, "journal load", case),
            }
            match store.read_original(&manifest.plan_id, 0) {
                Ok(bytes) => assert_eq!(
                    ContentHash::of(&bytes),
                    manifest.files[0].pre_hash,
                    "read_original returned bytes that are not the original (E-6)"
                ),
                Err(e) => assert_store_error(&e, "read_original", case),
            }
            for (what, result) in [
                ("list", store.list().map(|_| ())),
                ("nonterminal", store.nonterminal().map(|_| ())),
                ("evict", store.evict().map(|_| ())),
            ] {
                if let Err(e) = result {
                    assert_store_error(&e, what, case);
                }
            }
            Ran::checked()
        },
    )
    .assert_executed_fraction(EDIT_STATE_JOURNAL_STORE_MIN_EXECUTED);
}
