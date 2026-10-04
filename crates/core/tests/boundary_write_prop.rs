//! Adversarial / property tests for `Boundary::resolve_write` and `open_read`.
//!
//! Tester only: does not change `crates/core/src` in the committed tree. Mutation
//! self-proof (temporarily disabling a check) is done locally, then reverted.
//!
//! ## Observed error-code map for deliberate refuse fixtures
//!
//! | Input class | `resolve_write` code |
//! | --- | --- |
//! | `.git/**`, `.GIT/**`, `sub/.git` (file or dir)/**, `.env*`, `id_rsa*` | `protected_path` |
//! | file/dir symlink as final component; hard link (`nlink>1`); mode `0444`; FIFO; socket; directory | `unsupported_target` |
//! | path under configured `state_dir` | `protected_path` |
//! | path under a read-only root | `outside_workspace` |
//! | absolute / `../` outside the workspace | `outside_workspace` |
//! | missing path that lexically stays inside | `not_found` |
//!
//! Codes outside `{protected_path, unsupported_target, outside_workspace, not_found}`
//! on a deliberate refuse fixture are a test failure (no fuzzy `io_error` for write).
//! Random PRNG paths may additionally see `invalid_args` / `limit_exceeded` /
//! `io_error` when walking non-directory nodes (socket/FIFO); that is allowed only
//! outside the deliberate catalogue.
#![cfg(unix)]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::field_reassign_with_default
)]

mod common;

use opencrayast_core::ErrorCode;
use opencrayast_core::boundary::{Boundary, BoundaryConfig, FileIdentity, ResolvedPath};
use opencrayast_core::error::ToolError;
use opencrayast_core::limits::Limits;
use opencrayast_core::protected::is_protected;
use std::fs;
use std::os::unix::fs::{MetadataExt, PermissionsExt, symlink};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};
/// Create a FIFO at `path`; `false` when the case must be dropped, with the reason printed.
/// Delegates to `common`, where the skip decision is a pure function with its own guard test.
#[cfg(unix)]
fn make_fifo(path: &Path) -> bool {
    common::make_fifo(path)
}

/// Create a character device at `path`; false when the platform has no `mknodat` or the
/// capability is missing. `rustix` only exposes `mknodat` on Linux, so everywhere else this
/// is a documented skip rather than a weaker test.
#[cfg(unix)]
fn make_char_device(path: &Path, major: u32, minor: u32) -> bool {
    #[cfg(target_os = "linux")]
    {
        let c = std::ffi::CString::new(path.to_str().unwrap()).unwrap();
        rustix::fs::mknodat(
            rustix::fs::CWD,
            c.as_c_str(),
            rustix::fs::FileType::CharacterDevice,
            rustix::fs::Mode::from_raw_mode(0o600),
            rustix::fs::makedev(major, minor),
        )
        .is_ok()
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = (path, major, minor);
        false
    }
}

const PROP_ITERS: u32 = 10_000;
const CALL_TIMEOUT: Duration = Duration::from_secs(2);
const SEED_FIXED: u64 = 0x71e_c0de_0002;

struct XorShift64 {
    state: u64,
}

impl XorShift64 {
    fn new(seed: u64) -> Self {
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
        (self.next_u64() as usize) % n
    }
}

/// Paths that must be refused by `resolve_write`, with the allowed code set.
#[derive(Clone)]
struct RefuseCase {
    path: String,
    /// Human label for assertion messages.
    label: &'static str,
}

struct Fixture {
    _ws: tempfile::TempDir,
    _out: tempfile::TempDir,
    _read: tempfile::TempDir,
    _state: tempfile::TempDir,
    ws_canon: PathBuf,
    out_canon: PathBuf,
    read_canon: PathBuf,
    state_canon: PathBuf,
    /// Relative names usable as PRNG alphabet tokens.
    names: Vec<String>,
    /// Deliberate refuse catalogue.
    refuse: Vec<RefuseCase>,
    /// Known-good writable regular files (relative).
    good: Vec<String>,
    boundary: Arc<Boundary>,
}

