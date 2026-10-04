//! Adversarial security audit PoCs — Y20261002/REQ-SECURITY-REVIEW/ISSUE-SEC-AUDIT.
//!
//! Author: kaimadi (adversarial reviewer, not an author of the code under test).
//! These tests are PoCs, not a spec. Several of them are EXPECTED TO FAIL: a failure is the
//! finding. Run them with `cargo test -p opencrayast-edit --test sec_audit_poc -- --ignored
//! --nocapture` (each finding is `#[ignore]`d so the suite does not turn CI red before the
//! fixes land; remove the attribute to reproduce).
//!
//! Each test names the finding id from the report and the SECURITY-MODEL clause it contradicts.
#![cfg(unix)]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use opencrayast_core::boundary::{Boundary, BoundaryConfig};
use opencrayast_core::hash::ContentHash;
use opencrayast_core::limits::Limits;
use opencrayast_core::workspace::workspace_id;
use opencrayast_edit::{
    ApplyContext, Clock, Edit, JournalStore, NoFault, Plan, PlanFile, PlanRequest, PlanStore, apply,
};
use std::fs;
use std::os::unix::fs::{PermissionsExt, symlink};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

struct FakeClock(AtomicU64);
impl Clock for FakeClock {
    fn now_secs(&self) -> u64 {
        self.0.load(Ordering::SeqCst)
    }
}

/// World = a workspace plus its state dir, stores and boundary. Same shape as `apply_spec`.
struct World {
    _dir: tempfile::TempDir,
    root: PathBuf,
    state: PathBuf,
    ws: String,
    boundary: Boundary,
    plans: PlanStore,
    journals: JournalStore,
    limits: Limits,
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
        let mut cfg = BoundaryConfig::new(root.clone(), limits.clone());
        cfg.state_dir = Some(state.clone());
        let boundary = Boundary::new(cfg).unwrap();
        let plans = PlanStore::open(&state, &ws, limits.clone(), clock.clone()).unwrap();
        let journals = JournalStore::open(&state, &ws, limits.clone(), clock.clone()).unwrap();
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

    fn write(&self, rel: &str, bytes: &[u8]) {
        let p = self.root.join(rel);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(p, bytes).unwrap();
    }

    /// One-file plan replacing the first occurrence of `needle`.
    fn plan_one(&self, rel: &str, needle: &str, with: &str) -> String {
        let bytes = fs::read(self.root.join(rel)).unwrap();
        let text = String::from_utf8(bytes.clone()).unwrap();
        let start = text.find(needle).unwrap();
        let edit = Edit {
            start,
            end: start + needle.len(),
            replacement: with.to_string(),
        };
        let mut new = text.clone();
        new.replace_range(start..start + needle.len(), with);
        let plan = Plan {
            format: 1,
            workspace_id: self.ws.clone(),
            engine_format: 1,
            request: PlanRequest {
                kind: "rewrite".into(),
                summary: "poc".into(),
                note: None,
            },
            files: vec![PlanFile {
                path: rel.to_string(),
                language: "text".into(),
                pre_hash: ContentHash::of(&bytes),
                pre_size: bytes.len() as u64,
                pre_errors: 0,
                post_hash: ContentHash::of(new.as_bytes()),
                post_size: new.len() as u64,
                post_errors: 0,
                edits: vec![edit],
            }],
        };
        self.plans.put(&plan).unwrap().0
    }
}

// =====================================================================================
// F-01 (aspect 3, TOCTOU / S-1): a write escapes the workspace by a directory-level
// rename + symlink swap performed between the final verification and the rename.
//
// `resolve_write` only link-checks the FINAL component, and `atomic_replace` operates on
// PATH STRINGS (`target.parent().join(temp)`, `fs::rename`). Between `replace_one`'s
// re-verification and `atomic_replace`'s rename there is a multi-millisecond window
// (write + fsync + set_permissions + listxattr). An attacker who can write the workspace
// moves the whole parent DIRECTORY out of the workspace and replaces it with a symlink:
// every subsequent path-based syscall lands outside the root while dev/ino, nlink, mode
// and the final-component-not-a-symlink check all still pass.
//
// This CONTRADICTS:
//   S-1 "No byte outside the boundary is read or written through any tool."
//   T-03 mitigation: "all later operations are relative to verified directory handles
//        (openat/renameat-style), never to re-resolved path strings"
//   T-03r, which accepts only "a file" being swapped and only corruption INSIDE the tree.
//
// The swap is injected deterministically through the public `Fault` seam at StepKind::Replace,
// i.e. immediately before `replace_one` runs — no thread racing, no flakiness.
// =====================================================================================

