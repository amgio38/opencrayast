//! Helpers shared by more than one test binary. A module under `tests/` is compiled into each
//! test that declares it, not into a test target of its own.
#![allow(dead_code, reason = "each test binary uses a different subset")]

pub mod fuzz;

use std::io;
use std::os::unix::net::UnixListener;
use std::path::Path;

/// Whether this platform offers a way to create a FIFO: `rustix::fs::mknodat` is Linux-only,
/// so everywhere else this is the external `mkfifo`.
pub const fn fifo_creation_available() -> bool {
    cfg!(target_os = "linux")
}

/// Whether a test on THIS host may expect a FIFO to appear.
///
/// `fifo_creation_available` says the platform has a mechanism; this says whether this
/// particular build is compiled to use it, which is the same question asked from the other
/// side. It is what lets a caller distinguish "no FIFO because this platform cannot make one"
/// (skip) from "no FIFO although it can" (broken fixture, fail) without reading errno.
pub const fn fifo_can_be_made_on_this_host() -> bool {
    fifo_creation_available()
}

/// Whether the FIFO case of a test may be dropped, as a pure function of what was observed.
///
/// The same discipline as [`socket_bind_plan`]: a test whose target list loses the FIFO stays
/// green, so the conditions under which that may happen are pinned from both sides. A mutation
/// that skips for a cause the suite could have fixed - a wrong mode, a path that is already
/// there - goes red here instead of quietly shortening the test.
pub const fn fifo_plan(
    platform_offers_fifo_creation: bool,
    made: bool,
    errno: Option<std::io::ErrorKind>,
) -> bool {
    if made {
        return false;
    }
    if !platform_offers_fifo_creation {
        return true;
    }
    // The platform has a way to make a FIFO and it failed. Only the three kernel refusals that
    // a sandbox can legitimately impose are a skip; anything else (a busy path, a name in use,
    // a read-only filesystem that is not a permission problem) is our own bug.
    matches!(
        errno,
        Some(
            std::io::ErrorKind::PermissionDenied
                | std::io::ErrorKind::Unsupported
                | std::io::ErrorKind::InvalidInput
        )
    )
}

/// Create a FIFO at `path`.
///
/// `true` when the node exists and is a FIFO; `false` when the case must be dropped, the reason
/// having already been printed. Panics on any failure that [`fifo_plan`] says must not be a
/// skip, so a broken fixture cannot hide behind a green test.
///
/// What this deliberately does not do is open the FIFO: an open blocks until a writer arrives,
/// which would hang the very tests that exist to prove the boundary does not block on one.
/// Existence is confirmed with `symlink_metadata` instead.
#[cfg(unix)]
pub fn make_fifo(path: &Path) -> bool {
    match try_make_fifo(path) {
        Ok(()) => true,
        Err(kind) => {
            println!("mkfifo failed at {}: {kind:?}", path.display());
            assert!(
                fifo_plan(fifo_creation_available(), false, Some(kind)),
                "creating a FIFO failed for {kind:?}, which is not a reason to drop the FIFO \
                 cases: this platform offers a way to make one, so the fixture is broken"
            );
            false
        }
    }
}

/// The raw attempt, with the errno kept so [`fifo_plan`] can judge it. Every failure is
/// classified before anyone can call it a skip.
#[cfg(unix)]
fn try_make_fifo(path: &Path) -> Result<(), std::io::ErrorKind> {
    #[cfg(target_os = "linux")]
    {
        use std::os::unix::ffi::OsStrExt;
        let c = std::ffi::CString::new(path.as_os_str().as_bytes())
            .map_err(|_| std::io::ErrorKind::InvalidInput)?;
        rustix::fs::mknodat(
            rustix::fs::CWD,
            c.as_c_str(),
            rustix::fs::FileType::Fifo,
            rustix::fs::Mode::from_raw_mode(0o600),
            0,
        )
        .map_err(|e| e.kind())?;
    }
    #[cfg(not(target_os = "linux"))]
    {
        let status = std::process::Command::new("mkfifo")
            .arg(path)
            .status()
            .map_err(|e| e.kind())?;
        if !status.success() {
            return Err(std::io::ErrorKind::Other);
        }
    }
    // Confirm the node without opening it. This never follows and never blocks, which an open
    // would: a FIFO opened for reading waits for a writer forever, and hanging the fixture is
    // precisely what the tests that use it exist to rule out.
    use std::os::unix::fs::FileTypeExt;
    let md = std::fs::symlink_metadata(path).map_err(|e| e.kind())?;
    if !md.file_type().is_fifo() {
        return Err(std::io::ErrorKind::Other);
    }
    Ok(())
}

