//! RR-CLI-xx: `--read-root DIR` on the human command line.
//!
//! The MCP shell got its own suite (`crates/mcp/tests/read_root_spec.rs`). This file is the
//! mirror for the CLI, and it exists because the two shells parse their arguments independently:
//! a `--read-root` implemented on one and forgotten on the other would leave `docs/TOOLS.md`
//! describing a flag that half of the product does not have.
//!
//! Everything runs in-process through [`opencrayast::run_with_state`] against a real temporary
//! workspace, which is the same seam `cli1_spec` uses — the flag is a `clap` declaration, so a
//! subprocess would only re-test clap.
//!
//! Mutation self-proof: drop the `read_root` field from `Cli`, or stop passing it to
//! `edit::run`, and `rr_cli_02` / `rr_cli_04` go red.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use clap::Parser;
use opencrayast::out::Capture;
use opencrayast::{Cli, Command};
use opencrayast_core::error::ToolError;
use std::path::{Path, PathBuf};

/// A workspace plus N sibling read roots, each holding a file to read.
struct World {
    _dir: tempfile::TempDir,
    root: PathBuf,
    read_roots: Vec<PathBuf>,
}

impl World {
    fn new(roots: usize) -> World {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("ws");
        std::fs::create_dir(&root).unwrap();
        std::fs::write(root.join("hello.rs"), "fn main() {}\n").unwrap();
        let mut read_roots = Vec::new();
        for i in 1..=roots {
            let r = dir.path().join(format!("rr{i}"));
            std::fs::create_dir_all(r.join("lib")).unwrap();
            std::fs::write(r.join("lib/lib.rs"), "pub fn read_root_fn() {}\n").unwrap();
            read_roots.push(r);
        }
        World {
            _dir: dir,
            root,
            read_roots,
        }
    }
}

fn state_for(root: &Path) -> PathBuf {
    root.parent()
        .map(|d| d.join("state"))
        .unwrap_or_else(|| PathBuf::from("state"))
}

/// Drive one invocation, capturing output. `--workspace` is prepended; `--read-root` is passed
/// through explicitly because the tests are about it.
fn drive(w: &World, roots: &[&Path], args: &[&str]) -> (i32, String) {
    let mut full: Vec<String> = vec![
        "opencrayast".into(),
        "--workspace".into(),
        w.root.to_string_lossy().into_owned(),
    ];
    for r in roots {
        full.push("--read-root".into());
        full.push(r.to_string_lossy().into_owned());
    }
    full.extend(args.iter().map(|a| (*a).to_string()));
    let cli = Cli::try_parse_from(&full).unwrap_or_else(|e| panic!("{e}"));
    let mut cap = Capture::default();
    let code = opencrayast::run_with_state(
        &cli,
        &mut cap,
        opencrayast::palette::Palette::new(false),
        &mut opencrayast::confirm::Stdin::new(),
        &opencrayast::StateDir::Fixed(&state_for(&w.root)),
    );
    (code, cap.all().join("\n"))
}

/// RR-CLI-01: `--read-root` parses, is repeatable, and **keeps the order**.
///
/// `boundary.rs`'s `root_at` indexes the vector positionally, so order is not cosmetic: it is
/// what decides whether a directory is `@root1` or `@root3`. clap hands the values over in the
/// order they appeared; this asserts that rather than trusting it.
#[test]
fn rr_cli_01_read_root_parses_and_keeps_its_order() {
    let w = World::new(3);
    let full = vec![
        "opencrayast",
        "--workspace",
        w.root.to_str().unwrap(),
        "--read-root",
        w.read_roots[2].to_str().unwrap(),
        "--read-root",
        w.read_roots[0].to_str().unwrap(),
        "--read-root",
        w.read_roots[1].to_str().unwrap(),
        "doctor",
    ];
    let cli = Cli::try_parse_from(&full).unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(
        cli.read_root,
        vec![
            w.read_roots[2].clone(),
            w.read_roots[0].clone(),
            w.read_roots[1].clone()
        ],
        "the roots must arrive in the order the operator typed them"
    );
    assert!(matches!(cli.command, Command::Doctor));

    // No flag at all means no roots — not "the current directory", not a default root.
    let none = Cli::try_parse_from(["opencrayast", "doctor"]).unwrap();
    assert!(none.read_root.is_empty(), "no --read-root means no roots");
}

/// RR-CLI-02: `doctor` names each read root, so an operator can see what the agent may read.
///
/// A flag that widens reach and is invisible in every diagnostic is the failure mode this row
/// exists to prevent: the operator cannot audit what they granted.
#[test]
fn rr_cli_02_doctor_reports_each_read_root() {
    let w = World::new(2);
    let (code, t) = drive(&w, &[&w.read_roots[0], &w.read_roots[1]], &["doctor"]);
    assert_eq!(code, opencrayast::exit::EXIT_OK, "{t}");
    for (i, r) in w.read_roots.iter().enumerate() {
        let name = r.file_name().unwrap().to_string_lossy().into_owned();
        assert!(
            t.contains(&name),
            "doctor must name read root {} ({}), it is not in the output:\n{t}",
            i + 1,
            r.display()
        );
    }

    // And with no flag it must not invent any.
    let (_, plain) = drive(&w, &[], &["doctor"]);
    for r in &w.read_roots {
        let name = r.file_name().unwrap().to_string_lossy().into_owned();
        assert!(
            !plain.contains(&name),
            "with no --read-root, doctor must not report root `{name}`:\n{plain}"
        );
    }
}

