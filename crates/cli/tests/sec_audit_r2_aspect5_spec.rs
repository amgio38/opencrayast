//! R2-A5-CLI-xx: aspect 5 (error-message leakage) on the **CLI** surface.
//!
//! SECURITY-MODEL T-19 protects "files outside the workspace" partly through the *absence of
//! information in errors*: a caller that can only name paths must not read back whether one
//! exists, what mode it has, or how many hard links point at it. `core` proves that at the
//! `Boundary` (SECFIX1-03, SECFIX1-04) and `crates/edit/tests/sec_audit_poc` proves it on the edit
//! path; `crates/mcp/tests/sec_audit_r2_aspect5_spec.rs` covers the MCP renderer.
//!
//! **This file covers the third renderer.** The CLI sanitises through its own `Op::diag` path with
//! its own palette, which is a different code path from the MCP shell's, and a core-level
//! guarantee does not survive two renderers on its own.
//!
//! The method: name a path that exists outside every root, and one that does not, and require the
//! two refusals to be **byte-identical**. Any difference is the oracle.
//!
//! Mutation self-proof: include the path in the refusal text → both tests go red.
// Unix-only: the fixture gives a file distinctive mode bits, read through `std::os::unix`.
// Without this the file does not compile on Windows, and CI builds a Windows leg — a test that
// cannot compile is not a passing test.
#![cfg(unix)]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::process::{Command, Stdio};

/// A workspace holding one file with deliberately distinctive metadata: mode 0600 and two hard
/// links, so a refusal that leaked either would be unmistakable in the text.
fn world() -> (tempfile::TempDir, std::path::PathBuf) {
    let d = tempfile::tempdir().unwrap();
    let ws = d.path().join("ws");
    std::fs::create_dir(&ws).unwrap();
    std::fs::write(ws.join("secret.rs"), "fn secret_symbol() {}\n").unwrap();
    std::fs::set_permissions(
        ws.join("secret.rs"),
        <std::fs::Permissions as std::os::unix::fs::PermissionsExt>::from_mode(0o600),
    )
    .unwrap();
    std::fs::hard_link(ws.join("secret.rs"), ws.join("second-link.rs")).unwrap();
    (d, ws)
}

// ---------------------------------------------------------------- CLI surface

/// Drive the real CLI binary through a pipe and capture both streams.
fn cli(args: &[&str]) -> (String, String) {
    let child = Command::new(env!("CARGO_BIN_EXE_opencrayast"))
        .env("NO_COLOR", "1")
        .env_remove("XDG_CONFIG_HOME")
        .args(args)
        .stdin(Stdio::null())
        .output()
        .unwrap();
    let out = String::from_utf8_lossy(&child.stdout).into_owned();
    let err = String::from_utf8_lossy(&child.stderr).into_owned();
    (out, err)
}

/// R2-A5-03: the CLI's refusal for a present and an absent outside path is byte-identical.
///
/// The CLI is a *different renderer* — `Op::diag` escapes through its own palette path — so the
/// core guarantee is not automatically inherited. This is the same assertion as R2-A5-01 on the
/// other surface.
#[test]
fn r2a5_03_cli_refusals_are_byte_identical_for_present_and_absent_paths() {
    let (d, ws) = world();
    let state = d.path().join("state");

    let present = "/etc/hostname".to_string();
    let (_o1, e1) = cli(&[
        "--workspace",
        ws.to_str().unwrap(),
        "--config",
        state.to_str().unwrap(),
        "edit",
        "preview",
        "--language",
        "rust",
        "--path",
        &present,
        "--pattern",
        "x",
        "--replacement",
        "y",
    ]);
    let absent = "/etc/no-such-file-4f2a9c".to_string();
    let (_o2, e2) = cli(&[
        "--workspace",
        ws.to_str().unwrap(),
        "--config",
        state.to_str().unwrap(),
        "edit",
        "preview",
        "--language",
        "rust",
        "--path",
        &absent,
        "--pattern",
        "x",
        "--replacement",
        "y",
    ]);

    assert!(
        !e1.is_empty() || !_o1.is_empty(),
        "the CLI must answer a preview request"
    );
    assert_eq!(
        e1, e2,
        "the CLI's refusal for {present} and for {absent} must be byte-identical; \
         stdout was {:?} / {:?}",
        _o1, _o2
    );
}

/// R2-A5-04: the CLI's refusal names the class and a next step, and nothing about the target.
///
/// `SECURITY-MODEL` T-19 is a claim about text as much as about behaviour. A useful refusal and
/// a non-leaking one are not in tension: the class and the next step are constant, the target is
/// not.
#[test]
fn r2a5_04_the_cli_refusal_is_useful_and_still_says_nothing_about_the_target() {
    let (_d, ws) = world();
    let outside = "/etc/hostname".to_string();
    let (out, err) = cli(&[
        "--workspace",
        ws.to_str().unwrap(),
        "edit",
        "preview",
        "--language",
        "rust",
        "--path",
        &outside,
        "--pattern",
        "x",
        "--replacement",
        "y",
    ]);
    let combined = format!("{out}{err}");

    assert!(
        combined.contains("outside") || combined.contains("workspace"),
        "the refusal must name the class of problem: {combined}"
    );
    assert!(
        combined.to_lowercase().contains("next"),
        "a refusal must carry a next step, like every other one in this product: {combined}"
    );
    assert!(
        !combined.contains(&outside),
        "the refusal must not echo the absolute path back: {combined}"
    );
}
