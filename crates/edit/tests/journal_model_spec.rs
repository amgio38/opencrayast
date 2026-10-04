//! Spec for ISSUE-EDIT-4: the journal model and the recovery decision (E-6, E-8, E-9, E-13, E-14;
//! EDT-09, EDT-10, EDT-12, EDT-22 at model level). Never weaken; add cases.
//!
//! The model: a toy "filesystem" of N file contents plus the journal (originals and manifest).
//! Apply, undo and recovery are written here as scripts of atomic durable steps. A crash is
//! "stop before step k". After ANY crash, recovery (itself crash-injected) must leave the files
//! all-original or all-applied, never a mixture.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use opencrayast_core::ErrorCode;
use opencrayast_core::hash::ContentHash;
use opencrayast_edit::{
    FileClass, JournalFile, JournalState, Manifest, Recovery, classify, plan_recovery, plan_undo,
};

const WS: &str = "w-00112233445566778899aabbccddeeff";
const PID: &str = "p-jpqn7mmlutagkvspztoqj7hxra";

fn h(b: &[u8]) -> ContentHash {
    ContentHash::of(b)
}

// ---- the model ---------------------------------------------------------------------------

#[derive(Clone, Debug, PartialEq, Eq)]
struct World {
    files: Vec<Option<Vec<u8>>>,
    origs: Option<Vec<Vec<u8>>>,
    manifest: Option<Manifest>,
}

fn paths(n: usize) -> Vec<String> {
    (0..n).map(|i| format!("f{i:02}.rs")).collect()
}

fn mk_manifest(pre: &[Vec<u8>], post: &[Vec<u8>], state: JournalState) -> Manifest {
    Manifest {
        plan_id: PID.into(),
        plan_digest: h(b"a plan that is not this one"),
        workspace_id: WS.into(),
        state,
        files: paths(pre.len())
            .into_iter()
            .enumerate()
            .map(|(i, path)| JournalFile {
                path,
                pre_hash: h(&pre[i]),
                post_hash: h(&post[i]),
            })
            .collect(),
        progress: 0,
        created_at: 100,
        updated_at: 100,
    }
}

fn hashes(w: &World) -> Vec<Option<ContentHash>> {
    w.files.iter().map(|f| f.as_deref().map(h)).collect()
}

fn set_state(w: &mut World, to: JournalState) {
    let m = w.manifest.as_mut().unwrap();
    assert!(
        m.state.can_become(to),
        "illegal transition {:?} -> {:?}",
        m.state,
        to
    );
    m.state = to;
    m.updated_at += 1;
}

/// The apply script: a list of atomic steps. Step k is applied to the world.
#[derive(Clone, Copy, Debug)]
enum Step {
    SaveOrigs,
    WriteManifestPrepared,
    ToWriting,
    Rename(usize),
    Progress(usize),
    ToApplied,
}

fn apply_script(n: usize) -> Vec<Step> {
    let mut v = vec![
        Step::SaveOrigs,
        Step::WriteManifestPrepared,
        Step::ToWriting,
    ];
    for i in 0..n {
        v.push(Step::Rename(i));
        v.push(Step::Progress(i + 1));
    }
    v.push(Step::ToApplied);
    v
}

fn do_step(w: &mut World, s: Step, pre: &[Vec<u8>], post: &[Vec<u8>]) {
    match s {
        Step::SaveOrigs => w.origs = Some(pre.to_vec()),
        Step::WriteManifestPrepared => {
            w.manifest = Some(mk_manifest(pre, post, JournalState::Prepared))
        }
        Step::ToWriting => set_state(w, JournalState::Writing),
        Step::Rename(i) => w.files[i] = Some(post[i].clone()),
        Step::Progress(p) => w.manifest.as_mut().unwrap().progress = p as u64,
        Step::ToApplied => set_state(w, JournalState::Applied),
    }
}

