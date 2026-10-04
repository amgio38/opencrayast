//! Adversarial / property tests for `Boundary::resolve_read` (BND-16, BND-18, T-20).
//!
//! This crate is the **tester only**: it does not change `crates/core/src`. Failures that
//! look like implementation defects are reduced to a minimal `#[ignore]` case and reported
//! on the board for the write-side owner.
#![cfg(unix)]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::field_reassign_with_default
)]

use opencrayast_core::ErrorCode;
use opencrayast_core::boundary::{Boundary, BoundaryConfig, ResolvedPath};
use opencrayast_core::error::ToolError;
use opencrayast_core::limits::Limits;
use std::collections::BTreeSet;
use std::fs;
use std::os::unix::fs::symlink;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

/// Must match `OUTSIDE_MESSAGE` in `boundary.rs` (private); BND-18 requires one constant text.
const OUTSIDE_MESSAGE: &str = "Path is not inside the workspace or a read-only root.";
const OUTSIDE_NEXT: &str =
    "Pass a path relative to the workspace root, or configure a read root for it.";

const PROP_ITERS: u32 = 20_000;
const CALL_TIMEOUT: Duration = Duration::from_secs(2);
/// Fixed seed so a failure is reproducible; printed on every assertion failure.
const SEED_FIXED: u64 = 0x0cc2_a757;

/// Deterministic xorshift64* — no external PRNG crates.
struct XorShift64 {
    state: u64,
}

impl XorShift64 {
    fn new(seed: u64) -> Self {
        // Zero state is a fixed point; mix a non-zero constant.
        Self {
            state: if seed == 0 {
                0x9E37_79B9_7F4A_7C15
            } else {
                seed
            },
        }
    }

    fn next_u64(&mut self) -> u64 {
        let mut x = self.state;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.state = x;
        x
    }

    fn next_usize(&mut self, n: usize) -> usize {
        debug_assert!(n > 0);
        (self.next_u64() as usize) % n
    }
}

/// Every real entry under `root`, canonical and non-following, plus the root itself.
///
/// This is the oracle the outcome checks use. It is *data* about the fixture - what the
/// fixture actually put on disk - and not a re-spelling of the rule under test, so it can
/// disagree with `Boundary` instead of agreeing with it by construction.
fn tree_paths(root: &Path) -> BTreeSet<PathBuf> {
    let mut out = BTreeSet::new();
    let Ok(canon) = root.canonicalize() else {
        return out;
    };
    out.insert(canon.clone());
    let mut stack = vec![canon];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = fs::read_dir(&dir) else {
            continue;
        };
        for e in entries.flatten() {
            let p = e.path();
            // symlink_metadata, not metadata: a symlink is listed but never descended into,
            // so the oracle never inherits the target of a link the boundary may refuse.
            let Ok(md) = fs::symlink_metadata(&p) else {
                continue;
            };
            if md.is_dir() {
                stack.push(p.clone());
            }
            out.insert(p);
        }
    }
    out
}

struct Fixture {
    _ws: tempfile::TempDir,
    _out: tempfile::TempDir,
    _read: tempfile::TempDir,
    /// The single temp dir that holds the workspace, its string-prefix siblings and the
    /// read-only root. Held so the whole colliding family shares one parent directory.
    _family: tempfile::TempDir,
    ws_canon: PathBuf,
    out_canon: PathBuf,
    read_canon: PathBuf,
    /// Names that exist under the workspace (for the alphabet).
    names: Vec<String>,
    /// Basename of the outside directory — must never appear in `rel` or outside messages.
    out_basename: String,
    /// Basename of the workspace temp dir — must never appear in OutsideWorkspace messages.
    ws_basename: String,
    /// Exactly what the fixture put under the workspace, canonical. The containment oracle.
    ws_abs: BTreeSet<PathBuf>,
    /// Exactly what the fixture put under the read-only root, canonical.
    read_abs: BTreeSet<PathBuf>,
    /// Absolute paths that are a strict STRING extension of the workspace root or of a
    /// read root, and that really exist. Every one of them must be refused.
    escape_targets: Vec<EscapeTarget>,
    boundary: Arc<Boundary>,
}

