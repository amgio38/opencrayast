//! Spec for ISSUE-EDIT-3: the plan store (E-2, E-15; EDT-06, EDT-16, EDT-26, EDT-27). Never
//! weaken; add cases. Unix only for now (the store checks owner and mode bits).
#![cfg(unix)]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use opencrayast_core::ErrorCode;
use opencrayast_core::hash::ContentHash;
use opencrayast_core::limits::Limits;
use opencrayast_edit::{Clock, Edit, Plan, PlanFile, PlanRequest, PlanStore};
use std::fs;
use std::os::unix::fs::{PermissionsExt, symlink};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

const WS: &str = "w-00112233445566778899aabbccddeeff";
const OTHER_WS: &str = "w-ffeeddccbbaa99887766554433221100";

struct FakeClock(AtomicU64);
impl Clock for FakeClock {
    fn now_secs(&self) -> u64 {
        self.0.load(Ordering::SeqCst)
    }
}

struct Fixture {
    _dir: tempfile::TempDir,
    state: PathBuf,
    clock: Arc<FakeClock>,
}

impl Fixture {
    fn new() -> Fixture {
        let dir = tempfile::tempdir().unwrap();
        let state = dir.path().join("state");
        Fixture {
            _dir: dir,
            state,
            clock: Arc::new(FakeClock(AtomicU64::new(1_000_000))),
        }
    }
    fn open(&self, limits: Limits) -> PlanStore {
        PlanStore::open(&self.state, WS, limits, self.clock.clone()).unwrap()
    }
    fn advance(&self, secs: u64) {
        self.clock.0.fetch_add(secs, Ordering::SeqCst);
    }
    fn plans_dir(&self) -> PathBuf {
        self.state.join(format!("ws-{WS}")).join("plans")
    }
}

fn plan(n: u32) -> Plan {
    Plan {
        format: 1,
        workspace_id: WS.into(),
        engine_format: 1,
        request: PlanRequest {
            kind: "rewrite".into(),
            summary: format!("change number {n}"),
            note: None,
        },
        files: vec![PlanFile {
            path: "src/a.ts".into(),
            language: "typescript".into(),
            pre_hash: ContentHash::of(b"before"),
            pre_size: 20,
            pre_errors: 0,
            post_hash: ContentHash::of(b"after"),
            post_size: 31,
            post_errors: 0,
            edits: vec![Edit {
                start: 5,
                end: 12,
                replacement: "logger.debug(a, b)".into(),
            }],
        }],
    }
}

fn files_in(dir: &Path) -> Vec<String> {
    let mut v: Vec<String> = fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    v.sort();
    v
}

#[test]
fn put_then_get_round_trips_and_lays_out_the_files_privately() {
    let f = Fixture::new();
    let s = f.open(Limits::default());
    let p = plan(1);
    let (id, meta) = s.put(&p).unwrap();
    assert_eq!(id, p.id());
    assert_eq!(meta.created_at, 1_000_000);
    assert_eq!(
        meta.expires_at,
        1_000_000 + 15 * 60,
        "default TTL is 15 minutes"
    );

    let (back, m2) = s.get_for_write(&id).unwrap();
    assert_eq!(back, p);
    assert_eq!(m2, meta);
    let (back, _) = s.get_for_read(&id[..12]).unwrap();
    assert_eq!(back, p);

    let dir = f.plans_dir();
    assert_eq!(
        files_in(&dir),
        vec![format!("{id}.json"), format!("{id}.meta.json")]
    );
    assert_eq!(
        fs::read(dir.join(format!("{id}.json"))).unwrap(),
        p.canonical_bytes()
    );
    for name in files_in(&dir) {
        let mode = fs::metadata(dir.join(&name)).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "{name}");
    }
    for d in [&dir, &dir.parent().unwrap().to_path_buf()] {
        assert_eq!(fs::metadata(d).unwrap().permissions().mode() & 0o777, 0o700);
    }
}

#[test]
fn put_is_idempotent_and_does_not_extend_the_ttl() {
    let f = Fixture::new();
    let s = f.open(Limits::default());
    let (id, meta) = s.put(&plan(1)).unwrap();
    f.advance(300);
    let (id2, meta2) = s.put(&plan(1)).unwrap();
    assert_eq!((id, meta), (id2, meta2), "same plan, same envelope");
    assert_eq!(files_in(&f.plans_dir()).len(), 2);
}

