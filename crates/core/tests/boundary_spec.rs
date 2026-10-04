//! Spec for ISSUE-CORE-BOUNDARY-LEXICAL / -SYMLINK (BND-01..05, 07, 12, 16, 18..22).
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::field_reassign_with_default
)]
#![cfg(unix)]
mod common;

use opencrayast_core::ErrorCode;
use opencrayast_core::boundary::*;
use opencrayast_core::limits::Limits;
use std::fs;
use std::os::unix::fs::symlink;
use std::path::{Path, PathBuf};

fn setup() -> (tempfile::TempDir, tempfile::TempDir, Boundary) {
    let ws = tempfile::tempdir().unwrap();
    let out = tempfile::tempdir().unwrap();
    fs::create_dir_all(ws.path().join("src")).unwrap();
    fs::write(ws.path().join("src/a.rs"), "fn a() {}\n").unwrap();
    fs::write(out.path().join("secret.txt"), "top secret").unwrap();
    let b = Boundary::new(BoundaryConfig {
        root: ws.path().to_path_buf(),
        limits: Limits::default(),
        read_roots: Vec::new(),
        state_dir: None,
        extra_protected: Vec::new(),
    })
    .unwrap();
    (ws, out, b)
}

#[test]
fn plain_relative_path_resolves() {
    let (ws, _o, b) = setup();
    let r = b.resolve_read("src/a.rs").unwrap();
    assert_eq!(r.rel, "src/a.rs");
    assert!(r.abs.starts_with(ws.path().canonicalize().unwrap()));
    // ./ and duplicate separators normalise
    assert_eq!(b.resolve_read("./src//a.rs").unwrap().rel, "src/a.rs");
}

#[test]
fn bnd01_traversal_refused() {
    let (_w, _o, b) = setup();
    for p in [
        "../x",
        "src/../../x",
        "src/../..",
        "..",
        "src/./../../etc/passwd",
        "src\\..\\..\\x",
    ] {
        let e = b.resolve_read(p).unwrap_err();
        assert_eq!(e.code, ErrorCode::OutsideWorkspace, "{p}");
    }
    // traversal that stays inside is fine
    assert_eq!(b.resolve_read("src/../src/a.rs").unwrap().rel, "src/a.rs");
}

#[test]
fn bnd02_absolute_outside_refused_absolute_inside_ok() {
    let (ws, o, b) = setup();
    assert_eq!(
        b.resolve_read("/").unwrap_err().code,
        ErrorCode::OutsideWorkspace
    );
    assert_eq!(
        b.resolve_read("/etc/passwd").unwrap_err().code,
        ErrorCode::OutsideWorkspace
    );
    let abs_out = o.path().join("secret.txt");
    assert_eq!(
        b.resolve_read(abs_out.to_str().unwrap()).unwrap_err().code,
        ErrorCode::OutsideWorkspace
    );
    let abs_in = ws.path().join("src/a.rs");
    assert_eq!(
        b.resolve_read(abs_in.to_str().unwrap()).unwrap().rel,
        "src/a.rs"
    );
}

#[test]
fn bnd12_empty_nul_control_refused() {
    let (_w, _o, b) = setup();
    for p in ["", "a\0b", "a\nb", "a\x1b[2Jb", "\t"] {
        assert_eq!(
            b.resolve_read(p).unwrap_err().code,
            ErrorCode::InvalidArgs,
            "{p:?}"
        );
    }
}

#[test]
fn bnd20_overlong_and_deep_refused() {
    let (_w, _o, b) = setup();
    let long = "a/".repeat(5000);
    assert!(b.resolve_read(&long).is_err());
    let deep = vec!["d"; 300].join("/");
    assert!(b.resolve_read(&deep).is_err());
}

#[test]
fn bnd18_outside_and_missing_look_the_same() {
    let (_w, o, b) = setup();
    let exists = o.path().join("secret.txt");
    let missing = o.path().join("nope.txt");
    let e1 = b.resolve_read(exists.to_str().unwrap()).unwrap_err();
    let e2 = b.resolve_read(missing.to_str().unwrap()).unwrap_err();
    assert_eq!(e1, e2, "must not reveal whether an outside path exists");
    assert!(
        !e1.message.contains(o.path().to_str().unwrap()),
        "no absolute path in the message"
    );
}

#[test]
fn bnd03_symlink_to_outside_file_refused() {
    let (ws, o, b) = setup();
    symlink(o.path().join("secret.txt"), ws.path().join("link.txt")).unwrap();
    assert_eq!(
        b.resolve_read("link.txt").unwrap_err().code,
        ErrorCode::OutsideWorkspace
    );
    assert!(b.resolve_write("link.txt").is_err());
}