/// Run recovery as the shell would: ask `plan_recovery`, perform exactly that. `crash_after`
/// stops after that many file restores (before the final manifest update).
fn recover(
    w: &mut World,
    crash_after: Option<usize>,
) -> Result<bool, opencrayast_core::error::ToolError> {
    let Some(m) = w.manifest.clone() else {
        return Ok(false);
    };
    let cur = hashes(w);
    let (idx, end) = match plan_recovery(&m, &cur)? {
        Recovery::Nothing => return Ok(false),
        Recovery::MarkRolledBack => (vec![], JournalState::RolledBack),
        Recovery::RestoreThenRolledBack(i) => (i, JournalState::RolledBack),
        Recovery::RestoreThenUndone(i) => (i, JournalState::Undone),
    };
    for (done, i) in idx.iter().enumerate() {
        if crash_after == Some(done) {
            return Ok(true);
        }
        let orig = &w
            .origs
            .as_ref()
            .expect("originals exist once a manifest exists")[*i];
        assert_eq!(
            h(orig),
            m.files[*i].pre_hash,
            "an original is verified before it is written back"
        );
        w.files[*i] = Some(orig.clone());
    }
    if crash_after == Some(idx.len()) {
        return Ok(true);
    }
    set_state(w, end);
    Ok(false)
}

fn recover_fully(w: &mut World) {
    // a crashed recovery is recovered again, possibly crashing again, until it completes
    for _ in 0..4 {
        if !recover(w, None).unwrap_or(false) {
            return;
        }
    }
}

fn assert_all(w: &World, want: &[Vec<u8>], why: &str) {
    let got: Vec<Option<Vec<u8>>> = w.files.clone();
    let want: Vec<Option<Vec<u8>>> = want.iter().cloned().map(Some).collect();
    assert_eq!(got, want, "{why}");
}

struct Lcg(u64);
impl Lcg {
    fn below(&mut self, n: usize) -> usize {
        self.0 = self
            .0
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        ((self.0 >> 33) as usize) % n.max(1)
    }
}

fn random_case(r: &mut Lcg) -> (Vec<Vec<u8>>, Vec<Vec<u8>>) {
    let n = 1 + r.below(5);
    let pool: [&[u8]; 5] = [b"a", b"bb", b"", b"same", b"zzz"];
    let pre: Vec<Vec<u8>> = (0..n).map(|_| pool[r.below(5)].to_vec()).collect();
    let post: Vec<Vec<u8>> = (0..n)
        .map(|i| {
            if r.below(5) == 0 {
                pre[i].clone()
            } else {
                pool[r.below(5)].to_vec()
            }
        })
        .collect();
    (pre, post)
}

fn fresh(pre: &[Vec<u8>]) -> World {
    World {
        files: pre.iter().cloned().map(Some).collect(),
        origs: None,
        manifest: None,
    }
}

// ---- E-8: a crash anywhere in apply, and anywhere in recovery, never leaves a mixture ---------

#[test]
fn a_crash_between_any_two_apply_steps_is_recoverable_to_all_original_or_all_applied() {
    let mut r = Lcg(20261002);
    let mut exercised = 0;
    for _ in 0..300 {
        let (pre, post) = random_case(&mut r);
        let script = apply_script(pre.len());
        for crash_before in 0..=script.len() {
            let mut w = fresh(&pre);
            for s in &script[..crash_before] {
                do_step(&mut w, *s, &pre, &post);
            }
            // also crash inside recovery at every possible point
            let restores = pre.len() + 1;
            for rec_crash in std::iter::once(None).chain((0..=restores).map(Some)) {
                let mut v = w.clone();
                if recover(&mut v, rec_crash).unwrap() {
                    recover_fully(&mut v);
                }
                let completed = crash_before == script.len();
                if completed {
                    assert_all(&v, &post, "an apply that reached `applied` stays applied");
                    assert_eq!(v.manifest.as_ref().unwrap().state, JournalState::Applied);
                } else {
                    assert_all(
                        &v,
                        &pre,
                        &format!("crash before step {crash_before}: original restored"),
                    );
                    if let Some(m) = &v.manifest {
                        assert!(
                            m.state.is_terminal(),
                            "after recovery the journal is terminal, got {:?}",
                            m.state
                        );
                    }
                }
                // idempotence: recovering again changes nothing at all
                let snapshot = v.clone();
                recover(&mut v, None).unwrap();
                assert_eq!(v, snapshot, "recovery is idempotent");
                exercised += 1;
            }
        }
    }
    assert!(exercised > 10_000, "{exercised}");
}

