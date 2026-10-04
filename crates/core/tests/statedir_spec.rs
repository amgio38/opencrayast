//! Spec for ISSUE-CORE-STATEDIR (STA-01, STA-03).
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::field_reassign_with_default
)]
#![cfg(unix)]
use opencrayast_core::error::ErrorCode;
use opencrayast_core::statedir::ensure_state_dir;
use std::fs;
use std::os::unix::fs::{PermissionsExt, symlink};
use std::sync::Arc;

#[test]
fn creates_0700_and_is_idempotent() {
    let d = tempfile::tempdir().unwrap();
    let s = d.path().join("a/b/state");
    let p = ensure_state_dir(&s).unwrap();
    assert_eq!(
        fs::metadata(&p).unwrap().permissions().mode() & 0o777,
        0o700
    );
    assert!(ensure_state_dir(&s).is_ok());
}

#[test]
fn refuses_group_or_other_accessible_dir() {
    let d = tempfile::tempdir().unwrap();
    let s = d.path().join("state");
    fs::create_dir(&s).unwrap();
    fs::set_permissions(&s, fs::Permissions::from_mode(0o755)).unwrap();
    assert!(ensure_state_dir(&s).is_err());
    fs::set_permissions(&s, fs::Permissions::from_mode(0o770)).unwrap();
    assert!(ensure_state_dir(&s).is_err());
}

#[test]
fn refuses_symlink_and_non_directory() {
    let d = tempfile::tempdir().unwrap();
    let real = d.path().join("real");
    fs::create_dir(&real).unwrap();
    fs::set_permissions(&real, fs::Permissions::from_mode(0o700)).unwrap();
    let link = d.path().join("link");
    symlink(&real, &link).unwrap();
    assert!(
        ensure_state_dir(&link).is_err(),
        "a symlinked state dir must not be adopted"
    );
    let f = d.path().join("file");
    fs::write(&f, "").unwrap();
    assert!(ensure_state_dir(&f).is_err());
}

/// A pre-created directory that is not private is the STA-03 case: it must be refused,
/// never adopted and never silently repaired (T-21).
#[test]
fn refuses_attacker_precreated_group_readable_dir() {
    let d = tempfile::tempdir().unwrap();
    let s = d.path().join("state");
    fs::create_dir(&s).unwrap();
    fs::set_permissions(&s, fs::Permissions::from_mode(0o750)).unwrap();
    let e = ensure_state_dir(&s).unwrap_err();
    assert_eq!(e.code, ErrorCode::IoError);
    // Refusing must not have widened or narrowed the directory on disk.
    assert_eq!(
        fs::metadata(&s).unwrap().permissions().mode() & 0o777,
        0o750
    );
}

/// Refusal cases must all be `io_error`, and the message may name the state directory
/// (operator configuration, not workspace content) but must stay readable.
#[test]
fn refusal_codes_and_messages() {
    let d = tempfile::tempdir().unwrap();
    let s = d.path().join("state");
    fs::create_dir(&s).unwrap();
    fs::set_permissions(&s, fs::Permissions::from_mode(0o777)).unwrap();
    let e = ensure_state_dir(&s).unwrap_err();
    assert_eq!(e.code, ErrorCode::IoError);
    assert!(!e.next.is_empty());
    assert!(!e.message.contains('\0'));
}

/// A symlinked PARENT is accepted. Reasoning: confidentiality comes from the state
/// directory's own `0700` mode and ownership, not from the names above it (the operator
/// chose that spelling, and the canonical path we return is resolved through the link).
/// Only the final component is checked not-a-symlink, because that is the one
/// indirection an attacker can re-point after we create the directory and before we
/// adopt it (STA-01, STA-03).
#[test]
fn allows_symlinked_parent_but_still_requires_private_final_component() {
    let d = tempfile::tempdir().unwrap();
    let real_parent = d.path().join("real");
    fs::create_dir(&real_parent).unwrap();
    let parent_link = d.path().join("parent_link");
    symlink(&real_parent, &parent_link).unwrap();

    let via_link = parent_link.join("state");
    let p = ensure_state_dir(&via_link).unwrap();
    assert_eq!(
        fs::metadata(&p).unwrap().permissions().mode() & 0o777,
        0o700
    );
    // The returned path is canonical: it no longer goes through the link.
    assert!(!p.to_string_lossy().contains("parent_link"));
    assert_eq!(p, fs::canonicalize(real_parent.join("state")).unwrap());

    // ... but the final component itself is still refused when it is a link.
    let target = real_parent.join("target");
    fs::create_dir(&target).unwrap();
    fs::set_permissions(&target, fs::Permissions::from_mode(0o700)).unwrap();
    let state_link = parent_link.join("state2");
    symlink(&target, &state_link).unwrap();
    assert!(ensure_state_dir(&state_link).is_err());
}