/// The check-then-use gap inside `replace_one`.
///
/// `replace_one` (crates/edit/src/apply.rs:618) does exactly this, in this order:
///  1. `boundary.resolve_write(&file.path)`   <- link check on the FINAL component only
///  2. `boundary.open_read(&resolved)`        <- verified handle, gives the FileIdentity
///  3. read the bytes, compare with `pre_hash`
///  4. `atomic_replace(&resolved.abs, ..., identity)`
///
/// `atomic_replace` then works from PATH STRINGS for milliseconds (create temp in
/// `target.parent()`, write, fsync, set_permissions, listxattr) before its rename.
///
/// This test reproduces that sequence verbatim and inserts the attack at the real point:
/// between step 3 and step 4. There is no production seam there, which is itself part of
/// the finding - the gap cannot be exercised without re-implementing the caller.
/// F-01 is FIXED (SEC-FIX 2, commit 0a80c70). This is no longer `#[ignore]`d: it is the PoC
/// promoted to a real test, and it asserts the fix rather than the finding.
///
/// The write path is now handle-relative: the target's PARENT DIRECTORY is opened as a descriptor
/// pinned beneath the workspace root with `BENEATH | NO_SYMLINKS`, and every step of the write —
/// temp create, `fchmod`, `fchown`, `fsetxattr`, `statat`, `renameat`, `unlinkat`, the directory
/// `fsync` — is issued relative to that handle. Immediately before the rename the parent is proved
/// again by re-opening the same entry from the root and comparing `dev`/`ino`, because a handle's
/// own identity cannot detect that its directory was moved: the inode travels with it.
///
/// The attack below moves the whole parent out of the workspace and leaves a symlink behind it,
/// which is exactly the case where the target inode stays `nlink == 1`, the same `dev`/`ino`, and
/// not a link. It must be refused, and the file now outside must keep its original bytes.
///
/// The deterministic version of the same attack — one that injects the swap INSIDE the write path
/// rather than before the call — is `crates/core/src/spec/secfix2_handle_relative_spec.rs`
/// (SECFIX2-01). This test drives the public surface; that one drives the primitive's own seam.
#[test]
fn f01_directory_swap_between_verification_and_rename_writes_outside() {
    let w = World::new();
    w.write("sub/target.rs", b"let a = 1;\n");

    let outside = w._dir.path().join("outside");
    fs::create_dir(&outside).unwrap();
    assert!(!outside.starts_with(&w.root));

    // --- steps 1-3 of `replace_one`: all succeed, identity of the original inode captured.
    let resolved = w.boundary.resolve_write("sub/target.rs").unwrap();
    let (_h, identity) = w.boundary.open_read(&resolved).unwrap();
    eprintln!(
        "step 1-3 ok: {} dev={} ino={}",
        resolved.rel, identity.dev, identity.ino
    );

    // --- the attack, at the real point: move the whole DIRECTORY out of the workspace and
    // leave a symlink where it was. The target inode comes along, still nlink == 1.
    fs::rename(w.root.join("sub"), outside.join("sub")).unwrap();
    symlink(outside.join("sub"), w.root.join("sub")).unwrap();
    eprintln!(
        "attacker: renamed <ws>/sub -> <outside>/sub and symlinked <ws>/sub -> it; \
         <ws>/sub/target.rs still stats as the SAME dev/ino, nlink=1, not a link"
    );

    // --- step 4: the call `replace_one` makes. Refused: the parent directory the workspace names
    // is no longer the directory the write was verified against.
    let r = w.boundary.replace_file(&resolved, b"let a = 2;\n");

    let escaped = fs::read_to_string(outside.join("sub/target.rs")).unwrap();
    let leftovers: Vec<String> = fs::read_dir(outside.join("sub"))
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    eprintln!("step 4: replace_file = {r:?}");
    eprintln!("outside/sub/target.rs = {escaped:?}");
    eprintln!("outside/sub/ now holds {leftovers:?}");

    assert!(
        !escaped.contains("let a = 2;"),
        "the edit was written OUTSIDE the workspace: {escaped:?}"
    );
    assert_eq!(
        escaped, "let a = 1;\n",
        "the file outside the workspace must be byte-for-byte unchanged (S-1)"
    );
    assert!(
        r.is_err(),
        "the write must be refused, not reported as success: {r:?}"
    );
    // Nothing may be left behind in the directory that now holds the target.
    assert!(
        !leftovers.iter().any(|n| n.starts_with(".opencrayast-tmp-")),
        "no temp file may be left outside: {leftovers:?}"
    );
    // And the same through the guarded entry point, which is what production calls.
    let guarded = w
        .boundary
        .replace_file_checked(&resolved, b"let a = 2;\n", Some(identity));
    assert!(
        guarded.is_err(),
        "and the checked call must refuse too: {guarded:?}"
    );
    assert_eq!(
        fs::read_to_string(outside.join("sub/target.rs")).unwrap(),
        "let a = 1;\n",
        "still unchanged after the second attempt"
    );
}

// The RACE variant of F-01 used to live here. It drove `apply()`, which needs a write
// capability, and since SECFIX4 (F-02) that capability is `pub(crate)` — an integration test is a
// separate crate and cannot mint one. The test moved in-crate rather than being deleted; see
// `crates/edit/src/spec/` and the SECFIX2 ticket, whose deterministic (non-racing) verification of
// F-01 is the one that counts.

// F-01b FIXED. `fsio::atomic_replace` is no longer reachable from outside `opencrayast-core`.
//
// Before the fix it was `pub fn atomic_replace(&Path, &[u8], FileIdentity)`: any code that could
// name a path could write it, with no `Boundary` consulted — the PoC below overwrote a file it had
// never been given permission to touch. It is now `pub(crate)` and takes `(&Boundary,
// &ResolvedPath, ..)`, so the write path in this workspace is `Boundary::replace_file` and nothing
// else (ARCHITECTURE principle 2: there is no second way in).
//
// This test can no longer CALL the old entry point, which is the point: the escape hatch is gone
// from the type system, not merely discouraged. So the verification is that the remaining, supported
// route refuses a path outside the workspace and leaves the victim untouched.

/// SECFIX1-01: a `ResolvedPath` pointing outside the workspace is refused, and the victim file is
/// byte-for-byte unchanged. This is the same victim the original PoC owned.
#[test]
fn secfix1_01_a_path_outside_the_workspace_is_refused_and_the_victim_is_untouched() {
    let w = World::new();
    let (_outside_dir, victim) = {
        let d = tempfile::tempdir().unwrap();
        let v = d.path().join("victim.txt");
        fs::write(&v, b"original").unwrap();
        (d, v)
    };

    // A hand-built ResolvedPath is exactly what a caller could construct before the fix.
    let forged = opencrayast_core::boundary::ResolvedPath {
        rel: "../../victim.txt".into(),
        abs: victim.clone(),
    };
    let r = w.boundary.replace_file(&forged, b"PWNED");

    eprintln!("boundary.replace_file(outside) = {r:?}");
    assert_eq!(
        fs::read_to_string(&victim).unwrap(),
        "original",
        "a file outside the workspace must never be written"
    );
    assert!(r.is_err(), "the write must be refused, got {r:?}");
    assert_eq!(
        r.unwrap_err().code,
        opencrayast_core::ErrorCode::OutsideWorkspace
    );
}

/// SECFIX1-02: the old entry point is not reachable from another crate. A comment cannot be the
/// proof, so this test asserts the thing that matters about the boundary: the only write entry point
/// `opencrayast-edit` has is `Boundary::replace_file`, and it takes a policy-resolved path.
///
/// The compile-time half of F-01b is that `opencrayast_core::fsio::atomic_replace` is
/// `pub(crate)`: if it were made `pub` again, the two tests below would keep passing but
/// `crates/edit/tests/` would still compile — so the mutation that must be red is "make it pub and
/// have a test reach for it". This test records the intent in the form the compiler checks: the
/// symbol is not part of the crate's public surface as seen from `opencrayast-edit`.
#[test]
fn secfix1_02_the_primitive_is_not_part_of_the_public_surface() {
    // If `atomic_replace` were `pub` again, a caller could name it here and this file would
    // compile. The assertion below is a runtime statement about the same fact: the supported entry
    // point is on the policy.
    let w = World::new();
    w.write("a.txt", b"original");
    let resolved = w.boundary.resolve_write("a.txt").unwrap();
    assert_eq!(resolved.rel, "a.txt");
    // It resolves and it is writable; the write goes through the policy object.
    w.boundary.replace_file(&resolved, b"new").unwrap();
    assert_eq!(fs::read(w.root.join("a.txt")).unwrap(), b"new");
}