/// One deliberately-colliding outside path.
#[derive(Clone, Debug)]
struct EscapeTarget {
    /// Absolute path as an agent would spell it.
    path: String,
    /// Root whose name it string-extends (0 = workspace, 1.. = read root).
    owner: usize,
    /// Short human label for the failure message.
    label: String,
}

/// Build the sibling family: `ws`, `ws_evil`, `ws2`, ... beside one another, and the same
/// shape around the read-only root.
///
/// `ws` and `ws_evil` share a string prefix. `Path::starts_with` is component-wise and
/// refuses the sibling; a string-prefix `is_under` accepts it. The two are only
/// distinguishable when BOTH exist, so the fixture never lets the workspace be a bare
/// `tempfile::tempdir()` again - that name is random, so no sibling can extend it.
fn build_sibling_family(base: &Path) -> (PathBuf, PathBuf, Vec<EscapeTarget>, Vec<String>) {
    let ws = base.join("ws");
    let ro = base.join("ro");
    fs::create_dir_all(&ws).unwrap();
    fs::create_dir_all(&ro).unwrap();

    let mut escapes = Vec::new();
    let mut names = Vec::new();

    // (owner, suffix sibling name, file inside it)
    let shape: [(usize, &str, &str); 5] = [
        (0, "ws_evil", "sub/stolen.txt"),
        (0, "ws2", "stolen2.txt"),
        (0, "ws_evil/deeper", "deep/stolen3.txt"),
        // A name that is a string extension but not a path extension at all, and a FILE
        // rather than a directory, so the fixture is not one shape repeated.
        (0, "ws ", "spaced.txt"),
        (1, "ro_evil", "sub/borrowed.txt"),
    ];
    for (owner, dir, rel) in shape {
        let dir_path = base.join(dir);
        fs::create_dir_all(dir_path.join(rel).parent().unwrap()).unwrap();
        let file = dir_path.join(rel);
        fs::write(&file, "should never be readable\n").unwrap();
        escapes.push(EscapeTarget {
            path: file.to_string_lossy().into_owned(),
            owner,
            label: format!("{dir}/{rel}"),
        });
        names.push(dir_path.file_name().unwrap().to_string_lossy().into_owned());
        names.push(rel.rsplit('/').next().unwrap().to_string());
    }

    (ws, ro, escapes, names)
}

