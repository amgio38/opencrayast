//! SEC-FIX 2 (F-01): the write path is HANDLE-RELATIVE, and the parent directory is proved again
//! immediately before the rename.
//!
//! # The finding this closes
//!
//! `SEC-AUDIT` F-01 (High, primitive layer): `Boundary::resolve_write` link-checks only the FINAL
//! component, and `fsio::atomic_replace` then worked from PATH STRINGS for milliseconds —
//! `target.parent().join(temp)`, `write_all`, `sync_all`, `set_permissions`, `listxattr` — before
//! its `rename`. The asymmetry was exact: the read path opened relative to a PINNED root
//! descriptor with `BENEATH | NO_SYMLINKS`; the write path did not.
//!
//! The attack: move the whole PARENT DIRECTORY out of the workspace and leave a symlink where it
//! was. The target inode travels with the directory, so it is still `nlink == 1`, the same
//! `dev`/`ino`, and not a link. Every per-file check passes. The content lands outside.
//!
//! # Why these tests live in `src/`, and not in `tests/`
//!
//! The seam that manufactures the window is crate-private on purpose: a dependent must not be able
//! to inject a callback into the write path. That is also why the write path has no path string to
//! hand the hook — since SEC-FIX 2 the only names it holds are a leaf and a root-relative parent
//! path, and the leaf is all a test needs. Tests drive the real `Boundary::replace_file`, not a
//! re-implementation of it, so there is no second copy of the sequence that could drift.
//!
//! # What each test pins
//!
//! | Test | Invariant |
//! |------|-----------|
//! | `a_parent_directory_moved_out_of_the_workspace_is_refused` | 2 (parent re-proof) + 3 |
//! | `the_write_path_never_resolves_a_path_string` | 1 (no path operation anywhere in the write) |
//! | `a_parent_swapped_for_another_directory_is_refused` | 2, the "other inode" arm |
//! | `an_unmoved_parent_is_accepted` | the check is not vacuously refusing everything |

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use crate::ErrorCode;
use crate::boundary::BoundaryConfig;
use crate::fsio::BeforeRename;
use crate::fsio::PropertyCopy;
use std::fs;
use std::os::unix::fs::symlink;
use std::path::{Path, PathBuf};

/// A temp workspace with its policy, plus an "outside" directory that is NOT inside it.
struct Ws {
    /// Kept alive for the test's duration; the temp dir must outlive every path in it.
    _dir: tempfile::TempDir,
    root: PathBuf,
    outside: PathBuf,
    boundary: crate::boundary::Boundary,
}

impl Ws {
    fn new() -> Ws {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("ws");
        fs::create_dir_all(&root).unwrap();
        let outside = dir.path().join("outside");
        fs::create_dir_all(&outside).unwrap();
        let mut cfg = BoundaryConfig::new(root.clone(), crate::limits::Limits::default());
        cfg.state_dir = Some(dir.path().join("state"));
        let boundary = crate::boundary::Boundary::new(cfg).unwrap();
        Ws {
            _dir: dir,
            root,
            outside,
            boundary,
        }
    }

    fn put(&self, rel: &str, content: &[u8]) {
        let p = self.root.join(rel);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(&p, content).unwrap();
    }

    fn abs(&self, rel: &str) -> PathBuf {
        self.root.join(rel)
    }

    fn resolved(&self, rel: &str) -> crate::boundary::ResolvedPath {
        self.boundary.resolve_write(rel).unwrap()
    }

    fn read_outside(&self, rel: &str) -> Vec<u8> {
        fs::read(self.outside.join(rel)).unwrap_or_else(|e| {
            panic!("the file the attacker moved outside must still be readable: {e:?}")
        })
    }

    /// Every temp file left anywhere under `dir`, at any depth.
    fn temp_leftovers(&self, dir: &Path) -> Vec<PathBuf> {
        let mut out = Vec::new();
        let mut stack = vec![dir.to_path_buf()];
        while let Some(d) = stack.pop() {
            let Ok(rd) = fs::read_dir(&d) else { continue };
            for e in rd.flatten() {
                let p = e.path();
                let is_dir = e.file_type().map(|t| t.is_dir()).unwrap_or(false);
                if is_dir {
                    stack.push(p);
                } else if p
                    .file_name()
                    .map(|n| n.to_string_lossy().starts_with(".opencrayast-tmp-"))
                    .unwrap_or(false)
                {
                    out.push(p);
                }
            }
        }
        out
    }
}