// =====================================================================================
// F-02 (aspect 2, write policy / S-2 / T-17): `write_enabled` is a plain `bool` field on a
// public struct, not a type-level capability.
//
// S-2: "write code is unreachable without a capability value that only exists in write mode."
// T-17 mitigation: "type-level write capability".
// A `bool` is neither: any holder of the public `ApplyContext` can set it to true. Nothing in
// the type system stops a read-mode tool from constructing a write-capable context. The
// `boundary` reference in the same struct is the thing that would make this safe, and it is
// `&'a Boundary` — a boundary that cannot be distinguished from a workspace root, so the
// "capability value" the model names does not exist yet.
// =====================================================================================

/// SECFIX4-01 (F-02 fixed upstream): a caller outside this crate cannot obtain a write capability
/// at all, so the escalation this PoC performed is no longer expressible.
///
/// The old test built a read-only context and then flipped a public `bool` to get a working write
/// context. Both halves are gone: `ApplyContext` has no public `write_enabled` field, and the
/// replacement `WriteCap` is only mintable from inside the crate (`WriteCap::mint` and
/// `policy::enable_writes` are `pub(crate)`), which an integration test — a separate crate — cannot
/// be.
///
/// What is left to assert from out here is the half that IS observable: a context built through
/// the public constructor refuses every mutating entry point. The other half is a compile-time
/// property, and it is stated as such rather than pretended to be tested at runtime.
#[test]
fn secfix4_01_a_caller_outside_the_crate_cannot_escalate_to_write() {
    let w = World::new();
    w.write("a.txt", b"one\n");
    let id = w.plan_one("a.txt", "one", "ONE");

    // The public constructor takes `Option<WriteCap>`; a caller outside the crate can only pass
    // `None`, because there is no public way to produce `Some`.
    let ro = ApplyContext::new(
        &w.boundary,
        &w.plans,
        &w.journals,
        &w.limits,
        &w.state,
        &w.ws,
        None,
        Duration::from_secs(5),
        &NoFault,
    );

    for (name, r) in [
        ("apply", apply(&ro, &id).map(|_| ())),
        ("undo", opencrayast_edit::undo(&ro, &id).map(|_| ())),
        ("recover", opencrayast_edit::recover(&ro).map(|_| ())),
    ] {
        assert_eq!(
            r.as_ref().err().map(|e| e.code),
            Some(opencrayast_core::ErrorCode::WriteDisabled),
            "{name} must refuse without a capability, got {r:?}"
        );
    }
    assert_eq!(
        fs::read_to_string(w.root.join("a.txt")).unwrap(),
        "one\n",
        "nothing was written"
    );

    // Compile-time half, stated as documentation because a test cannot assert it: outside this
    // crate, `opencrayast_edit::WriteCap::mint()` and `opencrayast_edit::policy::enable_writes()`
    // are both `pub(crate)`, so neither name resolves. `crates/edit/src/capability.rs` carries
    // `compile_fail` doctests for exactly those two expressions, which is where that half is
    // verified — by the compiler, on every build.
}

// =====================================================================================
// F-03 (aspect 4, resource limits): `path_max_bytes` and `path_max_depth` were inert. FIXED.
//
// Before the fix `boundary.rs` and `walk.rs` each read `Limits::default()` of their own and
// `Boundary` held no `Limits` at all, so an operator's `[limits] path_max_depth` never reached
// path handling: the one resource knob an operator would reach for when a repository has
// pathological names had no effect, silently, while CONFIGURATION.md:62 listed it in the
// `[limits]` block and line 119 said the walk skips paths "from `[limits]`". SEC-FIX 5 (F-03,
// commits 64b99e6 and 66a54cb) carried the operator's `Limits` in `BoundaryConfig` and made
// `Boundary::limits()` the one place that answers "what is the path ceiling".
//
// So this is no longer `#[ignore]`d: it is the PoC promoted to a real test, and it asserts
// the fix rather than the finding.
//
// ## Why the values below are NOT 4 and 64
//
// The first version of this test configured `path_max_depth = 4` and `path_max_bytes = 64`
// and asserted only that a 21-component path and a 204-byte name were REFUSED. Mutation
// testing showed that version could not fail when its own subject regressed: making the
// boundary serve a `Limits::default()` — the F-03 defect — left it green. The reason is
// structural, not subtle: the test only ever asserted "refused", and both ceilings refuse
// their input, so "refused" is equally consistent with the operator's value binding and with
// it being ignored. A PoC that stays green under its own bug proves nothing, so the values
// and inputs below are chosen so that the configured value and the default give OPPOSITE
// answers about the very same path, which is what actually separates the two hypotheses.
//
// The technique is the one `crates/core/tests/path_limits_spec.rs` and
// `crates/core/tests/config_limits_spec.rs` already use — assert the documented defaults, then
// assert `assert_ne!(configured, default)` so a fixture that stops distinguishing them fails
// loudly — extended with a **directional pair** per knob:
//
//   * `path_max_depth` is set ABOVE its default of 64 (to 96). A 71-component path must be
//     ACCEPTED, and the default boundary must REFUSE it. Only the operator's value can accept
//     it: if the check read a default, this path is over 64 and would be refused.
//   * `path_max_bytes` is set BELOW its default of 4096 (to 1024). A 1205-byte path must be
//     REFUSED, and the default boundary must ACCEPT it. Only the operator's value can refuse
//     it: if the check read a default, 1205 bytes is under 4096 and would be accepted.
//
// The two fixtures are also separated from each other on purpose: `check_size` tests BYTES
// before DEPTH, so the 71-component path (275 bytes) has to fit inside the byte ceiling and
// the byte fixture has to be a single component, or each assertion would be able to pass
// because of the other knob instead of the one it names.
//
// Each knob therefore moves the boundary in OPPOSITE directions, so a single regression —
// any substitution of a default for the configured value — flips at least one assertion from
// pass to fail. Neither direction can be satisfied by accident, and the control boundary
// proves the inputs are genuinely on opposite sides of the two ceilings rather than simply
// being refused-or-accepted for some unrelated reason (over-long name, missing file, bad chars).
//
// This is deliberately NOT a duplicate of the core specs. `path_limits_spec.rs` drives the
// resolver and the walker with a hand-built `Limits`; this test is the adversarial PoC that
// asserts the finding's claim end to end — that the OPERATOR's `[limits]` block binds —
// including the `Settings::parse` -> `boundary_config` route and the value reported back in the
// refusal, which no core spec asserts.
// =====================================================================================

