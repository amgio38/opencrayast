//! Spec for ISSUE-EDIT-5: the on-disk journal store (E-6, E-9; EDT-05, EDT-16, EDT-25). Never
//! weaken; add cases. Unix only for now (the store checks owner and mode bits).
#![cfg(unix)]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use opencrayast_core::ErrorCode;
use opencrayast_core::hash::ContentHash;
use opencrayast_core::limits::Limits;
use opencrayast_edit::{
    Clock, Edit, JournalState, JournalStore, Manifest, Plan, PlanFile, PlanRequest,
};
use std::fs;
use std::os::unix::fs::{PermissionsExt, symlink};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

const WS: &str = "w-00112233445566778899aabbccddeeff";
const OTHER_WS: &str = "w-ffeeddccbbaa99887766554433221100";
const DAY: u64 = 86_400;

struct FakeClock(AtomicU64);
impl Clock for FakeClock {
    fn now_secs(&self) -> u64 {
        self.0.load(Ordering::SeqCst)
    }
}

struct Fx {
    _dir: tempfile::TempDir,
    state: PathBuf,
    clock: Arc<FakeClock>,
}

impl Fx {
    fn new() -> Fx {
        let dir = tempfile::tempdir().unwrap();
        let state = dir.path().join("state");
        Fx {
            _dir: dir,
            state,
            clock: Arc::new(FakeClock(AtomicU64::new(1_000_000))),
        }
    }
    fn open(&self, limits: Limits) -> JournalStore {
        JournalStore::open(&self.state, WS, limits, self.clock.clone()).unwrap()
    }
    fn advance(&self, secs: u64) {
        self.clock.0.fetch_add(secs, Ordering::SeqCst);
    }
    fn now(&self) -> u64 {
        self.clock.0.load(Ordering::SeqCst)
    }
    fn jdir(&self) -> PathBuf {
        self.state.join(format!("ws-{WS}")).join("journal")
    }
}

fn h(b: &[u8]) -> ContentHash {
    ContentHash::of(b)
}