#[test]
fn an_expired_plan_is_refused_listed_nowhere_and_swept() {
    let f = Fixture::new();
    let s = f.open(Limits::default());
    let (id, _) = s.put(&plan(1)).unwrap();
    f.advance(15 * 60 - 1);
    s.get_for_write(&id).unwrap();
    assert_eq!(s.list().unwrap().0.len(), 1);
    f.advance(1); // now == expires_at: expired
    assert_eq!(
        s.get_for_write(&id).unwrap_err().code,
        ErrorCode::PlanExpired
    );
    assert_eq!(
        s.get_for_read(&id).unwrap_err().code,
        ErrorCode::PlanExpired
    );
    assert_eq!(s.begin_use(&id).unwrap_err().code, ErrorCode::PlanExpired);
    assert!(s.list().unwrap().0.is_empty());
    assert_eq!(s.sweep().unwrap(), 1);
    assert!(files_in(&f.plans_dir()).is_empty());
    assert_eq!(
        s.get_for_write(&id).unwrap_err().code,
        ErrorCode::PlanNotFound
    );
}

#[test]
fn re_putting_an_expired_plan_gives_it_a_fresh_envelope() {
    let f = Fixture::new();
    let s = f.open(Limits::default());
    let (id, old) = s.put(&plan(1)).unwrap();
    f.advance(2000);
    let (id2, fresh) = s.put(&plan(1)).unwrap();
    assert_eq!(id, id2);
    assert_eq!(fresh.created_at, old.created_at + 2000);
    s.get_for_write(&id).unwrap();
}

#[test]
fn write_paths_need_the_full_id_read_paths_accept_an_unambiguous_prefix() {
    let f = Fixture::new();
    let s = f.open(Limits::default());
    let (id, _) = s.put(&plan(1)).unwrap();
    for short in [&id[..2], &id[..12], &id[..27], "", "p-"] {
        assert_eq!(
            s.get_for_write(short).unwrap_err().code,
            ErrorCode::InvalidArgs,
            "{short}"
        );
        assert_eq!(
            s.begin_use(short).unwrap_err().code,
            ErrorCode::InvalidArgs,
            "{short}"
        );
    }
    assert!(s.get_for_read(&id[..10]).is_ok());
    assert_eq!(
        s.get_for_read(&id[..9]).unwrap_err().code,
        ErrorCode::PlanNotFound,
        "too short"
    );
    assert_eq!(
        s.get_for_read("p-zzzzzzzzzzzz").unwrap_err().code,
        ErrorCode::PlanNotFound
    );
    // many plans: each full id is found by its own 10-character prefix, uniquely
    let big = Limits {
        plan_max_plans: 1000,
        plan_max_plans_per_process: 200,
        ..Limits::default()
    };
    let f2 = Fixture::new();
    let s2 = f2.open(big);
    let ids: Vec<String> = (0..200)
        .map(|n| s2.put(&plan(1000 + n)).unwrap().0)
        .collect();
    for i in &ids {
        assert_eq!(&s2.get_for_read(&i[..10]).unwrap().0.id(), i);
    }
}

#[test]
fn hostile_ids_never_touch_the_filesystem_outside_the_store() {
    let f = Fixture::new();
    let s = f.open(Limits::default());
    s.put(&plan(1)).unwrap();
    let outside = f.state.join("secret.json");
    fs::write(&outside, b"{}").unwrap();
    let long = "p-a".repeat(5000);
    for bad in [
        "../secret",
        "p-../../secret",
        "/etc/passwd",
        "p-abcdefghijklmnopqrstuvwxyz/../x",
        "a\0b",
        "p-\u{202e}abc",
        long.as_str(),
    ] {
        assert!(s.get_for_write(bad).is_err(), "{bad:?}");
        assert!(s.get_for_read(bad).is_err(), "{bad:?}");
        assert!(s.begin_use(bad).is_err(), "{bad:?}");
    }
    assert_eq!(fs::read(&outside).unwrap(), b"{}");
}