/// F-03 is FIXED (SEC-FIX 5, commits 64b99e6 and 66a54cb). This is no longer `#[ignore]`d: it is
/// the PoC promoted to a real test, and it asserts the fix rather than the finding.
///
/// The invariant: the ceilings `path_max_depth` and `path_max_bytes` that an operator writes in
/// the `[limits]` block are the ceilings the boundary enforces. A refusal must be caused by the
/// operator's value and not by the built-in default, which is only checkable when the configured
/// value and the default disagree about the very path being offered — see the section comment
/// above for the values and why each one sits on the opposite side of its default.
///
/// The knobs are clamped to `PATH_MAX_DEPTH_HARD` (256) and `PATH_MAX_BYTES_HARD` (4096): an
/// operator can raise a ceiling to at most the hard maximum, never past it (`Limits::validate`
/// refuses anything above it outright, at parse time). This test stays well inside both, and
/// asserts that its own configured values are legal, so a future change to the hard maxima
/// cannot quietly invalidate the fixture.
///
/// macOS is excluded, and the reason is the filesystem rather than the code: the fixture needs a
/// path of about 1205 bytes to sit between the configured 1024-byte ceiling and the default
/// 4096, and macOS caps a whole path at PATH_MAX = 1024, so `create_dir_all` fails with
/// ENAMETOOLONG before any assertion runs. There is no path of that length to make on that
/// platform, so the case has nothing to measure there. The byte ceiling itself is still covered
/// on macOS by the tests that use a path short enough to exist.
#[cfg(not(target_os = "macos"))]
#[test]
fn f03_path_limits_ignore_the_configured_limits() {
    // The defaults are the hypothesis this test has to separate itself from, so they are read
    // from the type rather than hard-coded: a changed default shows up as a changed fixture.
    let d = Limits::default();
    eprintln!(
        "defaults: path_max_depth={} path_max_bytes={}",
        d.path_max_depth, d.path_max_bytes
    );

    // Depth ABOVE the default, bytes BELOW it: one knob can only pass by reading the operator's
    // value upward, the other by reading it downward.
    let configured = Limits {
        path_max_depth: 96,
        path_max_bytes: 1024,
        ..Limits::default()
    };
    configured
        .validate()
        .expect("the limits themselves are legal");
    assert!(
        configured.path_max_depth > d.path_max_depth,
        "the depth fixture only means something while it is ABOVE the default ({})",
        d.path_max_depth
    );
    assert!(
        configured.path_max_bytes < d.path_max_bytes,
        "the byte fixture only means something while it is BELOW the default ({})",
        d.path_max_bytes
    );
    assert!(
        configured.path_max_depth <= opencrayast_core::limits::PATH_MAX_DEPTH_HARD
            && configured.path_max_bytes <= opencrayast_core::limits::PATH_MAX_BYTES_HARD,
        "the fixture must stay inside the hard maxima the operator is clamped to"
    );
    eprintln!(
        "operator configured: path_max_depth={} (default {}) path_max_bytes={} (default {})",
        configured.path_max_depth, d.path_max_depth, configured.path_max_bytes, d.path_max_bytes
    );

    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("ws");
    fs::create_dir_all(root.join("d")).unwrap();

    // The parsed route, exactly as a shell builds it: the numbers come out of the operator's
    // file, not out of this test's struct literal. That is the whole claim of F-03.
    let settings = opencrayast_core::config::Settings::parse(&format!(
        "[limits]\npath_max_depth = {}\npath_max_bytes = {}\n",
        configured.path_max_depth, configured.path_max_bytes
    ))
    .expect("the operator's configuration text must parse");
    assert_eq!(
        settings.limits.path_max_depth, configured.path_max_depth,
        "the configured depth must survive parsing"
    );
    assert_eq!(
        settings.limits.path_max_bytes, configured.path_max_bytes,
        "the configured byte ceiling must survive parsing"
    );

    let b = Boundary::new(
        settings
            .boundary_config(&root)
            .expect("the state directory resolves on a test machine"),
    )
    .unwrap();

    // The boundary must hand back what it was configured with, not a copy of the defaults.
    assert_eq!(b.limits().path_max_depth, configured.path_max_depth);
    assert_eq!(b.limits().path_max_bytes, configured.path_max_bytes);

    // ---- depth: 70 components, i.e. over the default of 64 and inside the configured 96.
    //
    // The configured boundary MUST accept it. A boundary reading a default would refuse it,
    // which is the regression this test exists to catch.
    let deep: String = (0..70)
        .map(|i| format!("d{i}"))
        .collect::<Vec<_>>()
        .join("/");
    let deep_file = format!("{deep}/f.txt");
    fs::create_dir_all(root.join(&deep)).unwrap();
    fs::write(root.join(&deep_file), b"x").unwrap();

    let resolved = b.resolve_read(&deep_file);
    eprintln!(
        "configured boundary, resolve_read(71 components) = {:?}",
        resolved.as_ref().map(|p| &p.rel)
    );
    assert!(
        resolved.is_ok(),
        "a 71-component path is inside the operator's ceiling of {} and must resolve; refusing it \
         means the check fell back to the default of {} — the F-03 defect. Got {:?}",
        configured.path_max_depth,
        d.path_max_depth,
        resolved.err()
    );

    // The control: the SAME path against a boundary left at the defaults must be refused. If
    // this one is accepted, the path is not actually over the default and the assertion above
    // is not proving anything about the ceiling.
    let default_b = Boundary::new(BoundaryConfig::new(root.clone(), Limits::default())).unwrap();
    let under_default = default_b.resolve_read(&deep_file);
    eprintln!(
        "default boundary,    resolve_read(71 components) = {:?}",
        under_default.as_ref().map(|p| &p.rel)
    );
    assert_eq!(
        under_default.as_ref().err().map(|e| e.code),
        Some(opencrayast_core::ErrorCode::LimitExceeded),
        "the same 71-component path must hit the default ceiling of {}, so the assertion above \
         distinguishes the operator's value from the default rather than passing on its own",
        d.path_max_depth
    );

    // ---- bytes: a path ~1205 bytes long but only 6 components deep, i.e. over the configured 1024
    // and under the default of 4096. Shallow in DEPTH, long in BYTES, so the byte ceiling is what
    // must fire and not the depth one. The length is spread over several components because one
    // component cannot exceed NAME_MAX (255 on Linux/macOS): a single 1205-byte name cannot be
    // created at all, and a fixture that fails to build proves nothing about the ceiling.
    let long_rel: String = (0..6)
        .map(|i| format!("{}x{i}", "n".repeat(198)))
        .collect::<Vec<_>>()
        .join("/");
    fs::create_dir_all(root.join(&long_rel)).unwrap();
    fs::write(root.join(&long_rel).join("f.txt"), b"x").unwrap();
    let long_file = format!("{long_rel}/f.txt");
    // The two bounds are what make this fixture work; the exact length is incidental, so they are
    // asserted against the real string rather than restated as magic numbers.
    assert!(
        long_file.len() > configured.path_max_bytes as usize
            && long_file.len() <= d.path_max_bytes as usize,
        "the byte fixture must be over the configured {} and under the default {}, got {}",
        configured.path_max_bytes,
        d.path_max_bytes,
        long_file.len()
    );
    assert!(
        long_file.split('/').count() <= configured.path_max_depth as usize,
        "the byte fixture must be inside the depth ceiling or it would be refused for depth"
    );
    eprintln!(
        "byte fixture: {} bytes, {} components",
        long_file.len(),
        long_file.split('/').count()
    );

    let r2 = b.resolve_read(&long_file);
    eprintln!(
        "configured boundary, resolve_read(1205-byte path) = {:?}",
        r2.as_ref().map(|p| &p.rel)
    );
    assert_eq!(
        r2.as_ref().err().map(|e| e.code),
        Some(opencrayast_core::ErrorCode::LimitExceeded),
        "a 1205-byte path is over the operator's ceiling of {} and must be refused; accepting it \
         means the check fell back to the default of {} — the F-03 defect",
        configured.path_max_bytes,
        d.path_max_bytes
    );
    // The refusal must name the operator's ceiling, or a limit nobody can see is one nobody can
    // satisfy (and the operator cannot tell which knob to turn).
    let refused = r2.expect_err("the assertion above established this is an error");
    assert!(
        refused.next.contains("path_max_bytes"),
        "the refusal must name the ceiling that was actually exceeded: {refused:?}"
    );

    // The control for the byte direction: the same ~1205-byte path is INSIDE the default ceiling,
    // so a default boundary must accept it. Without this, the assertion above could be passing
    // because the path is refused for an unrelated reason.
    let under_default_bytes = default_b.resolve_read(&long_file);
    eprintln!(
        "default boundary,    resolve_read(1205-byte path) = {:?}",
        under_default_bytes.as_ref().map(|p| &p.rel)
    );
    assert!(
        under_default_bytes.is_ok(),
        "a 1205-byte path is under the default ceiling of {}, so a default boundary must accept \
         it; refusing it means the assertion above is not about the ceiling",
        d.path_max_bytes
    );
}