fn allowed_refuse(code: ErrorCode) -> bool {
    matches!(
        code,
        ErrorCode::ProtectedPath
            | ErrorCode::UnsupportedTarget
            | ErrorCode::OutsideWorkspace
            | ErrorCode::NotFound
    )
}

fn setup_write_fixture() -> Fixture {
    let ws = tempfile::tempdir().unwrap();
    let out = tempfile::tempdir().unwrap();
    let read = tempfile::tempdir().unwrap();
    let state = tempfile::tempdir().unwrap();

    fs::create_dir_all(ws.path().join("src")).unwrap();
    fs::write(ws.path().join("src/ok.rs"), "fn ok() {}\n").unwrap();
    fs::write(ws.path().join("plain.txt"), "hi\n").unwrap();
    fs::write(ws.path().join("a b"), "space\n").unwrap();

    // VCS / secrets / case variants.
    fs::create_dir_all(ws.path().join(".git/objects")).unwrap();
    fs::write(ws.path().join(".git/config"), "[core]\n").unwrap();
    fs::create_dir_all(ws.path().join(".GIT")).unwrap();
    fs::write(ws.path().join(".GIT/config"), "cased\n").unwrap();
    fs::create_dir_all(ws.path().join("sub")).unwrap();
    fs::create_dir_all(ws.path().join("sub/.git")).unwrap();
    fs::write(ws.path().join("sub/.git/config"), "nested\n").unwrap();
    // `sub/.git` as a *file* in a sibling tree.
    fs::create_dir_all(ws.path().join("other")).unwrap();
    fs::write(ws.path().join("other/.git"), "gitfile\n").unwrap();
    fs::write(ws.path().join(".env"), "K=v\n").unwrap();
    fs::write(ws.path().join(".env.local"), "K=v\n").unwrap();
    fs::write(ws.path().join("id_rsa"), "KEY\n").unwrap();
    fs::write(ws.path().join("id_rsa.pub"), "PUB\n").unwrap();
    fs::write(ws.path().join("Id_Rsa"), "CASE\n").unwrap();

    // Symlinks: file → `.git/config`; dir → `.git`.
    symlink(
        ws.path().join(".git/config"),
        ws.path().join("link_git_config"),
    )
    .unwrap();
    symlink(ws.path().join(".git"), ws.path().join("dir_symlink")).unwrap();
    // Bypass-oriented: dir alias of workspace root.
    symlink(ws.path(), ws.path().join("ws_alias")).unwrap();

    // Hard-link pair inside the workspace.
    fs::write(ws.path().join("hl_a.rs"), "hl\n").unwrap();
    fs::hard_link(ws.path().join("hl_a.rs"), ws.path().join("hl_b.rs")).unwrap();

    // Read-only regular file.
    fs::write(ws.path().join("ro.rs"), "ro\n").unwrap();
    fs::set_permissions(ws.path().join("ro.rs"), fs::Permissions::from_mode(0o444)).unwrap();

    // Directory, FIFO, socket.
    fs::create_dir(ws.path().join("adir")).unwrap();
    let fifo = ws.path().join("pipe");
    let fifo_made = make_fifo(&fifo);
    if !fifo_made {
        eprintln!("skip: no FIFO could be created here; the FIFO cases are dropped");
    }
    let sock = ws.path().join("sock");
    // The path only has to EXIST for `resolve_write`; the file outlives the listener. An
    // authorised skip (a path this suite built that will not fit in `sun_path` here) is
    // announced by the helper and the socket cases are dropped with it; every other cause is
    // fatal. `socket_bind_plan` decides, and its guard test pins that decision.
    let sock_outcome = common::try_bind_unix_socket(&sock);
    let sock_made = !sock_outcome.is_skipped();
    if !sock_made {
        eprintln!(
            "skip: the socket cases are dropped - {}",
            sock_outcome.skip_reason().unwrap_or("<no reason recorded>")
        );
    }
    drop(sock_outcome.listener());

    // Outside + read root + state.
    fs::write(out.path().join("secret.txt"), "secret\n").unwrap();
    fs::create_dir_all(read.path().join("lib")).unwrap();
    fs::write(read.path().join("lib/lib.rs"), "pub fn x() {}\n").unwrap();
    fs::create_dir_all(state.path().join("plans")).unwrap();
    fs::write(state.path().join("plans/p.json"), "{}\n").unwrap();

    let refuse = vec![
        RefuseCase {
            path: ".git/config".into(),
            label: "dot-git-config",
        },
        RefuseCase {
            path: ".GIT/config".into(),
            label: "dot-GIT-config",
        },
        RefuseCase {
            path: "sub/.git/config".into(),
            label: "sub-dot-git-config",
        },
        RefuseCase {
            path: "other/.git".into(),
            label: "other-dot-git-file",
        },
        RefuseCase {
            path: ".env".into(),
            label: "dotenv",
        },
        RefuseCase {
            path: ".env.local".into(),
            label: "dotenv-local",
        },
        RefuseCase {
            path: "id_rsa".into(),
            label: "id_rsa",
        },
        RefuseCase {
            path: "Id_Rsa".into(),
            label: "Id_Rsa-case",
        },
        RefuseCase {
            path: "link_git_config".into(),
            label: "symlink-to-git-config",
        },
        RefuseCase {
            path: "dir_symlink/config".into(),
            label: "dir-symlink-git-config",
        },
        RefuseCase {
            path: "hl_a.rs".into(),
            label: "hardlink-a",
        },
        RefuseCase {
            path: "hl_b.rs".into(),
            label: "hardlink-b",
        },
        RefuseCase {
            path: "ro.rs".into(),
            label: "mode-0444",
        },
        RefuseCase {
            path: "adir".into(),
            label: "directory",
        },
        RefuseCase {
            path: "pipe".into(),
            label: "fifo",
        },
        RefuseCase {
            path: "sock".into(),
            label: "socket",
        },
        RefuseCase {
            path: "../x".into(),
            label: "dotdot-escape",
        },
        RefuseCase {
            path: "missing-nope.rs".into(),
            label: "missing-inside",
        },
    ];
    let refuse: Vec<RefuseCase> = if fifo_made {
        refuse
    } else {
        refuse.into_iter().filter(|c| c.path != "pipe").collect()
    };
    let refuse: Vec<RefuseCase> = if sock_made {
        refuse
    } else {
        refuse.into_iter().filter(|c| c.path != "sock").collect()
    };

    let names = vec![
        "src".into(),
        "ok.rs".into(),
        "plain.txt".into(),
        "a b".into(),
        ".git".into(),
        "config".into(),
        ".env".into(),
        "id_rsa".into(),
        "link_git_config".into(),
        "dir_symlink".into(),
        "hl_a.rs".into(),
        "ro.rs".into(),
        "adir".into(),
        "pipe".into(),
        "sock".into(),
        "ws_alias".into(),
    ];
    let names: Vec<String> = if fifo_made {
        names
    } else {
        names.into_iter().filter(|n| n != "pipe").collect()
    };
    let names: Vec<String> = if sock_made {
        names
    } else {
        names.into_iter().filter(|n| n != "sock").collect()
    };

    let good = vec!["src/ok.rs".into(), "plain.txt".into(), "a b".into()];

    let boundary = Boundary::new(BoundaryConfig {
        root: ws.path().to_path_buf(),
        read_roots: vec![read.path().to_path_buf()],
        state_dir: Some(state.path().to_path_buf()),
        limits: Limits::default(),
        extra_protected: Vec::new(),
    })
    .unwrap();

    Fixture {
        ws_canon: ws.path().canonicalize().unwrap(),
        out_canon: out.path().canonicalize().unwrap(),
        read_canon: read.path().canonicalize().unwrap(),
        state_canon: state.path().canonicalize().unwrap(),
        names,
        refuse,
        good,
        boundary: Arc::new(boundary),
        _ws: ws,
        _out: out,
        _read: read,
        _state: state,
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
        "é".into(),
        "\u{202e}".into(),
        "a".repeat(80),
    ];
    a.extend(fx.names.iter().cloned());
    a
}