// ---- E-13: undo is journaled; a crash anywhere in undo (and in its recovery) is repairable -----

#[test]
fn a_crash_between_any_two_undo_steps_is_recoverable_to_all_applied_or_all_original() {
    let mut r = Lcg(77);
    for _ in 0..300 {
        let (pre, post) = random_case(&mut r);
        let mut base = fresh(&pre);
        for s in apply_script(pre.len()) {
            do_step(&mut base, s, &pre, &post);
        }
        assert_eq!(base.manifest.as_ref().unwrap().state, JournalState::Applied);
        // the undo script: Applied -> Undoing, restore each file (with progress), -> Undone
        let n = pre.len();
        let steps = 1 + 2 * n + 1;
        for crash_before in 0..=steps {
            let mut w = base.clone();
            let cur = hashes(&w);
            assert_eq!(
                plan_undo(w.manifest.as_ref().unwrap(), &cur).unwrap(),
                (0..n).collect::<Vec<_>>()
            );
            let mut k = 0;
            let mut run = |w: &mut World, f: &mut dyn FnMut(&mut World)| {
                if k < crash_before {
                    f(w);
                }
                k += 1;
            };
            run(&mut w, &mut |w| set_state(w, JournalState::Undoing));
            for i in 0..n {
                run(&mut w, &mut |w| {
                    w.files[i] = Some(w.origs.as_ref().unwrap()[i].clone())
                });
                run(&mut w, &mut |w| {
                    w.manifest.as_mut().unwrap().progress = (i + 1) as u64
                });
            }
            run(&mut w, &mut |w| set_state(w, JournalState::Undone));
            for rec_crash in std::iter::once(None).chain((0..=n + 1).map(Some)) {
                let mut v = w.clone();
                if recover(&mut v, rec_crash).unwrap() {
                    recover_fully(&mut v);
                }
                match v.manifest.as_ref().unwrap().state {
                    JournalState::Applied => {
                        assert_all(&v, &post, "undo never started: still applied")
                    }
                    JournalState::Undone => assert_all(&v, &pre, "undo completed: originals"),
                    other => panic!("not terminal after recovery: {other:?}"),
                }
                let snapshot = v.clone();
                recover(&mut v, None).unwrap();
                assert_eq!(v, snapshot);
            }
        }
    }
}

// ---- E-14: classify first; a foreign edit stops everything and changes nothing ---------------

#[test]
fn a_foreign_edit_during_writing_or_undoing_is_reported_and_nothing_is_rewritten() {
    let pre = vec![b"one".to_vec(), b"two".to_vec(), b"three".to_vec()];
    let post = vec![b"ONE".to_vec(), b"TWO".to_vec(), b"THREE".to_vec()];
    for state in [JournalState::Writing, JournalState::Undoing] {
        for victim in 0..3 {
            for garbage in [Some(b"someone else wrote this".to_vec()), None] {
                let mut w = fresh(&pre);
                w.origs = Some(pre.clone());
                w.manifest = Some(mk_manifest(&pre, &post, state));
                w.files[0] = Some(post[0].clone()); // a mixed but consistent state
                w.files[victim] = garbage.clone();
                let before = w.clone();
                let e = recover(&mut w, None).unwrap_err();
                assert_eq!(e.code, ErrorCode::Diverged, "{state:?} victim {victim}");
                assert_eq!(w, before, "nothing was changed");
                for p in paths(3) {
                    assert!(
                        e.message.contains(&p),
                        "every file is classified in the message: {}",
                        e.message
                    );
                }
                assert!(e.message.contains("other"), "{}", e.message);
                assert!(!e.message.contains("someone else"), "never quotes content");
            }
        }
    }
}