fn setup_trap_fixture() -> Fixture {
    let ws = tempfile::tempdir().unwrap();
    let out = tempfile::tempdir().unwrap();
    let read = tempfile::tempdir().unwrap();
    // One parent for the workspace, its string-prefix siblings, and the read-only root, so
    // the colliding family really does share a directory. The parent is canonicalised before
    // the paths are built from it: on macOS a temp dir lives under /var, which is itself a
    // symlink to /private/var, so an uncanonicalised base produces escape paths that are
    // string-disjoint from `ws_canon` and the fixture precondition below fails on a tree the
    // fixture is actually correct for.
    let family = tempfile::tempdir().unwrap();
    let family_canon = family.path().canonicalize().unwrap();
    let (ws_root, read_root, escapes, sibling_names) = build_sibling_family(&family_canon);

    fs::create_dir_all(ws_root.join("src/nested")).unwrap();
    fs::write(ws_root.join("src/a.rs"), "fn a() {}\n").unwrap();
    fs::write(ws_root.join("src/nested/b.rs"), "fn b() {}\n").unwrap();
    fs::write(ws_root.join("plain.txt"), "ok\n").unwrap();
    fs::write(ws_root.join("..x"), "dotdotx\n").unwrap();
    fs::write(ws_root.join("a b"), "space\n").unwrap();

    // Unicode: the NFC and NFD spellings of é, each with a DIFFERENT suffix, so the two
    // directories are distinct on every filesystem - including APFS and HFS+, which normalise
    // names to NFD and would otherwise collapse both spellings onto one directory. The point
    // is that a path spelled either way reaches a real directory and cannot be used to spell a
    // traversal; it is not a test that the filesystem preserves normalisation forms.
    let nfc = "é_nfc"; // U+00E9 + _nfc
    let nfd = "e\u{0301}_nfd"; // e + combining acute + _nfd
    fs::create_dir_all(ws_root.join(nfc)).unwrap();
    fs::write(ws_root.join(nfc).join("f.txt"), "nfc\n").unwrap();
    let _ = fs::create_dir_all(ws_root.join(nfd));
    let _ = fs::write(ws_root.join(nfd).join("f.txt"), "nfd\n");

    let long_name = "a".repeat(200);
    fs::write(ws_root.join(&long_name), "long\n").unwrap();

    // Outside world: one real file, used as symlink target / absolute probe.
    fs::write(out.path().join("secret.txt"), "top secret\n").unwrap();
    fs::create_dir_all(out.path().join("outdir")).unwrap();
    fs::write(out.path().join("outdir/nested.txt"), "out nested\n").unwrap();

    // Symlinks: outside exist / missing / dir; dangling; into workspace; cycles.
    symlink(
        out.path().join("secret.txt"),
        ws_root.join("link_ext_exists"),
    )
    .unwrap();
    symlink(
        out.path().join("nope.txt"),
        ws_root.join("link_ext_missing"),
    )
    .unwrap();
    symlink(out.path().join("outdir"), ws_root.join("link_ext_dir")).unwrap();
    symlink(
        ws_root.join("does-not-exist-xyz"),
        ws_root.join("link_dangling"),
    )
    .unwrap();
    symlink(ws_root.join("src/a.rs"), ws_root.join("link_inside")).unwrap();
    symlink(ws_root.join("loop_b"), ws_root.join("loop_a")).unwrap();
    symlink(ws_root.join("loop_a"), ws_root.join("loop_b")).unwrap();
    symlink(ws_root.join("cyc_b"), ws_root.join("cyc_a")).unwrap();
    symlink(ws_root.join("cyc_c"), ws_root.join("cyc_b")).unwrap();
    symlink(ws_root.join("cyc_a"), ws_root.join("cyc_c")).unwrap();

    // Read-only root with a file.
    fs::create_dir_all(read_root.join("lib")).unwrap();
    fs::write(read_root.join("lib/lib.rs"), "pub fn x() {}\n").unwrap();

    let names = vec![
        "src".into(),
        "a.rs".into(),
        "nested".into(),
        "b.rs".into(),
        "plain.txt".into(),
        "..x".into(),
        "a b".into(),
        nfc.into(),
        "f.txt".into(),
        long_name,
        "link_ext_exists".into(),
        "link_ext_missing".into(),
        "link_ext_dir".into(),
        "link_dangling".into(),
        "link_inside".into(),
        "loop_a".into(),
        "loop_b".into(),
        "cyc_a".into(),
        "cyc_b".into(),
        "cyc_c".into(),
    ];
    // The prefix-sibling names belong in the alphabet: the sweep has to be able to SPELL
    // the collision, or a string-prefixed `is_under` would survive a run that never
    // produced the one input that separates them.
    let mut names = names;
    names.extend(sibling_names);

    let boundary = Boundary::new(BoundaryConfig {
        root: ws_root.clone(),
        read_roots: vec![read_root.clone()],
        limits: Limits::default(),
        state_dir: None,
        extra_protected: Vec::new(),
    })
    .unwrap();

    let ws_canon = ws_root.canonicalize().unwrap();
    let out_canon = out.path().canonicalize().unwrap();
    let read_canon = read_root.canonicalize().unwrap();
    let out_basename = out_canon
        .file_name()
        .unwrap()
        .to_string_lossy()
        .into_owned();
    let ws_basename = ws_canon.file_name().unwrap().to_string_lossy().into_owned();

    Fixture {
        _ws: ws,
        _out: out,
        _read: read,
        _family: family,
        ws_abs: tree_paths(&ws_canon),
        read_abs: tree_paths(&read_canon),
        escape_targets: escapes,
        ws_canon,
        out_canon,
        read_canon,
        names,
        out_basename,
        ws_basename,
        boundary: Arc::new(boundary),
    }
}