fn generate_path(rng: &mut XorShift64, fx: &Fixture, alpha: &[String]) -> String {
    match rng.next_usize(6) {
        0 => {
            let n = rng.next_usize(5);
            if n == 0 {
                return ".".into();
            }
            (0..n)
                .map(|_| alpha[rng.next_usize(alpha.len())].as_str())
                .collect::<Vec<_>>()
                .join("/")
        }
        1 => fx.good[rng.next_usize(fx.good.len())].clone(),
        2 => fx.refuse[rng.next_usize(fx.refuse.len())].path.clone(),
        3 => join_abs(&fx.ws_canon, &generate_path(rng, fx, alpha)),
        4 => join_abs(&fx.out_canon, "secret.txt"),
        _ => join_abs(&fx.read_canon, "lib/lib.rs"),
    }
}

fn join_abs(root: &Path, suffix: &str) -> String {
    if suffix.starts_with('/') {
        format!("{}{}", root.display(), suffix)
    } else if suffix == "." || suffix.is_empty() {
        root.to_string_lossy().into_owned()
    } else {
        root.join(suffix).to_string_lossy().into_owned()
    }
}

fn call_write(b: &Arc<Boundary>, path: &str) -> Result<Result<ResolvedPath, ToolError>, String> {
    let b2 = Arc::clone(b);
    let path_owned = path.to_string();
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            b2.resolve_write(&path_owned)
        }));
        let _ = tx.send(outcome);
    });
    match rx.recv_timeout(CALL_TIMEOUT) {
        Ok(Ok(r)) => Ok(r),
        Ok(Err(_)) => Err("resolve_write panicked".into()),
        Err(_) => Err("resolve_write exceeded 2s timeout".into()),
    }
}