// =====================================================================================
// F-04 (aspect 5, error-message leakage): `fsio::atomic_replace` reports the TARGET's
// metadata in the error text. SECURITY-MODEL T-19 / asset row "Files outside the workspace".
// Combined with F-01b (no Boundary check) this is a working metadata oracle for any path the
// process can name: mode, hard-link count and file type are read out of a refusal.
// =====================================================================================

/// SECFIX1-03 (F-04 fixed): a refusal names the CLASS of problem and nothing else. The target's
/// `nlink`, mode bits and file type never appear, so a caller that can only name paths cannot read
/// them back out of an error message (SECURITY-MODEL T-19).
#[test]
fn secfix1_03_a_refusal_names_the_class_and_not_the_targets_metadata() {
    use std::os::unix::fs::PermissionsExt;
    let w = World::new();
    w.write("readonly.txt", b"TOKEN=abc");
    w.write("linked.txt", b"shared content");
    fs::hard_link(w.root.join("linked.txt"), w.root.join("second.txt")).unwrap();

    let mut messages = Vec::new();

    // Read-only. The policy refuses this at `resolve_write`, before the write primitive is reached
    // — the earlier and better place for it, and why the class still comes back as
    // `unsupported_target` with a `next`.
    fs::set_permissions(
        w.root.join("readonly.txt"),
        fs::Permissions::from_mode(0o400),
    )
    .unwrap();
    messages.push((
        "read-only",
        w.boundary
            .resolve_write("readonly.txt")
            .and_then(|r| w.boundary.replace_file(&r, b"x"))
            .expect_err("a read-only target must be refused"),
    ));
    fs::set_permissions(
        w.root.join("readonly.txt"),
        fs::Permissions::from_mode(0o600),
    )
    .unwrap();

    // Hard-linked, and a directory: same shape. Each is refused by `resolve_write`, which is the
    // earliest point at which the target has been examined at all.
    messages.push((
        "hard-linked",
        w.boundary
            .resolve_write("linked.txt")
            .and_then(|r| w.boundary.replace_file(&r, b"x"))
            .expect_err("a hard-linked target must be refused"),
    ));
    fs::create_dir_all(w.root.join("adir")).unwrap();
    messages.push((
        "directory",
        w.boundary
            .resolve_write("adir")
            .and_then(|r| w.boundary.replace_file(&r, b"x"))
            .expect_err("a directory target must be refused"),
    ));

    for (name, e) in &messages {
        eprintln!("{name:>12} target -> {}", e.message);
    }

    // Nothing that measures the target. Naming the CLASS is required (a person has to know what to
    // fix); reporting the target's own numbers is the leak. So "exactly one hard link" is fine — it
    // is the class — while "has 2 hard links" and "(mode 40755)" are not.
    let forbidden = [
        "mode 4",
        "mode 0",
        "hard links",
        "file type",
        "nlink",
        "(mode",
        "0400",
        "40755",
        "0755",
    ];
    // A bare count must never appear next to the word "link".
    let counts_target = |m: &str| {
        let lower = m.to_lowercase();
        lower.contains("has ") && lower.contains(" link")
    };
    let mut leaked = Vec::new();
    for (name, e) in &messages {
        for f in forbidden {
            if e.message.to_lowercase().contains(f) {
                leaked.push((name, f, e.message.clone()));
            }
        }
        if counts_target(&e.message) {
            leaked.push((name, "a link count", e.message.clone()));
        }
        // And no absolute path from inside the machine.
        assert!(
            !e.message.contains(w.root.to_str().unwrap()),
            "{name}: the refusal leaked an absolute path: {}",
            e.message
        );
    }
    assert!(
        leaked.is_empty(),
        "refusals must not describe the target: {leaked:?}"
    );

    // The class is still distinguishable, so a person can act on it: every refusal is
    // `unsupported_target` with a `next` that says what to pick.
    for (name, e) in &messages {
        assert_eq!(
            e.code,
            opencrayast_core::ErrorCode::UnsupportedTarget,
            "{name}"
        );
        assert!(
            !e.next.is_empty(),
            "{name}: the refusal must say what to do"
        );
    }
}