/// Longest socket path this suite will try to bind.
///
/// `sockaddr_un.sun_path` is 108 bytes on Linux but only **104** on macOS, and the error a
/// longer path produces (`EINVAL` from `bind`) says nothing about the length. macOS also puts
/// its temporary directory under `/var/folders/<2>/<hash>/T/`, roughly 55 characters against
/// Linux's roughly 16, so a path that fits here does not automatically fit there. 100 leaves
/// room for the NUL and for the length to grow a little before someone has to think about it.
pub const UNIX_PATH_MAX_BYTES: usize = 100;

/// Why a unix socket could not be bound, separated into the ONE case that may be skipped and
/// everything else.
///
/// The tests that create a socket drop the socket case when the outcome is `Skipped` and stay
/// green. That makes the skip a silent weakening of what is under test unless the decision is
/// pinned from both sides: it may fire only when the platform genuinely cannot put a socket at
/// that path, and never for a reason the suite could have fixed (a leftover file, a path over
/// the limit we chose ourselves). Anything else is a real failure and stays one.
/// What the pure decision says about a socket bind, with no listener attached.
///
/// The listener is deliberately not part of this: the decision has to be constructible from
/// two booleans and an `Option<SocketReason>` alone, so that a test can pin every cell of it
/// without creating a single socket. [`SocketOutcome`] pairs a plan with the listener when
/// there is one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SocketPlan {
    /// Bind it: nothing here prevents a socket.
    Bind,
    /// The only skippable case; `&'static str` is the reason, printed as a SKIP note.
    Skip(&'static str),
    /// Anything else: the caller panics.
    Fatal,
}

/// The skip decision, as a pure function of what could possibly make it true.
///
/// The caller runs the bind and classifies the result into [`SocketReason`], then passes that
/// classification in here. Splitting it this way keeps the decision testable on any machine:
/// a developer on macOS, or on a container whose `TMPDIR` is deep enough to overflow `sun_path`,
/// checks here the branch they would only otherwise discover in CI - and, more to the point,
/// a mutation that makes this return `Skip` for a self-inflicted cause goes red.
pub const fn socket_bind_plan(
    platform_allows_unix_sockets: bool,
    path_len: usize,
    bind_err: Option<SocketReason>,
) -> SocketPlan {
    // A build error, not a test failure: raising the budget past the smallest kernel limit
    // would make this suite skip on macOS for paths Linux accepts, and that must never be
    // something a contributor discovers only by running the tests.
    const {
        assert!(
            UNIX_PATH_MAX_BYTES < SUN_PATH_LIMIT_BYTES,
            "the suite's own socket-path budget must stay under the smallest sun_path, or a \
             path that fits on Linux is silently skipped on macOS"
        );
    }
    if !platform_allows_unix_sockets {
        return SocketPlan::Skip("this platform has no AF_UNIX socket type");
    }
    // Over our own budget, or over what the kernel would accept on the smallest platform this
    // suite runs on. Both are "this host cannot hold a socket at this path"; the kernel check
    // matters because it keeps the rule true even if the constant above is raised later.
    if path_len > UNIX_PATH_MAX_BYTES || path_len > SUN_PATH_LIMIT_BYTES {
        return SocketPlan::Skip(
            "the socket path is too long for `sockaddr_un.sun_path` on this platform",
        );
    }
    match bind_err {
        None => SocketPlan::Bind,
        // `bind(2)` reports "no room in sun_path for this path" as EINVAL, the one errno that
        // carries that meaning here. Nothing else does, so it may be skipped.
        Some(SocketReason::PathTooLong) => {
            SocketPlan::Skip("the kernel refused the socket path as too long for `sun_path`")
        }
        Some(SocketReason::PermissionDenied) => SocketPlan::Fatal,
        Some(SocketReason::AlreadyExists) => SocketPlan::Fatal,
        Some(SocketReason::Other) => SocketPlan::Fatal,
    }
}