fn check_ok_write(fx: &Fixture, path: &str, r: &ResolvedPath) -> Result<(), String> {
    if !r.abs.starts_with(&fx.ws_canon) {
        return Err(format!(
            "Ok abs not under workspace: {:?} from {path:?}",
            r.abs
        ));
    }
    if r.abs.starts_with(&fx.read_canon) && r.abs != fx.ws_canon {
        // read root is separate; write Ok must not land there.
        if !r.abs.starts_with(&fx.ws_canon) {
            return Err(format!("Ok under read_root: {:?}", r.abs));
        }
    }
    if r.abs.starts_with(&fx.state_canon) {
        return Err(format!("Ok under state_dir: {:?}", r.abs));
    }

    let meta = fs::symlink_metadata(&r.abs).map_err(|e| format!("lstat: {e}"))?;
    if meta.file_type().is_symlink() {
        return Err(format!("Ok target is a symlink: {:?}", r.abs));
    }
    if !meta.file_type().is_file() {
        return Err(format!("Ok target is not a regular file: {:?}", r.abs));
    }
    if meta.nlink() != 1 {
        return Err(format!("Ok target nlink={} path={:?}", meta.nlink(), r.abs));
    }
    if meta.mode() & 0o200 == 0 {
        return Err(format!("Ok target not owner-writable: {:?}", r.abs));
    }

    let rel = r
        .abs
        .strip_prefix(&fx.ws_canon)
        .map_err(|_| format!("strip_prefix failed for {:?}", r.abs))?;
    if is_protected(rel, &[]) {
        return Err(format!("Ok target is_protected: rel={rel:?}"));
    }
    Ok(())
}