/// SECFIX2-01 (invariants 2 and 3): the parent directory is moved out of the workspace and
/// symlinked from its old place, inside the seam — after the write's own checks and immediately
/// before the rename. The write MUST be refused, and the file that is now outside MUST be
/// byte-for-byte unchanged.
///
/// This is the deterministic version of the PoC. It is not a race: the swap happens inside a
/// callback the production path invokes at a known point, so the state being tested is
/// manufactured exactly rather than hoped for. (The audit's own report says S-1 was downgraded
/// precisely because a racing test "passed 25 times in a row" — a test that cannot fail when the
/// guard is missing proves nothing.)
#[test]
fn a_parent_directory_moved_out_of_the_workspace_is_refused() {
    let ws = Ws::new();
    ws.put("sub/target.rs", b"let a = 1;\n");

    let resolved = ws.resolved("sub/target.rs");
    let (_h, identity) = ws.boundary.open_read(&resolved).unwrap();
    eprintln!(
        "verified: {} dev={} ino={}",
        resolved.rel, identity.dev, identity.ino
    );

    // The attack, staged inside the seam. `sub` — WITH the target inode still inside it — is
    // moved out of the workspace, and a symlink is left at the old path. Nothing about the target
    // changed: it is still nlink == 1, the same dev/ino, and not a link.
    let sub = ws.abs("sub");
    let stolen = ws.outside.join("sub");
    let seam = BeforeRename {
        hook: Some(&move |_leaf: &std::ffi::OsStr| {
            fs::rename(&sub, &stolen).unwrap();
            symlink(&stolen, &sub).unwrap();
        }),
    };

    let err = ws
        .boundary
        .replace_file_with_seam(
            &resolved,
            b"let a = 2;\n",
            Some(identity),
            &seam,
            &crate::fsio::PropertyCopy::default(),
        )
        .expect_err("a parent directory moved out of the workspace must be refused");

    eprintln!("refusal -> [{}] {}", err.code.as_str(), err.message);
    assert_eq!(
        err.code,
        ErrorCode::IoError,
        "a moved parent is an io_error, not a silent success: {err}"
    );
    // The whole point: the file outside the workspace still holds its ORIGINAL bytes.
    assert_eq!(
        ws.read_outside("sub/target.rs"),
        b"let a = 1;\n",
        "nothing may be written outside the workspace (S-1)"
    );
    assert!(
        ws.temp_leftovers(&ws.outside).is_empty(),
        "and no temp file may be left in the directory the target now lives in: {:?}",
        ws.temp_leftovers(&ws.outside)
    );
}