/// Concurrent first use: eight threads racing to create the same state directory must
/// all succeed (STA-01 relies on the directory ending up private and usable, not on
/// being the only creator).
#[test]
fn concurrent_creation_all_succeed() {
    let d = tempfile::tempdir().unwrap();
    let s = d.path().join("race/state");
    let barrier = std::sync::Arc::new(std::sync::Barrier::new(8));
    let handles: Vec<_> = (0..8)
        .map(|_| {
            let s = s.clone();
            let barrier = Arc::clone(&barrier);
            std::thread::spawn(move || {
                barrier.wait();
                ensure_state_dir(&s)
            })
        })
        .collect();
    for h in handles {
        let p = h.join().unwrap().unwrap();
        assert_eq!(
            fs::metadata(&p).unwrap().permissions().mode() & 0o777,
            0o700
        );
    }
}

/// The owner row of the decision table. Producing a foreign-owned directory needs root
/// AND a second uid to hand it to, so a root-only container cannot exercise it; that is
/// reported loudly instead of being asserted weakly. The mode stays 0700 throughout, so
/// ownership is the only thing that can make this fail.
#[test]
fn refuses_directory_owned_by_another_user() {
    if rustix::fs::Uid::as_raw(rustix::process::geteuid()) != 0 {
        eprintln!("skipping: chown to a foreign uid needs root");
        return;
    }
    let d = tempfile::tempdir().unwrap();
    let s = d.path().join("state");
    fs::create_dir(&s).unwrap();
    fs::set_permissions(&s, fs::Permissions::from_mode(0o700)).unwrap();
    const NOBODY: u32 = 65534;
    if rustix::fs::chown(
        &s,
        Some(rustix::fs::Uid::from_raw_unchecked(NOBODY)),
        Some(rustix::fs::Gid::from_raw_unchecked(NOBODY)),
    )
    .is_err()
    {
        eprintln!("skipping: this container cannot hand a directory to uid {NOBODY}");
        return;
    }
    assert_eq!(
        fs::metadata(&s).unwrap().permissions().mode() & 0o777,
        0o700,
        "the mode must stay private so only the owner row can refuse"
    );
    let e = ensure_state_dir(&s).unwrap_err();
    assert_eq!(e.code, ErrorCode::IoError);
}

// ---- the resolver: $XDG_STATE_HOME / $HOME. Run in a child process, always. ------------------
//
// `user_state_dir` reads two process-wide variables. `std::env::set_var` is `unsafe` in edition
// 2024 precisely because a concurrent getenv/setenv is UB, and this binary runs its tests in
// parallel — so setting one from a test thread would be a data race with every other test here.
// The crate also compiles with `-F unsafe-code`, so the guard cannot live in `src` at all.
//
// So each case is a **separate process** re-running this binary with the variables it needs set.
// That is the same technique `boundary_guards_spec.rs` uses for `HOME`, and it has the property
// that matters most here: a refusal really is observable, because there is no ambient HOME to
// accidentally answer for it.

/// Inert unless the parent re-runs this binary with a fixture environment. Prints one line.
#[test]
fn resolver_helper() {
    // Inert unless the parent re-runs this binary with a fixture environment (see `resolve_with`).
    if std::env::var_os(RESOLVER_HELPER_ENV).is_none() {
        return;
    }
    match opencrayast_core::statedir::user_state_dir() {
        Ok(dir) => println!("RESOLVED {}", dir.display()),
        Err(e) => println!("REFUSED {}|{}", e.code.as_str(), e.next),
    }
}

