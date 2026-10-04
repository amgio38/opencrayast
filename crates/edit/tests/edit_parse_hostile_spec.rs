//! Fuzz-style hostile-input tests for the plan and manifest deserialisers (EDT-20; FUZZ-2).
//!
//! Portable on purpose: these targets are pure functions over byte strings, so unlike the store
//! and journal targets in `edit_state_hostile_spec.rs` they carry no `#![cfg(unix)]` and run on
//! Windows CI too.
//!
//! The invariant under attack is the one a plan depends on to be trustworthy: **an accepted
//! document re-serialises to exactly its input bytes**. `Plan::parse` is the gate that enforces it,
//! so a fuzz target that only checked for "no panic" would be checking almost nothing.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use common::fuzz::{self, Case, Ran};
use opencrayast_core::ErrorCode;
use opencrayast_core::hash::ContentHash;
use opencrayast_core::limits::Limits;
use opencrayast_edit::{
    Edit, JournalFile, JournalState, Manifest, Plan, PlanFile, PlanRequest, PlanStore,
};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

const SEED: u64 = 0x00F0_0DE5_2026_1003;
const CASES: usize = 3000;
const WS: &str = "w-00112233445566778899aabbccddeeff";

// -- The executed-fraction floor, one constant per target -----------------------------
//
// One constant per target, declared here next to the targets it governs. All four measure
// 3000/3000, so each is 1.0 - the measurement, not a target.
//
// The floors used to be one shared `MIN_EXECUTED_FRACTION = 0.95` that every target in the crate
// imported. At 0.95 a 3.3% decline, a 10% decline and a 30% decline all passed, while all of these
// targets execute every case they ask for: the floor had no force against the tree it guarded.
//
// `edit_state.plan_id` is the one target here that CAN legitimately decline, and it is worth being
// precise about why it still does not need to: a platform without state-directory verification
// prints `SKIPPED` and returns BEFORE `run_cases` is called, so it is not a skipped case inside a
// sweep - the sweep never starts. Every case that does run is executed. That is why its floor is
// 1.0 and not something lower: if that target ever grows a decline path, this constant is where
// the exception belongs, in view, with a reason - not a shared number quietly lowered elsewhere.

/// `edit_state.plan_parse`: 3000/3000.
const EDIT_STATE_PLAN_PARSE_MIN_EXECUTED: f64 = 1.0;

/// `edit_state.plan_parse_named`: 3000/3000.
const EDIT_STATE_PLAN_PARSE_NAMED_MIN_EXECUTED: f64 = 1.0;

/// `edit_state.manifest_parse`: 3000/3000.
const EDIT_STATE_MANIFEST_PARSE_MIN_EXECUTED: f64 = 1.0;

/// `edit_state.plan_id`: 3000/3000. See the note above about the platform skip.
const EDIT_STATE_PLAN_ID_MIN_EXECUTED: f64 = 1.0;

fn lim() -> Limits {
    Limits::default()
}