/// `sun_path` needs the whole path plus a NUL.
///
/// `sockaddr_un.sun_path` is 108 bytes on Linux but only **104** on macOS, and the error a
/// longer path produces (`EINVAL` from `bind`) says nothing about the length. macOS also puts
/// its temporary directory under `/var/folders/<2>/<hash>/T/`, roughly 55 characters against
/// Linux's roughly 16, so a path that fits here does not automatically fit there.
///
/// Strictly greater than 104 leaves room for the NUL at both sizes. Sharing one constant
/// rather than a `cfg` per platform is deliberate: the platform limit becomes the smaller of
/// the two for everyone, and the [guard test](super::the_sock_skip_only_triggers_for_a_socket_this_platform_cannot_hold)
/// pins the boundary from both sides, so neither constant can drift past it unnoticed.
pub const SUN_PATH_LIMIT_BYTES: usize = 104;

/// Whether this platform allows AF_UNIX sockets at all.
///
/// Windows has none, so `sockaddr_un` does not exist and the fixed 104-byte limit says nothing
/// about it. Those suites do not reach this: they are `#![cfg(unix)]`, and `unix` already means
/// AF_UNIX. It is still an explicit input rather than an assumption, because the guard test
/// pins every cell of the matrix and an implicit assumption is exactly what it cannot see.
pub const fn unix_sockets_supported() -> bool {
    cfg!(unix)
}

/// How a failed `bind` is classified: only the causes that may become a skip.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SocketReason {
    /// `EINVAL` from `bind`, meaning the path did not fit in `sun_path`.
    PathTooLong,
    /// `EACCES`/`EPERM`: a directory permission, never a path length.
    PermissionDenied,
    /// `EADDRINUSE`: a leftover socket file this suite created and can remove.
    AlreadyExists,
    /// Anything else.
    Other,
}

/// Classify a `bind` error into the causes that may skip. Every error, not just `EINVAL`,
/// goes through here: a caller that cannot name the reason will guess, and a guessed reason is
/// a silent skip.
pub fn classify_bind_error(e: &io::Error) -> SocketReason {
    match e.raw_os_error() {
        Some(code) if code == rustix::io::Errno::INVAL.raw_os_error() => SocketReason::PathTooLong,
        Some(code)
            if code == rustix::io::Errno::ACCESS.raw_os_error()
                || code == rustix::io::Errno::PERM.raw_os_error() =>
        {
            SocketReason::PermissionDenied
        }
        Some(code) if code == rustix::io::Errno::ADDRINUSE.raw_os_error() => {
            SocketReason::AlreadyExists
        }
        _ => SocketReason::Other,
    }
}

/// A socket bind that actually happened: the plan, plus the listener when there is one.
///
/// Splitting the listener out of the decision is what makes the decision testable. A guard test
/// constructs [`SocketPlan`] from two booleans and an errno; only this type is built by running
/// a real `bind`.
#[derive(Debug)]
pub enum SocketOutcome {
    /// The socket exists. The listener must stay alive: dropping it closes the socket, and a
    /// closed socket is a different file for the tests that use this.
    Bound(Box<UnixListener>),
    /// The only skippable case, carrying why.
    Skipped(&'static str),
    /// Every other cause. The caller panics; the classified reason was printed by
    /// [`try_bind_unix_socket`], so the panic message points at it.
    Fatal,
}

impl SocketOutcome {
    /// The listener, or `None` for a skip that [`socket_bind_plan`] authorised.
    ///
    /// Panics on [`SocketOutcome::Fatal`] rather than returning `None`, because a `None` from
    /// here would be read as "drop the socket case" - exactly the silent degradation these types
    /// exist to prevent.
    pub fn listener(self) -> Option<UnixListener> {
        match self {
            SocketOutcome::Bound(l) => Some(*l),
            SocketOutcome::Skipped(why) => {
                println!("SKIPPED: {why}");
                None
            }
            SocketOutcome::Fatal => panic!(
                "a unix socket could not be bound for a reason that must not be skipped; the \
                 classified cause is on the line above"
            ),
        }
    }

