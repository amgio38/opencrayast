//! LMT-07 — a binary file that is *named* like source must be refused, never edited.
//!
//! Why it lives in-crate: applying a plan needs
//! write mode, and SEC-FIX 4 makes the write capability mintable only from inside the crate.
//! The external `tests/lmt07_binary_edit_spec.rs` is now a pointer to this file.
//!
//! Why it matters: a plan for such a file cannot be built honestly — its bytes are not text —
//! so a plan naming one is either forged or built by a confused caller. Either way apply must
//! refuse, and it must refuse BEFORE it trusts `pre_hash`, or the UTF-8 check becomes an
//! ordering detail a later refactor can quietly move.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use crate::apply::{ApplyContext, NoFault, apply};
use crate::{Clock, Edit, Plan, PlanFile, PlanRequest, PlanStore, policy};
use opencrayast_core::ErrorCode;
use opencrayast_core::boundary::{Boundary, BoundaryConfig};
use opencrayast_core::hash::ContentHash;
use opencrayast_core::limits::Limits;
use opencrayast_core::workspace::workspace_id;
use std::fs;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

struct FakeClock(AtomicU64);
impl Clock for FakeClock {
    fn now_secs(&self) -> u64 {
        self.0.load(Ordering::SeqCst)
    }
}

struct World {
    _dir: tempfile::TempDir,
    root: PathBuf,
    state: PathBuf,
    ws: String,
    boundary: Boundary,
    plans: PlanStore,
    journals: crate::JournalStore,
    limits: Limits,
    /// Kept so the stores can be reopened over the same directories; not read after `new`.
    #[allow(dead_code)]
    clock: Arc<FakeClock>,
}

impl World {
    fn new() -> World {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("ws");
        fs::create_dir(&root).unwrap();
        let state = dir.path().join("state");
        let ws = workspace_id(&root).unwrap();
        let clock = Arc::new(FakeClock(AtomicU64::new(1_000_000)));
        let limits = Limits::default();
        let boundary = Boundary::new(BoundaryConfig {
            root: root.clone(),
            state_dir: Some(state.clone()),
            limits: Limits::default(),
            read_roots: Vec::new(),
            extra_protected: Vec::new(),
        })
        .unwrap();
        let plans = PlanStore::open(&state, &ws, limits.clone(), clock.clone()).unwrap();
        let journals =
            crate::JournalStore::open(&state, &ws, limits.clone(), clock.clone()).unwrap();
        World {
            _dir: dir,
            root,
            state,
            ws,
            boundary,
            plans,
            journals,
            limits,
            clock,
        }
    }

    fn ctx(&self) -> ApplyContext<'_> {
        ApplyContext::new(
            &self.boundary,
            &self.plans,
            &self.journals,
            &self.limits,
            &self.state,
            &self.ws,
            Some(policy::enable_writes()),
            Duration::from_secs(5),
            &NoFault,
        )
    }

    /// Store a one-file plan whose recorded hashes match `bytes` exactly.
    ///
    /// The plan is therefore perfectly CONSISTENT with the file — apply can refuse it for no
    /// reason except that the bytes are not UTF-8. That is what makes this a test of LMT-07
    /// rather than of the stale-plan check.
    fn plan_for(&self, rel: &str, bytes: &[u8]) -> String {
        // One insertion of three bytes at offset 0, so the recorded post_* must be the
        // pre-image plus those three bytes - `PlanStore::put` checks this arithmetic, and a
        // plan that fails it would be rejected as corrupt before apply ever looks at the file.
        let mut post = Vec::with_capacity(bytes.len() + 3);
        post.extend_from_slice(b"// ");
        post.extend_from_slice(bytes);
        let plan = Plan {
            format: 1,
            workspace_id: self.ws.clone(),
            engine_format: 1,
            request: PlanRequest {
                kind: "rewrite".into(),
                summary: "lmt07".into(),
                note: None,
            },
            files: vec![PlanFile {
                path: rel.to_string(),
                // A source-looking name, which is the whole point: the extension says Rust and
                // the bytes say something else entirely.
                language: "rust".into(),
                pre_hash: ContentHash::of(bytes),
                pre_size: bytes.len() as u64,
                pre_errors: 0,
                post_hash: ContentHash::of(&post),
                post_size: post.len() as u64,
                post_errors: 0,
                edits: vec![Edit {
                    start: 0,
                    end: 0,
                    replacement: "// ".into(),
                }],
            }],
        };
        self.plans.put(&plan).unwrap().0
    }
}