#[test]
fn plan_recovery_follows_its_decision_table() {
    let pre = vec![b"a".to_vec(), b"b".to_vec()];
    let post = vec![b"A".to_vec(), b"B".to_vec()];
    let cur = |a: &[u8], b: &[u8]| vec![Some(h(a)), Some(h(b))];
    let m = |s| mk_manifest(&pre, &post, s);
    // terminal states: nothing, and the hashes are not even consulted
    for s in [
        JournalState::Applied,
        JournalState::RolledBack,
        JournalState::Undone,
    ] {
        assert_eq!(
            plan_recovery(&m(s), &cur(b"x", b"y")).unwrap(),
            Recovery::Nothing,
            "{s:?}"
        );
    }
    // prepared: VERIFIED, not believed (E-16). Every target still at pre_hash -> the claim holds.
    assert_eq!(
        plan_recovery(&m(JournalState::Prepared), &cur(b"a", b"b")).unwrap(),
        Recovery::MarkRolledBack
    );
    // ...and a target at post_hash means the claim was false: restore it rather than throw the
    // original away. Same direction, honest work.
    assert_eq!(
        plan_recovery(&m(JournalState::Prepared), &cur(b"A", b"b")).unwrap(),
        Recovery::RestoreThenRolledBack(vec![0])
    );
    // ...and a target that matches neither is refused outright.
    assert_eq!(
        plan_recovery(&m(JournalState::Prepared), &cur(b"x", b"y"))
            .unwrap_err()
            .code,
        ErrorCode::Diverged
    );
    // writing
    let w = m(JournalState::Writing);
    assert_eq!(
        plan_recovery(&w, &cur(b"a", b"b")).unwrap(),
        Recovery::RestoreThenRolledBack(vec![])
    );
    assert_eq!(
        plan_recovery(&w, &cur(b"A", b"b")).unwrap(),
        Recovery::RestoreThenRolledBack(vec![0])
    );
    assert_eq!(
        plan_recovery(&w, &cur(b"a", b"B")).unwrap(),
        Recovery::RestoreThenRolledBack(vec![1])
    );
    assert_eq!(
        plan_recovery(&w, &cur(b"A", b"B")).unwrap(),
        Recovery::RestoreThenRolledBack(vec![0, 1])
    );
    assert_eq!(
        plan_recovery(&w, &cur(b"A", b"zz")).unwrap_err().code,
        ErrorCode::Diverged
    );
    assert_eq!(
        plan_recovery(&w, &[Some(h(b"A")), None]).unwrap_err().code,
        ErrorCode::Diverged
    );
    // undoing
    let u = m(JournalState::Undoing);
    assert_eq!(
        plan_recovery(&u, &cur(b"A", b"b")).unwrap(),
        Recovery::RestoreThenUndone(vec![0])
    );
    assert_eq!(
        plan_recovery(&u, &cur(b"a", b"b")).unwrap(),
        Recovery::RestoreThenUndone(vec![])
    );
    assert_eq!(
        plan_recovery(&u, &cur(b"q", b"B")).unwrap_err().code,
        ErrorCode::Diverged
    );
    // a caller bug is internal, not a panic
    assert_eq!(
        plan_recovery(&w, &[Some(h(b"a"))]).unwrap_err().code,
        ErrorCode::Internal
    );
}

#[test]
fn plan_undo_requires_applied_and_every_file_post() {
    let pre = vec![b"a".to_vec(), b"b".to_vec()];
    let post = vec![b"A".to_vec(), b"B".to_vec()];
    let m = mk_manifest(&pre, &post, JournalState::Applied);
    let cur = |a: &[u8], b: &[u8]| vec![Some(h(a)), Some(h(b))];
    assert_eq!(plan_undo(&m, &cur(b"A", b"B")).unwrap(), vec![0, 1]);
    for bad in [
        cur(b"a", b"B"),
        cur(b"A", b"b"),
        cur(b"A", b"edited"),
        vec![Some(h(b"A")), None],
    ] {
        let e = plan_undo(&m, &bad).unwrap_err();
        assert_eq!(e.code, ErrorCode::Diverged);
        assert!(
            e.message.contains("f00.rs") && e.message.contains("f01.rs"),
            "{}",
            e.message
        );
    }
    for s in [
        JournalState::Prepared,
        JournalState::Writing,
        JournalState::Undoing,
        JournalState::RolledBack,
        JournalState::Undone,
    ] {
        let m = mk_manifest(&pre, &post, s);
        assert_eq!(
            plan_undo(&m, &cur(b"A", b"B")).unwrap_err().code,
            ErrorCode::InvalidArgs,
            "{s:?}"
        );
    }
}