    /// Whether this outcome removes the socket case from a test's list of things under test.
    ///
    /// The distinction this makes explicit: `None` from [`Self::listener`] means "this skip is
    /// authorised, here is why", while a bare `Option<UnixListener>` from an unclassified helper
    /// means "we could not find out, carry on without it". Only the first is a skip.
    pub const fn is_skipped(&self) -> bool {
        matches!(self, SocketOutcome::Skipped(_))
    }

    /// The reason, when this is a skip.
    pub const fn skip_reason(&self) -> Option<&'static str> {
        match self {
            SocketOutcome::Skipped(why) => Some(why),
            SocketOutcome::Bound(_) | SocketOutcome::Fatal => None,
        }
    }
}

/// Bind a unix socket at `path`, classifying every outcome so a caller never has to guess why.
///
/// Runs the bind, hands the classified errno to the pure [`socket_bind_plan`], and returns the
/// resulting outcome with the listener attached. Every failure is classified before anything
/// can call it a skip.
pub fn try_bind_unix_socket(path: &Path) -> SocketOutcome {
    let len = path.as_os_str().as_encoded_bytes().len();
    let allow = unix_sockets_supported();
    if !allow {
        // Nothing to call: `bind` has no AF_UNIX to bind on such a platform.
        return match socket_bind_plan(allow, len, None) {
            SocketPlan::Skip(why) => SocketOutcome::Skipped(why),
            SocketPlan::Bind | SocketPlan::Fatal => SocketOutcome::Fatal,
        };
    }
    match UnixListener::bind(path) {
        Ok(l) => SocketOutcome::Bound(Box::new(l)),
        Err(e) => {
            let why = classify_bind_error(&e);
            println!("bind failed at {len} bytes: {e} ({why:?})");
            match socket_bind_plan(allow, len, Some(why)) {
                SocketPlan::Skip(why) => SocketOutcome::Skipped(why),
                // `bind` returned an error, so `Bind` here would mean the plan disagreed with
                // the kernel; treat it as fatal rather than claiming a socket that is not there.
                SocketPlan::Bind | SocketPlan::Fatal => SocketOutcome::Fatal,
            }
        }
    }
}

/// Bind a unix socket at `path`, or `None` with a printed SKIP when the path cannot hold one.
///
/// The caller keeps the listener: dropping it closes the socket, and a closed socket is a
/// different file for the tests that use this. Panics on every cause that
/// [`socket_bind_plan`] says must not be skipped.
pub fn bind_unix_socket(path: &Path) -> Option<UnixListener> {
    try_bind_unix_socket(path).listener()
}