#[test]
fn a_plan_for_another_workspace_is_refused_and_a_foreign_store_is_separate() {
    let f = Fixture::new();
    let s = f.open(Limits::default());
    let mut p = plan(1);
    p.workspace_id = OTHER_WS.into();
    assert_eq!(s.put(&p).unwrap_err().code, ErrorCode::WrongWorkspace);
    assert!(files_in(&f.plans_dir()).is_empty());

    let (id, _) = s.put(&plan(1)).unwrap();
    let other = PlanStore::open(&f.state, OTHER_WS, Limits::default(), f.clock.clone()).unwrap();
    assert_eq!(
        other.get_for_write(&id).unwrap_err().code,
        ErrorCode::PlanNotFound
    );
    // a plan file copied into the other workspace's directory is still refused there
    let other_dir = f.state.join(format!("ws-{OTHER_WS}")).join("plans");
    for ext in ["json", "meta.json"] {
        fs::copy(
            f.plans_dir().join(format!("{id}.{ext}")),
            other_dir.join(format!("{id}.{ext}")),
        )
        .unwrap();
    }
    assert_eq!(
        other.get_for_write(&id).unwrap_err().code,
        ErrorCode::WrongWorkspace
    );
}

#[test]
fn invalid_workspace_ids_and_state_dirs_are_refused() {
    let f = Fixture::new();
    for ws in [
        "",
        "w-1",
        "../x",
        "w-00112233445566778899AABBCCDDEEFF",
        "w-00112233445566778899aabbccddeeff/..",
    ] {
        let e = PlanStore::open(&f.state, ws, Limits::default(), f.clock.clone()).unwrap_err();
        assert_eq!(e.code, ErrorCode::InvalidArgs, "{ws}");
    }
    assert!(!f.state.exists(), "an invalid workspace id creates nothing");
    // a state directory with group/other bits is refused, not repaired
    fs::create_dir_all(&f.state).unwrap();
    fs::set_permissions(&f.state, fs::Permissions::from_mode(0o755)).unwrap();
    let e = PlanStore::open(&f.state, WS, Limits::default(), f.clock.clone()).unwrap_err();
    assert_eq!(e.code, ErrorCode::IoError);
    assert_eq!(
        fs::metadata(&f.state).unwrap().permissions().mode() & 0o777,
        0o755
    );
}

// ---- quotas: a full store refuses, it never evicts (E-15, EDT-16, EDT-27) --------------------

#[test]
fn a_full_store_refuses_new_plans_and_never_evicts_an_unexpired_one() {
    let f = Fixture::new();
    let s = f.open(Limits {
        plan_max_plans: 3,
        ..Limits::default()
    });
    let ids: Vec<String> = (0..3).map(|n| s.put(&plan(n)).unwrap().0).collect();
    for n in 3..20 {
        assert_eq!(s.put(&plan(n)).unwrap_err().code, ErrorCode::LimitExceeded);
    }
    for id in &ids {
        s.get_for_write(id).unwrap();
    }
    assert_eq!(s.list().unwrap().0.len(), 3);
}

#[test]
fn only_expired_plans_make_room_and_never_in_use_ones() {
    let f = Fixture::new();
    let s = f.open(Limits {
        plan_max_plans: 3,
        ..Limits::default()
    });
    let a = s.put(&plan(0)).unwrap().0;
    let b = s.put(&plan(1)).unwrap().0;
    f.advance(600);
    let c = s.put(&plan(2)).unwrap().0;
    f.advance(400); // a and b expired (1000 > 900), c not
    let (d, _) = s.put(&plan(3)).unwrap(); // makes room by deleting expired a and b
    assert_eq!(
        s.get_for_write(&a).unwrap_err().code,
        ErrorCode::PlanNotFound
    );
    assert_eq!(
        s.get_for_write(&b).unwrap_err().code,
        ErrorCode::PlanNotFound
    );
    s.get_for_write(&c).unwrap();
    s.get_for_write(&d).unwrap();
}

#[test]
fn an_in_use_plan_survives_expiry_sweep_and_make_room() {
    let f = Fixture::new();
    let s = f.open(Limits {
        plan_max_plans: 1,
        ..Limits::default()
    });
    let (id, _) = s.put(&plan(0)).unwrap();
    let guard = s.begin_use(&id).unwrap();
    assert_eq!(guard.id(), id);
    assert_eq!(guard.plan(), &plan(0));
    f.advance(10_000); // long expired
    assert_eq!(s.sweep().unwrap(), 0, "in use: not swept");
    assert_eq!(
        s.put(&plan(1)).unwrap_err().code,
        ErrorCode::LimitExceeded,
        "in use: not evicted to make room"
    );
    assert_eq!(files_in(&f.plans_dir()).len(), 2);
    assert_eq!(
        guard.plan(),
        &plan(0),
        "the guard still holds the verified plan"
    );
    drop(guard);
    assert_eq!(s.sweep().unwrap(), 1);
    s.put(&plan(1)).unwrap();
}