#[test]
fn classification_is_by_hash_and_post_wins_when_pre_equals_post() {
    let f = JournalFile {
        path: "a".into(),
        pre_hash: h(b"a"),
        post_hash: h(b"b"),
    };
    assert_eq!(classify(Some(&h(b"b")), &f), FileClass::Post);
    assert_eq!(classify(Some(&h(b"a")), &f), FileClass::Pre);
    assert_eq!(classify(Some(&h(b"c")), &f), FileClass::Other);
    assert_eq!(classify(None, &f), FileClass::Other);
    let same = JournalFile {
        path: "a".into(),
        pre_hash: h(b"a"),
        post_hash: h(b"a"),
    };
    assert_eq!(classify(Some(&h(b"a")), &same), FileClass::Post);
}

// ---- the state machine ------------------------------------------------------------------

#[test]
fn the_transition_table_is_exactly_the_documented_one() {
    use JournalState::*;
    let all = [Prepared, Writing, Applied, Undoing, RolledBack, Undone];
    // rows = from, columns = to, in `all` order
    let table: [[bool; 6]; 6] = [
        [false, true, false, false, true, false],
        [false, false, true, false, true, false],
        [false, false, false, true, false, false],
        [false, false, true, false, false, true],
        [false; 6],
        [false; 6],
    ];
    for (i, from) in all.iter().enumerate() {
        for (j, to) in all.iter().enumerate() {
            assert_eq!(from.can_become(*to), table[i][j], "{from:?} -> {to:?}");
        }
    }
    for s in all {
        assert_eq!(JournalState::parse(s.as_str()), Some(s));
    }
    assert_eq!(RolledBack.as_str(), "rolled_back");
    assert!(RolledBack.is_terminal() && Undone.is_terminal());
    assert!(
        !Applied.is_terminal()
            && !Prepared.is_terminal()
            && !Writing.is_terminal()
            && !Undoing.is_terminal()
    );
    for bad in ["", "Applied", "APPLIED", "applied ", "rolled-back", "done"] {
        assert_eq!(JournalState::parse(bad), None, "{bad:?}");
    }
}

// ---- the manifest ------------------------------------------------------------------------

fn sample_manifest() -> Manifest {
    Manifest {
        plan_id: PID.into(),
        plan_digest: h(b"a plan that is not this one"),
        workspace_id: WS.into(),
        state: JournalState::Writing,
        files: vec![
            JournalFile {
                path: "a.rs".into(),
                pre_hash: h(b"before"),
                post_hash: h(b"after"),
            },
            JournalFile {
                path: "b/c.rs".into(),
                pre_hash: h(b"x"),
                post_hash: h(b"y"),
            },
        ],
        progress: 1,
        created_at: 100,
        updated_at: 105,
    }
}

const GOLDEN_MANIFEST: &str = "{\"created_at\":100,\"files\":[{\"path\":\"a.rs\",\"post_hash\":\"sha256:f39592393ef0859cb196a52693d2cea00fb2df784b3c04ae54aa7cadb8e562f8\",\"pre_hash\":\"sha256:6db7d803e74f1ffa7d8f5adc0bf95b3e15bf4c8373fffadf546227cc6c6742cb\"},{\"path\":\"b/c.rs\",\"post_hash\":\"sha256:a1fce4363854ff888cff4b8e7875d600c2682390412a8cf79b37d0b11148b0fa\",\"pre_hash\":\"sha256:2d711642b726b04401627ca9fbac32f5c8530fb1903cc4db02258717921a4881\"}],\"plan_digest\":\"sha256:0fdc50a33e16c9a9938849ef90bef2861013d9564d0a503689fb4d7799b083eb\",\"plan_id\":\"p-jpqn7mmlutagkvspztoqj7hxra\",\"progress\":1,\"state\":\"writing\",\"updated_at\":105,\"workspace_id\":\"w-00112233445566778899aabbccddeeff\"}";