fn check_one_write(fx: &Fixture, path: &str) -> Result<(), String> {
    let r1 = call_write(&fx.boundary, path)?;
    let r2 = call_write(&fx.boundary, path)?;
    match (&r1, &r2) {
        (Ok(a), Ok(b)) if a == b => {}
        (Err(a), Err(b)) if a == b => {}
        _ => {
            return Err(format!(
                "non-deterministic write resolve:\n  {r1:?}\n  {r2:?}"
            ));
        }
    }
    match r1 {
        Ok(ref r) => check_ok_write(fx, path, r)?,
        Err(ref e) => {
            // Random garbage may be InvalidArgs / LimitExceeded; that is fine.
            if matches!(
                e.code,
                ErrorCode::ProtectedPath
                    | ErrorCode::UnsupportedTarget
                    | ErrorCode::OutsideWorkspace
                    | ErrorCode::NotFound
                    | ErrorCode::InvalidArgs
                    | ErrorCode::LimitExceeded
                    // Walking through a socket/FIFO/dir-as-file can yield inspect failures on
                    // random garbage; the deliberate refuse catalogue still forbids this code.
                    | ErrorCode::IoError
            ) {
                // ok
            } else {
                return Err(format!(
                    "unexpected error code {:?} for {path:?}: {e}",
                    e.code
                ));
            }
        }
    }
    Ok(())
}

#[test]
fn write_prop_random_paths() {
    let seed = SEED_FIXED;
    let fx = setup_write_fixture();
    let alpha = alphabet(&fx);
    let mut rng = XorShift64::new(seed);
    for i in 0..PROP_ITERS {
        let path = generate_path(&mut rng, &fx, &alpha);
        if let Err(msg) = check_one_write(&fx, &path) {
            panic!("write property failure i={i} seed={seed:#x} path={path:?}: {msg}");
        }
    }
}

#[test]
fn deliberate_refuse_catalogue_uses_expected_codes() {
    let fx = setup_write_fixture();
    // Absolute outside / read_root / state_dir added here so paths are absolute.
    let mut cases = fx.refuse.clone();
    cases.push(RefuseCase {
        path: fx
            .out_canon
            .join("secret.txt")
            .to_string_lossy()
            .into_owned(),
        label: "abs-outside",
    });
    cases.push(RefuseCase {
        path: fx
            .read_canon
            .join("lib/lib.rs")
            .to_string_lossy()
            .into_owned(),
        label: "read-root-file",
    });
    cases.push(RefuseCase {
        path: fx
            .state_canon
            .join("plans/p.json")
            .to_string_lossy()
            .into_owned(),
        label: "state-dir-file",
    });

    for c in &cases {
        let r = call_write(&fx.boundary, &c.path)
            .unwrap_or_else(|e| panic!("{} timed/panic: {e}", c.label));
        match r {
            Ok(ok) => panic!("{} must be refused, got Ok({ok:?})", c.label),
            Err(e) => assert!(
                allowed_refuse(e.code),
                "{}: unexpected code {:?} ({e})",
                c.label,
                e.code
            ),
        }
    }
}

#[test]
fn protected_bypass_spellings_all_refused() {
    let fx = setup_write_fixture();
    let bypasses = [
        "dir_symlink/config",
        "dir_symlink/.git/config", // if alias points at .git, extra component may 404 — still refuse
        "./.git/./config",
        ".git//config",
        ".git/config/",
        ".GIT/config",
        ".git/CONFIG", // may be NotFound on case-sensitive FS; still must not be Ok write
        "src/../.git/config",
        "ws_alias/.git/config",
        ".git/config/.",
    ];
    for p in bypasses {
        let r = call_write(&fx.boundary, p).unwrap_or_else(|e| panic!("{p}: {e}"));
        match r {
            Ok(ok) => panic!("bypass {p:?} must be refused, got Ok({ok:?})"),
            Err(e) => assert!(
                allowed_refuse(e.code),
                "bypass {p:?}: unexpected code {:?} ({e})",
                e.code
            ),
        }
    }
}

#[test]
fn known_good_writes_ok() {
    let fx = setup_write_fixture();
    for p in &fx.good {
        let r = call_write(&fx.boundary, p)
            .unwrap_or_else(|e| panic!("{p}: {e}"))
            .unwrap_or_else(|e| panic!("{p}: expected Ok, got {e}"));
        check_ok_write(&fx, p, &r).unwrap_or_else(|e| panic!("{p}: {e}"));
    }
}