/// SECFIX2-02 (invariant 1): the write performs NO path-string operation on the target or its
/// parent. Asserted on the source, because that is the only thing that can be true of every
/// operation rather than of one call this test happens to make.
///
/// The write path is the set of functions `atomic_replace` actually runs, listed by name. Its
/// bodies are extracted and searched for the path-based spellings — each of which takes a path and
/// would re-resolve a name an attacker could have swapped:
///
/// - `std::fs::rename` (would be `renameat` from the pinned handle),
/// - `set_permissions` (would be `fchmod`),
/// - `std::fs::remove_file` (would be `unlinkat`),
/// - `rustix::fs::listxattr` / `getxattr` (would be `flistxattr` / `fgetxattr` on a handle),
/// - `symlink_metadata` (would be `statat` through the handle),
/// - `std::fs::metadata` (would be `fstat` on a handle).
///
/// The `f`-prefixed forms are NOT matches: those take a descriptor, which is the point. The test
/// checks the exact qualified path-based names for the same reason.
#[test]
fn the_write_path_never_resolves_a_path_string() {
    let src =
        fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("src/fsio.rs")).unwrap();

    // The functions `atomic_replace` reaches. `recheck_target` and `finalize_after_rename_for_test`
    // are deliberately NOT here: they are the documented path-based TEST seams (and `fsync_dir`
    // belongs to the state store, which has no boundary to pin a handle against).
    const WRITE_PATH: &[&str] = &[
        "atomic_replace",
        "atomic_replace_with_seam",
        "observe_identity",
        "atomic_replace_inner",
        "fstat_leaf",
        "write_and_replace",
        "recheck_leaf",
        "create_temp",
        "open_leaf_for_attrs",
        "read_target_xattrs",
        "list_xattrs",
        "copy_xattrs",
        "finalize_after_rename",
        "fsync_dir_handle",
    ];

    let forbidden = [
        "std::fs::rename(",
        "set_permissions(",
        "std::fs::remove_file(",
        "rustix::fs::listxattr(",
        "rustix::fs::getxattr(",
        "symlink_metadata(",
        "std::fs::metadata(",
    ];

    for name in WRITE_PATH {
        let body = fn_body(&src, name).unwrap_or_else(|| {
            panic!("the write path function `{name}` is gone; this test must be updated to say what replaced it")
        });
        // Strip line comments: prose about a path is not a path operation.
        let code: String = body
            .lines()
            .map(str::trim_start)
            .filter(|l| !l.starts_with("//"))
            .collect::<Vec<_>>()
            .join("\n");
        for needle in forbidden {
            let hits: Vec<&str> = code.lines().filter(|l| l.contains(needle)).collect();
            assert!(
                hits.is_empty(),
                "`{name}` must not use `{needle}` — it re-resolves a name an attacker could \
                 have swapped; the handle-relative form is required. found: {hits:?}"
            );
        }
    }

    // The positive half, on the whole write path at once: the handle-relative operations this fix
    // is about must actually be present. Without it, a `fsio.rs` that deleted the work and kept
    // only the policy would satisfy every assertion above.
    //
    // Each entry is (call, what it replaced):
    let required: &[(&str, &str)] = &[
        ("openat(", "path-based open of the target"),
        ("statat(", "symlink_metadata on the target"),
        ("fchmod(", "set_permissions on the temp path"),
        ("fchown(", "the path-based chown"),
        ("fsetxattr(", "setxattr on the temp path"),
        ("renameat(", "std::fs::rename(tmp, target)"),
        ("unlinkat(", "std::fs::remove_file(tmp)"),
    ];
    for (call, replaced) in required {
        assert!(
            src.contains(call),
            "the write path should use `{call}` ({replaced}); if the fix was reverted, say so \
             in this test rather than deleting it"
        );
    }

    // And the parent-directory re-proof must be CALLED from inside the write path, not merely defined.
    // Checked in `write_and_replace`'s own body: counting occurrences in the file would also pass if
    // the only mention were a doc comment or the definition itself.
    let war = fn_body(&src, "write_and_replace").expect("write_and_replace must still exist");
    assert!(
        war.contains("parent_still_at_root("),
        "write_and_replace must re-prove the parent directory immediately before the rename \
         (SEC-FIX 2, invariant 2)"
    );
    assert!(
        war.contains("recheck_leaf("),
        "write_and_replace must re-check the target through the handle immediately before the rename"
    );
    assert!(
        src.contains("open_write_parent("),
        "the pinned parent handle must be opened by the write path"
    );
}

/// The body of `fn <name>` in `src`, found by its signature line and matched braces.
///
/// Deliberately simple rather than a parser: this test wants to read the same code a reader would,
/// and a real parser would hide exactly the comment and brace cases it is meant to notice.
fn fn_body(src: &str, name: &str) -> Option<String> {
    let lines: Vec<&str> = src.lines().collect();
    let start = lines.iter().position(|l| {
        let l = l.trim();
        // Strip a visibility / `fn` prefix, then require the declared NAME to be exactly `name`
        // and to be followed by the parameter list. The name may carry generic parameters
        // (`fn copy_xattrs<Fd: AsFd>(..)`), so what is required is that the character after the
        // name is `<` or `(` — never a letter, which would be a longer identifier that merely
        // starts with this name.
        let rest = match l
            .strip_prefix("pub(crate) fn ")
            .or_else(|| l.strip_prefix("pub fn "))
            .or_else(|| l.strip_prefix("fn "))
        {
            Some(r) => r,
            None => return false,
        };
        let tail = match rest.strip_prefix(name) {
            Some(t) => t,
            None => return false,
        };
        tail.starts_with('(') || tail.starts_with('<')
    })?;
    // Include the preceding doc comments, so `fn_body` returns the whole declaration.
    let mut first = start;
    while first > 0 && lines[first - 1].trim_start().starts_with("///") {
        first -= 1;
    }
    let mut depth = 0i32;
    let mut opened = false;
    let mut out = String::new();
    for line in &lines[first..] {
        out.push_str(line);
        out.push('\n');
        for c in line.chars() {
            match c {
                '{' => {
                    depth += 1;
                    opened = true;
                }
                '}' => {
                    depth -= 1;
                    // A `}` at depth 0 before any `{` is a stray closer, not the end of the body:
                    // attributes like `#[cfg(unix)]` carry no braces and must not end the scan.
                    if opened && depth <= 0 {
                        return Some(out);
                    }
                }
                _ => {}
            }
        }
    }
    Some(out)
}