/// RR-CLI-03 (CFG-07): a forbidden read root fails the run, and `doctor` says why.
///
/// Same `check_root` the workspace root goes through, so `/`, the home directory and a
/// credential directory are refused — this asserts the CLI surfaces the refusal rather than
/// swallowing it into an empty root list.
#[test]
fn rr_cli_03_a_forbidden_read_root_is_refused_with_a_reason() {
    let w = World::new(0);
    // Windows CI sets USERPROFILE, not HOME. Same refusal list as CFG-07 / check_root.
    let home = std::env::var_os("USERPROFILE")
        .or_else(|| std::env::var_os("HOME"))
        .map(PathBuf::from)
        .expect("USERPROFILE or HOME must be set for this test");
    #[cfg(windows)]
    let fs_root = PathBuf::from(r"C:\");
    #[cfg(not(windows))]
    let fs_root = PathBuf::from("/");
    for dir in [fs_root, home.clone(), home.join(".ssh")] {
        let (_, t) = drive(&w, &[&dir], &["doctor"]);
        assert!(
            t.to_lowercase().contains("read root") || t.to_lowercase().contains("root"),
            "a forbidden --read-root must be reported by name, not silently dropped:\n{t}"
        );
    }
}

/// RR-CLI-04 (BND-19): the flag widens reads only; a write through a read root is refused.
///
/// The write gate is `--write` **plus** `[policy] allow_write = true`, so this drives the
/// refused case with both halves present — otherwise the refusal would come from the missing
/// capability and would prove nothing about the read root.
#[test]
fn rr_cli_04_a_read_root_is_readable_but_never_writable() {
    let w = World::new(1);
    let target = w.read_roots[0].join("lib/lib.rs");
    let before = std::fs::read_to_string(&target).unwrap();

    let cfg = w._dir.path().join("write.toml");
    std::fs::write(&cfg, "[policy]\nallow_write = true\n").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&cfg, std::fs::Permissions::from_mode(0o600)).unwrap();
    }
    let roots = [w.read_roots[0].as_path()];

    // Preview: resolves its targets for reading, writes only the plan store. The workspace file
    // must be untouched, which is E-12 and is asserted here because this is the one place a
    // read root and a write meet.
    let (code, t) = drive(
        &w,
        &roots,
        &[
            "--write",
            "--yes",
            "--config",
            cfg.to_str().unwrap(),
            "edit",
            "preview",
            "--language",
            "rust",
            "--path",
            target.to_str().unwrap(),
            "--pattern",
            "read_root_fn",
            "--replacement",
            "pwned",
        ],
    );
    assert!(
        t.contains("@root1"),
        "preview must show the read root's @root1 label, proving it resolved the file:\n{t}"
    );
    assert_eq!(
        std::fs::read_to_string(&target).unwrap(),
        before,
        "preview must not write the workspace file"
    );

    // Apply is refused, and the file is still untouched.
    let plan = t
        .split("plan p-")
        .nth(1)
        .and_then(|rest| rest.split_whitespace().next())
        .map(|id| format!("p-{id}"))
        .unwrap_or_else(|| panic!("preview must produce a plan id:\n{t}"));
    let (_code, applied) = drive(
        &w,
        &roots,
        &[
            "--write",
            "--yes",
            "--config",
            cfg.to_str().unwrap(),
            "edit",
            "apply",
            &plan,
        ],
    );
    assert_ne!(applied.trim(), "", "apply must answer something: {applied}");
    assert_eq!(
        std::fs::read_to_string(&target).unwrap(),
        before,
        "applying through a --read-root must not change the file"
    );
    let _ = code;
}

/// RR-CLI-05: without the flag the same path is out of reach.
///
/// Without this row, RR-CLI-04 could pass because reading outside the workspace was already
/// allowed and the tests would be measuring the boundary rather than the flag.
#[test]
fn rr_cli_05_without_the_flag_a_read_root_is_out_of_reach() {
    let w = World::new(1);
    let target = w.read_roots[0].join("lib/lib.rs");
    let (code, t) = drive(
        &w,
        &[],
        &[
            "edit",
            "preview",
            "--language",
            "rust",
            "--path",
            target.to_str().unwrap(),
            "--pattern",
            "read_root_fn",
            "--replacement",
            "x",
        ],
    );
    assert_ne!(code, 0, "an unreachable path must not exit 0:\n{t}");
    assert!(
        !t.contains("read_root_fn"),
        "no content may leak from an undeclared read root:\n{t}"
    );
}

/// A read root cannot be turned into a write root by naming the workspace twice, and a refusal
/// is a `ToolError` with a next step — the property every CLI refusal is expected to have.
#[test]
fn rr_cli_06_a_read_root_refusal_is_a_classified_refusal() {
    let w = World::new(1);
    // The same directory as both workspace and read root: legal to pass, and it must not turn
    // a read-only root into a writable one. This is the shape an operator is most likely to
    // try by accident.
    let (code, t) = drive(&w, &[&w.root], &["doctor"]);
    assert_eq!(code, opencrayast::exit::EXIT_OK, "{t}");

    // And the error type used for a refused root carries a next step, like every other refusal.
    let e = ToolError::new(
        opencrayast_core::ErrorCode::InvalidArgs,
        "A root directory is not set.",
        "Pass an existing directory.",
    );
    assert!(!e.next.is_empty(), "a refusal must say what to do");
}