fn alphabet(fx: &Fixture) -> Vec<String> {
    let mut a = vec![
        "a".into(),
        "b".into(),
        ".".into(),
        "..".into(),
        "/".into(),
        "//".into(),
        "\\".into(),
        "\0".into(),
        "\n".into(),
        "\t".into(),
        "\u{1b}".into(),
        "é".into(),
        "e\u{0301}".into(),
        "\u{202e}".into(), // RLO bidi
        "\u{200f}".into(), // RLM
        "a".repeat(64),
        "a".repeat(300),
    ];
    a.extend(fx.names.iter().cloned());
    a
}

fn generate_path(rng: &mut XorShift64, fx: &Fixture, alpha: &[String]) -> String {
    // Kind 5 exists for one reason: a prefix-sibling escape has to be REACHABLE, not
    // merely possible. Absorbing `ws_evil` into the alphabet is not enough, because an
    // absolute spelling is only ever built as `root.join(suffix)`, and the colliding
    // sibling is a *sibling* - it is never a component of the workspace root. This arm
    // spells the sibling targets directly, and puts a genuine inside file beside them so
    // the sweep sees both outcomes from the same generator.
    let kind = rng.next_usize(6);
    match kind {
        0 => {
            // Relative: join 0..6 alphabet tokens with `/` (sometimes empty join → ".").
            let n = rng.next_usize(7);
            if n == 0 {
                return ".".into();
            }
            let mut parts = Vec::with_capacity(n);
            for _ in 0..n {
                parts.push(alpha[rng.next_usize(alpha.len())].as_str());
            }
            parts.join("/")
        }
        1 => {
            // Absolute under workspace.
            let suffix = generate_path(rng, fx, alpha);
            join_abs(&fx.ws_canon, &suffix)
        }
        2 => {
            // Absolute under outside.
            let suffix = generate_path(rng, fx, alpha);
            join_abs(&fx.out_canon, &suffix)
        }
        3 => {
            // Absolute under read root.
            let suffix = generate_path(rng, fx, alpha);
            join_abs(&fx.read_canon, &suffix)
        }
        5 => {
            // Prefix-sibling probes. Two thirds are the escape targets themselves; the
            // rest are real inside files, so this arm is not purely a refusal generator.
            if rng.next_usize(3) == 0 {
                fx.ws_canon.join("plain.txt").to_string_lossy().into_owned()
            } else {
                fx.escape_targets[rng.next_usize(fx.escape_targets.len())]
                    .path
                    .clone()
            }
        }
        _ => {
            // Raw absolute-ish probes.
            match rng.next_usize(6) {
                0 => fx
                    .out_canon
                    .join("secret.txt")
                    .to_string_lossy()
                    .into_owned(),
                1 => fx.out_canon.join("nope.txt").to_string_lossy().into_owned(),
                2 => "/etc/passwd".into(),
                3 => fx.ws_canon.to_string_lossy().into_owned(),
                // The BND-18 shape at random: a REAL outside file against a non-existent
                // name in the very same directory, so no sweep run can leave the pair
                // uncovered.
                4 => fx
                    .out_canon
                    .join(format!("sibling-{:04}-missing.txt", rng.next_usize(64)))
                    .to_string_lossy()
                    .into_owned(),
                _ => generate_path(rng, fx, alpha),
            }
        }
    }
}

fn join_abs(root: &Path, suffix: &str) -> String {
    // Avoid doubling when suffix is absolute.
    if suffix.starts_with('/') {
        format!("{}{}", root.display(), suffix)
    } else if suffix == "." || suffix.is_empty() {
        root.to_string_lossy().into_owned()
    } else {
        root.join(suffix).to_string_lossy().into_owned()
    }
}