/// The skip decision is pure, so the branch a macOS developer or a deep-`TMPDIR` container would
/// take is checked here rather than discovered there - and, more to the point, so a mutation that
/// skips for a cause this suite could have fixed goes red instead of quietly shortening every
/// socket test on the machine.
///
/// Mirrors `the_eacces_skip_only_triggers_for_root_without_setpriv` in `boundary_harden_spec`,
/// which pins the same discipline for `setpriv`. Living here rather than in one spec file means
/// every binary that declares `mod common` compiles and runs these.
#[test]
fn the_sock_skip_only_triggers_for_a_socket_this_platform_cannot_hold() {
    const OVER_BUDGET: &str =
        "the socket path is too long for `sockaddr_un.sun_path` on this platform";

    // The length cells below are pinned to `UNIX_PATH_MAX_BYTES` rather than to a literal,
    // because the suite's budget is deliberately stricter than the smallest kernel limit: a path
    // that fits here also fits on macOS. That relationship is asserted at compile time inside
    // `socket_bind_plan`, so raising the budget past `sun_path` is a build error rather than a
    // skip nobody notices.

    // The ordinary cell: this platform has AF_UNIX, the path is short, bind succeeded.
    assert_eq!(
        socket_bind_plan(true, 40, None),
        SocketPlan::Bind,
        "a 40-byte path on a unix host is not a reason to skip"
    );

    // The budget boundary, from both sides. Exactly at the budget binds; one over is the skip,
    // and it is a skip because of that one byte and nothing else.
    assert_eq!(
        socket_bind_plan(true, UNIX_PATH_MAX_BYTES, None),
        SocketPlan::Bind,
        "a path of exactly the budget must bind: skipping here would skip on a host that can \
         bind the socket"
    );
    assert_eq!(
        socket_bind_plan(true, UNIX_PATH_MAX_BYTES + 1, None),
        SocketPlan::Skip(OVER_BUDGET),
        "one byte over the budget is the skip, and only because of that one byte"
    );
    assert_eq!(
        socket_bind_plan(true, SUN_PATH_LIMIT_BYTES + 1, None),
        SocketPlan::Skip(OVER_BUDGET),
        "past the kernel limit every platform is a skip too"
    );

    // The kernel's own verdict is the same one cause; nothing else is. The path is inside the
    // budget here, so this cell exercises the errno rule rather than the length rule above -
    // which is the case a mutation that ignored the errno would get wrong.
    assert_eq!(
        socket_bind_plan(true, 40, Some(SocketReason::PathTooLong)),
        SocketPlan::Skip("the kernel refused the socket path as too long for `sun_path`"),
    );
    for reason in [
        SocketReason::PermissionDenied,
        SocketReason::AlreadyExists,
        SocketReason::Other,
    ] {
        assert_eq!(
            socket_bind_plan(true, 40, Some(reason)),
            SocketPlan::Fatal,
            "{reason:?} is a fixture bug or a real failure, never a silent skip"
        );
    }

    // No AF_UNIX at all: the other genuine skip. On a `cfg(unix)` host this cell is unreachable
    // in practice, and the assertion below says so rather than leaving a branch nobody has run.
    assert_eq!(
        socket_bind_plan(false, 40, None),
        SocketPlan::Skip("this platform has no AF_UNIX socket type"),
    );
    assert!(
        unix_sockets_supported(),
        "this suite is #![cfg(unix)]; if it ever builds on a platform without AF_UNIX, the \
         guards above and the UNIX_PATH_MAX_BYTES budget need a real answer for it"
    );
}

/// Same discipline for the FIFO case: a test whose target list loses the FIFO stays green, so
/// the conditions under which that may happen are pinned from both sides.
#[test]
fn the_fifo_skip_only_triggers_when_no_fifo_can_be_made() {
    assert!(
        !fifo_plan(true, true, None),
        "a FIFO that was made is never a skip"
    );
    assert!(
        !fifo_plan(true, false, None),
        "a failure with no errno to judge is not a skip - that is exactly the silent case"
    );
    assert!(
        fifo_plan(false, false, None),
        "no mknodat and no mkfifo is a real skip"
    );
    for kind in [
        std::io::ErrorKind::PermissionDenied,
        std::io::ErrorKind::Unsupported,
        std::io::ErrorKind::InvalidInput,
    ] {
        assert!(
            fifo_plan(true, false, Some(kind)),
            "{kind:?} is a sandbox refusing the request, which is a skip"
        );
    }
    for kind in [
        std::io::ErrorKind::AlreadyExists,
        std::io::ErrorKind::NotFound,
        std::io::ErrorKind::Other,
    ] {
        assert!(
            !fifo_plan(true, false, Some(kind)),
            "{kind:?} is our own bug (a stale path, a wrong parent), never a skip"
        );
    }
}