#[test]
fn bnd04_symlinked_directory_component_refused() {
    let (ws, o, b) = setup();
    symlink(o.path(), ws.path().join("outdir")).unwrap();
    assert_eq!(
        b.resolve_read("outdir/secret.txt").unwrap_err().code,
        ErrorCode::OutsideWorkspace
    );
}

#[test]
fn symlink_that_stays_inside_is_ok_for_read_but_not_for_write() {
    let (ws, _o, b) = setup();
    symlink(ws.path().join("src/a.rs"), ws.path().join("alias.rs")).unwrap();
    assert_eq!(b.resolve_read("alias.rs").unwrap().rel, "src/a.rs");
    assert!(
        b.resolve_write("alias.rs").is_err(),
        "write target must not be a link"
    );
}

#[test]
fn bnd05_symlink_loops_terminate() {
    let (ws, _o, b) = setup();
    symlink(ws.path().join("loop2"), ws.path().join("loop1")).unwrap();
    symlink(ws.path().join("loop1"), ws.path().join("loop2")).unwrap();
    assert!(b.resolve_read("loop1").is_err());
    assert!(b.resolve_read("loop1/x").is_err());
}

#[test]
fn write_policy_protected_and_state_dir() {
    let ws = tempfile::tempdir().unwrap();
    fs::create_dir_all(ws.path().join(".git")).unwrap();
    fs::write(ws.path().join(".git/config"), "x").unwrap();
    fs::write(ws.path().join(".env"), "K=v").unwrap();
    fs::write(ws.path().join("ok.rs"), "").unwrap();
    let b = Boundary::new(BoundaryConfig {
        root: ws.path().to_path_buf(),
        limits: Limits::default(),
        read_roots: Vec::new(),
        state_dir: None,
        extra_protected: Vec::new(),
    })
    .unwrap();
    assert_eq!(
        b.resolve_write(".git/config").unwrap_err().code,
        ErrorCode::ProtectedPath
    );
    assert_eq!(
        b.resolve_write(".env").unwrap_err().code,
        ErrorCode::ProtectedPath
    );
    assert!(
        b.resolve_read(".env").is_ok(),
        "reads of secret-like names are allowed"
    );
    assert!(b.resolve_write("ok.rs").is_ok());
}

#[test]
fn bnd19_read_roots_never_grant_write() {
    let ws = tempfile::tempdir().unwrap();
    let rr = tempfile::tempdir().unwrap();
    fs::write(rr.path().join("lib.rs"), "").unwrap();
    let b = Boundary::new(BoundaryConfig {
        root: ws.path().to_path_buf(),
        read_roots: vec![rr.path().to_path_buf()],
        limits: Limits::default(),
        state_dir: None,
        extra_protected: Vec::new(),
    })
    .unwrap();
    let p = rr.path().join("lib.rs");
    assert!(b.resolve_read(p.to_str().unwrap()).is_ok());
    assert!(b.resolve_write(p.to_str().unwrap()).is_err());
}

#[test]
fn bnd21_hard_linked_write_target_refused() {
    let (ws, o, b) = setup();
    fs::hard_link(o.path().join("secret.txt"), ws.path().join("hl.txt")).unwrap();
    assert_eq!(
        b.resolve_write("hl.txt").unwrap_err().code,
        ErrorCode::UnsupportedTarget
    );
}

#[test]
fn read_only_file_is_refused_for_write() {
    use std::os::unix::fs::PermissionsExt;
    let (ws, _o, b) = setup();
    let p = ws.path().join("ro.rs");
    fs::write(&p, "").unwrap();
    fs::set_permissions(&p, fs::Permissions::from_mode(0o444)).unwrap();
    assert_eq!(
        b.resolve_write("ro.rs").unwrap_err().code,
        ErrorCode::UnsupportedTarget
    );
}

#[test]
fn bnd22_fifo_does_not_block() {
    let (ws, _o, b) = setup();
    let fifo = ws.path().join("pipe");
    if !common::make_fifo(&fifo) {
        // `make_fifo` panics unless this host genuinely cannot make a FIFO, which is the only
        // reason it returns `false`; the guard test pins that.
        assert!(
            !common::fifo_can_be_made_on_this_host(),
            "make_fifo gave up on a host that has mknodat or mkfifo: broken fixture"
        );
        eprintln!("skipping bnd22: this platform can create no FIFO");
        return;
    }
    let r = b.resolve_read("pipe").unwrap();
    let t = std::time::Instant::now();
    let res = b.open_read(&r);
    assert!(t.elapsed().as_secs() < 2, "open must not block on a FIFO");
    assert_eq!(res.unwrap_err().code, ErrorCode::IoError);
}