/// SECFIX2-03 (invariant 2, the "other inode" arm): the parent is not replaced by a SYMLINK but
/// swapped for a DIFFERENT real directory. A `dev`/`ino` comparison of the workspace entry is what
/// catches this; re-opening by name alone would not, because the name still resolves to a
/// directory.
#[test]
fn a_parent_swapped_for_another_directory_is_refused() {
    let ws = Ws::new();
    ws.put("sub/target.rs", b"let a = 1;\n");

    let resolved = ws.resolved("sub/target.rs");

    // Build the attacker's directory BEFORE the write, so the swap itself is one rename.
    let decoy = ws.outside.join("decoy");
    fs::create_dir_all(&decoy).unwrap();
    fs::write(decoy.join("target.rs"), b"decoy content\n").unwrap();

    let sub = ws.abs("sub");
    let parked = ws.outside.join("parked");
    let seam = BeforeRename {
        hook: Some(&move |_leaf: &std::ffi::OsStr| {
            // Move the real directory aside (inside the workspace this time — so nothing leaves
            // it), then put a DIFFERENT directory at the same name.
            fs::rename(&sub, &parked).unwrap();
            fs::rename(&decoy, &sub).unwrap();
        }),
    };

    let err = ws
        .boundary
        .replace_file_with_seam(
            &resolved,
            b"PWNED\n",
            None,
            &seam,
            &crate::fsio::PropertyCopy::default(),
        )
        .expect_err("a parent swapped for another directory must be refused");

    eprintln!("swapped parent -> [{}] {}", err.code.as_str(), err.message);
    assert_eq!(err.code, ErrorCode::IoError, "{err}");
    // The decoy's file is the one now at the original path; it must be untouched.
    assert_eq!(
        fs::read(ws.abs("sub/target.rs")).unwrap(),
        b"decoy content\n",
        "the file now at that name must not be written through"
    );
    assert!(
        ws.temp_leftovers(&ws.outside).is_empty(),
        "no temp file may be left behind: {:?}",
        ws.temp_leftovers(&ws.outside)
    );
}

/// SECFIX2-04: the parent check is not vacuously refusing everything — an ordinary write still
/// works. Without this, SECFIX2-01/03 would also pass if the guard simply rejected all writes.
#[test]
fn an_unmoved_parent_is_accepted() {
    let ws = Ws::new();
    ws.put("sub/target.rs", b"let a = 1;\n");
    let resolved = ws.resolved("sub/target.rs");

    ws.boundary
        .replace_file(&resolved, b"let a = 2;\n")
        .expect("an ordinary write with a parent that never moved must succeed");

    assert_eq!(fs::read(ws.abs("sub/target.rs")).unwrap(), b"let a = 2;\n");
    assert!(ws.temp_leftovers(&ws.root).is_empty());
}