#[test]
fn re_putting_an_expired_in_use_plan_is_busy() {
    let f = Fixture::new();
    let s = f.open(Limits::default());
    let (id, _) = s.put(&plan(0)).unwrap();
    let guard = s.begin_use(&id).unwrap();
    f.advance(5000);
    assert_eq!(s.put(&plan(0)).unwrap_err().code, ErrorCode::Busy);
    drop(guard);
    s.put(&plan(0)).unwrap();
}

#[test]
fn the_store_size_cap_refuses_instead_of_evicting() {
    let f = Fixture::new();
    // 1 MiB cap; each plan carries ~300 KiB of replacement text
    let mk = |n: u32| {
        let mut p = plan(n);
        let text = "x".repeat(300 * 1024);
        p.files[0].post_size = 20 - 7 + text.len() as u64;
        p.files[0].edits[0].replacement = text;
        p
    };
    let s = f.open(Limits {
        plan_max_store_mib: 1,
        ..Limits::default()
    });
    let mut kept = vec![];
    let mut refused = 0;
    for n in 0..8 {
        match s.put(&mk(n)) {
            Ok((id, _)) => kept.push(id),
            Err(e) => {
                assert_eq!(e.code, ErrorCode::LimitExceeded);
                refused += 1;
            }
        }
    }
    assert!(
        !kept.is_empty() && refused > 0,
        "kept {} refused {refused}",
        kept.len()
    );
    let total: u64 = files_in(&f.plans_dir())
        .iter()
        .map(|n| fs::metadata(f.plans_dir().join(n)).unwrap().len())
        .sum();
    assert!(total <= 1024 * 1024, "store holds {total} bytes");
    for id in &kept {
        s.get_for_write(id).unwrap();
    }
}

#[test]
fn the_per_process_quota_counts_unexpired_puts_of_this_store_value() {
    let f = Fixture::new();
    let s = f.open(Limits {
        plan_max_plans_per_process: 2,
        plan_max_plans: 100,
        ..Limits::default()
    });
    s.put(&plan(0)).unwrap();
    s.put(&plan(1)).unwrap();
    assert_eq!(s.put(&plan(2)).unwrap_err().code, ErrorCode::LimitExceeded);
    // an idempotent re-put of an existing plan does not use quota
    s.put(&plan(0)).unwrap();
    // a second process (another PlanStore value on the same directory) has its own count
    let other = f.open(Limits {
        plan_max_plans_per_process: 2,
        plan_max_plans: 100,
        ..Limits::default()
    });
    other.put(&plan(2)).unwrap();
    // expiry frees quota
    f.advance(2000);
    s.put(&plan(3)).unwrap();
}

#[test]
fn concurrent_puts_never_exceed_the_cap_and_never_corrupt() {
    let f = Fixture::new();
    let limits = Limits {
        plan_max_plans: 10,
        plan_max_plans_per_process: 200,
        ..Limits::default()
    };
    let s = Arc::new(f.open(limits.clone()));
    let mut handles = vec![];
    for t in 0..8u32 {
        let s = s.clone();
        handles.push(std::thread::spawn(move || {
            let mut ok = vec![];
            for n in 0..20u32 {
                // half the threads fight over the same plans
                let which = if t % 2 == 0 { n } else { 100 + t * 100 + n };
                if let Ok((id, _)) = s.put(&plan(which)) {
                    ok.push(id);
                }
            }
            ok
        }));
    }
    let mut accepted = std::collections::HashSet::new();
    for h in handles {
        accepted.extend(h.join().unwrap());
    }
    let (listed, bad) = s.list().unwrap();
    assert!(bad.is_empty(), "{bad:?}");
    assert!(
        listed.len() <= 10,
        "{} plans stored, cap is 10",
        listed.len()
    );
    for l in &listed {
        s.get_for_write(&l.id).unwrap();
    }
    // every plan a caller was told was stored is still readable (nothing was evicted)
    for id in &accepted {
        s.get_for_write(id).unwrap();
    }
    assert_eq!(accepted.len(), listed.len());
    assert!(
        files_in(&f.plans_dir()).iter().all(|n| !n.contains("tmp")),
        "no temp leftovers"
    );
}