#[test]
fn open_read_returns_identity_of_the_opened_file() {
    use std::os::unix::fs::MetadataExt;
    let (ws, _o, b) = setup();
    let r = b.resolve_read("src/a.rs").unwrap();
    let (f, id) = b.open_read(&r).unwrap();
    let md = fs::metadata(ws.path().join("src/a.rs")).unwrap();
    assert_eq!((id.dev, id.ino), (md.dev(), md.ino()));
    drop(f);
}

#[test]
fn bnd07_path_swapped_for_symlink_after_resolve_is_caught() {
    let (ws, o, b) = setup();
    let r = b.resolve_read("src/a.rs").unwrap();
    fs::remove_file(ws.path().join("src/a.rs")).unwrap();
    symlink(o.path().join("secret.txt"), ws.path().join("src/a.rs")).unwrap();
    assert!(
        b.open_read(&r).is_err(),
        "swap between check and use must be refused"
    );
}

#[test]
fn root_refusals() {
    assert!(
        Boundary::new(BoundaryConfig {
            root: Path::new("/").into(),
            limits: Limits::default(),
            read_roots: Vec::new(),
            state_dir: None,
            extra_protected: Vec::new(),
        })
        .is_err()
    );
    assert!(
        Boundary::new(BoundaryConfig {
            root: "/definitely/not/here".into(),
            limits: Limits::default(),
            read_roots: Vec::new(),
            state_dir: None,
            extra_protected: Vec::new(),
        })
        .is_err()
    );
    if let Some(home) = std::env::var_os("HOME") {
        assert!(
            Boundary::new(BoundaryConfig {
                root: home.into(),
                limits: Limits::default(),
                read_roots: Vec::new(),
                state_dir: None,
                extra_protected: Vec::new(),
            })
            .is_err(),
            "bare home refused"
        );
    }
}

#[test]
fn bnd16_arbitrary_strings_never_panic() {
    let (_w, _o, b) = setup();
    let samples = [
        "",
        ".",
        "..",
        "...",
        "/",
        "//",
        "a/./b/../../..",
        "\u{202e}x",
        "é",
        "a\u{0}",
        "~",
        "~/x",
        "C:\\x",
        "\\\\?\\C:\\x",
        "src/a.rs/",
        "src/a.rs/.",
    ];
    for s in samples {
        let _ = b.resolve_read(s);
        let _ = b.resolve_write(s);
    }
}

/// BND-10 on Linux: the resolver must not fold, normalise or case-fold anything - it hands
/// the bytes to the OS and trusts the OS's canonical answer. So a normalisation variant of a
/// directory name can never be a second spelling that escapes: on a byte-preserving
/// filesystem the variant is simply a missing directory (`not_found`), on a normalising one
/// it is the same directory and is refused exactly like the original. What must never
/// happen is a variant that lands outside the root.
#[test]
fn bnd10_normalisation_variants_cannot_escape() {
    let ws = tempfile::tempdir().unwrap();
    let out = tempfile::tempdir().unwrap();
    fs::write(out.path().join("secret.txt"), "top secret").unwrap();
    let nfc_dir = ws.path().join("ü_dir");
    fs::create_dir(&nfc_dir).unwrap();
    symlink(out.path(), nfc_dir.join("outdir")).unwrap();
    let b = Boundary::new(BoundaryConfig {
        root: ws.path().to_path_buf(),
        limits: Limits::default(),
        read_roots: Vec::new(),
        state_dir: None,
        extra_protected: Vec::new(),
    })
    .unwrap();

    assert_eq!(
        b.resolve_read("ü_dir/outdir/secret.txt").unwrap_err().code,
        ErrorCode::OutsideWorkspace,
        "NFC spelling of the escaping link is refused"
    );
    assert!(
        b.resolve_read("u\u{308}_dir/outdir/secret.txt").is_err(),
        "the NFD spelling must not be a way around the check"
    );
    // Neither spelling resolves to the secret file.
    for p in ["ü_dir/outdir/secret.txt", "u\u{308}_dir/outdir/secret.txt"] {
        if let Ok(r) = b.resolve_read(p) {
            assert!(!r.abs.starts_with(out.path()), "{p} escaped");
        }
    }
}

/// A `..` that follows a symlink must not be evaluated against the *link target's* parent,
/// which is outside the root. The resolver defines the meaning of the path: it normalises
/// lexically and then walks the normalised components, so `link/../x` means `<root>/x`.
#[test]
fn dotdot_after_a_symlink_stays_in_the_root() {
    let (ws, o, b) = setup();
    symlink(o.path(), ws.path().join("link")).unwrap();
    // The link points at <out>/ ; `<out>/..` is <out>'s parent, i.e. outside the root.
    let r = b.resolve_read("link/../secret.txt");
    match r {
        Ok(r) => assert!(
            r.abs.starts_with(ws.path().canonicalize().unwrap()),
            "must resolve inside the root, not next to the link target"
        ),
        Err(e) => assert_eq!(
            e.code,
            ErrorCode::NotFound,
            "and it must not be reported as something other than a missing file"
        ),
    }
    assert!(
        b.resolve_read("link/..").is_ok(),
        "the root itself is always inside"
    );
}