/// A workspace plus the policy that governs it, for the fsio suites.
///
/// Since F-01b, `fsio::atomic_replace` is crate-private and takes a `&Boundary`, so a test outside
/// the crate cannot call it with a bare temp path. These helpers build a REAL boundary over a temp
/// directory instead — there is no test-only back door, because a back door would be the second way
/// in that the fix removed.
#[cfg(unix)]
pub mod atomic {
    use opencrayast_core::boundary::{Boundary, BoundaryConfig, ResolvedPath};
    use std::fs;
    use std::path::{Path, PathBuf};

    /// A temp workspace with its policy.
    pub struct Ws {
        /// The temp dir; keep it alive for the test's duration.
        pub dir: tempfile::TempDir,
        /// The workspace root.
        pub root: PathBuf,
        /// The policy every write goes through.
        pub boundary: Boundary,
    }

    impl Ws {
        /// A workspace with a state dir beside it (never a write target).
        pub fn new() -> Ws {
            let dir = tempfile::tempdir().unwrap();
            let root = dir.path().join("ws");
            fs::create_dir_all(&root).unwrap();
            let state = dir.path().join("state");
            let boundary = Boundary::new({
                let mut cfg =
                    BoundaryConfig::new(root.clone(), opencrayast_core::limits::Limits::default());
                cfg.state_dir = Some(state);
                cfg
            })
            .unwrap();
            Ws {
                dir,
                root,
                boundary,
            }
        }

        /// Write `content` to `rel` inside the workspace, creating parents.
        pub fn put(&self, rel: &str, content: &[u8]) {
            let p = self.root.join(rel);
            fs::create_dir_all(p.parent().unwrap()).unwrap();
            fs::write(&p, content).unwrap();
        }

        /// The absolute path of `rel` inside the workspace.
        pub fn abs(&self, rel: &str) -> PathBuf {
            self.root.join(rel)
        }

        /// The policy-resolved path of `rel`, as a caller would hold it.
        pub fn resolved(&self, rel: &str) -> ResolvedPath {
            self.boundary.resolve_write(rel).unwrap()
        }

        /// Replace `rel` through the policy — the only way to write in these suites.
        pub fn replace(
            &self,
            rel: &str,
            content: &[u8],
        ) -> Result<(), opencrayast_core::ToolError> {
            let r = self.boundary.resolve_write(rel)?;
            self.boundary.replace_file(&r, content)
        }

        /// Replace `rel`, panicking with the error on refusal.
        pub fn replace_ok(&self, rel: &str, content: &[u8]) {
            self.replace(rel, content)
                .unwrap_or_else(|e| panic!("replace {rel}: {e}"));
        }

        /// The current contents of `rel`.
        pub fn read(&self, rel: &str) -> Vec<u8> {
            fs::read(self.root.join(rel)).unwrap()
        }

        /// `.opencrayast-tmp-*` left anywhere under the workspace.
        pub fn temp_leftovers(&self) -> Vec<PathBuf> {
            let mut out = Vec::new();
            let mut stack = vec![self.root.clone()];
            while let Some(d) = stack.pop() {
                for e in fs::read_dir(&d).unwrap().flatten() {
                    let p = e.path();
                    if e.file_type().unwrap().is_dir() {
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

    /// A file outside any workspace, for the refusals that must not depend on being inside one.
    pub fn outside_file(name: &str, content: &[u8]) -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join(name);
        fs::write(&p, content).unwrap();
        (dir, p)
    }

    /// `st_dev`/`st_ino` of `p`, for the tests that assert the identity check still fires.
    pub fn ident(p: &Path) -> opencrayast_core::boundary::FileIdentity {
        use std::os::unix::fs::MetadataExt;
        let m = fs::metadata(p).unwrap();
        opencrayast_core::boundary::FileIdentity {
            dev: m.dev(),
            ino: m.ino(),
        }
    }
}