// ---- the store directory is attacker-reachable state ----------------------------------------

#[test]
fn tampering_with_the_stored_bytes_is_detected_on_every_read_path() {
    let f = Fixture::new();
    let s = f.open(Limits::default());
    let (id, _) = s.put(&plan(1)).unwrap();
    let path = f.plans_dir().join(format!("{id}.json"));
    let good = fs::read(&path).unwrap();
    let evil = String::from_utf8(good.clone())
        .unwrap()
        .replace("logger.debug(a, b)", "evil.exec(a, b)");
    fs::write(&path, evil).unwrap();
    for r in [s.get_for_write(&id), s.get_for_read(&id)] {
        assert_eq!(r.unwrap_err().code, ErrorCode::PlanCorrupt);
    }
    assert_eq!(s.begin_use(&id).unwrap_err().code, ErrorCode::PlanCorrupt);
    let (listed, bad) = s.list().unwrap();
    assert!(listed.is_empty());
    assert_eq!(
        bad,
        vec![id.clone()],
        "listing reports the bad plan without failing"
    );
    fs::write(&path, b"not json").unwrap();
    assert_eq!(
        s.get_for_write(&id).unwrap_err().code,
        ErrorCode::PlanCorrupt
    );
    fs::write(&path, &good).unwrap();
    s.get_for_write(&id).unwrap();
}

#[test]
fn permissions_symlinks_and_a_broken_envelope_are_refused() {
    let f = Fixture::new();
    let s = f.open(Limits::default());
    let (id, _) = s.put(&plan(1)).unwrap();
    let path = f.plans_dir().join(format!("{id}.json"));
    let meta = f.plans_dir().join(format!("{id}.meta.json"));

    fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
    assert_eq!(
        s.get_for_write(&id).unwrap_err().code,
        ErrorCode::PlanCorrupt,
        "group/other readable"
    );
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
    s.get_for_write(&id).unwrap();

    // a symlink in place of the plan file, pointing at a valid copy elsewhere
    let elsewhere = f.state.join("copy.json");
    fs::copy(&path, &elsewhere).unwrap();
    fs::remove_file(&path).unwrap();
    symlink(&elsewhere, &path).unwrap();
    assert_eq!(
        s.get_for_write(&id).unwrap_err().code,
        ErrorCode::PlanCorrupt,
        "symlink"
    );
    fs::remove_file(&path).unwrap();
    fs::copy(&elsewhere, &path).unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
    s.get_for_write(&id).unwrap();

    // envelope problems
    let good_meta = fs::read(&meta).unwrap();
    fs::write(&meta, b"garbage").unwrap();
    fs::set_permissions(&meta, fs::Permissions::from_mode(0o600)).unwrap();
    assert_eq!(
        s.get_for_write(&id).unwrap_err().code,
        ErrorCode::PlanCorrupt
    );
    fs::write(
        &meta,
        br#"{"created_at":10,"expires_at":5,"producer_version":"x"}"#,
    )
    .unwrap();
    assert_eq!(
        s.get_for_write(&id).unwrap_err().code,
        ErrorCode::PlanCorrupt,
        "expires before created"
    );
    fs::write(&meta, &good_meta).unwrap();
    s.get_for_write(&id).unwrap();

    // a missing envelope means "no such plan", and sweep cleans the orphan
    fs::remove_file(&meta).unwrap();
    assert_eq!(
        s.get_for_write(&id).unwrap_err().code,
        ErrorCode::PlanNotFound
    );
    assert_eq!(s.list().unwrap().0.len(), 0);
    s.sweep().unwrap();
    assert!(!path.exists(), "orphan plan file removed");
}

#[test]
fn a_different_plan_already_stored_under_the_id_is_corrupt_and_left_alone() {
    let f = Fixture::new();
    let s = f.open(Limits::default());
    let p = plan(1);
    let id = p.id();
    let path = f.plans_dir().join(format!("{id}.json"));
    fs::write(&path, b"squatter").unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
    assert_eq!(s.put(&p).unwrap_err().code, ErrorCode::PlanCorrupt);
    assert_eq!(fs::read(&path).unwrap(), b"squatter");
}