#[test]
fn the_manifest_canonical_bytes_match_the_independent_reference_and_round_trip() {
    let m = sample_manifest();
    assert_eq!(
        String::from_utf8(m.canonical_bytes()).unwrap(),
        GOLDEN_MANIFEST
    );
    assert_eq!(Manifest::parse(GOLDEN_MANIFEST.as_bytes()).unwrap(), m);
    for s in [
        JournalState::Prepared,
        JournalState::Writing,
        JournalState::Applied,
        JournalState::Undoing,
        JournalState::RolledBack,
        JournalState::Undone,
    ] {
        let mut m = sample_manifest();
        m.state = s;
        assert_eq!(Manifest::parse(&m.canonical_bytes()).unwrap(), m);
    }
}

fn corrupt(s: &str) -> ErrorCode {
    Manifest::parse(s.as_bytes()).unwrap_err().code
}

#[test]
fn only_exactly_canonical_manifests_are_accepted() {
    let g = GOLDEN_MANIFEST;
    for bad in [
        format!(" {g}"),
        format!("{g}\n"),
        format!("{g}{g}"),
        g.replace("\"progress\":1", "\"progress\":1.0"),
        g.replace("\"progress\":1", "\"progress\":-1"),
        g.replace("\"progress\":1", "\"progress\":\"1\""),
        g.replace("\"state\":\"writing\"", "\"state\":\"Writing\""),
        g.replace("\"state\":\"writing\"", "\"state\":\"finished\""),
        g.replace("\"created_at\":100,", ""),
        g.replace("\"created_at\":100,", "\"created_at\":100,\"extra\":1,"),
        g.replace(
            "\"created_at\":100,",
            "\"created_at\":100,\"created_at\":100,",
        ),
        g.replace("sha256:", "sha512:"),
        g.replace(",\"files\"", ", \"files\""),
        String::new(),
        "[]".into(),
        "[".repeat(100_000),
    ] {
        assert_eq!(
            corrupt(&bad),
            ErrorCode::PlanCorrupt,
            "{}",
            &bad[..bad.len().min(60)]
        );
    }
    assert_eq!(
        Manifest::parse(&[0xff, 0xfe]).unwrap_err().code,
        ErrorCode::PlanCorrupt
    );
    assert_eq!(
        Manifest::parse(&vec![b' '; 5 * 1024 * 1024])
            .unwrap_err()
            .code,
        ErrorCode::PlanCorrupt
    );
}

#[test]
fn manifest_check_rejects_structural_nonsense() {
    sample_manifest().check().unwrap();
    let mut cases: Vec<(&str, Manifest)> = vec![];
    let mut m = sample_manifest();
    m.plan_id = "p-short".into();
    cases.push(("short plan id", m));
    let mut m = sample_manifest();
    m.workspace_id = "w-1234".into();
    cases.push(("bad workspace", m));
    let mut m = sample_manifest();
    m.files.clear();
    cases.push(("no files", m));
    let mut m = sample_manifest();
    m.files.reverse();
    cases.push(("unsorted", m));
    let mut m = sample_manifest();
    m.files[1].path = "a.rs".into();
    cases.push(("duplicate", m));
    for bad in ["", "/abs", "../x", "a/../b", "a//b", "a\\b", "a\u{0}b"] {
        let mut m = sample_manifest();
        m.files[0].path = bad.into();
        cases.push(("bad path", m));
    }
    let mut m = sample_manifest();
    m.progress = 3;
    cases.push(("progress beyond files", m));
    let mut m = sample_manifest();
    m.updated_at = 99;
    cases.push(("updated before created", m));
    let mut m = sample_manifest();
    m.files = (0..501)
        .map(|i| JournalFile {
            path: format!("f{i:04}.rs"),
            pre_hash: h(b"a"),
            post_hash: h(b"b"),
        })
        .collect();
    cases.push(("too many files", m));
    for (name, m) in cases {
        assert_eq!(
            m.check().unwrap_err().code,
            ErrorCode::PlanCorrupt,
            "{name}"
        );
    }
}