/// Call `resolve_read` with a 2s wall-clock budget and panic catch (BND-16).
fn resolve_timed(b: &Arc<Boundary>, path: &str) -> Result<Result<ResolvedPath, ToolError>, String> {
    let b2 = Arc::clone(b);
    let path_owned = path.to_string();
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let outcome = catch_unwind(AssertUnwindSafe(|| b2.resolve_read(&path_owned)));
        let _ = tx.send(outcome);
    });
    match rx.recv_timeout(CALL_TIMEOUT) {
        Ok(Ok(r)) => Ok(r),
        Ok(Err(_)) => Err("resolve_read panicked".into()),
        Err(_) => Err("resolve_read exceeded 2s timeout".into()),
    }
}

fn err_triple(e: &ToolError) -> (ErrorCode, &str, &str) {
    (e.code, e.message.as_str(), e.next.as_str())
}

/// The containment oracle, derived from what the fixture put on disk.
///
/// Membership in the walk is the claim "this file is inside the workspace"; the check
/// below is the claim "the boundary agrees". Comparing those two is a real assertion,
/// because a wrong `is_under` makes the boundary return `Ok` for a path the walk never
/// listed. The previous version of this file called `Path::starts_with` here, which
/// restated the rule under test and could only ever confirm it.
///
/// Membership is not negated: a path the walk missed is not treated as outside, so the
/// oracle can never wrongly demand a refusal. Only the forward direction is pinned.
fn under_ws(fx: &Fixture, abs: &Path) -> bool {
    fx.ws_abs.contains(abs)
}

fn under_read(fx: &Fixture, abs: &Path) -> bool {
    fx.read_abs.contains(abs)
}

fn check_ok_properties(fx: &Fixture, path: &str, r: &ResolvedPath) -> Result<(), String> {
    let canon = fs::canonicalize(&r.abs).map_err(|e| format!("canonicalize(abs): {e}"))?;
    if canon != r.abs {
        return Err(format!(
            "abs is not canonical: abs={:?} canonicalize={:?}",
            r.abs, canon
        ));
    }

    let under_ws = under_ws(fx, &r.abs);
    let under_read = under_read(fx, &r.abs);
    if !under_ws && !under_read {
        return Err(format!(
            "abs {:?} is under neither workspace nor read_root (it is in neither, or in the \
             string-prefix sibling of, one of them)",
            r.abs
        ));
    }

    // `..` as a path component is forbidden; a file literally named `..x` is fine.
    if r.rel == ".." || r.rel.starts_with("../") || r.rel.contains("/../") || r.rel.ends_with("/..")
    {
        return Err(format!("rel contains '..' segment: {:?}", r.rel));
    }
    if r.rel.starts_with('/') {
        return Err(format!("rel starts with '/': {:?}", r.rel));
    }
    for c in Path::new(&r.rel).components() {
        if matches!(c, Component::ParentDir) {
            return Err(format!("rel has ParentDir component: {:?}", r.rel));
        }
    }

    if under_ws && !under_read && r.rel.starts_with("@root") {
        return Err(format!("workspace hit labelled as read root: {:?}", r.rel));
    }
    if under_read && !under_ws && !r.rel.starts_with("@root1") {
        return Err(format!("read_root hit without @root1 prefix: {:?}", r.rel));
    }

    if r.rel.contains(&fx.out_basename) {
        return Err(format!(
            "rel leaks outside basename {:?}: {:?}",
            fx.out_basename, r.rel
        ));
    }
    let out_str = fx.out_canon.to_string_lossy();
    if r.rel.contains(out_str.as_ref()) {
        return Err(format!("rel leaks outside absolute path: {:?}", r.rel));
    }

    let _ = path;
    Ok(())
}