/// Bytes that are not valid UTF-8: an ELF-ish prefix, then a lone 0xFF/0xFE and a truncated
/// three-byte sequence. Built from values rather than written as a literal, because
/// `clippy::invalid_from_utf8` can see through a `const` and would (correctly) complain that
/// a literal which is never UTF-8 makes the assertion below a compile-time certainty. The
/// point of the fixture is what `from_utf8` does at RUN time on a real file.
fn binary() -> Vec<u8> {
    vec![
        0x7f, b'E', b'L', b'F', 0x02, 0x01, 0x01, 0x00, // ELF-ish header
        0xff, 0xfe, 0xfd, // never valid in UTF-8
        0x00, 0x80, 0x81, // continuation bytes with no lead
        0xc0, 0xaf, // an overlong encoding, also invalid
    ]
}

/// LMT-07 — apply refuses a non-UTF-8 target with `not_utf8`, having written nothing.
#[test]
fn lmt07_apply_refuses_a_binary_file_named_like_source() {
    let w = World::new();
    let rel = "src/binary.rs";
    fs::create_dir_all(w.root.join("src")).unwrap();
    fs::write(w.root.join(rel), binary()).unwrap();

    // Sanity: the fixture really is the case under test.
    assert!(
        std::str::from_utf8(&binary()).is_err(),
        "the fixture must not be UTF-8"
    );
    assert_eq!(PathBuf::from(rel).extension().unwrap(), "rs");

    // The write policy must ACCEPT this path: it is a regular, single-linked, writable file
    // inside the workspace. If resolve_write refused it, the refusal would prove nothing about
    // the UTF-8 check.
    assert!(
        w.boundary.resolve_write(rel).is_ok(),
        "the write policy must accept the path, so the refusal below is the UTF-8 check"
    );

    let id = w.plan_for(rel, &binary());
    let err = apply(&w.ctx(), &id).unwrap_err();

    assert_eq!(
        err.code,
        ErrorCode::NotUtf8,
        "a binary target must be refused as not_utf8, got {err:?}"
    );
    // The refusal names the file and says what to do, and quotes no content from it.
    assert!(
        err.message.contains(rel) && err.next.contains("UTF-8"),
        "the refusal must name the file and how to proceed: {err:?}"
    );
    assert!(
        !err.message.contains('\u{ff}'),
        "the refusal must not quote the offending bytes: {err:?}"
    );

    // Nothing was written: the file is byte-identical and no journal exists.
    assert_eq!(fs::read(w.root.join(rel)).unwrap(), binary());
    assert!(
        !w.journals.exists(&id).unwrap(),
        "a refused apply must not create a journal"
    );
}

/// LMT-07 — the refusal happens BEFORE the plan's recorded hashes are trusted.
///
/// Same fixture, but the plan's `pre_hash` is deliberately wrong. If the UTF-8 check ran
/// after the hash comparison the caller would get `stale_plan` instead, and a refactor that
/// reordered the two would turn this test red — which is the point: it pins the ordering, not
/// just the code.
#[test]
fn lmt07_not_utf8_is_checked_before_the_plan_hash() {
    let w = World::new();
    let rel = "src/blob.rs";
    fs::create_dir_all(w.root.join("src")).unwrap();
    fs::write(w.root.join(rel), binary()).unwrap();

    let mut plan = Plan {
        format: 1,
        workspace_id: w.ws.clone(),
        engine_format: 1,
        request: PlanRequest {
            kind: "rewrite".into(),
            summary: "lmt07".into(),
            note: None,
        },
        files: vec![PlanFile {
            path: rel.to_string(),
            language: "rust".into(),
            // A hash of something else entirely: if the UTF-8 check came second this plan
            // would be reported as stale and the ordering guarantee would be untested.
            pre_hash: ContentHash::of(b"not the file at all"),
            pre_size: 999_999,
            pre_errors: 0,
            post_hash: ContentHash::of(b"still not the file"),
            post_size: 999_999 + 3,
            post_errors: 0,
            edits: vec![Edit {
                start: 0,
                end: 0,
                replacement: "// ".into(),
            }],
        }],
    };
    // The plan is stored exactly as written, mutation and all.
    plan.request.summary = "lmt07 ordering".into();
    let id = w.plans.put(&plan).unwrap().0;

    let err = apply(&w.ctx(), &id).unwrap_err();
    assert_eq!(
        err.code,
        ErrorCode::NotUtf8,
        "the UTF-8 check must come before the pre_hash comparison, got {err:?}"
    );
    assert_eq!(fs::read(w.root.join(rel)).unwrap(), binary());
}