/// SECFIX2-05: the write preserves the target's extended attributes through the handle-relative
/// path (ACLs and SELinux labels live in xattrs on Linux, per EDIT-MODEL "Preserving file
/// properties"). Skipped explicitly — never silently — when the filesystem cannot carry `user.*`.
#[test]
fn attributes_survive_a_handle_relative_replace() {
    let ws = Ws::new();
    ws.put("f.txt", b"original");

    // Probe in its OWN directory: a probe file inside the directory under test would be counted
    // by the leftover assertions and make them fail for the wrong reason.
    let probe_dir = tempfile::tempdir().unwrap();
    let probe = probe_dir.path().join("probe");
    let Ok(probe_file) = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&probe)
    else {
        eprintln!("SKIPPED: this filesystem refuses to create files at all");
        return;
    };
    use std::os::fd::AsFd;
    if rustix::fs::fsetxattr(
        probe_file.as_fd(),
        &b"user.opencrayprobe"[..],
        &[1u8][..],
        rustix::fs::XattrFlags::empty(),
    )
    .is_err()
    {
        eprintln!("SKIPPED: this filesystem refuses user.* xattrs (not tmpfs/ext4?)");
        return;
    }

    let target = ws.abs("f.txt");
    let file = fs::OpenOptions::new().write(true).open(&target).unwrap();
    rustix::fs::fsetxattr(
        file.as_fd(),
        &b"user.opencraytest"[..],
        b"preserved-value",
        rustix::fs::XattrFlags::empty(),
    )
    .unwrap();

    let resolved = ws.resolved("f.txt");
    ws.boundary
        .replace_file(&resolved, b"replaced")
        .expect("the replace must succeed on a filesystem that supports xattrs");

    // Read it back through a HANDLE, not by path: a single `fgetxattr` into an empty buffer
    // returns the size and fills nothing, which is a quiet way to compare against an empty value.
    let after = fs::OpenOptions::new().read(true).open(&target).unwrap();
    let mut value: Vec<u8> = Vec::new();
    let n = rustix::fs::fgetxattr(after.as_fd(), &b"user.opencraytest"[..], &mut value).unwrap();
    value.clear();
    value.resize(n, 0u8);
    let n = rustix::fs::fgetxattr(after.as_fd(), &b"user.opencraytest"[..], &mut value).unwrap();
    value.truncate(n);

    assert_eq!(
        value, b"preserved-value",
        "the extended attribute must survive the handle-relative replace"
    );
    assert_eq!(fs::read(&target).unwrap(), b"replaced");
}

/// ATOMIC-PROPS A: "preserve or refuse, never do our best".
///
/// The original test for this rule could not fail. It provoked a failed attribute copy with
/// `trusted.*`, which needs CAP_SYS_ADMIN to write and which root can copy anyway — so in a root
/// container (this one, `euid == 0`) it took the SUCCESS branch and the refusal was never
/// executed. Removing the refusal from the write path left the suite green, which is exactly the
/// failure this test now exists to make impossible.
///
/// `fsio::PropertyCopy` is the seam that allows it: the test supplies the failure directly, so the
/// refusal branch runs identically as root and as an unprivileged user, on every filesystem that
/// can hold an attribute at all.
#[test]
#[cfg(any(target_os = "linux", target_os = "macos"))]
fn an_attribute_that_cannot_be_copied_is_refused() {
    use std::os::fd::AsFd;

    let ws = Ws::new();
    ws.put("f.txt", b"old content");
    let target = ws.abs("f.txt");

    // `user.*` is writable by the owner without any capability, so this attribute really exists
    // and really has to be carried across — no early return, no skip.
    let f = fs::OpenOptions::new().write(true).open(&target).unwrap();
    rustix::fs::fsetxattr(
        f.as_fd(),
        &b"user.opencraytest"[..],
        b"preserved-value",
        rustix::fs::XattrFlags::empty(),
    )
    .expect("user.* xattrs are writable by the file owner on both Linux and macOS");
    drop(f);

    let seam = BeforeRename::default();
    // The injected failure: the temp file refuses the attribute.
    let attrs = PropertyCopy {
        hook: Some(&|_attrs: &[crate::fsio::Xattr]| Err(rustix::io::Errno::PERM)),
    };

    let resolved = ws.resolved("f.txt");
    let err = ws
        .boundary
        .replace_file_with_seam(&resolved, b"new content", None, &seam, &attrs)
        .expect_err("an attribute that cannot be copied must abort the whole replace");

    assert_eq!(
        err.code,
        ErrorCode::UnsupportedTarget,
        "the refusal is unsupported_target, not a bare io_error: {err}"
    );
    assert!(
        err.message
            .contains("Extended attributes cannot be preserved"),
        "the message must name the real reason: {}",
        err.message
    );
    assert_eq!(
        fs::read(&target).unwrap(),
        b"old content",
        "a refused replace must leave the target byte-for-byte untouched"
    );
    assert!(
        ws.temp_leftovers(&ws.root).is_empty(),
        "and must clean up its temp file: {:?}",
        ws.temp_leftovers(&ws.root)
    );
}