/// A plan over `originals.len()` files whose pre hashes and sizes match `originals`.
fn plan(tag: u32, originals: &[Vec<u8>]) -> Plan {
    Plan {
        format: 1,
        workspace_id: WS.into(),
        engine_format: 1,
        request: PlanRequest {
            kind: "rewrite".into(),
            summary: format!("change {tag}"),
            note: None,
        },
        files: originals
            .iter()
            .enumerate()
            .map(|(i, o)| PlanFile {
                path: format!("f{i:02}.rs"),
                language: "rust".into(),
                pre_hash: h(o),
                pre_size: o.len() as u64,
                pre_errors: 0,
                post_hash: h(b"post"),
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

fn origs(n: usize) -> Vec<Vec<u8>> {
    (0..n)
        .map(|i| format!("original content of file {i}\n").into_bytes())
        .collect()
}

fn files_in(dir: &Path) -> Vec<String> {
    let mut v: Vec<String> = fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    v.sort();
    v
}

fn mode(p: &Path) -> u32 {
    fs::metadata(p).unwrap().permissions().mode() & 0o777
}

#[test]
fn create_lays_out_the_journal_privately_and_returns_a_prepared_manifest() {
    let f = Fx::new();
    let s = f.open(Limits::default());
    let o = origs(2);
    let p = plan(1, &o);
    let m = s.create(&p, &o).unwrap();
    assert_eq!(m.plan_id, p.id());
    assert_eq!(m.workspace_id, WS);
    assert_eq!(m.state, JournalState::Prepared);
    assert_eq!(m.progress, 0);
    assert_eq!((m.created_at, m.updated_at), (f.now(), f.now()));
    assert_eq!(m.files.len(), 2);
    for (i, jf) in m.files.iter().enumerate() {
        assert_eq!(jf.path, format!("f{i:02}.rs"));
        assert_eq!(jf.pre_hash, h(&o[i]));
        assert_eq!(jf.post_hash, h(b"post"));
    }
    assert_eq!(s.load(&m.plan_id).unwrap(), m);
    assert!(s.exists(&m.plan_id).unwrap());

    let d = f.jdir().join(&m.plan_id);
    assert_eq!(
        files_in(&f.jdir()),
        vec![m.plan_id.clone()],
        "no temp leftovers"
    );
    assert_eq!(
        files_in(&d),
        vec!["manifest.json".to_string(), "orig".to_string()]
    );
    assert_eq!(
        fs::read(d.join("manifest.json")).unwrap(),
        m.canonical_bytes()
    );
    assert_eq!(
        Manifest::parse(&fs::read(d.join("manifest.json")).unwrap()).unwrap(),
        m
    );
    for (i, want) in o.iter().enumerate() {
        assert_eq!(&fs::read(d.join("orig").join(i.to_string())).unwrap(), want);
        assert_eq!(mode(&d.join("orig").join(i.to_string())), 0o600);
        assert_eq!(&s.read_original(&m.plan_id, i).unwrap(), want);
    }
    assert_eq!(mode(&d.join("manifest.json")), 0o600);
    for dir in [
        &d,
        &d.join("orig"),
        &f.jdir(),
        &f.jdir().parent().unwrap().to_path_buf(),
    ] {
        assert_eq!(mode(dir), 0o700, "{}", dir.display());
    }
}

#[test]
fn a_plan_can_be_journaled_only_once_in_any_state() {
    let f = Fx::new();
    let s = f.open(Limits::default());
    let o = origs(2);
    let p = plan(1, &o);
    let id = s.create(&p, &o).unwrap().plan_id;
    let path = f.jdir().join(&id).join("manifest.json");
    let steps: [(JournalState, u64); 4] = [
        (JournalState::Prepared, 0),
        (JournalState::Writing, 0),
        (JournalState::Applied, 2),
        (JournalState::Undoing, 0),
    ];
    for (to, progress) in steps {
        if to != JournalState::Prepared {
            s.set_state(&id, to, progress, &p).unwrap();
        }
        let before = fs::read(&path).unwrap();
        assert_eq!(
            s.create(&p, &o).unwrap_err().code,
            ErrorCode::AlreadyApplied,
            "{to:?}"
        );
        assert_eq!(fs::read(&path).unwrap(), before, "nothing touched");
    }
    s.set_state(&id, JournalState::Undone, 2, &p).unwrap();
    assert_eq!(
        s.create(&p, &o).unwrap_err().code,
        ErrorCode::AlreadyApplied
    );
    assert_eq!(files_in(&f.jdir()), vec![id], "no temp leftovers");
}

#[test]
fn create_verifies_the_originals_against_the_plan_and_creates_nothing_on_failure() {
    let f = Fx::new();
    let s = f.open(Limits::default());
    let o = origs(2);
    let p = plan(1, &o);
    let mut wrong_hash = o.clone();
    wrong_hash[1] = b"original content of file 9\n".to_vec(); // same length, other bytes
    assert_eq!(wrong_hash[1].len(), o[1].len());
    let mut wrong_len = o.clone();
    wrong_len[0].push(b'!');
    for (name, bad) in [
        ("too few", o[..1].to_vec()),
        ("too many", [o.clone(), vec![b"x".to_vec()]].concat()),
        ("wrong hash", wrong_hash),
        ("wrong length", wrong_len),
    ] {
        assert_eq!(
            s.create(&p, &bad).unwrap_err().code,
            ErrorCode::PlanCorrupt,
            "{name}"
        );
    }
    assert!(
        files_in(&f.jdir()).is_empty(),
        "nothing was created, not even a temp dir"
    );
    // a plan for another workspace
    let mut foreign = plan(2, &o);
    foreign.workspace_id = OTHER_WS.into();
    assert_eq!(
        s.create(&foreign, &o).unwrap_err().code,
        ErrorCode::WrongWorkspace
    );
    assert!(files_in(&f.jdir()).is_empty());
}

#[test]
fn the_per_plan_size_cap_is_enforced_before_anything_is_written() {
    let f = Fx::new();
    let s = f.open(Limits {
        journal_max_plan_mib: 1,
        ..Limits::default()
    });
    let big = vec![vec![b'x'; 2 * 1024 * 1024]];
    let p = plan(1, &big);
    assert_eq!(
        s.create(&p, &big).unwrap_err().code,
        ErrorCode::LimitExceeded
    );
    assert!(files_in(&f.jdir()).is_empty());
}

#[test]
fn the_total_cap_evicts_only_evictable_journals_and_otherwise_refuses() {
    let f = Fx::new();
    let s = f.open(Limits {
        journal_max_total_mib: 1,
        ..Limits::default()
    });
    let big = |c: u8| vec![vec![c; 600 * 1024]];
    // A is applied (evictable): creating B evicts it
    let pa = plan(1, &big(b'a'));
    let a = s.create(&pa, &big(b'a')).unwrap().plan_id;
    s.set_state(&a, JournalState::Writing, 0, &pa).unwrap();
    s.set_state(&a, JournalState::Applied, 1, &pa).unwrap();
    let pb = plan(2, &big(b'b'));
    let b = s.create(&pb, &big(b'b')).unwrap().plan_id;
    assert_eq!(
        s.load(&a).unwrap_err().code,
        ErrorCode::PlanNotFound,
        "evicted to make room"
    );
    s.load(&b).unwrap();
    // B is still Prepared (not evictable): creating C is refused and B is untouched
    let pc = plan(3, &big(b'c'));
    assert_eq!(
        s.create(&pc, &big(b'c')).unwrap_err().code,
        ErrorCode::LimitExceeded
    );
    s.load(&b).unwrap();
    assert_eq!(files_in(&f.jdir()), vec![b.clone()]);
    // once B is rolled back it is evictable again
    s.set_state(&b, JournalState::RolledBack, 0, &pb).unwrap();
    s.create(&pc, &big(b'c')).unwrap();
    assert_eq!(s.load(&b).unwrap_err().code, ErrorCode::PlanNotFound);
}

#[test]
fn set_state_walks_the_state_machine_durably_and_refuses_everything_else() {
    let f = Fx::new();
    let s = f.open(Limits::default());
    let o = origs(3);
    let p1 = plan(1, &o);
    let id = s.create(&p1, &o).unwrap().plan_id;
    let path = f.jdir().join(&id).join("manifest.json");
    f.advance(10);
    let m = s.set_state(&id, JournalState::Writing, 0, &p1).unwrap();
    assert_eq!(
        (m.state, m.progress, m.updated_at),
        (JournalState::Writing, 0, f.now())
    );
    assert_eq!(m.created_at, f.now() - 10);
    assert_eq!(fs::read(&path).unwrap(), m.canonical_bytes());
    assert_eq!(mode(&path), 0o600);
    assert_eq!(
        files_in(&f.jdir().join(&id)),
        vec!["manifest.json".to_string(), "orig".to_string()]
    );
    // illegal transitions and out-of-range progress are caller bugs: refused, nothing written
    let before = fs::read(&path).unwrap();
    for (to, progress) in [
        (JournalState::Prepared, 0),
        (JournalState::Undoing, 0),
        (JournalState::Undone, 0),
        (JournalState::Applied, 4),
    ] {
        assert_eq!(
            s.set_state(&id, to, progress, &p1).unwrap_err().code,
            ErrorCode::InvalidArgs,
            "{to:?}"
        );
        assert_eq!(fs::read(&path).unwrap(), before);
    }
    s.set_state(&id, JournalState::Applied, 3, &p1).unwrap();
    assert_eq!(s.load(&id).unwrap().state, JournalState::Applied);
    // terminal states do not move
    s.set_state(&id, JournalState::Undoing, 0, &p1).unwrap();
    s.set_state(&id, JournalState::Undone, 3, &p1).unwrap();
    assert_eq!(
        s.set_state(&id, JournalState::Applied, 3, &p1)
            .unwrap_err()
            .code,
        ErrorCode::InvalidArgs
    );
    let absent = "p-aaaaaaaaaaaaaaaaaaaaaaaaaa";
    assert_eq!(
        s.set_state(absent, JournalState::Writing, 0, &p1)
            .unwrap_err()
            .code,
        ErrorCode::PlanNotFound
    );
    assert_eq!(
        s.set_state("p-short", JournalState::Writing, 0, &p1)
            .unwrap_err()
            .code,
        ErrorCode::InvalidArgs
    );
}

#[test]
fn set_progress_is_monotonic_and_only_while_writing_or_undoing() {
    let f = Fx::new();
    let s = f.open(Limits::default());
    let o = origs(3);
    let p2 = plan(1, &o);
    let id = s.create(&p2, &o).unwrap().plan_id;
    assert_eq!(
        s.set_progress(&id, 1, &p2).unwrap_err().code,
        ErrorCode::InvalidArgs,
        "prepared"
    );
    s.set_state(&id, JournalState::Writing, 0, &p2).unwrap();
    assert_eq!(s.set_progress(&id, 1, &p2).unwrap().progress, 1);
    assert_eq!(
        s.set_progress(&id, 1, &p2).unwrap().progress,
        1,
        "equal is allowed"
    );
    assert_eq!(
        s.set_progress(&id, 0, &p2).unwrap_err().code,
        ErrorCode::InvalidArgs,
        "backwards"
    );
    assert_eq!(
        s.set_progress(&id, 4, &p2).unwrap_err().code,
        ErrorCode::InvalidArgs,
        "beyond the files"
    );
    assert_eq!(s.set_progress(&id, 3, &p2).unwrap().progress, 3);
    s.set_state(&id, JournalState::Applied, 3, &p2).unwrap();
    assert_eq!(
        s.set_progress(&id, 3, &p2).unwrap_err().code,
        ErrorCode::InvalidArgs,
        "applied"
    );
    s.set_state(&id, JournalState::Undoing, 0, &p2).unwrap();
    assert_eq!(s.set_progress(&id, 2, &p2).unwrap().progress, 2);
}

#[test]
fn an_original_is_re_verified_every_time_it_is_read() {
    let f = Fx::new();
    let s = f.open(Limits::default());
    let o = origs(2);
    let id = s.create(&plan(1, &o), &o).unwrap().plan_id;
    let orig0 = f.jdir().join(&id).join("orig").join("0");
    assert_eq!(
        s.read_original(&id, 5).unwrap_err().code,
        ErrorCode::InvalidArgs
    );
    assert_eq!(
        s.read_original(&id, usize::MAX).unwrap_err().code,
        ErrorCode::InvalidArgs
    );

    let good = fs::read(&orig0).unwrap();
    fs::write(&orig0, &good[..good.len() - 1]).unwrap(); // truncated
    assert_eq!(
        s.read_original(&id, 0).unwrap_err().code,
        ErrorCode::PlanCorrupt
    );
    let mut altered = good.clone();
    altered[0] ^= 1; // same length, other bytes
    fs::write(&orig0, &altered).unwrap();
    assert_eq!(
        s.read_original(&id, 0).unwrap_err().code,
        ErrorCode::PlanCorrupt
    );
    fs::write(&orig0, &good).unwrap();
    assert_eq!(s.read_original(&id, 0).unwrap(), good);
    assert_eq!(
        s.read_original(&id, 1).unwrap(),
        o[1],
        "the other original is unaffected"
    );

    fs::set_permissions(&orig0, fs::Permissions::from_mode(0o644)).unwrap();
    assert_eq!(
        s.read_original(&id, 0).unwrap_err().code,
        ErrorCode::PlanCorrupt,
        "group/other readable"
    );
    fs::set_permissions(&orig0, fs::Permissions::from_mode(0o600)).unwrap();
    let elsewhere = f.state.join("copy");
    fs::copy(&orig0, &elsewhere).unwrap();
    fs::remove_file(&orig0).unwrap();
    symlink(&elsewhere, &orig0).unwrap();
    assert_eq!(
        s.read_original(&id, 0).unwrap_err().code,
        ErrorCode::PlanCorrupt,
        "symlink"
    );
    fs::remove_file(&orig0).unwrap();
    assert_eq!(
        s.read_original(&id, 0).unwrap_err().code,
        ErrorCode::PlanCorrupt,
        "missing original"
    );
}

#[test]
fn load_rejects_ids_missing_journals_and_tampered_manifests() {
    let f = Fx::new();
    let s = f.open(Limits::default());
    let o = origs(1);
    let id = s.create(&plan(1, &o), &o).unwrap().plan_id;
    for short in [
        "",
        "p-",
        &id[..12],
        "../x",
        "p-aaaaaaaaaaaaaaaaaaaaaaaaaa/..",
    ] {
        assert_eq!(
            s.load(short).unwrap_err().code,
            ErrorCode::InvalidArgs,
            "{short}"
        );
        assert_eq!(
            s.exists(short).unwrap_err().code,
            ErrorCode::InvalidArgs,
            "{short}"
        );
    }
    assert_eq!(
        s.load("p-aaaaaaaaaaaaaaaaaaaaaaaaaa").unwrap_err().code,
        ErrorCode::PlanNotFound
    );
    assert!(!s.exists("p-aaaaaaaaaaaaaaaaaaaaaaaaaa").unwrap());

    let path = f.jdir().join(&id).join("manifest.json");
    let good = fs::read(&path).unwrap();
    fs::write(&path, b"garbage").unwrap();
    assert_eq!(s.load(&id).unwrap_err().code, ErrorCode::PlanCorrupt);
    fs::write(&path, &good).unwrap();
    s.load(&id).unwrap();
    // a valid manifest that belongs to another plan, copied under this id
    let other_o = origs(2);
    let other_id = s.create(&plan(2, &other_o), &other_o).unwrap().plan_id;
    fs::copy(f.jdir().join(&other_id).join("manifest.json"), &path).unwrap();
    assert_eq!(
        s.load(&id).unwrap_err().code,
        ErrorCode::PlanCorrupt,
        "manifest names another plan"
    );
    fs::write(&path, &good).unwrap();
    // wrong permissions on the manifest
    fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
    assert_eq!(s.load(&id).unwrap_err().code, ErrorCode::PlanCorrupt);
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
    // a journal directory copied into another workspace's store is refused there
    let other = JournalStore::open(&f.state, OTHER_WS, Limits::default(), f.clock.clone()).unwrap();
    let dst = f
        .state
        .join(format!("ws-{OTHER_WS}"))
        .join("journal")
        .join(&id);
    fs::create_dir(&dst).unwrap();
    fs::set_permissions(&dst, fs::Permissions::from_mode(0o700)).unwrap();
    fs::copy(&path, dst.join("manifest.json")).unwrap();
    assert_eq!(other.load(&id).unwrap_err().code, ErrorCode::PlanCorrupt);
}

#[test]
fn list_and_nonterminal_see_every_journal_and_report_the_unreadable_ones() {
    let f = Fx::new();
    let s = f.open(Limits::default());
    let mk = |tag: u32| {
        let o = origs(1);
        let ptag = plan(tag, &o);
        let id = s.create(&ptag, &o).unwrap().plan_id;
        (id, ptag)
    };
    let ((a, _), (b, pb), (c, pc)) = (mk(1), mk(2), mk(3));
    s.set_state(&b, JournalState::Writing, 0, &pb).unwrap();
    s.set_state(&c, JournalState::Writing, 0, &pc).unwrap();
    s.set_state(&c, JournalState::Applied, 1, &pc).unwrap();
    fs::create_dir(f.jdir().join(".tmp-crashed")).unwrap(); // a crashed create: ignored
    let (listed, bad) = s.list().unwrap();
    let mut want = vec![a.clone(), b.clone(), c.clone()];
    want.sort();
    assert_eq!(
        listed.iter().map(|m| m.plan_id.clone()).collect::<Vec<_>>(),
        want
    );
    assert!(bad.is_empty(), "{bad:?}");
    let nt: Vec<String> = s
        .nonterminal()
        .unwrap()
        .into_iter()
        .map(|m| m.plan_id)
        .collect();
    let mut want_nt = vec![a.clone(), b.clone()];
    want_nt.sort();
    assert_eq!(nt, want_nt, "prepared and writing, not applied");

    // an unreadable journal is listed as bad, and recovery is told rather than left to skip it
    let broken = "p-bbbbbbbbbbbbbbbbbbbbbbbbbb";
    let d = f.jdir().join(broken);
    fs::create_dir(&d).unwrap();
    fs::set_permissions(&d, fs::Permissions::from_mode(0o700)).unwrap();
    fs::write(d.join("manifest.json"), b"nonsense").unwrap();
    let (listed, bad) = s.list().unwrap();
    assert_eq!(listed.len(), 3);
    assert_eq!(bad, vec![broken.to_string()]);
    let e = s.nonterminal().unwrap_err();
    assert_eq!(e.code, ErrorCode::PlanCorrupt);
    assert!(e.message.contains(broken), "{}", e.message);
}

#[test]
fn retention_by_age_evicts_old_evictable_journals_only() {
    let f = Fx::new();
    let s = f.open(Limits {
        journal_retention_days: 7,
        ..Limits::default()
    });
    let mk = |tag: u32, to: &[(JournalState, u64)]| {
        let o = origs(1);
        let ptag = plan(tag, &o);
        let id = s.create(&ptag, &o).unwrap().plan_id;
        for (st, p) in to {
            s.set_state(&id, *st, *p, &ptag).unwrap();
        }
        id
    };
    let applied = [(JournalState::Writing, 0), (JournalState::Applied, 1)];
    let old_applied = mk(1, &applied); // t0
    let stuck_writing = mk(2, &[(JournalState::Writing, 0)]); // t0, never evictable
    let stuck_undoing = mk(
        3,
        &[
            (JournalState::Writing, 0),
            (JournalState::Applied, 1),
            (JournalState::Undoing, 0),
        ],
    );
    f.advance(3 * DAY);
    let rolled_back = mk(
        4,
        &[(JournalState::Writing, 0), (JournalState::RolledBack, 0)],
    ); // t0+3d
    f.advance(3 * DAY);
    let fresh_applied = mk(5, &applied); // t0+6d
    f.advance(DAY - 1); // t0+7d-1s: the oldest is 1 second short of the limit
    assert!(s.evict().unwrap().is_empty());
    f.advance(1); // t0+7d: age == retention => evicted
    assert_eq!(s.evict().unwrap(), vec![old_applied.clone()]);
    assert_eq!(
        s.load(&old_applied).unwrap_err().code,
        ErrorCode::PlanNotFound
    );
    for keep in [&stuck_writing, &stuck_undoing, &rolled_back, &fresh_applied] {
        s.load(keep).unwrap();
    }
    f.advance(30 * DAY); // everything is ancient, but only the evictable ones go
    let mut gone = s.evict().unwrap();
    gone.sort();
    let mut want = vec![rolled_back, fresh_applied];
    want.sort();
    assert_eq!(gone, want);
    for keep in [&stuck_writing, &stuck_undoing] {
        s.load(keep).unwrap();
    }
    assert!(s.evict().unwrap().is_empty(), "idempotent");
}

#[test]
fn retention_by_size_evicts_the_oldest_first_with_ties_broken_by_id() {
    let f = Fx::new();
    let s = f.open(Limits {
        journal_max_total_mib: 1,
        journal_retention_days: 365,
        ..Limits::default()
    });
    let big = |c: u8| vec![vec![c; 300 * 1024]];
    let mk = |tag: u32, c: u8, advance: u64| {
        f.advance(advance);
        let ptag = plan(tag, &big(c));
        let id = s.create(&ptag, &big(c)).unwrap().plan_id;
        s.set_state(&id, JournalState::Writing, 0, &ptag).unwrap();
        s.set_state(&id, JournalState::Applied, 1, &ptag).unwrap();
        id
    };
    let first = mk(1, b'a', 0);
    let second = mk(2, b'b', 10);
    let third = mk(3, b'c', 10);
    // 3 x 300 KiB fits in 1 MiB; a fourth does not, and creating it evicts the oldest
    let fourth = mk(4, b'd', 10);
    assert_eq!(s.load(&first).unwrap_err().code, ErrorCode::PlanNotFound);
    for keep in [&second, &third, &fourth] {
        s.load(keep).unwrap();
    }
    // same updated_at: the smaller plan id goes first
    let fx2 = Fx::new();
    let s2 = fx2.open(Limits {
        journal_max_total_mib: 1,
        journal_retention_days: 365,
        ..Limits::default()
    });
    let ids: Vec<String> = (0..3)
        .map(|t| {
            let ptag = plan(10 + t, &big(b'x' + t as u8));
            let id = s2.create(&ptag, &big(b'x' + t as u8)).unwrap().plan_id;
            s2.set_state(&id, JournalState::Writing, 0, &ptag).unwrap();
            s2.set_state(&id, JournalState::Applied, 1, &ptag).unwrap();
            id
        })
        .collect();
    let mut sorted = ids.clone();
    sorted.sort();
    let extra = s2
        .create(&plan(99, &big(b'q')), &big(b'q'))
        .unwrap()
        .plan_id;
    s2.load(&extra).unwrap();
    assert_eq!(
        s2.load(&sorted[0]).unwrap_err().code,
        ErrorCode::PlanNotFound,
        "smallest id evicted first"
    );
    s2.load(&sorted[1]).unwrap();
    s2.load(&sorted[2]).unwrap();
}

#[test]
fn evict_removes_leftover_temp_directories_and_never_reads_them_as_journals() {
    let f = Fx::new();
    let s = f.open(Limits::default());
    let o = origs(1);
    let id = s.create(&plan(1, &o), &o).unwrap().plan_id;
    // a crashed create that got as far as writing a complete-looking manifest
    let tmp = f.jdir().join(".tmp-deadbeef");
    fs::create_dir(&tmp).unwrap();
    fs::copy(
        f.jdir().join(&id).join("manifest.json"),
        tmp.join("manifest.json"),
    )
    .unwrap();
    assert_eq!(
        s.list().unwrap().0.len(),
        1,
        "temp directories are not journals"
    );
    assert!(s.evict().unwrap().is_empty());
    assert_eq!(files_in(&f.jdir()), vec![id], "the temp directory is gone");
}

#[test]
fn a_replaced_journal_directory_is_refused_by_every_operation() {
    for how in ["symlink", "other_dir"] {
        let f = Fx::new();
        let s = f.open(Limits::default());
        let o = origs(1);
        let p = plan(1, &o);
        let id = s.create(&p, &o).unwrap().plan_id;
        let elsewhere = f.state.join("elsewhere");
        fs::create_dir(&elsewhere).unwrap();
        fs::rename(f.jdir(), f.state.join("moved")).unwrap();
        if how == "symlink" {
            symlink(&elsewhere, f.jdir()).unwrap();
        } else {
            fs::create_dir(f.jdir()).unwrap();
        }
        let before = (
            fs::read_dir(&elsewhere).unwrap().count(),
            fs::read_dir(f.jdir()).unwrap().count(),
        );
        let p = plan(2, &o);
        let io = ErrorCode::IoError;
        assert_eq!(s.create(&p, &o).unwrap_err().code, io, "{how}: create");
        assert_eq!(s.load(&id).unwrap_err().code, io, "{how}: load");
        assert_eq!(s.exists(&id).unwrap_err().code, io, "{how}: exists");
        assert_eq!(
            s.set_state(&id, JournalState::Writing, 0, &p)
                .unwrap_err()
                .code,
            io,
            "{how}: set_state"
        );
        assert_eq!(
            s.set_progress(&id, 0, &p).unwrap_err().code,
            io,
            "{how}: set_progress"
        );
        assert_eq!(
            s.read_original(&id, 0).unwrap_err().code,
            io,
            "{how}: read_original"
        );
        assert_eq!(s.list().unwrap_err().code, io, "{how}: list");
        assert_eq!(s.nonterminal().unwrap_err().code, io, "{how}: nonterminal");
        assert_eq!(s.evict().unwrap_err().code, io, "{how}: evict");
        let after = (
            fs::read_dir(&elsewhere).unwrap().count(),
            fs::read_dir(f.jdir()).unwrap().count(),
        );
        assert_eq!(before, after, "{how}: nothing written anywhere");
    }
}

#[test]
fn racing_creates_of_one_plan_produce_exactly_one_journal() {
    let f = Fx::new();
    let s = Arc::new(f.open(Limits::default()));
    let o = origs(3);
    let p = Arc::new(plan(1, &o));
    let o = Arc::new(o);
    let handles: Vec<_> = (0..8)
        .map(|_| {
            let (s, p, o) = (s.clone(), p.clone(), o.clone());
            std::thread::spawn(move || s.create(&p, &o).map(|m| m.plan_id).map_err(|e| e.code))
        })
        .collect();
    let results: Vec<_> = handles.into_iter().map(|h| h.join().unwrap()).collect();
    let ok = results.iter().filter(|r| r.is_ok()).count();
    assert_eq!(ok, 1, "{results:?}");
    assert!(
        results
            .iter()
            .all(|r| r.is_ok() || *r == Err(ErrorCode::AlreadyApplied)),
        "{results:?}"
    );
    assert_eq!(
        files_in(&f.jdir()),
        vec![p.id()],
        "one journal, no temp leftovers"
    );
    s.load(&p.id()).unwrap();
}

#[test]
fn invalid_workspace_ids_and_loose_state_directories_are_refused() {
    let f = Fx::new();
    for ws in ["", "w-1", "../x", "w-00112233445566778899AABBCCDDEEFF"] {
        let e = JournalStore::open(&f.state, ws, Limits::default(), f.clock.clone()).unwrap_err();
        assert_eq!(e.code, ErrorCode::InvalidArgs, "{ws}");
    }
    assert!(!f.state.exists(), "an invalid workspace id creates nothing");
    fs::create_dir_all(&f.state).unwrap();
    fs::set_permissions(&f.state, fs::Permissions::from_mode(0o755)).unwrap();
    let e = JournalStore::open(&f.state, WS, Limits::default(), f.clock.clone()).unwrap_err();
    assert_eq!(e.code, ErrorCode::IoError);
    assert_eq!(mode(&f.state), 0o755, "never repaired");
}

/// An unrelated `create` must **never** expire another plan's journal.
///
/// `create` used to call `evict_locked` with no size cap at all, so the age pass deleted *every*
/// journal past `journal_retention_days` on every single create. Executed: apply a plan, wait
/// past the retention window, then apply an unrelated second plan — and the first plan's journal,
/// the only copy of the originals that makes its edit undoable, was destroyed as a side effect of
/// work that had nothing to do with it. The refusal the operator then got was a bare
/// `plan_not_found`, which cannot distinguish "never applied" from "silently reaped".
///
/// An expiry is a statement about *this* journal's age. It must not be triggered by an
/// unrelated request, so the age pass on the `create` path is bounded by the store's size cap.
#[test]
fn an_unrelated_create_does_not_expire_another_plans_journal() {
    let f = Fx::new();
    // Generous total-size cap: the store is nowhere near full, which is exactly the case the old
    // code got wrong — it evicted on age regardless of how much room there was.
    let s = f.open(Limits {
        journal_retention_days: 7,
        ..Limits::default()
    });
    let mk = |tag: u32| {
        let o = origs(1);
        let p = plan(tag, &o);
        let id = s.create(&p, &o).unwrap().plan_id;
        s.set_state(&id, JournalState::Writing, 0, &p).unwrap();
        s.set_state(&id, JournalState::Applied, 1, &p).unwrap();
        id
    };

    let a = mk(1);
    f.advance(30 * DAY); // A is now far past its 7-day retention

    // An unrelated second journal, created while A is long expired.
    let b = mk(2);
    s.load(&a).expect(
        "A's journal must survive an unrelated create: expiry is A's own business, and the \
         store is nowhere near its size cap",
    );
    assert!(s.exists(&a).unwrap());
    // And B is present too — the test is not passing because create failed.
    s.load(&b).unwrap();

    // The same aged journal IS reclaimable by the maintenance pass, so the above is a gate and
    // not a removal of the policy. B was created after the clock had already advanced, so it is
    // not yet aged and must survive: retention is per-journal age, never a store-wide wipe.
    assert_eq!(s.evict().unwrap(), vec![a.clone()]);
    s.load(&b)
        .expect("B is not past retention and must survive A's eviction");
}

/// And the boundary of the fix: when the store genuinely IS over its size cap, an aged journal is
/// still fair game for `create` to reclaim. Without this the fix would have been "never evict on
/// create", which turns the size cap into a `limit_exceeded` refusal instead.
#[test]
fn create_still_reclaims_an_aged_journal_when_the_store_is_over_its_cap() {
    let f = Fx::new();
    // `journal_max_total_mib` is in MiB and the smallest it can be is 1, which still fits one
    // small journal. So the cap alone cannot make the store over-full with just A in it; what
    // forces the reclamation is that TWO aged journals do not fit in one MiB while ONE does.
    // Creating both first, then the third, is the over-cap case.
    let s = f.open(Limits {
        journal_retention_days: 7,
        journal_max_total_mib: 1,
        ..Limits::default()
    });
    let mk_with = |tag: u32, size: usize| {
        let o = vec![vec![b'x'; size]];
        let p = plan(tag, &o);
        let id = s.create(&p, &o).unwrap().plan_id;
        s.set_state(&id, JournalState::Writing, 0, &p).unwrap();
        s.set_state(&id, JournalState::Applied, 1, &p).unwrap();
        id
    };

    // Two journals, each around 700 KB, both aged. Together (1.4 MB) they exceed the one-MiB
    // cap while either one alone does not, so the third create has to reclaim to fit.
    let a = mk_with(1, 700_000);
    f.advance(30 * DAY);
    let second = mk_with(2, 700_000);
    f.advance(30 * DAY);

    let o2 = vec![b"small journal\n".to_vec()];
    let third = s
        .create(&plan(3, &o2), &o2)
        .expect("aged journals are reclaimed to make room");
    // Reclaiming the OLDEST aged journal is enough to fit the incoming one, so exactly one goes.
    // Not "every aged journal": the point of the fix is that `create` frees only the room it
    // needs, and the size pass stops as soon as it has it.
    assert!(
        !s.exists(&a).unwrap(),
        "the oldest aged journal is reclaimed to make room"
    );
    s.exists(&second)
        .unwrap()
        .then_some(())
        .expect("the second aged journal survives: one reclaim was enough");
    s.load(&third.plan_id)
        .expect("and the incoming journal is stored");
}

/// The age pass deletes **oldest first**, so when the cap bites the least valuable bytes go. A
/// fresh journal is never the one sacrificed for an aged one.
#[test]
fn the_age_pass_removes_the_oldest_first() {
    let f = Fx::new();
    let s = f.open(Limits {
        journal_retention_days: 7,
        ..Limits::default()
    });
    let mk = |tag: u32| {
        let o = origs(1);
        let p = plan(tag, &o);
        let id = s.create(&p, &o).unwrap().plan_id;
        s.set_state(&id, JournalState::Writing, 0, &p).unwrap();
        s.set_state(&id, JournalState::Applied, 1, &p).unwrap();
        id
    };
    let older = mk(1);
    f.advance(10 * DAY);
    let newer = mk(2);
    f.advance(10 * DAY); // both past retention, `newer` is 10 days fresher

    let mut gone = s.evict().unwrap();
    gone.sort();
    let mut want = vec![older.clone(), newer.clone()];
    want.sort();
    assert_eq!(gone, want, "both are past retention, so both go");
}