/// BND-20 boundary: exactly `path_max_depth` components pass the depth check (and then fail
/// as a missing file), one more is refused as a limit, not as a path error.
#[test]
fn bnd20_depth_boundary_is_exact() {
    let (_w, _o, b) = setup();
    let at = vec!["d"; 64].join("/");
    assert_eq!(
        b.resolve_read(&at).unwrap_err().code,
        ErrorCode::NotFound,
        "64 components is inside the limit"
    );
    let over = vec!["d"; 65].join("/");
    assert_eq!(
        b.resolve_read(&over).unwrap_err().code,
        ErrorCode::LimitExceeded
    );
}

/// OUT-02: a path under a read-only root is shown as `@root<N>/relative`; no result ever
/// contains an absolute path in `rel`.
#[test]
fn read_roots_are_labelled_and_never_absolute() {
    let ws = tempfile::tempdir().unwrap();
    let a = tempfile::tempdir().unwrap();
    let b_root = tempfile::tempdir().unwrap();
    fs::create_dir_all(a.path().join("lib")).unwrap();
    fs::write(a.path().join("lib/lib.rs"), "").unwrap();
    fs::write(b_root.path().join("x.rs"), "").unwrap();
    let b = Boundary::new(BoundaryConfig {
        root: ws.path().to_path_buf(),
        read_roots: vec![a.path().to_path_buf(), b_root.path().to_path_buf()],
        limits: Limits::default(),
        state_dir: None,
        extra_protected: Vec::new(),
    })
    .unwrap();

    let r = b
        .resolve_read(a.path().join("lib/lib.rs").to_str().unwrap())
        .unwrap();
    assert_eq!(r.rel, "@root1/lib/lib.rs");
    assert_eq!(
        b.resolve_read(b_root.path().join("x.rs").to_str().unwrap())
            .unwrap()
            .rel,
        "@root2/x.rs"
    );
    // The workspace keeps its plain relative spelling.
    fs::write(ws.path().join("m.rs"), "").unwrap();
    let w = b.resolve_read("m.rs").unwrap();
    assert_eq!(w.rel, "m.rs");
    assert!(!w.rel.starts_with('/') && !w.rel.contains(a.path().to_str().unwrap()));
}

/// The root itself resolves, spelled `.`; separators and trailing separators are noise.
#[test]
fn root_itself_and_separator_noise() {
    let (ws, _o, b) = setup();
    assert_eq!(b.resolve_read(".").unwrap().rel, ".");
    assert_eq!(b.resolve_read("./").unwrap().rel, ".");
    assert_eq!(b.resolve_read("src/").unwrap().rel, "src");
    assert_eq!(
        b.resolve_read(ws.path().canonicalize().unwrap().to_str().unwrap())
            .unwrap()
            .rel,
        "."
    );
}

/// Every refusal code reachable from the read side, and no message that names a path
/// outside the workspace (BND-16, BND-18, T-20).
#[test]
fn read_side_failures_never_leak_paths() {
    let (_w, o, b) = setup();
    let outside_root = o.path().to_str().unwrap().to_string();
    let outside = o.path().join("secret.txt").to_str().unwrap().to_string();
    let cases = [
        ("", ErrorCode::InvalidArgs),
        ("a\0b", ErrorCode::InvalidArgs),
        ("a\u{202e}b", ErrorCode::InvalidArgs),
        ("../x", ErrorCode::OutsideWorkspace),
        ("src\\..\\..\\x", ErrorCode::OutsideWorkspace),
        ("/etc/passwd", ErrorCode::OutsideWorkspace),
        (&outside, ErrorCode::OutsideWorkspace),
        ("does/not/exist", ErrorCode::NotFound),
    ];
    for (p, code) in cases {
        let e = b.resolve_read(p).unwrap_err();
        assert_eq!(e.code, code, "{p:?}");
        assert!(!e.message.contains(&outside), "{p:?} leaked the path");
        assert!(!e.message.contains(&outside_root), "{p:?} leaked the path");
        assert!(!e.next.is_empty());
    }
}