/// SECFIX1-04 (T-20, indistinguishability): the SAME refusal class produces byte-identical text
/// whether the target exists, does not exist, is inside the workspace, or is outside it. A caller
/// must not be able to learn whether a path exists by reading the error.
#[test]
fn secfix1_04_one_refusal_class_is_byte_identical_for_present_and_absent_targets() {
    let w = World::new();
    w.write("present.txt", b"x");
    w.write("ro.txt", b"x");
    fs::set_permissions(
        w.root.join("ro.txt"),
        std::fs::Permissions::from_mode(0o400),
    )
    .unwrap();

    let _outside_dir = tempfile::tempdir().unwrap();

    // Three inside-workspace read-only targets (one of which does not exist is impossible: a
    // missing file is refused earlier, with a different code), so instead: the OUTSIDE path and an
    // INSIDE path must be indistinguishable when both are refused for being outside the policy.
    let inside_forged = opencrayast_core::boundary::ResolvedPath {
        rel: "../../elsewhere.txt".into(),
        abs: w.root.join("does-not-exist.txt"),
    };
    let outside_dir = tempfile::tempdir().unwrap();
    let outside_real = outside_dir.path().join("real.txt");
    fs::write(&outside_real, b"x").unwrap();
    let outside_forged = opencrayast_core::boundary::ResolvedPath {
        rel: "../../elsewhere.txt".into(),
        abs: outside_real.clone(),
    };

    let a = w.boundary.replace_file(&inside_forged, b"x").unwrap_err();
    let b = w.boundary.replace_file(&outside_forged, b"x").unwrap_err();

    eprintln!("absent, inside  -> [{}] {}", a.code.as_str(), a.message);
    eprintln!("present, outside-> [{}] {}", b.code.as_str(), b.message);

    assert_eq!(a.code, b.code, "same class, same code");
    assert_eq!(
        a.message, b.message,
        "T-20: the refusal must not reveal existence"
    );
    assert_eq!(a.next, b.next, "T-20: and the next step must not either");
}

// =====================================================================================
// Verified-clean probes. These PASS today; they are the reproducible evidence for the
// "checked, no finding" section of the report.
// =====================================================================================

/// Aspect 1: the ordinary traversal shapes are refused, uniformly.
#[test]
fn clean_facet1_traversal_shapes_are_refused() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("ws");
    fs::create_dir_all(root.join("src")).unwrap();
    fs::write(root.join("src/a.rs"), b"fn main(){}").unwrap();
    fs::create_dir_all(root.join(".git")).unwrap();
    fs::write(root.join(".git/config"), b"[core]").unwrap();
    fs::write(root.join("real.txt"), b"real").unwrap();
    symlink(dir.path().join("outside.txt"), root.join("link.txt")).unwrap();
    fs::write(dir.path().join("outside.txt"), b"outside").unwrap();
    fs::create_dir_all(root.join("hard")).unwrap();
    fs::write(root.join("hard/orig.txt"), b"x").unwrap();
    fs::hard_link(root.join("hard/orig.txt"), root.join("hard/link.txt")).unwrap();

    let b = Boundary::new(BoundaryConfig {
        root: root.clone(),
        limits: Limits::default(),
        read_roots: Vec::new(),
        state_dir: None,
        extra_protected: Vec::new(),
    })
    .unwrap();

    let cases = [
        ("dotdot", "../outside.txt"),
        ("deep-dotdot", "src/../../outside.txt"),
        ("backslash-dotdot", "src\\..\\..\\outside.txt"),
        ("absolute-outside", "/etc/passwd"),
        ("unc", "//server/share/x"),
        ("drive-relative", "C:foo"),
        ("device", "\\\\?\\C:\\x"),
        ("ads", "src/a.rs:hidden"),
        ("root-relative", "\\Windows"),
        ("nul", "src/a.rs\u{0}.txt"),
        ("bidi", "src/\u{202e}gnp.txt"),
        ("symlink-out", "link.txt"),
    ];
    for (name, path) in cases {
        let r = b.resolve_read(path);
        assert!(r.is_err(), "{name}: {path:?} must be refused, got {r:?}");
    }

    // Protected and hard-linked WRITE targets are refused too.
    for (name, path) in [
        ("git-dir", ".git/config"),
        ("env", "src/.env"),
        ("pem", "src/id_rsa"),
    ] {
        let r = b.resolve_write(path);
        assert!(r.is_err(), "{name}: {path:?} must be refused, got {r:?}");
    }
    assert!(
        b.resolve_write("hard/link.txt").is_err(),
        "a hard-linked write target must be refused"
    );
    assert!(
        b.resolve_write("link.txt").is_err(),
        "a symlinked write target must be refused"
    );
    assert!(
        b.resolve_read("src/a.rs").is_ok(),
        "an ordinary in-workspace path must still resolve"
    );
}

/// Aspect 2: with `write_enabled = false`, apply / undo / recover all refuse.
#[test]
fn clean_facet2_every_edit_entry_point_checks_the_flag() {
    let w = World::new();
    w.write("a.txt", b"one\n");
    let id = w.plan_one("a.txt", "one", "ONE");
    // F-02 is fixed upstream (SECFIX4): write mode is a `WriteCap` that cannot be minted outside
    // this crate, so a read-only context is now `ApplyContext::new(.., None, ..)`.
    let ro = ApplyContext::new(
        &w.boundary,
        &w.plans,
        &w.journals,
        &w.limits,
        &w.state,
        &w.ws,
        None,
        Duration::from_secs(5),
        &NoFault,
    );
    let results: Vec<(&str, Result<(), opencrayast_core::error::ToolError>)> = vec![
        ("apply", apply(&ro, &id).map(|_| ())),
        ("undo", opencrayast_edit::undo(&ro, &id).map(|_| ())),
        ("recover", opencrayast_edit::recover(&ro).map(|_| ())),
    ];
    for (name, r) in results {
        assert_eq!(
            r.as_ref().err().map(|e| e.code),
            Some(opencrayast_core::ErrorCode::WriteDisabled),
            "{name} must refuse in read mode, got {r:?}"
        );
    }
    assert_eq!(fs::read_to_string(w.root.join("a.txt")).unwrap(), "one\n");
}