fn check_outside_err(fx: &Fixture, e: &ToolError) -> Result<(), String> {
    if e.code != ErrorCode::OutsideWorkspace {
        return Ok(()); // other codes are fine for garbage input
    }
    if e.message != OUTSIDE_MESSAGE {
        return Err(format!(
            "OutsideWorkspace message mismatch: {:?}",
            e.message
        ));
    }
    if e.next != OUTSIDE_NEXT {
        return Err(format!("OutsideWorkspace next mismatch: {:?}", e.next));
    }
    let hay = format!("{}{}", e.message, e.next);
    if hay.contains(&fx.out_basename) || hay.contains(&fx.ws_basename) {
        return Err(format!(
            "OutsideWorkspace text contains a temp basename: {hay:?}"
        ));
    }
    let out_str = fx.out_canon.to_string_lossy();
    let ws_str = fx.ws_canon.to_string_lossy();
    if hay.contains(out_str.as_ref()) || hay.contains(ws_str.as_ref()) {
        return Err(format!(
            "OutsideWorkspace text contains an absolute path fragment: {hay:?}"
        ));
    }
    Ok(())
}

fn check_one(fx: &Fixture, path: &str) -> Result<(), String> {
    let r1 = resolve_timed(&fx.boundary, path)?;
    let r2 = resolve_timed(&fx.boundary, path)?;
    // Determinism (property 5).
    match (&r1, &r2) {
        (Ok(a), Ok(b)) if a == b => {}
        (Err(a), Err(b)) if a == b => {}
        _ => {
            return Err(format!(
                "non-deterministic results:\n  first={r1:?}\n  second={r2:?}"
            ));
        }
    }

    match &r1 {
        Ok(r) => check_ok_properties(fx, path, r)?,
        Err(e) => check_outside_err(fx, e)?,
    }
    Ok(())
}

#[test]
fn bnd16_adversarial_random_paths() {
    let seed = SEED_FIXED;
    let fx = setup_trap_fixture();
    let alpha = alphabet(&fx);
    let mut rng = XorShift64::new(seed);

    for i in 0..PROP_ITERS {
        let path = generate_path(&mut rng, &fx, &alpha);
        if let Err(msg) = check_one(&fx, &path) {
            panic!("property failure at i={i} seed={seed:#x} path={path:?}: {msg}");
        }
    }
}

#[test]
fn bnd18_outside_exist_vs_missing_indistinguishable() {
    let fx = setup_trap_fixture();
    let exists = fx.out_canon.join("secret.txt");
    let missing = fx.out_canon.join("nope-sibling-xyz.txt");
    let e1 = resolve_timed(&fx.boundary, exists.to_str().unwrap())
        .unwrap()
        .unwrap_err();
    let e2 = resolve_timed(&fx.boundary, missing.to_str().unwrap())
        .unwrap()
        .unwrap_err();
    assert_eq!(
        err_triple(&e1),
        err_triple(&e2),
        "must not reveal whether an outside path exists"
    );
    assert_eq!(e1.code, ErrorCode::OutsideWorkspace);
    check_outside_err(&fx, &e1).unwrap();
}

#[test]
fn bnd18_symlink_outside_exist_vs_missing_indistinguishable() {
    let fx = setup_trap_fixture();
    let e1 = resolve_timed(&fx.boundary, "link_ext_exists")
        .unwrap()
        .unwrap_err();
    let e2 = resolve_timed(&fx.boundary, "link_ext_missing")
        .unwrap()
        .unwrap_err();
    assert_eq!(
        err_triple(&e1),
        err_triple(&e2),
        "symlink to outside exist/missing must look identical"
    );
    assert_eq!(e1.code, ErrorCode::OutsideWorkspace);
    check_outside_err(&fx, &e1).unwrap();
}

#[test]
fn known_inside_paths_still_ok() {
    let fx = setup_trap_fixture();
    let paths: Vec<String> = vec![
        "src/a.rs".into(),
        "plain.txt".into(),
        "..x".into(),
        "a b".into(),
        "link_inside".into(),
        ".".into(),
        fx.ws_canon.join("src/a.rs").to_string_lossy().into_owned(),
        fx.read_canon
            .join("lib/lib.rs")
            .to_string_lossy()
            .into_owned(),
    ];
    for p in &paths {
        let r = resolve_timed(&fx.boundary, p)
            .unwrap_or_else(|e| panic!("timed/panic on {p:?}: {e}"))
            .unwrap_or_else(|e| panic!("expected Ok for {p:?}, got {e}"));
        check_ok_properties(&fx, p, &r).unwrap_or_else(|e| panic!("{p:?}: {e}"));
    }
}