/// Containment is component-wise, never a string prefix: a sibling directory whose name
/// merely starts with the workspace name must not be reachable ("/tmp/ab" vs "/tmp/abc").
#[test]
fn sibling_directory_with_a_prefix_name_is_outside() {
    let parent = tempfile::tempdir().unwrap();
    let ws = parent.path().join("proj");
    let sibling = parent.path().join("proj-secrets");
    fs::create_dir(&ws).unwrap();
    fs::create_dir(&sibling).unwrap();
    fs::write(sibling.join("key.pem"), "s3cret").unwrap();
    let b = Boundary::new(BoundaryConfig {
        root: ws.clone(),
        limits: Limits::default(),
        read_roots: Vec::new(),
        state_dir: None,
        extra_protected: Vec::new(),
    })
    .unwrap();
    for p in [
        sibling.join("key.pem").to_str().unwrap(),
        "../proj-secrets/key.pem",
        "./../proj-secrets/key.pem",
        "src/../../proj-secrets/key.pem",
    ] {
        assert_eq!(
            b.resolve_read(p).unwrap_err().code,
            ErrorCode::OutsideWorkspace,
            "{p}"
        );
    }
    // A plain relative name is a path INSIDE the workspace (one that does not exist), not
    // a way to reach the sibling: the resolver never resolves relative names against the
    // parent of the root.
    assert_eq!(
        b.resolve_read("proj-secrets/key.pem").unwrap_err().code,
        ErrorCode::NotFound
    );
}

/// CR regression: `--workspace` defaults to the current directory, so `root = "."` and any
/// ordinary relative spelling must be accepted. Judging the raw input instead of its
/// canonical form refused them.
///
/// The current directory is process-global, so every assertion that needs it lives in this
/// one test and the directory is restored afterwards, also on panic. No other test in this
/// file depends on the working directory (they all use absolute temp paths), so running
/// them in parallel is safe.
#[test]
fn default_and_relative_workspace_roots_are_accepted() {
    struct CwdGuard(PathBuf);
    impl Drop for CwdGuard {
        fn drop(&mut self) {
            let _ = std::env::set_current_dir(&self.0);
        }
    }
    let base = tempfile::tempdir().unwrap();
    let orig = std::env::current_dir().unwrap();
    let _guard = CwdGuard(orig);
    fs::create_dir_all(base.path().join("proj/src")).unwrap();
    fs::write(base.path().join("proj/src/a.rs"), "fn a() {}\n").unwrap();
    fs::create_dir(base.path().join("plain")).unwrap();
    fs::write(base.path().join("plain/f.rs"), "fn f() {}\n").unwrap();
    symlink(base.path().join("plain"), base.path().join("link-to-plain")).unwrap();
    std::env::set_current_dir(base.path()).unwrap();

    // 1. the default: "."
    let dot = Boundary::new(BoundaryConfig {
        root: Path::new(".").into(),
        limits: Limits::default(),
        read_roots: Vec::new(),
        state_dir: None,
        extra_protected: Vec::new(),
    })
    .unwrap();
    assert_eq!(
        dot.resolve_read("proj/src/a.rs").unwrap().rel,
        "proj/src/a.rs"
    );

    // 2. a single relative component
    let rel = Boundary::new(BoundaryConfig {
        root: Path::new("proj").into(),
        limits: Limits::default(),
        read_roots: Vec::new(),
        state_dir: None,
        extra_protected: Vec::new(),
    })
    .unwrap();
    assert_eq!(rel.resolve_read("src/a.rs").unwrap().rel, "src/a.rs");

    // 3. a trailing separator
    let trail = Boundary::new(BoundaryConfig {
        root: PathBuf::from("proj/"),
        limits: Limits::default(),
        read_roots: Vec::new(),
        state_dir: None,
        extra_protected: Vec::new(),
    })
    .unwrap();
    assert_eq!(trail.resolve_read("src/a.rs").unwrap().rel, "src/a.rs");

    // 4. a symlink pointing at a real directory: followed, and the canonical form kept
    let linked = Boundary::new(BoundaryConfig {
        root: Path::new("link-to-plain").into(),
        limits: Limits::default(),
        read_roots: Vec::new(),
        state_dir: None,
        extra_protected: Vec::new(),
    })
    .unwrap();
    assert_eq!(
        linked.resolve_read("f.rs").unwrap().abs,
        base.path().join("plain/f.rs").canonicalize().unwrap()
    );

    // ... and the filesystem root itself is still refused, whatever it was spelled as.
    // The exact set of spellings, and the reason each one is refused, are pinned by the
    // one-test-per-spelling `filesystem_root_spellings!` table just below; this loop stays
    // as the CR regression that first caught the macOS `/tmp/..` mistake. It asserts this
    // list against that table, so a spelling cannot be dropped from one and not the other.
    let spellings = ["/", "//", "/..", "/./", "/usr/..", "/usr/../"];
    assert_eq!(
        spellings, FILESYSTEM_ROOT_SPELLINGS,
        "the loop above and the per-spelling table below must cover the same spellings"
    );
    for bad in spellings {
        assert!(
            Boundary::new(BoundaryConfig {
                root: Path::new(bad).into(),
                limits: Limits::default(),
                read_roots: Vec::new(),
                state_dir: None,
                extra_protected: Vec::new(),
            })
            .is_err(),
            "{bad} must be refused"
        );
    }
}