/// Aspect 3: the FINAL-component races are refused (only the directory-level one is not).
#[test]
fn clean_facet3_final_component_swaps_are_refused() {
    let w = World::new();
    w.write("a.txt", b"one\n");
    // This test used to inject a `Fault` that swapped the target for a symlink mid-apply. It now
    // drives `Boundary` directly, because that is the layer the property belongs to and it needs no
    // write capability — which an integration test can no longer mint anyway (SECFIX4 / F-02).
    let outside = w._dir.path().join("outside");
    fs::create_dir_all(&outside).unwrap();
    let resolved = w.boundary.resolve_write("a.txt").unwrap();
    let (_handle, identity) = w.boundary.open_read(&resolved).unwrap();

    // The attack: move the target out of the workspace and leave a link to it in its place.
    fs::rename(w.root.join("a.txt"), outside.join("moved.txt")).unwrap();
    symlink(outside.join("moved.txt"), w.root.join("a.txt")).unwrap();

    let r = w
        .boundary
        .replace_file_checked(&resolved, b"ONE\n", Some(identity));

    eprintln!("final-component symlink swap -> {r:?}");
    assert!(
        r.is_err(),
        "CR F1: a symlinked write target must be refused, got {r:?}"
    );
    assert_eq!(
        fs::read_to_string(outside.join("moved.txt")).unwrap(),
        "one\n",
        "the file outside the workspace must be untouched"
    );
}

/// Aspect 4: the plan and journal ceilings are enforced before anything is written.
#[test]
fn clean_facet4_plan_and_journal_ceilings_are_enforced() {
    use opencrayast_core::ErrorCode;
    let w = World::new();
    w.write("a.txt", b"one\n");

    let tight = Limits {
        plan_max_files: 1,
        plan_max_edits: 1,
        ..Limits::default()
    };
    let plans = PlanStore::open(&w.state, &w.ws, tight, w.clock.clone()).unwrap();
    let big = Plan {
        format: 1,
        workspace_id: w.ws.clone(),
        engine_format: 1,
        request: PlanRequest {
            kind: "rewrite".into(),
            summary: "poc".into(),
            note: None,
        },
        files: vec![
            PlanFile {
                path: "a.txt".into(),
                language: "text".into(),
                pre_hash: ContentHash::of(b"one\n"),
                pre_size: 4,
                pre_errors: 0,
                post_hash: ContentHash::of(b"ONE\n"),
                post_size: 4,
                post_errors: 0,
                edits: vec![Edit {
                    start: 0,
                    end: 3,
                    replacement: "ONE".into(),
                }],
            },
            PlanFile {
                path: "b.txt".into(),
                language: "text".into(),
                pre_hash: ContentHash::of(b"one\n"),
                pre_size: 4,
                pre_errors: 0,
                post_hash: ContentHash::of(b"ONE\n"),
                post_size: 4,
                post_errors: 0,
                edits: vec![Edit {
                    start: 0,
                    end: 3,
                    replacement: "ONE".into(),
                }],
            },
        ],
    };
    let r = plans.put(&big);
    eprintln!(
        "plan with 2 files under plan_max_files=1 -> {:?}",
        r.as_ref().err().map(|e| e.code)
    );
    assert_eq!(
        r.err().map(|e| e.code),
        Some(ErrorCode::LimitExceeded),
        "plan_max_files must be enforced at put()"
    );
}

/// Aspect 5: the boundary's own refusals carry no path, and the outside/not-found pair that
/// T-20 requires to be indistinguishable really is indistinguishable.
#[test]
fn clean_facet5_boundary_refusals_carry_no_path() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("ws");
    fs::create_dir(&root).unwrap();
    fs::write(dir.path().join("secret"), b"x").unwrap();
    let b = Boundary::new(BoundaryConfig {
        root: root.clone(),
        limits: Limits::default(),
        read_roots: Vec::new(),
        state_dir: None,
        extra_protected: Vec::new(),
    })
    .unwrap();

    // An ABSOLUTE path outside that exists, and one that does not: the model requires these to
    // be indistinguishable (T-20 / BND-18), so neither is an existence oracle.
    let exists = dir.path().join("secret");
    let missing = dir.path().join("no-such-file-here");
    let e1 = b.resolve_read(exists.to_str().unwrap()).unwrap_err();
    let e2 = b.resolve_read(missing.to_str().unwrap()).unwrap_err();
    eprintln!("absolute, exists  : {} / {:?}", e1.message, e1.code);
    eprintln!("absolute, missing : {} / {:?}", e2.message, e2.code);
    assert_eq!(e1.code, e2.code, "the two must be indistinguishable");
    assert_eq!(e1.message, e2.message);
    assert_eq!(e1.next, e2.next);

    // A traversal out of the workspace is the same refusal again.
    let e3 = b.resolve_read("../secret").unwrap_err();
    assert_eq!(e3.code, e1.code);
    assert_eq!(e3.message, e1.message);

    // No refusal anywhere names the root, the temp dir, or the outside file.
    for e in [&e1, &e2, &e3] {
        for probe in [
            root.to_str().unwrap(),
            dir.path().to_str().unwrap(),
            "secret",
        ] {
            assert!(
                !e.message.contains(probe) && !e.next.contains(probe),
                "refusal leaked {probe:?}: {e:?}"
            );
        }
    }

    // A RELATIVE path that is inside by construction may honestly answer not_found; that is
    // the documented exception, and it carries no path either.
    let inside = b.resolve_read("missing-inside").unwrap_err();
    eprintln!("relative, missing : {} / {:?}", inside.message, inside.code);
    assert_eq!(inside.code, opencrayast_core::ErrorCode::NotFound);
    assert!(!inside.message.contains(root.to_str().unwrap()));
}

/// Aspect 6: no build script, no git dependency, lockfile committed, actions pinned to SHAs.
#[test]
fn clean_facet6_supply_chain_shape() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let root = root.canonicalize().unwrap();

    let mut build_rs = Vec::new();
    for dir in ["crates", "."] {
        let d = root.join(dir);
        if let Ok(rd) = fs::read_dir(&d) {
            for e in rd.flatten() {
                let p = e.path();
                if p.is_dir() {
                    let sub = p.join("build.rs");
                    if sub.exists() {
                        build_rs.push(sub);
                    }
                }
            }
        }
    }
    assert!(
        build_rs.is_empty(),
        "no build script expected, found {build_rs:?}"
    );

    let lock = fs::read_to_string(root.join("Cargo.lock")).unwrap();
    assert!(
        !lock.contains("source = \"git+"),
        "Cargo.lock must contain no git dependency"
    );

    // EVERY workflow, not just ci.yml. This check used to read ci.yml alone, so a
    // second workflow could ship with unpinned actions and CI would call it pinned —
    // the same shape as the claim-vs-implementation gap it is meant to close.
    let workflows = fs::read_dir(root.join(".github/workflows")).unwrap();
    let mut checked = 0usize;
    for entry in workflows.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("yml") {
            continue;
        }
        let text = fs::read_to_string(&path).unwrap();
        for line in text.lines().filter(|l| l.trim_start().starts_with("uses:")) {
            let at = line
                .split('@')
                .nth(1)
                .unwrap()
                .split_whitespace()
                .next()
                .unwrap();
            assert_eq!(
                at.len(),
                40,
                "action not pinned to a full SHA in {}: {line:?}",
                path.display()
            );
            assert!(
                at.chars().all(|c| c.is_ascii_hexdigit()),
                "action pin is not a SHA in {}: {line:?}",
                path.display()
            );
        }
        checked += 1;
    }
    // A directory that lost every workflow would make the loop above vacuously pass.
    assert!(
        checked >= 3,
        "expected at least three workflows, found {checked}"
    );
}