#[test]
fn symlink_cycles_do_not_hang_or_panic() {
    let fx = setup_trap_fixture();
    for p in ["loop_a", "loop_b", "cyc_a", "cyc_b", "cyc_c", "loop_a/x"] {
        let r = resolve_timed(&fx.boundary, p).unwrap_or_else(|e| panic!("{p}: {e}"));
        assert!(r.is_err(), "{p} should not resolve through a cycle: {r:?}");
    }
}

// ---------------------------------------------------------------------------------------
// Prefix-sibling escapes (RE Y20261002/REQ-CORE-BOUNDARY/ISSUE-CORE-BOUNDARY-PROP-IS-UNDER-
// STARTS-WITH).
//
// The tests above can no longer tell a correct `is_under` from a string-prefixed one, and
// the tests below exist precisely because of that. The mutation
//   `path.to_string_lossy().starts_with(root.to_string_lossy().as_ref())`
// applied to `is_under` is a real boundary escape, and the assertions below are written on
// OUTCOMES - `resolve_read` must REFUSE - so they go red without any helper that restates
// the rule.

/// The dedicated named test. Red on its own under the string-prefix mutation, and red
/// under the `is_under -> true` reverse mutation, which is why both are reported.
///
/// Nothing here computes containment. The fixture is the claim ("these paths are
/// siblings of the workspace root and really exist") and `resolve_read` is the
/// observation; the only assertion is that the two disagree, by way of a refusal.
#[test]
fn is_under_string_prefix_sibling_is_refused() {
    let fx = setup_trap_fixture();
    assert!(
        !fx.escape_targets.is_empty(),
        "fixture must build at least one prefix sibling"
    );

    for t in &fx.escape_targets {
        // Precondition: this really is a strict string extension of the root it targets,
        // and really is on disk. Stated as plain string work - it is a fact about the
        // fixture, not a re-implementation of `is_under`.
        let root: &Path = if t.owner == 0 {
            &fx.ws_canon
        } else {
            &fx.read_canon
        };
        assert!(
            t.path.starts_with(root.to_string_lossy().as_ref())
                && t.path != root.to_string_lossy().into_owned()
                && !Path::new(&t.path).starts_with(root),
            "fixture is wrong: {:?} is not a strict string extension of {:?} that shares no \
             leading component",
            t.path,
            root
        );
        assert!(
            fs::symlink_metadata(&t.path).is_ok(),
            "fixture is wrong: escape target {:?} does not exist",
            t.path
        );

        let r = resolve_timed(&fx.boundary, &t.path)
            .unwrap_or_else(|e| panic!("[{}] {}: {e}", t.label, t.path));
        match r {
            Ok(resolved) => panic!(
                "[{}] BOUNDARY ESCAPE: {t:?} resolved to abs={:?} rel={:?}; it is a sibling \
                 of, not a child of, the root",
                t.label, resolved.abs, resolved.rel
            ),
            Err(e) => {
                assert_eq!(
                    e.code,
                    ErrorCode::OutsideWorkspace,
                    "[{}] {t:?}: refused, but not with outside_error ({})",
                    t.label,
                    e.message
                );
                check_outside_err(&fx, &e).unwrap_or_else(|m| panic!("[{}] {m}", t.label));
            }
        }
    }
}