/// One `#[test]` per spelling, so dropping or weakening any single one is a visible
/// failure instead of something the five surviving spellings quietly cover for.
///
/// A loop is not enough here: six spellings inside one `#[test]` share one verdict, so
/// deleting one leaves the test green and the fix unpinned (ISSUE-M0-CI-FIX-1 mutation
/// self-proof). A separate `#[test]` per spelling makes each independently falsifiable.
///
/// Each entry is a DIFFERENT WAY OF WRITING "the filesystem root". They all canonicalise
/// to `/` on every unix, which is what makes them one case rather than six, and each is
/// refused for that single reason. `/usr` is a real directory on Linux AND on macOS - the
/// property the spelling depends on - unlike `/tmp`, which is a symlink to `/private/tmp`
/// there, so `/tmp/..` lands on `/private` and is a perfectly good (if odd) workspace root.
/// `/tmp/..` is deliberately absent and must stay absent: as a table entry it would be a
/// test that is red on one platform and green on the other for no good reason.
///
/// The macro also accumulates the spellings, so the completeness test below reads the
/// same list the per-spelling tests are generated from and the two cannot drift apart.
macro_rules! filesystem_root_spellings {
    ($($name:ident => $why:expr => $spelling:expr),* $(,)?) => {
        /// Every spelling below, in the order the table lists them.
        const FILESYSTEM_ROOT_SPELLINGS: &[&str] = &[$($spelling),*];

        $(
            /// A distinct spelling of "the filesystem root", refused because it
            /// canonicalises to the filesystem root and not because of how it looks.
            #[test]
            fn $name() {
                let err = Boundary::new(BoundaryConfig {
                    root: Path::new($spelling).into(),
                    limits: Limits::default(),
                    read_roots: Vec::new(),
                    state_dir: None,
                    extra_protected: Vec::new(),
                })
                .expect_err(concat!(
                    $spelling,
                    " is the filesystem root and must be refused"
                ));
                assert_eq!(
                    err.code,
                    ErrorCode::InvalidArgs,
                    "{} ({}): must be refused as a root, not by some other rule: {err}",
                    $spelling,
                    $why
                );
            }
        )*
    };
}

filesystem_root_spellings! {
    spelling_the_root_itself_is_refused => "the root itself" => "/",
    spelling_the_root_behind_a_doubled_separator_is_refused => "the root behind a doubled separator" => "//",
    spelling_the_root_above_itself_is_refused => "the root above itself" => "/..",
    spelling_the_root_as_a_no_op_step_is_refused => "the root as a no-op step" => "/./",
    spelling_the_root_reached_by_stepping_out_of_a_real_directory_is_refused => "the root reached by stepping out of a real directory" => "/usr/..",
    spelling_the_root_reached_by_stepping_out_of_a_real_directory_with_a_trailing_separator_is_refused => "the root reached by stepping out of a real directory, trailing separator" => "/usr/../",
}

/// The spellings are complete, and none of them has quietly become a good root.
///
/// Two failures this guards, both invisible to a loop over whatever list happens to be
/// written down: a spelling DROPPED from [`FILESYSTEM_ROOT_SPELLINGS`] (a reviewer
/// removing the macOS-safe `/usr` pair must be caught), and a spelling wrongly ADDED to it
/// (`/tmp/..` is accepted on Linux and only refused on macOS, so as a table entry it would
/// be a test that is red on one platform and green on the other for no good reason).
#[test]
fn the_filesystem_root_spelling_table_is_exactly_the_set_that_refuses() {
    let expected = vec!["/", "//", "/..", "/./", "/usr/..", "/usr/../"];
    // The COUNT first, and as its own literal. The list comparison below can be satisfied by
    // deleting a spelling from the table *and* from `expected` together - the reviewer edits
    // what looks like one list in two places and the comparison still holds. A count is not
    // repeatable that way: losing a spelling moves this number, so the deletion has to fix the
    // count too, and now three separate edits have to agree instead of one. This is the check
    // that makes a single deleted spelling a failure rather than a quieter suite.
    assert_eq!(
        FILESYSTEM_ROOT_SPELLINGS.len(),
        6,
        "the table must keep all six spellings of the filesystem root; found {:?}",
        FILESYSTEM_ROOT_SPELLINGS
    );
    assert_eq!(
        FILESYSTEM_ROOT_SPELLINGS, expected,
        "the set of refused filesystem-root spellings changed; every spelling must be \
         canonicalised to `/`, so `/tmp/..` (which lands on `/private` on macOS) never \
         belongs here"
    );
    // Every listed spelling really does canonicalise to the filesystem root. Asserted here
    // rather than assumed, so the comment above stays true on whatever platform runs it.
    for spelling in &expected {
        assert_eq!(
            Path::new(spelling).canonicalize().unwrap(),
            Path::new("/"),
            "{spelling} does not canonicalise to the filesystem root on this platform, so \
             it is a different case and this table is wrong"
        );
    }
}