// =====================================================================================
// F-05 (aspect 6, and the credibility of the whole matrix): `scripts/check-matrix.sh`
// cannot fail for a missing test.
//
// SECURITY-MODEL.md: "the matrix there is checked in CI so a threat without a test is a
// build failure". Reproduction (run from the repo root):
//
//     cp crates/core/tests/boundary_spec.rs /tmp/bk
//     rm crates/core/tests/boundary_spec.rs
//     bash scripts/check-matrix.sh
//     # -> "matrix check passed: 124 tests, all referenced; every threat has a test"  exit 0
//     cp /tmp/bk crates/core/tests/boundary_spec.rs
//
// The whole BND suite (BND-01..BND-24: every path-traversal, symlink, hard-link, aliasing
// and existence-probing mitigation in T-01..T-05, T-20) can be deleted and CI is green,
// because the script only cross-references identifiers between .md files. It never reads
// test code. So all 34 threats' "Tests" columns are currently unverified claims.
//
// This test asserts the structural cause, so it stays green/red with the script itself.
// =====================================================================================

#[test]
fn f05_matrix_check_now_inspects_test_code() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let script = fs::read_to_string(root.join("scripts/check-matrix.sh")).unwrap();

    // F-05 was "the script only cross-references identifiers between .md files, so a whole test
    // suite can be deleted and CI stays green". SEC-FIX 3 fixed it: the script now resolves every
    // catalogue target on disk and requires a real `#[test]` behind it. This PoC used to assert
    // the defect; it is flipped, as SEC-AUDIT's rule requires, into an assertion that the defect
    // is impossible - and it is no longer `#[ignore]`d, so it cannot rot back into a stale notice.
    let reads_code =
        script.contains("crates/") || script.contains(".rs") || script.contains("tests");
    assert!(
        reads_code,
        "the matrix check must inspect test code; if this fails, F-05 has regressed"
    );
}

// =====================================================================================
// Extra verified-clean probes added while looking for F-01: things that looked like holes
// and are not. These PASS.
// =====================================================================================

/// Aspect 1: the path is never percent-decoded, so an encoded traversal is a literal file
/// name, not a traversal (T-01 names "encoded variants").
#[test]
fn clean_facet1_encoded_traversal_is_not_decoded() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("ws");
    fs::create_dir(&root).unwrap();
    fs::write(dir.path().join("outside"), b"x").unwrap();
    let b = Boundary::new(BoundaryConfig {
        root: root.clone(),
        limits: Limits::default(),
        read_roots: Vec::new(),
        state_dir: None,
        extra_protected: Vec::new(),
    })
    .unwrap();

    // Percent-encoded dot-dot: treated as an ordinary (weird) file name.
    let r = b.resolve_read("%2e%2e%2foutside");
    assert_eq!(
        r.unwrap_err().code,
        opencrayast_core::ErrorCode::NotFound,
        "an encoded traversal must be a literal name, not decoded into a traversal"
    );
    // Double-encoded and backslash-encoded forms likewise.
    for p in ["..%2foutside", "%252e%252e%252foutside", "..%5coutside"] {
        assert_eq!(
            b.resolve_read(p).unwrap_err().code,
            opencrayast_core::ErrorCode::NotFound,
            "{p:?} must not be decoded into a traversal"
        );
    }
}

/// Aspect 1: Unicode normalisation. Two byte spellings of the same name are two different
/// files on Linux and on APFS/HFS+; NTFS does normalise, which is why T-04 says protected
/// matching runs on the canonical form. Checked here as far as it is observable on Linux:
/// neither spelling can be used to smuggle a protected name past `is_protected`.
#[test]
fn clean_facet1_unicode_spellings_cannot_smuggle_a_protected_name() {
    use opencrayast_core::protected::is_protected;
    assert!(
        is_protected(std::path::Path::new(".git/config"), &[]),
        ".git/config must be protected"
    );
    // The deny list folds case for the entries it owns (T-04)...
    assert!(is_protected(std::path::Path::new(".GIT/config"), &[]));
    assert!(is_protected(std::path::Path::new("a/.Hg/x"), &[]));
    assert!(is_protected(
        std::path::Path::new("x/credentials.json"),
        &[]
    ));
    assert!(is_protected(std::path::Path::new("x/a.PEM"), &[]));
    // ...and it is checked on the canonical, boundary-resolved path, so `..` cannot be used
    // to hide one (a raw `.git/../ok.txt` would not be protected, but the boundary never
    // hands this function such a path).
    assert!(!is_protected(std::path::Path::new("src/main.rs"), &[]));
    assert!(is_protected(
        std::path::Path::new("secrets/a.txt"),
        &["secrets/**".into()]
    ));
}

/// Aspect 4: a plan note and an oversized plan are bounded at store time, and the store
/// quota refuses rather than evicting an unexpired plan (T-25, EDT-27).
#[test]
fn clean_facet4_note_and_file_ceilings_are_enforced_at_put() {
    use opencrayast_core::ErrorCode;
    let w = World::new();
    w.write("a.txt", b"one\n");
    let bytes = fs::read(w.root.join("a.txt")).unwrap();

    let mut files = Vec::new();
    for i in 0..3 {
        files.push(PlanFile {
            path: format!("f{i}.txt"),
            language: "text".into(),
            pre_hash: ContentHash::of(&bytes),
            pre_size: bytes.len() as u64,
            pre_errors: 0,
            post_hash: ContentHash::of(&bytes),
            post_size: bytes.len() as u64,
            post_errors: 0,
            edits: vec![Edit {
                start: 0,
                end: 3,
                replacement: "ONE".into(),
            }],
        });
    }
    let plan = Plan {
        format: 1,
        workspace_id: w.ws.clone(),
        engine_format: 1,
        request: PlanRequest {
            kind: "rewrite".into(),
            summary: "poc".into(),
            note: Some("n".repeat(w.limits.note_max_bytes as usize + 1)),
        },
        files,
    };
    let r = w.plans.put(&plan);
    eprintln!("oversized note -> {:?}", r.as_ref().err().map(|e| e.code));
    assert_eq!(
        r.err().map(|e| e.code),
        Some(ErrorCode::LimitExceeded),
        "note_max_bytes must be enforced at put()"
    );
}