/// A clock that does not move, so an expiry can never turn a case flaky.
struct FakeClock(AtomicU64);
impl opencrayast_edit::Clock for FakeClock {
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
            note: n.is_multiple_of(3).then(|| format!("note {n}")),
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

/// The canonical bytes of several valid plans, as seeds. These are the corpus: mutations of a
/// document that really is canonical, which is the only place the equality gate can be probed.
fn plan_seeds() -> Vec<String> {
    (0..8)
        .map(|n| String::from_utf8(plan(n).canonical_bytes()).unwrap())
        .collect()
}

#[test]
fn a_mutated_plan_document_never_panics_and_is_canonical_when_accepted() {
    let seeds = plan_seeds();
    let seed_refs: Vec<&str> = seeds.iter().map(String::as_str).collect();
    fuzz::run_cases(
        "edit_state.plan_parse",
        SEED,
        &seed_refs,
        CASES,
        |case: &Case| {
            match Plan::parse(case.input.as_bytes(), &lim()) {
                Ok(p) => {
                    // The canonicality gate, as an invariant rather than as a claim: an accepted
                    // document re-serialises to exactly the bytes it was given.
                    assert_eq!(
                        p.canonical_bytes(),
                        case.input.as_bytes(),
                        "an accepted plan is not in canonical form"
                    );
                    // And `parse` runs `check`, so an accepted plan passes a fresh check.
                    p.check(&lim()).expect("an accepted plan passes check");
                }
                Err(e) => assert_eq!(
                    e.code,
                    ErrorCode::PlanCorrupt,
                    "a plan document refused for an unexpected reason: {e:?}"
                ),
            }
            // Both arms assert on what `parse` decided, so there is nothing to decline here.
            Ran::checked()
        },
    )
    .assert_executed_fraction(EDIT_STATE_PLAN_PARSE_MIN_EXECUTED);
}
#[test]
fn a_mutated_plan_document_never_verifies_under_the_wrong_id() {
    let seeds = plan_seeds();
    let seed_refs: Vec<&str> = seeds.iter().map(String::as_str).collect();
    // Every seed's own id, so roughly half the cases are "the right id" and half are not.
    let ids: Arc<Vec<String>> = Arc::new((0..8).map(|n| plan(n).id()).collect());
    let ids = Arc::clone(&ids);
    fuzz::run_cases(
        "edit_state.plan_parse_named",
        SEED ^ 0x9E37,
        &seed_refs,
        CASES,
        move |case: &Case| {
            // The id of the seed this case is derived from, mutated a little: some cases ask for
            // the right id and some for a wrong one, and neither may panic.
            let id = &ids[case.index % ids.len()];
            let asked = if case.index.is_multiple_of(4) {
                {
                    let last = id.chars().last().unwrap();
                    id.replace(
                        last,
                        if case.index.is_multiple_of(8) {
                            "z"
                        } else {
                            "3"
                        },
                    )
                }
            } else {
                id.clone()
            };
            match Plan::parse_named(&asked, case.input.as_bytes(), &lim()) {
                Ok(p) => {
                    assert_eq!(p.canonical_bytes(), case.input.as_bytes());
                    assert_eq!(
                        p.id(),
                        asked,
                        "parse_named accepted bytes under an id they do not hash to"
                    );
                }
                Err(e) => assert_eq!(
                    e.code,
                    ErrorCode::PlanCorrupt,
                    "a plan document refused for an unexpected reason: {e:?}"
                ),
            }
            // Both arms assert on what `parse` decided, so there is nothing to decline here.
            Ran::checked()
        },
    )
    .assert_executed_fraction(EDIT_STATE_PLAN_PARSE_NAMED_MIN_EXECUTED);
}

/// What the store directory looks like after an operation: every path under it, relative and
/// sorted, with the contents hashed. Compared before and after to prove nothing was written
/// outside, and to see what an operation did inside.
/// A valid manifest, varied a little per index.
fn manifest(n: usize) -> Manifest {
    let states = [
        JournalState::Prepared,
        JournalState::Writing,
        JournalState::Applied,
        JournalState::Undoing,
        JournalState::RolledBack,
        JournalState::Undone,
    ];
    let paths = ["src/a.ts", "src/b.ts"];
    Manifest {
        plan_id: plan(n).id(),
        plan_digest: ContentHash::of(plan(n).canonical_bytes().as_slice()),
        workspace_id: WS.into(),
        state: states[n % states.len()],
        files: vec![JournalFile {
            path: paths[n % paths.len()].into(),
            pre_hash: ContentHash::of(format!("before{n}").as_bytes()),
            post_hash: ContentHash::of(format!("after{n}").as_bytes()),
        }],
        progress: (n % 2) as u64,
        created_at: 1_000_000 + n as u64,
        updated_at: 1_000_000 + n as u64,
    }
}

/// The manifest is the same kind of document as the plan - canonical JSON, and an accepted
/// document has to re-serialise to exactly its input bytes. The invariants are identical, so
/// the target is too.
#[test]
fn a_mutated_manifest_document_never_panics_and_is_canonical_when_accepted() {
    let seeds: Vec<String> = (0..8)
        .map(|n| String::from_utf8(manifest(n).canonical_bytes()).unwrap())
        .collect();
    let seed_refs: Vec<&str> = seeds.iter().map(String::as_str).collect();
    fuzz::run_cases(
        "edit_state.manifest_parse",
        SEED ^ 0x5851,
        &seed_refs,
        CASES,
        |case: &Case| {
            match Manifest::parse(case.input.as_bytes()) {
                Ok(m) => {
                    assert_eq!(
                        m.canonical_bytes(),
                        case.input.as_bytes(),
                        "an accepted manifest is not in canonical form"
                    );
                    m.check().expect("an accepted manifest passes check");
                }
                Err(e) => assert_eq!(
                    e.code,
                    ErrorCode::PlanCorrupt,
                    "a manifest refused for an unexpected reason: {e:?}"
                ),
            }
            // `Manifest::parse` is total: every input is either accepted or refused, and both
            // outcomes are asserted on, so no case has a reason to be declined.
            Ran::checked()
        },
    )
    .assert_executed_fraction(EDIT_STATE_MANIFEST_PARSE_MIN_EXECUTED);
}
/// A plan id becomes a file name in the store directory, so this target needs a real store -
/// and `ensure_state_dir` (owner and mode verification, STA-01) is unix-only, answering
/// `unsupported_target` elsewhere. The skip announces itself and names the reason, as the
/// hazard table in `docs/TESTING.md` requires; the three parse targets above run everywhere.
#[test]
fn a_hostile_plan_id_never_becomes_a_path() {
    let dir = tempfile::tempdir().unwrap();
    let state = dir.path().join("state");
    let clock = clock();
    let store = match PlanStore::open(&state, WS, lim(), clock) {
        Ok(store) => Arc::new(store),
        Err(e) if e.code == ErrorCode::UnsupportedTarget => {
            eprintln!("SKIPPED: this platform has no state directory verification ({e:?})");
            return;
        }
        Err(e) => panic!("a unix platform must be able to open a store: {e:?}"),
    };
    let store = Arc::clone(&store);
    fuzz::run_cases(
        "edit_state.plan_id",
        SEED,
        &["p-", "../", "p-../..", "\\u{0}", "p-a", WS, "p-zzzz"],
        CASES,
        move |case: &Case| {
            let asked = format!("{}{}", case.input, case.input);
            match store.get_for_write(&asked) {
                Ok(_) => panic!("{asked:?} was accepted as a plan id"),
                Err(e) => assert_eq!(
                    e.code,
                    ErrorCode::InvalidArgs,
                    "{asked:?} was refused, but not as a malformed id: {e:?}"
                ),
            }
            if let Ok(guard) = store.begin_use(&asked) {
                drop(guard);
                panic!("{asked:?} was accepted as a plan id");
            }
            // Nothing here is conditional: every case asserts that the hostile id is refused, so
            // there is no honest reason for any case to be skipped.
            Ran::checked()
        },
    )
    .assert_executed_fraction(EDIT_STATE_PLAN_ID_MIN_EXECUTED);
}