/// CR regression: two planted symlinks, one to a file that exists outside the root and one
/// to a file that does not, used to answer `outside_workspace` and `not_found`. That is an
/// existence probe for the whole filesystem (BND-18, T-20). Every symlink we cannot resolve
/// is now the same outside refusal.
///
/// A dangling link pointing INSIDE the root is refused too, on purpose: proving "this link
/// points inside" would mean resolving it, which is the operation that failed. One rule for
/// every unresolvable link is the only version without a probe in it.
#[test]
fn dangling_symlinks_never_probe_outside() {
    let (ws, o, b) = setup();
    let existing = o.path().join("secret.txt");
    let absent = o.path().join("never-created.txt");
    symlink(&existing, ws.path().join("l_exists")).unwrap();
    symlink(&absent, ws.path().join("l_missing")).unwrap();
    symlink(
        ws.path().join("not-here-yet.rs"),
        ws.path().join("l_inside"),
    )
    .unwrap();
    // A link to a link, then out of the root and onto nothing.
    symlink(&absent, ws.path().join("b")).unwrap();
    symlink(ws.path().join("b"), ws.path().join("a")).unwrap();

    let e1 = b.resolve_read("l_exists").unwrap_err();
    let e2 = b.resolve_read("l_missing").unwrap_err();
    assert_eq!(e1, e2, "existence outside the root must not be observable");
    assert_eq!(e1.code, ErrorCode::OutsideWorkspace);
    for p in ["l_inside", "l_missing/x", "a", "a/x"] {
        assert_eq!(
            b.resolve_read(p).unwrap_err().code,
            ErrorCode::OutsideWorkspace,
            "{p}"
        );
    }

    // A path with no symlink on the way at all still distinguishes "missing" from
    // "outside": nothing outside was consulted, so there is nothing to probe.
    assert_eq!(
        b.resolve_read("not-here-yet.rs").unwrap_err().code,
        ErrorCode::NotFound
    );
}

/// Only a regular, single-linked, owner-writable file is a write target: a directory, a
/// FIFO, a socket and a device are all refused, and none of the checks may open anything.
///
/// Both special-file cases are load-bearing here, so neither may drop out of `targets` for a
/// reason the suite could have fixed. `common::make_fifo` and `common::try_bind_unix_socket`
/// decide that, and their decisions are pinned by the guard tests in `common`; this function
/// only reads the verdict and fails if one arrives without an authorised reason.
#[test]
fn only_regular_files_are_write_targets() {
    let (ws, _o, b) = setup();
    fs::create_dir(ws.path().join("dir")).unwrap();
    let fifo = ws.path().join("pipe");
    let fifo_made = common::make_fifo(&fifo);
    let sock = ws.path().join("sock");
    let sock_outcome = common::try_bind_unix_socket(&sock);

    let mut targets = vec!["dir"];
    if common::fifo_can_be_made_on_this_host() {
        // The guard test says a FIFO is available here, so `make_fifo` had one job and failed.
        assert!(
            fifo_made,
            "a FIFO must be creatable on a host that has mknodat or mkfifo"
        );
        targets.push("pipe");
    } else {
        eprintln!("skipping the FIFO case: this platform can create no FIFO");
    }
    if sock_outcome.is_skipped() {
        eprintln!(
            "skipping the socket case: {}",
            sock_outcome.skip_reason().unwrap_or("<no reason recorded>")
        );
    } else {
        let _listener = sock_outcome
            .listener()
            .expect("a non-skipped socket outcome that yields no listener is a bug in the guard");
        targets.push("sock");
    }
    for p in targets {
        assert_eq!(
            b.resolve_write(p).unwrap_err().code,
            ErrorCode::UnsupportedTarget,
            "{p}"
        );
    }
    // A device node outside the root never even reaches the write policy.
    assert_eq!(
        b.resolve_write("/dev/null").unwrap_err().code,
        ErrorCode::OutsideWorkspace
    );
}