/// Run the helper with exactly `env` set (plus the marker), and return what it printed.
fn resolve_with(env: &[(&str, &str)]) -> String {
    let exe = std::env::current_exe().expect("test binary path");
    let mut cmd = std::process::Command::new(exe);
    cmd.args(["--exact", "resolver_helper", "--nocapture"]);
    // A child inherits the parent's environment, so an ambient XDG_STATE_HOME or HOME has to be
    // removed explicitly for the "not set" cases to mean anything.
    for v in ["XDG_STATE_HOME", "HOME", "LOCALAPPDATA"] {
        cmd.env_remove(v);
    }
    for (k, v) in env {
        cmd.env(k, v);
    }
    cmd.env(RESOLVER_HELPER_ENV, "1");
    let out = cmd.output().expect("running the resolver helper");
    let text = String::from_utf8_lossy(&out.stdout).into_owned();
    text.lines()
        .find(|l| l.starts_with("RESOLVED ") || l.starts_with("REFUSED "))
        .unwrap_or_else(|| panic!("the helper printed no verdict:\n{text}"))
        .to_string()
}

const RESOLVER_HELPER_ENV: &str = "OPENCRAYAST_STATEDIR_HELPER";

/// XDG section 3: an absolute `XDG_STATE_HOME` wins, and `opencrayast` is appended exactly once.
#[test]
fn an_absolute_xdg_state_home_wins() {
    let line = resolve_with(&[("XDG_STATE_HOME", "/xdg/state"), ("HOME", "/home/somebody")]);
    assert_eq!(line, "RESOLVED /xdg/state/opencrayast", "got: {line}");
}

/// The documented fallback, and the one ARCHITECTURE.md "State on disk" has always named.
#[test]
fn without_xdg_state_home_it_is_home_local_state() {
    let line = resolve_with(&[("HOME", "/home/somebody")]);
    assert_eq!(
        line, "RESOLVED /home/somebody/.local/state/opencrayast",
        "got: {line}"
    );
}

/// XDG section 3 calls a relative `XDG_STATE_HOME` "not a valid path": it must be **ignored**,
/// not resolved against the working directory. If it were resolved, state would land wherever
/// the tool happened to be run from.
#[test]
fn a_relative_xdg_state_home_is_ignored_rather_than_resolved() {
    let line = resolve_with(&[
        ("XDG_STATE_HOME", "relative/state"),
        ("HOME", "/home/somebody"),
    ]);
    assert_eq!(
        line, "RESOLVED /home/somebody/.local/state/opencrayast",
        "a relative XDG_STATE_HOME must not win: {line}"
    );
}

/// Neither variable set: a refusal naming the variable to set, and — the property that matters —
/// **no fallback to the workspace**, which is the thing being removed.
#[test]
fn no_home_and_no_xdg_is_refused_and_never_falls_back() {
    let line = resolve_with(&[]);
    assert!(
        line.starts_with("REFUSED io_error|"),
        "expected a refusal, got: {line}"
    );
    let next = line
        .split_once('|')
        .map(|(_, n)| n)
        .unwrap_or_default()
        .to_string();
    assert!(
        next.contains("XDG_STATE_HOME") && next.contains("HOME"),
        "the refusal must name the variables to set: {next}"
    );
    assert!(
        next.contains("never written into the workspace"),
        "the refusal must say the workspace is not a fallback: {next}"
    );
    assert!(
        !line.contains(".opencrayast"),
        "the refusal must not quote a path that could point back at the workspace: {line}"
    );
}

/// An empty `HOME` is as undetermined as an unset one: joining onto it would produce a relative
/// path that later resolves against whatever directory the tool ran from.
#[test]
fn an_empty_home_is_refused_rather_than_joined_onto() {
    let line = resolve_with(&[("HOME", "")]);
    assert!(
        line.starts_with("REFUSED io_error|"),
        "an empty HOME must not produce a relative path: {line}"
    );
}

/// The result carries no `ws-` segment, because the stores append it themselves. A resolver that
/// appended it here would make every store produce `ws-w-…/ws-w-…/`.
#[test]
fn the_result_carries_no_workspace_segment() {
    let line = resolve_with(&[("XDG_STATE_HOME", "/xdg/state")]);
    assert_eq!(line.matches("ws-").count(), 0, "got: {line}");
}

/// And the legacy dotfile name is never produced, which is the relocation itself.
#[test]
fn the_result_is_never_inside_a_workspace_dotfile() {
    let line = resolve_with(&[("XDG_STATE_HOME", "/xdg/state"), ("HOME", "/home/somebody")]);
    assert!(
        !line.contains("/.opencrayast"),
        "state must not resolve into the workspace's old dotfile: {line}"
    );
}