/// The companion: the refusal has to be a DECISION, not an accident of a missing file.
///
/// For every target the boundary refused, plain `fs::read` must still succeed - the
/// bytes are really there and really reachable to this process. If a target were also
/// unreadable on disk, the refusal would prove nothing, and an `is_under` that refused
/// everything would score just as well as a correct one.
#[test]
fn is_under_string_prefix_sibling_is_not_readable() {
    let fx = setup_trap_fixture();
    for t in &fx.escape_targets {
        // NOTE: `resolve_timed` returns the panic/timeout outcome in the OUTER result, so
        // the inner one has to be unwrapped before it can be matched. Matching the outer
        // `Ok(_)` would swallow the refusal and read as an escape.
        let inner = resolve_timed(&fx.boundary, &t.path)
            .unwrap_or_else(|e| panic!("[{}] {t:?}: {e}", t.label));
        let Err(e) = inner else {
            panic!("[{}] {t:?} resolved; expected a refusal", t.label);
        };
        assert_eq!(
            e.code,
            ErrorCode::OutsideWorkspace,
            "[{}] {t:?}: refused with the wrong code ({})",
            t.label,
            e.message
        );
        // And the file is really there, so the refusal is a decision and not an accident
        // of a missing path.
        assert_eq!(
            fs::read(&t.path).ok().as_deref(),
            Some(&b"should never be readable\n"[..]),
            "fixture is wrong: {t:?} should be a real, readable file"
        );
    }
}

/// The positive half: the workspace itself and a read root whose names are the PREFIXES of
/// the colliding siblings must still resolve. Without this the test above would also pass
/// against a `is_under` that refuses everything, which is the other direction a mutation
/// can go wrong.
#[test]
fn is_under_prefix_siblings_leave_the_real_roots_readable() {
    let fx = setup_trap_fixture();
    for p in [
        fx.ws_canon.join("plain.txt").to_string_lossy().into_owned(),
        fx.read_canon
            .join("lib/lib.rs")
            .to_string_lossy()
            .into_owned(),
    ] {
        let r = resolve_timed(&fx.boundary, &p)
            .unwrap_or_else(|e| panic!("{p:?}: {e}"))
            .unwrap_or_else(|e| panic!("{p:?} should still resolve: {e}"));
        check_ok_properties(&fx, &p, &r).unwrap_or_else(|e| panic!("{p:?}: {e}"));
    }
}

/// BND-18 on the shape the sweep could not reach before: an absolute input that is a real
/// file outside the workspace, paired with a name in that SAME directory that does not
/// exist. Both must come back as the one `outside_error` - not `not_found`, not
/// `io_error`, and not two different wordings. The pair differs only in whether the name
/// is on disk, so any difference between the two answers is an existence oracle.
#[test]
fn bnd18_absolute_outside_sibling_exist_vs_missing_indistinguishable() {
    let fx = setup_trap_fixture();
    let exists = fx.out_canon.join("secret.txt");
    let missing = fx.out_canon.join("secret-zz.txt"); // same directory, same prefix, absent

    assert!(
        fs::symlink_metadata(&exists).is_ok(),
        "fixture: {exists:?} must exist"
    );
    assert!(
        fs::symlink_metadata(&missing).is_err(),
        "fixture: {missing:?} must NOT exist"
    );
    assert_eq!(
        exists.parent().unwrap(),
        missing.parent().unwrap(),
        "the two probes must be siblings, so the only difference is existence"
    );

    let e1 = resolve_timed(&fx.boundary, exists.to_str().unwrap())
        .unwrap()
        .unwrap_err();
    let e2 = resolve_timed(&fx.boundary, missing.to_str().unwrap())
        .unwrap()
        .unwrap_err();

    assert_eq!(
        err_triple(&e1),
        err_triple(&e2),
        "an absolute outside path must not be distinguishable by existence: \
         existing={:?} missing={:?}",
        err_triple(&e1),
        err_triple(&e2)
    );
    assert_eq!(
        e1.code,
        ErrorCode::OutsideWorkspace,
        "an existing outside file must be outside_error, not not_found: {e1:?}"
    );
    assert_eq!(
        e2.code,
        ErrorCode::OutsideWorkspace,
        "a missing outside file must be outside_error, not not_found: {e2:?}"
    );
    check_outside_err(&fx, &e1).unwrap();
    check_outside_err(&fx, &e2).unwrap();
}