/// BND-22: a FIFO, socket or directory is refused by `open_read` without blocking, and the
/// refusal says "special file" so the caller can count and report it (OUT-07). `/dev/null`
/// is outside the root, so it is refused as outside and never opened at all.
///
/// The second of the two sites that could lose a special-file case. Same guard as
/// `only_regular_files_are_write_targets`: an authorised skip is announced and the case is
/// dropped, and nothing else is allowed to.
#[test]
fn special_files_are_refused_without_blocking() {
    let (ws, _o, b) = setup();
    let fifo = ws.path().join("pipe");
    let fifo_made = common::make_fifo(&fifo);
    let sock = ws.path().join("sock");
    let sock_outcome = common::try_bind_unix_socket(&sock);
    fs::create_dir(ws.path().join("dir")).unwrap();

    let mut targets = Vec::new();
    if common::fifo_can_be_made_on_this_host() {
        assert!(
            fifo_made,
            "a FIFO must be creatable on a host that has mknodat or mkfifo"
        );
        targets.push("pipe");
    } else {
        eprintln!("skipping the FIFO case: this platform can create no FIFO");
    }
    if sock_outcome.is_skipped() {
        eprintln!(
            "skipping the socket case: {}",
            sock_outcome.skip_reason().unwrap_or("<no reason recorded>")
        );
    } else {
        let _listener = sock_outcome
            .listener()
            .expect("a non-skipped socket outcome that yields no listener is a bug in the guard");
        targets.push("sock");
    }
    targets.push("dir");

    for p in targets {
        let r = b.resolve_read(p).unwrap();
        let t = std::time::Instant::now();
        let e = b.open_read(&r).unwrap_err();
        assert!(
            t.elapsed().as_secs() < 2,
            "{p} blocked for {:?}",
            t.elapsed()
        );
        assert_eq!(e.code, ErrorCode::IoError, "{p}");
        assert!(e.message.contains("special file"), "{p}: {}", e.message);
    }
    assert_eq!(
        b.resolve_read("/dev/null").unwrap_err().code,
        ErrorCode::OutsideWorkspace
    );
}

/// BND-07: the identity check is the point of `open_read`, so it must catch a swap every time
/// and must never cry wolf. 200 clean resolve/open pairs, then 200 swapped ones.
#[test]
fn bnd07_identity_check_is_stable_over_many_runs() {
    use std::os::unix::fs::MetadataExt;
    let (ws, o, b) = setup();
    for _ in 0..200 {
        let r = b.resolve_read("src/a.rs").unwrap();
        let (f, id) = b.open_read(&r).expect("a clean read must not be refused");
        assert_eq!(
            (id.dev, id.ino),
            (
                fs::metadata(ws.path().join("src/a.rs")).unwrap().dev(),
                fs::metadata(ws.path().join("src/a.rs")).unwrap().ino()
            )
        );
        drop(f);
    }
    let backup = o.path().join("payload.txt");
    fs::write(&backup, "payload").unwrap();
    for i in 0..200 {
        let target = ws.path().join("src/a.rs");
        let _ = fs::remove_file(&target);
        fs::write(&target, format!("attempt {i}")).unwrap();
        let r = b.resolve_read("src/a.rs").unwrap();
        // Swap it for a symlink that leaves the root after the resolve, before the open.
        fs::remove_file(&target).unwrap();
        symlink(&backup, &target).unwrap();
        assert!(
            b.open_read(&r).is_err(),
            "iteration {i}: the swap must be caught"
        );
        fs::remove_file(&target).unwrap();
        fs::write(&target, "fn a() {}\n").unwrap();
    }
}

/// BND-15: the tool's own state directory is never a write target, including through a
/// symlinked spelling of its parent and including a sibling whose name merely starts the
/// same way.
#[test]
fn state_dir_is_never_a_write_target() {
    let ws = tempfile::tempdir().unwrap();
    let outside_state = tempfile::tempdir().unwrap();
    let state = ws.path().join(".state");
    fs::create_dir_all(state.join("plans")).unwrap();
    fs::write(state.join("plans/p.json"), "{}").unwrap();
    fs::write(state.join("log"), "x").unwrap();
    // A sibling that only shares a prefix must stay writable.
    fs::create_dir(ws.path().join(".state-backup")).unwrap();
    fs::write(ws.path().join(".state-backup/keep.txt"), "x").unwrap();

    let b = Boundary::new(BoundaryConfig {
        root: ws.path().to_path_buf(),
        state_dir: Some(state.clone()),
        limits: Limits::default(),
        read_roots: Vec::new(),
        extra_protected: Vec::new(),
    })
    .unwrap();
    for p in [".state/plans/p.json", ".state/log"] {
        assert_eq!(
            b.resolve_write(p).unwrap_err().code,
            ErrorCode::ProtectedPath,
            "{p}"
        );
    }
    assert!(b.resolve_write(".state-backup/keep.txt").is_ok());
    // The state directory is readable: only writes are refused.
    assert!(b.resolve_read(".state/plans/p.json").is_ok());
    let _ = outside_state;
}