#[test]
fn open_read_special_files_return_within_two_seconds() {
    let fx = setup_write_fixture();
    // Re-bind socket (file may remain from setup).
    let sock = fx.ws_canon.join("sock");
    let _ = fs::remove_file(&sock);
    let sock_outcome = common::try_bind_unix_socket(&sock);

    let mut special = vec!["adir"];
    if sock_outcome.is_skipped() {
        eprintln!(
            "skip: the socket open_read case is dropped - {}",
            sock_outcome.skip_reason().unwrap_or("<no reason recorded>")
        );
    } else {
        let _listener = sock_outcome
            .listener()
            .expect("a non-skipped socket outcome that yields no listener is a bug in the guard");
        special.push("sock");
    }
    if ws_has_fifo(&fx) {
        special.push("pipe");
    } else {
        eprintln!("skip: no FIFO in the fixture; the FIFO open_read case is dropped");
    }
    special.sort_unstable();
    for p in special {
        let resolved = fx
            .boundary
            .resolve_read(p)
            .unwrap_or_else(|e| panic!("resolve_read {p}: {e}"));
        let t = Instant::now();
        let err = fx.boundary.open_read(&resolved).unwrap_err();
        assert!(
            t.elapsed() < CALL_TIMEOUT,
            "{p} blocked for {:?}",
            t.elapsed()
        );
        assert_eq!(err.code, ErrorCode::IoError, "{p}");
        assert!(err.message.contains("special file"), "{p}: {}", err.message);
    }

    // Device node: needs `mknodat` (Linux) and the capability. Anywhere else this is a
    // documented skip, not a weaker assertion.
    let dev = fx.ws_canon.join("devnullish");
    if make_char_device(&dev, 1, 3) {
        let resolved = fx.boundary.resolve_read("devnullish").unwrap();
        let t = Instant::now();
        let err = fx.boundary.open_read(&resolved).unwrap_err();
        assert!(t.elapsed() < CALL_TIMEOUT);
        assert_eq!(err.code, ErrorCode::IoError);
    } else {
        eprintln!(
            "skip: no character device could be created here (no mknodat or no capability); \
             device open_read not exercised"
        );
    }
}

/// Whether the fixture actually contains the FIFO (it may not on a platform without
/// `mknodat`/`mkfifo`).
fn ws_has_fifo(fx: &Fixture) -> bool {
    let md = fs::symlink_metadata(fx.ws_canon.join("pipe"));
    use std::os::unix::fs::FileTypeExt;
    md.is_ok_and(|m| m.file_type().is_fifo())
}

#[test]
fn open_read_identity_matches_fstat() {
    let fx = setup_write_fixture();
    let r = fx.boundary.resolve_read("src/ok.rs").unwrap();
    let (f, id) = fx.boundary.open_read(&r).unwrap();
    let md = fs::metadata(fx.ws_canon.join("src/ok.rs")).unwrap();
    assert_eq!(
        id,
        FileIdentity {
            dev: md.dev(),
            ino: md.ino()
        }
    );
    drop(f);
}

#[test]
fn open_read_rejects_symlink_swap_after_resolve() {
    let fx = setup_write_fixture();
    let target = fx.ws_canon.join("src/ok.rs");
    let r = fx.boundary.resolve_read("src/ok.rs").unwrap();
    fs::remove_file(&target).unwrap();
    symlink(fx.out_canon.join("secret.txt"), &target).unwrap();
    assert!(
        fx.boundary.open_read(&r).is_err(),
        "post-resolve symlink swap must be refused"
    );
    // Restore for other tests sharing nothing — fixture is local.
}

#[test]
fn open_read_deterministic_on_regular_file() {
    let fx = setup_write_fixture();
    // Recreate ok.rs in case a prior test in this process mutated a shared path — this
    // fixture is fresh per test function.
    let r = fx.boundary.resolve_read("plain.txt").unwrap();
    let a = fx.boundary.open_read(&r).unwrap();
    let b = fx.boundary.open_read(&r).unwrap();
    assert_eq!(a.1, b.1);
    drop(a.0);
    drop(b.0);
}
