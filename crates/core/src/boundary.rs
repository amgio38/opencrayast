//! The one path policy. Every filesystem access in the project goes through `Boundary`
//! (ARCHITECTURE principle 2; SECURITY-MODEL T-01..T-05, T-20, T-31).

use crate::error::{ErrorCode, ToolError};
use crate::limits::Limits;
use std::fs::File;
use std::path::{Component, Path, PathBuf};

/// The single refusal text for "not inside any root". Deliberately identical for "outside
/// the root", "does not exist" and "cannot be proved to be inside": BND-18 / T-20 forbid
/// telling an agent which paths outside the workspace exist. Never contains a path.
const OUTSIDE_MESSAGE: &str = "Path is not inside the workspace or a read-only root.";
const OUTSIDE_NEXT: &str =
    "Pass a path relative to the workspace root, or configure a read root for it.";

/// Static configuration of a boundary.
///
/// **No `Default`.** It used to derive one, and every construction site that spread the
/// remaining fields silently received `Limits::default()` rather than the operator's —
/// the container existed and nothing poured into it (SEC-FIX 5 CR). With `Default` gone,
/// `root` and `limits` are mandatory everywhere, so a caller has to state which limits
/// the boundary is held to. Use [`BoundaryConfig::new`] for the common case.
#[derive(Debug, Clone)]
pub struct BoundaryConfig {
    /// Workspace root; must exist. `/`, drive roots and a bare home directory are refused.
    pub root: PathBuf,
    /// Extra READ-ONLY roots (never writable, BND-19). Same refusals as `root`.
    pub read_roots: Vec<PathBuf>,
    /// The state directory: never a write target (BND-15).
    pub state_dir: Option<PathBuf>,
    /// Extra protected globs (added to the built-ins).
    pub extra_protected: Vec<String>,
    /// The operator's limits, carried to every path check (SECFIX5-01).
    ///
    /// This field is what makes `path_max_bytes` and `path_max_depth` real. Both used to be
    /// read from `Limits::default()` at their two separate call sites, so a configured value
    /// could never arrive and `[limits] path_max_depth` in the user file bound nothing. The
    /// limits live here so there is exactly ONE place that answers "what is the path
    /// ceiling", and [`Boundary::limits`] is the only way to ask.
    pub limits: Limits,
}

impl BoundaryConfig {
    /// The common case: a workspace root and the limits to hold it to.
    ///
    /// Takes the limits rather than defaulting them, so no caller can build a boundary
    /// without deciding what bounds it.
    pub fn new(root: impl Into<PathBuf>, limits: Limits) -> Self {
        Self {
            root: root.into(),
            limits,
            read_roots: Vec::new(),
            state_dir: None,
            extra_protected: Vec::new(),
        }
    }
}

/// Device + inode (or Windows file id) of an open file.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct FileIdentity {
    /// Device.
    pub dev: u64,
    /// Inode / file id.
    pub ino: u64,
}

/// A path that passed the policy.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedPath {
    /// Workspace-relative display path (forward slashes). The ONLY path ever shown to agents.
    pub rel: String,
    /// Canonical absolute path (never shown to agents).
    pub abs: PathBuf,
}

/// The policy object.
#[derive(Debug)]
pub struct Boundary {
    /// Canonical workspace root; the only root that may be written.
    root: PathBuf,
    /// Canonical read-only roots in configuration order; `@root<N>` labels use this index.
    read_roots: Vec<PathBuf>,
    /// Canonical state directory, never a write target (BND-15). Kept as configured when it
    /// does not exist yet, because the state module is what creates it.
    state_dir: Option<PathBuf>,
    /// Extra protected globs from the configuration; the built-ins cannot be removed (T-05).
    extra_protected: Vec<String>,
    /// The operator's limits, as configured (SECFIX5-01). Read through [`Boundary::limits`]
    /// and nowhere else, so the two path checks cannot drift onto a different value.
    limits: Limits,
    /// Directory descriptor of the canonical workspace root, opened once. Every read is
    /// opened *relative to this handle*, never by re-resolving the root's path string, so
    /// the part of the workspace we mean cannot change underneath us (T-03).
    #[cfg(unix)]
    root_fd: rustix::fd::OwnedFd,
    /// Directory descriptors of the read-only roots, in the same order as `read_roots`.
    #[cfg(unix)]
    read_root_fds: Vec<rustix::fd::OwnedFd>,
}

/// The result of the shared resolution: which root owns the path, its canonical form, and
/// the spelling it was reached by (symlinks NOT expanded). The write policy needs both:
/// only the spelled form can show that the final component was a link.
struct Resolved {
    /// Index into the roots: 0 is the workspace, 1.. are the read roots.
    owner: usize,
    /// Canonical path (symlinks expanded by the OS).
    canonical: PathBuf,
    /// Absolute spelled path with links left alone.
    lexical: PathBuf,
}

/// A pinned handle on a write target's PARENT DIRECTORY, plus everything needed to prove at the
/// last moment that this directory is still the one the workspace names (SEC-FIX 2 / F-01).
///
/// The handle is the point. Every step of the write that used to take a path string — creating
/// the temp file, setting its mode and owner, copying its extended attributes, renaming it over
/// the target, syncing the directory — is issued relative to this descriptor instead, so none of
/// them can be redirected by swapping a name in between (T-03).
#[cfg(unix)]
pub(crate) struct WriteParent {
    /// `O_DIRECTORY | O_NOFOLLOW`, opened beneath the pinned workspace root handle.
    pub(crate) dir: rustix::fd::OwnedFd,
    /// `dev`/`ino` read from the handle at open time. Compared again immediately before the rename.
    pub(crate) identity: FileIdentity,
    /// The parent's path relative to the workspace root, used ONLY to re-open the same entry
    /// through the root handle for the final comparison — never for the write itself.
    pub(crate) parent_rel: PathBuf,
    /// The target's own name, relative to `dir`.
    pub(crate) leaf: std::ffi::OsString,
}

/// One lexical component list plus whether the input was absolute.
struct Lexical {
    /// `true` when the input started at the filesystem root.
    absolute: bool,
    /// Windows absolute disk paths carry their drive here (`"C:"`). Empty on every other shape,
    /// including Unix absolute paths and Windows relative ones. Kept on every platform so the
    /// absolute-disk branch is compiled and linted where the tests for it live (Linux CI).
    drive: Option<String>,
    /// Normalised components (no `.`, no empty, no `..`).
    components: Vec<String>,
}

impl Boundary {
    /// Validate and canonicalise the configuration.
    pub fn new(cfg: BoundaryConfig) -> Result<Self, ToolError> {
        if cfg.root.as_os_str().is_empty() {
            return Err(config_error(
                "The workspace root is not set.",
                "Set --workspace to a directory.",
            ));
        }
        let root = check_root(&cfg.root, RootKind::Workspace)?;

        let mut read_roots = Vec::with_capacity(cfg.read_roots.len());
        for r in &cfg.read_roots {
            let c = check_root(r, RootKind::ReadOnly)?;
            if c != root {
                read_roots.push(c);
            }
        }

        // Pin each root with a directory handle now, so a later open is relative to the
        // directory we validated rather than to a path string that could be re-pointed.
        #[cfg(unix)]
        let (root_fd, read_root_fds) = (
            open_root_dir(&root)?,
            read_roots
                .iter()
                .map(|r| open_root_dir(r))
                .collect::<Result<Vec<_>, _>>()?,
        );

        // A state directory that does not exist yet is normal (the state module creates it
        // with the right mode); canonicalise it when it is already there so the write-side
        // containment check compares like with like.
        let state_dir = cfg.state_dir.as_ref().map(|p| match p.canonicalize() {
            Ok(c) => c,
            Err(_) => p.clone(),
        });

        Ok(Self {
            root,
            read_roots,
            state_dir,
            extra_protected: cfg.extra_protected,
            limits: cfg.limits,
            #[cfg(unix)]
            root_fd,
            #[cfg(unix)]
            read_root_fds,
        })
    }

    /// The limits this boundary enforces, as the operator configured them (SECFIX5-01).
    ///
    /// This is the single source for the path-length and path-depth ceilings. Both the
    /// resolver and the directory walker ask here; neither of them constructs a `Limits` of
    /// its own, so there is no second copy of the default to drift (SECFIX5-03). A caller
    /// that needs a ceiling must ask the boundary it is walking, not the defaults.
    pub fn limits(&self) -> &Limits {
        &self.limits
    }

    /// Resolve `path` (workspace-relative, or absolute inside a root) for READING.
    /// Refuses: empty, NUL/control chars, over `Limits` length/depth, `..` escapes,
    /// absolute paths outside all roots, symlinks/reparse points leaving the root, any
    /// intermediate symlink that leaves the root. Outside-root and not-found paths give the
    /// SAME `outside_workspace` refusal (BND-18). Never panics (BND-16).
    pub fn resolve_read(&self, path: &str) -> Result<ResolvedPath, ToolError> {
        let r = self.resolve_path(path)?;

        if r.lexical.as_os_str().is_empty() {
            // The root itself. `.` is the only spelling that is inside by definition.
            return Ok(ResolvedPath {
                rel: ".".to_string(),
                abs: r.canonical.clone(),
            });
        }
        Ok(ResolvedPath {
            rel: display_rel(&r.canonical, self.root_at(r.owner), r.owner),
            abs: r.canonical,
        })
    }

    /// Everything both directions share: character and limit checks, lexical normalisation,
    /// the existing-prefix walk and the containment proof.
    fn resolve_path(&self, path: &str) -> Result<Resolved, ToolError> {
        check_input_chars(path)?;
        check_size(path, &self.limits)?;
        let lex = lexically_normalise(path)?;
        let (owner, canonical, lexical) = self.resolve_existing(&lex)?;
        Ok(Resolved {
            owner,
            canonical,
            lexical,
        })
    }

    /// Resolve `path` for WRITING: everything `resolve_read` checks, plus: only the workspace
    /// root (never a read root), final component must be an existing regular file that is not
    /// a link, `nlink == 1`, not read-only, not protected (`protected_path`), not under the
    /// state dir.
    pub fn resolve_write(&self, path: &str) -> Result<ResolvedPath, ToolError> {
        let r = self.resolve_path(path)?;

        // A read-only root is never writable, however the path was spelled (BND-19).
        if r.owner != 0 {
            return Err(outside_error());
        }
        // The workspace root itself, however it was spelled. Judged on the CANONICAL form, not
        // on `r.lexical`: `lexical` is built from the root with the input's components appended,
        // so it is never empty and testing it here could never be true — the branch was dead,
        // and every spelling of the root fell through to the regular-file check below and was
        // reported as "must be a regular file, not a directory", which is true of the root and
        // answers none of the operator's question.
        if r.canonical == self.root {
            return Err(unsupported(
                "A write target must be a file, not the workspace root.",
            ));
        }

        // The built-in deny list and the state directory are pure path logic, so they are
        // checked on EVERY platform, before anything that needs `MetadataExt`. Two reasons:
        // a protected target stays protected even where the rest of the write policy cannot
        // be evaluated yet, and the rule does not change shape per platform. The structural
        // checks below (link, file type, hard links, mode) are unix-only for now.
        let rel = r.canonical.strip_prefix(&self.root).unwrap_or(&r.canonical);
        if crate::protected::is_protected(rel, &self.extra_protected) {
            return Err(protected_error(
                "A write target is on the protected list.",
                "Pick a target that is not version-control metadata or a secret-like file.",
            ));
        }
        if let Some(state) = &self.state_dir
            && is_under(&r.canonical, state)
        {
            return Err(protected_error(
                "A write target is inside the tool's state directory.",
                "Pick a target inside the workspace, outside the state directory.",
            ));
        }

        // Structural checks: link / file type / hard links / writable. Unix uses
        // MetadataExt (nlink, mode); Windows uses portable file-type checks plus readonly.
        // Hard-link count is not available on stable Rust for Windows (`number_of_links`
        // needs `windows_by_handle`). A junction leaf may report as a directory or a
        // symlink depending on the runtime — either way it is refused below.
        #[cfg(windows)]
        {
            let meta = std::fs::symlink_metadata(&r.lexical)
                .map_err(|_| unsupported("A write target must be an existing file."))?;
            if meta.file_type().is_symlink() {
                return Err(unsupported("A write target must not be a symlink."));
            }
            if !meta.file_type().is_file() {
                return Err(unsupported(
                    "A write target must be a regular file, not a directory, FIFO, socket or \
                     device.",
                ));
            }
            if meta.permissions().readonly() {
                return Err(unsupported("A write target must be writable by its owner."));
            }

            Ok(ResolvedPath {
                rel: display_rel(&r.canonical, &self.root, 0),
                abs: r.canonical,
            })
        }

        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;

            // The link check is on the SPELLED path, not on the canonical one: the canonical
            // path has already followed the link, so checking it would never see a link at
            // all. A write target is the one thing that has to be reached by a stable name
            // (T-02, T-03).
            let meta = std::fs::symlink_metadata(&r.lexical)
                .map_err(|_| unsupported("A write target must be an existing file."))?;
            if meta.file_type().is_symlink() {
                return Err(unsupported("A write target must not be a symlink."));
            }
            if !meta.file_type().is_file() {
                return Err(unsupported(
                    "A write target must be a regular file, not a directory, FIFO, socket or \
                     device.",
                ));
            }
            // A hard link cannot be told apart by path alone, and rewriting one would change
            // the file the other name points at (BND-21, T-02).
            if meta.nlink() > 1 {
                return Err(unsupported(
                    "A write target must have exactly one hard link.",
                ));
            }
            // The mode, not the current process's privileges: refusing a read-only target is
            // the right answer whether or not we could have written it anyway.
            if meta.mode() & 0o200 == 0 {
                return Err(unsupported("A write target must be writable by its owner."));
            }

            Ok(ResolvedPath {
                rel: display_rel(&r.canonical, &self.root, 0),
                abs: r.canonical,
            })
        }
    }

    /// Open a resolved path for reading with no-follow and non-blocking flags, then `fstat`:
    /// must be a regular file (FIFO/socket/device => skipped with `io_error` that says
    /// "special file", never blocks - BND-22). Re-checks the opened handle's identity against
    /// the canonical path (BND-07).
    pub fn open_read(&self, p: &ResolvedPath) -> Result<(File, FileIdentity), ToolError> {
        // Windows counterpart of the unix block below. The proofs that survive the port and the one
        // that does not are both named here, because this function is the read path's security
        // boundary and a reader has to know which guarantees they are getting.
        //
        // Survives:
        //   - containment is re-proved from the CANONICAL path, never from `p.abs`;
        //   - a reparse point anywhere in the path is resolved by `canonicalize` BEFORE
        //     containment is judged, so a link leading outside produces a canonical path that IS
        //     outside and is refused;
        //   - the file is asked what it is, and a non-regular file (FIFO, device, directory) is
        //     refused rather than read.
        //
        // Does not survive:
        //   - the open is not dir-relative, so there is a window between `canonicalize` and
        //     `open` in which a component could be swapped. The unix path closes this with
        //     `openat2`/`NO_SYMLINKS`; Windows has no stable equivalent
        //     (`FILE_FLAG_OPEN_REPARSE_POINT` needs `unsafe`, which this workspace forbids). It
        //     is the same accepted same-user window the state directory documents
        //     (SECURITY-MODEL T-03r).
        //   - `O_NONBLOCK` has no counterpart, so a FIFO would block if opened. That is why the
        //     metadata check refuses a non-file BEFORE the handle is created.
        #[cfg(windows)]
        {
            let canonical = p.abs.canonicalize().map_err(|_| outside_error())?;
            let owner =
                (0..=self.read_roots.len()).find(|i| is_under(&canonical, self.root_at(*i)));
            let Some(_owner) = owner else {
                return Err(outside_error());
            };

            let on_disk = std::fs::symlink_metadata(&canonical)
                .map_err(|_| failed("could not be inspected"))?;
            if !on_disk.is_file() {
                return Err(special_file_error());
            }

            let file = File::open(&canonical).map_err(|e| {
                if e.kind() == std::io::ErrorKind::NotFound {
                    ToolError::new(
                        ErrorCode::NotFound,
                        "The file no longer exists.",
                        "Re-read the directory and pick a file that is still there.",
                    )
                } else {
                    ToolError::new(
                        ErrorCode::IoError,
                        "The file could not be opened.",
                        "Check the permissions of the file and of the directories above it.",
                    )
                }
            })?;
            let meta = file
                .metadata()
                .map_err(|_| failed("could not be inspected"))?;
            if !meta.is_file() {
                return Err(special_file_error());
            }
            use std::os::windows::fs::MetadataExt;
            Ok((
                file,
                FileIdentity {
                    dev: meta.creation_time(),
                    ino: 0,
                },
            ))
        }

        #[cfg(unix)]
        {
            // `ResolvedPath` is a public struct, so its fields are not evidence: re-prove
            // containment here instead of trusting whatever the caller kept (principle 2:
            // there is no second way in).
            let canonical = p.abs.canonicalize().map_err(|_| outside_error())?;
            let owner =
                (0..=self.read_roots.len()).find(|i| is_under(&canonical, self.root_at(*i)));
            let Some(owner) = owner else {
                return Err(outside_error());
            };

            // Open relative to the PINNED ROOT HANDLE, with the whole path resolved beneath
            // it and no symlink followed anywhere along it (T-03). No path string takes part
            // in the open: re-resolving `<root>/a/b/c` would follow a `b` that was replaced
            // by a symlink between our check and the syscall, and `O_NOFOLLOW` covers only
            // the LAST component. `NO_SYMLINKS` covers every component and `BENEATH` makes
            // the kernel refuse any step that would leave the root.
            //
            // Then ask the HANDLE what it is: open first, stat the handle - never stat the
            // path and then open it.
            use std::os::unix::fs::MetadataExt;
            let relative = canonical
                .strip_prefix(self.root_at(owner))
                .map_err(|_| outside_error())?;
            let fd = open_beneath(
                self.root_fd_at(owner),
                relative,
                rustix::fs::OFlags::RDONLY | rustix::fs::OFlags::CLOEXEC | last_component_flags(),
            )
            .map_err(open_error)?;

            let st = rustix::fs::fstat(&fd).map_err(|_| failed("could not be inspected"))?;
            if !rustix::fs::FileType::from_raw_mode(st.st_mode).is_file() {
                // A FIFO, socket, device or directory. O_NONBLOCK is why this returns at
                // all instead of blocking forever (BND-22): the caller counts and reports
                // these, we never read one silently. Same wording as the errno path below,
                // so one rule covers both ways of learning that a node is not a file.
                return Err(special_file_error());
            }

            let opened = FileIdentity {
                // `dev_t` is an `i32` on macOS and a `u64` on Linux, and `st_ino` is not
                // 64 bits everywhere, so widen both instead of assuming a width. The values
                // are never negative device or inode numbers.
                dev: st.st_dev as u64,
                ino: st.st_ino as u64,
            };
            // The handle decides, not the name (T-03/BND-07): if the path was swapped after
            // it was resolved, the two identities disagree.
            let on_disk =
                std::fs::metadata(&canonical).map_err(|_| failed("could not be inspected"))?;
            if opened.dev != on_disk.dev() || opened.ino != on_disk.ino() {
                return Err(ToolError::new(
                    ErrorCode::IoError,
                    "The path was replaced while it was being opened.",
                    "Retry the read; if it keeps failing the workspace is changing underneath.",
                ));
            }

            // Regular files ignore O_NONBLOCK, but leaving the flag on the descriptor would
            // leak into anything that later polls it.
            let flags = rustix::fs::fcntl_getfl(&fd).map_err(|_| failed("could not be opened"))?;
            rustix::fs::fcntl_setfl(&fd, flags & !rustix::fs::OFlags::NONBLOCK)
                .map_err(|_| failed("could not be opened"))?;

            Ok((File::from(fd), opened))
        }
    }

    /// Open a resolved DIRECTORY for listing, with the same proof `open_read` makes.
    ///
    /// `ResolvedPath` is a public struct, so its fields are not evidence: containment is
    /// re-proved from the canonical path, and then the directory is opened relative to the
    /// PINNED ROOT HANDLE with the whole path resolved beneath it and no symlink followed
    /// anywhere along it (T-03). Listing a directory is not a weaker operation than opening
    /// a file - it is the same rule with `O_DIRECTORY` added - so it goes through this
    /// function and nowhere else (ARCHITECTURE principle 2).
    ///
    /// The root itself has an empty relative path, which no `openat`-family call accepts.
    /// Re-opening it as `"."` relative to the PINNED descriptor is the way to get an
    /// independent handle on it: no component is resolved at all (there is no way to leave
    /// the root when the path is `"."`), and the new open file description has its own read
    /// offset.
    ///
    /// It must NOT be a `dup` of the pinned descriptor: a duplicated descriptor SHARES its
    /// offset, so listing the root once would move the boundary's own descriptor and every
    /// later listing would start from where the previous one stopped. Two callers listing
    /// the same root would also interleave on one offset. This was not hypothetical.
    #[cfg(unix)]
    pub(crate) fn open_dir_for_listing(
        &self,
        dir: &ResolvedPath,
    ) -> Result<rustix::fd::OwnedFd, ToolError> {
        let canonical = dir.abs.canonicalize().map_err(|_| outside_error())?;
        let Some(owner) =
            (0..=self.read_roots.len()).find(|i| is_under(&canonical, self.root_at(*i)))
        else {
            return Err(outside_error());
        };
        let root_fd = self.root_fd_at(owner);
        let relative = canonical
            .strip_prefix(self.root_at(owner))
            .map_err(|_| outside_error())?;
        // `O_DIRECTORY` turns "not a directory" into one refusal here instead of a stream
        // read that fails later; `O_NOFOLLOW` is belt-and-braces for the last component,
        // because `NO_SYMLINKS` already covers it.
        let oflags = rustix::fs::OFlags::RDONLY
            | rustix::fs::OFlags::DIRECTORY
            | rustix::fs::OFlags::NOFOLLOW
            | rustix::fs::OFlags::CLOEXEC;
        if relative.as_os_str().is_empty() {
            return rustix::fs::openat(root_fd, ".", oflags, rustix::fs::Mode::empty())
                .map_err(|_| failed("could not be opened"));
        }
        open_beneath(root_fd, relative, oflags).map_err(listing_error)
    }

    /// Replace `resolved`'s file with `content`, atomically.
    ///
    /// The single supported way to write a workspace file. It is a method on the policy so that
    /// every write in this workspace passes through this object: `fsio::atomic_replace` is
    /// crate-private and cannot be reached with a bare path, which is what keeps ARCHITECTURE
    /// principle 2 ("there is no second way in") true rather than aspirational.
    ///
    /// The caller still resolves the path with [`Boundary::resolve_write`] — that is what produces
    /// the `rel` used for messages — and this re-proves containment from the absolute path, so a
    /// `ResolvedPath` that was built by hand or kept from an earlier call is not authority.
    ///
    /// Refusals are `outside_workspace` (not in this workspace), `unsupported_target` (a symlink,
    /// a non-regular file, a hard-linked or read-only target), `not_found`, `io_error` (the
    /// identity moved under us), and `limit_exceeded`. None of them repeats anything about the
    /// target beyond its class (F-04).
    #[cfg(unix)]
    pub fn replace_file(&self, resolved: &ResolvedPath, content: &[u8]) -> Result<(), ToolError> {
        self.replace_file_checked(resolved, content, None)
    }

    /// [`Boundary::replace_file`] with the identity the caller verified before deciding to write.
    ///
    /// `expect` closes the window between the caller's read and this write: a target that was
    /// replaced by another file, or turned into a link, in between is refused instead of being
    /// silently overwritten (E-7). Pass `None` only when no earlier read exists to compare against.
    pub fn replace_file_checked(
        &self,
        resolved: &ResolvedPath,
        content: &[u8],
        expect: Option<FileIdentity>,
    ) -> Result<(), ToolError> {
        crate::fsio::atomic_replace(self, resolved, content, expect)
    }

    /// [`Boundary::replace_file_checked`] with a test seam that fires after the target has been
    /// checked and immediately before the rename.
    ///
    /// Exists because the window is microseconds wide: a test that races it passes when the guard
    /// it is meant to be checking is missing (CR F2). A test that passes a hook here can make the
    /// precise change it wants and then assert the entry point refused. The hook type itself is
    /// crate-private, so a dependent cannot reach this method at all.
    #[cfg(all(test, unix))]
    pub(crate) fn replace_file_with_seam(
        &self,
        resolved: &ResolvedPath,
        content: &[u8],
        expect: Option<FileIdentity>,
        seam: &crate::fsio::BeforeRename<'_>,
        attrs: &crate::fsio::PropertyCopy<'_>,
    ) -> Result<(), ToolError> {
        crate::fsio::atomic_replace_with_seam(self, resolved, content, expect, seam, attrs)
    }

    /// Prove that an absolute path is inside the workspace, right now.
    ///
    /// The write path in `atomic_replace` takes a `&ResolvedPath`, but `ResolvedPath` is a public
    /// struct whose fields a caller can build or keep after the workspace moved. So this re-derives
    /// containment from `abs` alone, exactly as `open_read` does for the read side: a path outside
    /// the workspace — or under a read-only root — is `outside_workspace`, and the message is the
    /// one outside refusal with no hint about whether anything exists there (BND-18).
    ///
    /// Returns the canonical path, which the caller then uses instead of `abs`.
    ///
    /// Unix-only: the only caller is `fsio::atomic_replace`, which is itself unix-only. Left
    /// ungated it was dead code on Windows, and `-D warnings` turns that into a build failure.
    #[cfg(unix)]
    pub(crate) fn reprove_write_containment(&self, abs: &Path) -> Result<PathBuf, ToolError> {
        // `canonicalize` failing is reported as `outside_workspace` whether the path is outside the
        // workspace or simply does not exist. That is deliberate (BND-18 / T-20: the refusal must
        // not reveal existence), and it is why this returns the outside refusal rather than
        // `not_found` — a missing target inside the workspace is answered this way too.
        let canonical = abs.canonicalize().map_err(|_| outside_error())?;
        // Index 0 only: a read-only root is never a write target, however it was spelled.
        if !is_under(&canonical, &self.root) {
            return Err(outside_error());
        }
        Ok(canonical)
    }

    /// Open the PARENT DIRECTORY of a write target as a handle pinned beneath the workspace root
    /// (SEC-FIX 2 / F-01, T-03).
    ///
    /// This is the write side of the asymmetry the audit found: the read path has always opened
    /// relative to the PINNED ROOT DESCRIPTOR with `BENEATH | NO_SYMLINKS`, while the write path
    /// worked from path strings for milliseconds before its rename. Moving a whole parent
    /// directory out of the workspace and leaving a symlink behind it passes every per-file check
    /// — the target inode comes along, still `nlink == 1`, the same `dev`/`ino`, and not a link.
    ///
    /// What is returned is everything the rest of the write needs, and no path string:
    ///
    /// - `dir`: the handle. The temp file is created in it, the rename is issued from it, and
    ///   the directory is synced through it, so no step re-resolves a name;
    /// - `identity`: `dev`/`ino` of that handle, read from the handle. The write path compares
    ///   it again immediately before the rename (invariant 2: the parent must still be the
    ///   directory that was verified);
    /// - `parent_rel`: the parent's path relative to the workspace root, used ONLY to re-open the
    ///   same entry through the root handle for that final comparison;
    /// - `leaf`: the target's own name, relative to `dir`.
    ///
    /// A `ResolvedPath` is a public struct, so containment is re-derived from `abs` here rather
    /// than trusted, exactly as in `open_read`.
    #[cfg(unix)]
    pub(crate) fn open_write_parent(&self, canonical: &Path) -> Result<WriteParent, ToolError> {
        use std::os::unix::fs::MetadataExt;

        // Re-prove containment from the path alone first: a caller can hand-build a
        // `ResolvedPath`, and only this proves the path is in the workspace at all.
        let canonical = self.reprove_write_containment(canonical)?;
        let parent = canonical.parent().ok_or_else(|| {
            ToolError::new(
                ErrorCode::InvalidArgs,
                "Target has no parent directory",
                "Pass a path inside the workspace, not a filesystem root.",
            )
        })?;
        let leaf = canonical
            .file_name()
            .map(std::ffi::OsStr::to_os_string)
            .ok_or_else(|| {
                ToolError::new(
                    ErrorCode::InvalidArgs,
                    "Target has no file name",
                    "Pass a path to a file inside the workspace.",
                )
            })?;
        // Index 0 only: a read-only root is never a write target, however it was spelled.
        let parent_rel = parent
            .strip_prefix(&self.root)
            .map_err(|_| outside_error())?
            .to_owned();

        let oflags = rustix::fs::OFlags::RDONLY
            | rustix::fs::OFlags::DIRECTORY
            | rustix::fs::OFlags::NOFOLLOW
            | rustix::fs::OFlags::CLOEXEC;
        let root_fd = self.root_fd_at(0);
        // The root itself has an empty relative path, which no `openat`-family call accepts, so
        // re-open it as "." relative to the pinned descriptor: no component is resolved at all.
        let dir = if parent_rel.as_os_str().is_empty() {
            rustix::fs::openat(root_fd, ".", oflags, rustix::fs::Mode::empty())
                .map_err(|_| outside_error())?
        } else {
            open_beneath(root_fd, &parent_rel, oflags).map_err(|_| outside_error())?
        };

        // Ask the HANDLE what it is, never the path (T-03 / BND-07).
        let st = rustix::fs::fstat(&dir).map_err(|_| failed("could not be opened"))?;
        let identity = FileIdentity {
            // `dev_t` is an `i32` on macOS and a `u64` on Linux, so widen rather than assume.
            dev: st.st_dev as u64,
            ino: st.st_ino as u64,
        };
        // And confirm the handle really is the directory that `abs` names right now. Without this
        // the handle would be trusted on its own terms, which is the same mistake as trusting a
        // caller's `FileIdentity`.
        let disk_md = std::fs::metadata(parent).map_err(|_| failed("could not be opened"))?;
        if identity.dev != disk_md.dev() || identity.ino != disk_md.ino() {
            return Err(ToolError::new(
                ErrorCode::IoError,
                "The path was replaced while it was being opened.",
                "Retry the write; if it keeps failing the workspace is changing underneath.",
            ));
        }

        Ok(WriteParent {
            dir,
            identity,
            parent_rel,
            leaf,
        })
    }

    /// Re-open the parent directory entry through the PINNED ROOT HANDLE and compare its
    /// `dev`/`ino` with `held` (SEC-FIX 2, invariant 2).
    ///
    /// `held` alone cannot answer the question: a handle is not a name, so a directory that was
    /// moved out of the workspace and symlinked from its old place still has exactly the right
    /// `dev`/`ino` on the handle we are holding. What changed is whether the workspace's own
    /// entry still resolves to it. So the check is: open the same relative path again, from the
    /// root, with the same `BENEATH | NO_SYMLINKS` proof, and compare identities. A moved parent
    /// either fails to open (it is now a symlink) or opens to a different inode.
    #[cfg(unix)]
    pub(crate) fn parent_still_at_root(
        &self,
        parent_rel: &Path,
        held: &FileIdentity,
    ) -> Result<(), ToolError> {
        let oflags = rustix::fs::OFlags::RDONLY
            | rustix::fs::OFlags::DIRECTORY
            | rustix::fs::OFlags::NOFOLLOW
            | rustix::fs::OFlags::CLOEXEC;
        let root_fd = self.root_fd_at(0);
        let fresh = if parent_rel.as_os_str().is_empty() {
            rustix::fs::openat(root_fd, ".", oflags, rustix::fs::Mode::empty())
        } else {
            open_beneath(root_fd, parent_rel, oflags)
        };
        let Ok(fresh) = fresh else {
            return Err(parent_moved_error());
        };
        let st = rustix::fs::fstat(&fresh).map_err(|_| parent_moved_error())?;
        if st.st_dev as u64 != held.dev || st.st_ino as u64 != held.ino {
            return Err(parent_moved_error());
        }
        Ok(())
    }

    /// Refuse a target whose FINAL COMPONENT is a symlink, checked BEFORE anything canonicalises.
    ///
    /// `canonicalize()` resolves the leaf, so a check after it can never see a link: the old
    /// `is_symlink()` refusal in the write path was a dead branch that the module documentation
    /// still advertised (CR F1). This does the check on the path as the caller spelled it, and it
    /// refuses rather than following: writing through a link the caller did not name is a silent
    /// target substitution even when the link stays inside the workspace.
    ///
    /// Unix-only, for the same reason as [`Boundary::reprove_write_containment`]: its only caller
    /// is the unix-only write path.
    #[cfg(unix)]
    pub(crate) fn refuse_leaf_symlink(&self, abs: &Path) -> Result<(), ToolError> {
        match std::fs::symlink_metadata(abs) {
            Ok(md) if md.file_type().is_symlink() => Err(ToolError::new(
                ErrorCode::UnsupportedTarget,
                "Target is a link, not a file this tool may replace.",
                "Write to the real path, or remove the link and retry.",
            )),
            Ok(_) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(_) => Err(ToolError::new(
                ErrorCode::IoError,
                "Cannot inspect the target.",
                "Re-read the file and rebuild the plan.",
            )),
        }
    }

    /// Roots in canonical form: index 0 is the workspace, 1.. are the read roots.
    fn root_at(&self, index: usize) -> &Path {
        if index == 0 {
            &self.root
        } else {
            &self.read_roots[index - 1]
        }
    }

    /// The pinned directory handle for the root at `index`.
    #[cfg(unix)]
    fn root_fd_at(&self, index: usize) -> &rustix::fd::OwnedFd {
        if index == 0 {
            &self.root_fd
        } else {
            &self.read_root_fds[index - 1]
        }
    }

    /// Canonicalise as much of `candidate` as exists, then prove containment.
    ///
    /// Returns the root index that owns the result and the canonical absolute path.
    /// Containment is decided on the canonical path only: symlinks are expanded by the OS,
    /// never by us, so there is no string-level way to fake it (T-02).
    fn resolve_existing(&self, lex: &Lexical) -> Result<(usize, PathBuf, PathBuf), ToolError> {
        // BND-18/T-20: an absolute input may name anything on the machine, so an IO error
        // while walking it (EACCES, ENOTDIR, ENAMETOOLONG, a stale NFS handle) must be
        // indistinguishable from every other refusal - otherwise the error code is a probe
        // for what exists outside the workspace. A relative input is by construction inside
        // the workspace, so its IO errors are honest information and stay `io_error`.
        let walk_error = |what: &str| -> ToolError {
            if lex.absolute {
                outside_error()
            } else {
                failed(what)
            }
        };
        // Longest existing prefix of the candidate, component by component, without
        // following links (so we notice a planted one instead of walking through it).
        let mut saw_symlink = false;
        // Absolute Windows disk paths start at `C:\` (or whichever drive was spelled), not at
        // `/` — `PathBuf::from("/")` on Windows is a root-relative curiosity that never matches
        // a canonical `\\?\C:\…` workspace. Relative inputs stay anchored at the workspace.
        let mut prefix = if let Some(drive) = lex.drive.as_deref() {
            PathBuf::from(format!("{drive}\\"))
        } else if lex.absolute {
            PathBuf::from("/")
        } else {
            self.root.clone()
        };
        let mut remaining: Vec<&String> = lex.components.iter().collect();
        let mut tail: Vec<&String> = Vec::new();
        while let Some(c) = remaining.first() {
            prefix.push(c);
            match std::fs::symlink_metadata(&prefix) {
                Ok(md) => {
                    if md.file_type().is_symlink() {
                        saw_symlink = true;
                    }
                    remaining.remove(0);
                    continue;
                }
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                    prefix.pop();
                    tail.push(c);
                    break;
                }
                Err(_) => return Err(walk_error("could not be inspected")),
            }
        }

        // Canonicalise the deepest existing prefix; the OS expands its symlinks.
        let mut resolved = match prefix.canonicalize() {
            Ok(p) => p,
            // A link we cannot follow - dangling, or a loop (ELOOP). If a symlink was on
            // the way we cannot prove anything about where it led, so this is the plain
            // outside refusal and never `not_found`: a repository full of planted links
            // would otherwise be a probe for which paths outside the workspace exist
            // (BND-18, T-20). Refusing a dangling link that happens to point *inside* the
            // root is the deliberate price of that uniformity.
            Err(_) if saw_symlink => return Err(outside_error()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                // Absolute inputs (Unix `/…` or Windows `C:\…`) that never reached an
                // existing prefix under a root must not distinguish "missing" from
                // "outside" — that difference is the probe oracle (BND-18). Relative
                // inputs are inside the workspace by construction, so their absence is
                // honest `not_found`.
                if lex.absolute {
                    return Err(outside_error());
                }
                return Err(ToolError::new(
                    ErrorCode::NotFound,
                    "Path does not exist.",
                    "Check the spelling of the path.",
                ));
            }
            Err(_) => return Err(walk_error("its links could not be resolved")),
        };
        // Re-append the components that do not exist yet.
        for c in &tail {
            resolved.push(c);
        }

        // Containment, on the canonical path, component-wise (`starts_with`, never string
        // prefix: `/tmp/ab` must not match `/tmp/abc`).
        let owner = (0..=self.read_roots.len())
            .find(|i| is_under(&resolved, self.root_at(*i)))
            .ok_or_else(outside_error)?;

        // A missing target is only distinguishable from "outside" once we know it is
        // inside (BND-18): report `not_found` only in that case.
        if !resolved.exists() {
            return Err(ToolError::new(
                ErrorCode::NotFound,
                "Path does not exist.",
                "Check the spelling of the path.",
            ));
        }
        // The spelled path, links left unexpanded: only this one can show that the final
        // component was a symlink, which the write policy has to refuse.
        let mut lexical = if let Some(drive) = lex.drive.as_deref() {
            PathBuf::from(format!("{drive}\\"))
        } else if lex.absolute {
            PathBuf::from("/")
        } else {
            self.root.clone()
        };
        for c in &lex.components {
            lexical.push(c);
        }

        Ok((owner, resolved, lexical))
    }
}

/// Which kind of root is being validated; only read roots may be refused for the extra
/// reasons (credential directories, T-32).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RootKind {
    Workspace,
    ReadOnly,
}

/// Validate a set of extra READ-ONLY roots: one answer per root, in the order given.
///
/// This is [`check_root`] with `RootKind::ReadOnly`, exposed because a shell has to be able to
/// *report* on the roots the operator passed without building a whole [`Boundary`] to learn
/// whether they are acceptable — `doctor` needs the per-root verdict, and the refusals must come
/// from the same function that will later refuse them for real. A second list in the CLI would be
/// a second thing to forget, and the failure would be silent: the CLI would report a root as fine
/// and the boundary would then refuse it.
///
/// The answers are in the same order as the input, which is what makes them the `@root<N>`
/// labels (`root_at` indexes positionally).
pub fn validate_read_roots(roots: &[PathBuf]) -> Vec<Result<PathBuf, ToolError>> {
    roots
        .iter()
        .map(|r| check_root(r, RootKind::ReadOnly))
        .collect()
}

/// Refuse `/`, a drive/UNC root, a bare home directory and (for read roots) credential
/// directories; require the directory to exist; canonicalise what is left.
fn check_root(dir: &Path, kind: RootKind) -> Result<PathBuf, ToolError> {
    if dir.as_os_str().is_empty() {
        return Err(config_error(
            "A root directory is not set.",
            "Pass an existing directory.",
        ));
    }
    // Canonicalise FIRST, then judge. `.` and `proj` are perfectly good workspace roots -
    // `--workspace` defaults to the current directory - and judging the raw spelling is
    // what refused them. Judging the canonical path also closes `link-to-home`.
    //
    // But one spelling has to be judged *before* that call, because the call itself is the
    // attack: canonicalising a UNC path makes Windows resolve a host and open an SMB
    // session (BND-24). A network or device path never gets as far as canonicalisation.
    refuse_network_path(dir)?;
    let canonical = dir.canonicalize().map_err(|_| {
        config_error(
            "A root directory does not exist or cannot be read.",
            "Point --workspace (or --read-root) at an existing directory.",
        )
    })?;
    let md = std::fs::metadata(&canonical)
        .map_err(|_| config_error("A root directory cannot be read.", "Check its permissions."))?;
    if !md.is_dir() {
        return Err(config_error(
            "A root is not a directory.",
            "Point --workspace (or --read-root) at a directory.",
        ));
    }
    if is_filesystem_root(&canonical) {
        return Err(config_error(
            "A filesystem root is refused as a workspace root.",
            "Choose a directory inside the tree you want to work on.",
        ));
    }
    if let Some(home) = home_dir() {
        if same_dir(&canonical, &home) {
            return Err(config_error(
                "The home directory itself is refused as a root.",
                "Choose a project directory instead of the home directory.",
            ));
        }
        if kind == RootKind::ReadOnly {
            for name in CREDENTIAL_DIRS {
                if same_dir(&canonical, &home.join(name)) {
                    return Err(config_error(
                        "A credential directory is refused as a read root.",
                        "Choose a directory that holds no keys or cloud credentials.",
                    ));
                }
            }
        }
    }
    Ok(canonical)
}

/// Refuse a path that names the network, a device, or a drive the caller did not name
/// *before* any filesystem call, at every entry point that takes an operator-supplied root
/// (BND-24).
///
/// `\\server\share` and `\\.\pipe\x` are not odd names: on Windows, canonicalising one makes
/// the kernel resolve a host name and open an SMB session. Handed an attacker-chosen host that
/// is a DNS lookup plus an NTLM challenge - an outbound authentication request to a machine the
/// attacker controls, and a hang while the timeout expires. So the string is judged first, on
/// every platform, and never reaches the filesystem.
///
/// The same call is where drive-*relative* spellings die, and that one is not only an oracle.
/// `C:proj` canonicalises on Windows to *the current directory of drive C* - a directory the
/// process chose, not the caller - so as a `--workspace` or `--read-root` it is a location
/// escape rather than a mistake. `C:\proj` is a perfectly good root and stays legal: the
/// refusal is exactly the drive-relative shape, `letter:` with nothing that separates it from
/// a path.
///
/// One message for all three spellings, so refusing a UNC path, a device path and a
/// drive-relative path are indistinguishable to a caller - which is the point of judging the
/// string rather than reporting what canonicalisation thought of it.
///
/// **The verbatim exception.** `\\?\C:\x` is *not* one of the three, and is allowed: it is a
/// local disk, and it is what `canonicalize` itself returns on Windows for a perfectly ordinary
/// `C:\...` root, so refusing it makes the most common Windows root unusable. `\\?\UNC\...`,
/// `\\?\GLOBALROOT\...`, `\\?\Volume{...}\...` and every other namespace under the prefix keep
/// the refusal, and so does `\\.\...` outright. See `is_verbatim_disk_path`. This exception
/// applies to **operator-supplied roots only** (`--workspace`, `--read-root`, `workspace_id`):
/// a `\\?\` anywhere in an agent-supplied relative path stays refused, because there the prefix
/// is never canonicalisation output - it is only ever attacker input.
pub(crate) fn refuse_network_path(path: &Path) -> Result<(), ToolError> {
    let Some(s) = path.to_str() else {
        // Not valid UTF-8: nothing here can be a network path spelling, and the caller's
        // input check has already refused what it cannot spell.
        return Ok(());
    };
    let b = s.as_bytes();
    // Two leading separators in any mixture: UNC. The `\\?\` and `\\.\` device prefixes both
    // start with two backslashes and so are already covered; they are named here because the
    // reason a caller reaches for is "device paths", not "UNC".
    //
    // The one exception is a *verbatim disk* path, `\\?\C:\x`: two separators, but naming a
    // local volume, not a host. Windows' own `canonicalize` hands that form back - it is what
    // `GetFinalPathNameByHandle` returns for an ordinary `C:\...` root - so refusing it
    // refuses the output of the tool's own canonicalisation on the most common Windows root
    // there is. `\\?\` also means "stop normalising": the rest of the string is passed to the
    // object manager verbatim, which is precisely what makes the *other* `\\?\` forms
    // dangerous and why they stay refused (BND-24). The letter must be followed by `:` and a
    // separator, so `\\?\UNC\...`, `\\?\GLOBALROOT\...`, `\\?\Volume{...}\...`,
    // `\\?\pipe\...` and a bare `\\?\C:` all keep the two-separator refusal.
    let two_separators = matches!(b.first(), Some(b'\\') | Some(b'/'))
        && matches!(b.get(1), Some(b'\\') | Some(b'/'))
        && !is_verbatim_disk_path(b);
    // A drive with no separator after the colon: drive-relative, resolved by Windows against
    // that drive's current directory. A separator makes it an ordinary absolute path.
    let drive_relative = b.len() >= 2
        && b[0].is_ascii_alphabetic()
        && b[1] == b':'
        && !matches!(b.get(2), Some(b'\\') | Some(b'/'));
    if two_separators || drive_relative {
        return Err(network_root_error());
    }
    Ok(())
}

/// True for `\\?\C:\x` — the verbatim (extended-length) spelling of a **local disk** path.
///
/// Windows' `GetFinalPathNameByHandle`, and therefore Rust's `canonicalize`, returns
/// `\\?\C:\Users\…` for an ordinary `C:\Users\…`. That string is a local volume, not a host:
/// there is no name to resolve and no session to open, so refusing it would refuse the output
/// of the tool's own canonicalisation and make `C:\…` unusable as a `--workspace` or
/// `--read-root` on the platform where it is spelled most naturally (BND-24).
///
/// `\\?\` means "hand the rest of the string to the object manager verbatim, with no
/// normalisation", which is exactly why the *non*-disk forms under it stay refused: they are the
/// escape hatch out of path normalisation and into the device namespaces. The shape therefore
/// has to be a drive letter, then a colon, then a separator — no more, no less:
///
/// * allowed: `\\?\C:\x`, `\\?\z:/x` (any letter, either separator, any case);
/// * refused: `\\?\UNC\srv\share` (a network location under the verbatim prefix — the
///   whole reason the prefix is special), `\\?\GLOBALROOT\…` (the user-visible root of a
///   filtered block device, `??\C:\` under the hood, reachable only with elevation),
///   `\\?\Volume{…}\…` (a volume GUID — no canonical form, so a caller cannot hold a stable
///   identity for it), `\\?\pipe\…` and any other namespace, and a bare `\\?\C:` with no
///   separator, which is the drive-*relative* form the separate rule already refuses.
///
/// `\\.\…` is never allowed: that prefix names the device namespace outright.
fn is_verbatim_disk_path(b: &[u8]) -> bool {
    // `\\?\` or `\\.\`? Only the `?` form is considered here, and only a disk.
    let rest = match b.get(..4) {
        Some(r) if r == b"\\\\?\\" => &b[4..],
        _ => return false,
    };
    // `<letter>:` then a separator: `\\?\C:\x`.
    rest.len() >= 3
        && rest[0].is_ascii_alphabetic()
        && rest[1] == b':'
        && matches!(rest[2], b'\\' | b'/')
}

/// The one refusal for a root that names the network, a device, or a drive implicitly.
fn network_root_error() -> ToolError {
    config_error(
        "Network, device and drive-relative paths are not supported as workspace roots.",
        "Point --workspace (or --read-root) at a directory on a local filesystem, spelled \
         with a separator after any drive letter.",
    )
}

/// Credential directories that must never become a read root (T-32, CFG-07).
const CREDENTIAL_DIRS: &[&str] = &[".ssh", ".gnupg", ".aws", ".config/gcloud"];

/// Reject anything that is not a plain, printable, single-line path string.
///
/// C0/C1 control characters and DEL are `char::is_control`. The zero-width, bidirectional
/// and invisible characters are listed explicitly because they are *format* characters, not
/// control characters: they are how a path that says `.git` is made to display as something
/// else (T-34), and a path we cannot read back is a path we refuse.
fn check_input_chars(path: &str) -> Result<(), ToolError> {
    if path.is_empty() {
        return Err(ToolError::new(
            ErrorCode::InvalidArgs,
            "The path is empty.",
            "Pass a non-empty path relative to the workspace root.",
        ));
    }
    for c in path.chars() {
        if is_refused_char(c) {
            return Err(ToolError::new(
                ErrorCode::InvalidArgs,
                "The path contains a control or invisible character.",
                "Remove the control or invisible characters from the path.",
            ));
        }
    }
    Ok(())
}

/// True for control, zero-width, bidi and other invisible characters.
fn is_refused_char(c: char) -> bool {
    c.is_control()
        || matches!(
            c,
            '\u{00ad}' | '\u{200b}'..='\u{200f}' | '\u{2028}' | '\u{2029}' | '\u{202a}'..='\u{202e}'
                | '\u{2060}'..='\u{2064}' | '\u{2066}'..='\u{2069}' | '\u{feff}'
        )
}

/// Length and depth limits (BND-20, T-06).
///
/// `limits` is the boundary's, never a freshly built default: the point of SECFIX5-01 is
/// that this check obeys the value the operator configured. The messages name the ceiling
/// that was actually exceeded, because a limit nobody can see is one nobody can satisfy.
fn check_size(path: &str, limits: &Limits) -> Result<(), ToolError> {
    if path.len() as u64 > limits.path_max_bytes {
        return Err(ToolError::new(
            ErrorCode::LimitExceeded,
            format!(
                "The path is longer than the maximum path length ({} bytes).",
                limits.path_max_bytes
            ),
            "Shorten the path or work closer to the workspace root, or raise \
             limits.path_max_bytes in the user configuration.",
        ));
    }
    let depth = path.split('/').filter(|c| !c.is_empty()).count() as u64;
    // The EFFECTIVE depth ceiling, not the raw field: `path_max_depth` is the one
    // operator-tunable limit, so a request above `PATH_MAX_DEPTH_HARD` is clamped rather
    // than refused, and the guard stays bound. Reading the field directly here would let an
    // operator's 999999 defeat the check outright.
    let max_depth = limits.clamped_path_max_depth();
    if depth > max_depth {
        return Err(ToolError::new(
            ErrorCode::LimitExceeded,
            format!("The path has more components than the maximum path depth ({max_depth})."),
            "Use a shallower path inside the workspace root, or raise \
             limits.path_max_depth in the user configuration.",
        ));
    }
    Ok(())
}

/// Purely lexical normalisation: `.`, empty components and `..` are resolved, and an
/// escape above the starting point is refused (BND-01).
///
/// Backslash is treated as a separator *for the escape analysis only*: on Unix it is a
/// perfectly legal file-name character, but `src\..\..\x` must not be a way to spell a
/// traversal that a Windows client would execute, so a backslash-separated `..` is checked
/// and refused here and the original bytes are still what the filesystem sees.
fn lexically_normalise(path: &str) -> Result<Lexical, ToolError> {
    // Extended-length / device prefixes are refused before anything else (BND-11, BND-24).
    if has_extended_length_prefix(path) {
        return Err(outside_error());
    }

    // Absolute Windows disk paths (`C:\proj\a.rs`, `C:/proj/a.rs`) are accepted here and
    // judged by containment later — the same rule Unix absolute paths already get
    // (`/tmp/ws/a.rs` inside the workspace is fine; `/etc/passwd` is not). Drive-*relative*
    // (`C:foo`), UNC, root-relative (`\Windows`) and ADS stay refused below.
    if let Some((drive, rest)) = windows_absolute_disk(path) {
        let mut components: Vec<String> = Vec::new();
        for c in rest.split(['/', '\\']) {
            match c {
                "" | "." => {}
                ".." => {
                    if components.pop().is_none() {
                        return Err(outside_error());
                    }
                }
                other if other.contains(':') => return Err(outside_error()),
                other => {
                    if cfg!(windows) && windows_name_hazard(other) {
                        return Err(outside_error());
                    }
                    components.push(other.to_string());
                }
            }
        }
        return Ok(Lexical {
            absolute: true,
            drive: Some(drive),
            components,
        });
    }

    // Windows spellings of "somewhere else entirely": a drive-relative, a UNC or a
    // root-relative prefix, or a colon in a component (an alternate data stream). On Unix
    // these are odd file names, but they are never workspace-relative paths an agent means,
    // and refusing them on every platform keeps one rule (BND-02) - and keeps the rule
    // testable on Linux.
    if has_drive_or_unc_prefix(path) || has_colon_component(path) {
        return Err(outside_error());
    }

    // A component Windows cannot name as written, or that names a device rather than a file
    // (BND-11). This one is genuinely platform-dependent, so unlike the rules above it is
    // *consulted* only on Windows - via `cfg!` rather than `#[cfg]`, so the code is compiled,
    // linted and reviewable on every platform and the tests below can call the pure function
    // directly. `aux.rs` and `con.go` are ordinary files on Unix, and a real project has them;
    // refusing them there would be refusing the code base, not defending it.
    //
    // The `outside_error()` rather than an I/O error is the whole point: on Windows,
    // `resolve_read(">>")` otherwise fails in `open` with `ERROR_INVALID_NAME`, which arrives
    // as `io_error`, while a genuinely absent path arrives as `not_found`. Same answer, two
    // codes, and the difference tells an attacker which names are illegal - the pure form of
    // the probe oracle (BND-18, T-20) that the drive rules above exist to prevent.
    if cfg!(windows) && path.split(['/', '\\']).any(windows_name_hazard) {
        return Err(outside_error());
    }

    let absolute = path.starts_with('/');
    let mut components: Vec<String> = Vec::new();
    for c in path.split('/') {
        match c {
            "" | "." => {}
            ".." => {
                if components.pop().is_none() {
                    return Err(outside_error());
                }
            }
            other => components.push(other.to_string()),
        }
    }

    // Same traversal analysis with `\` as the separator (BND-01).
    let mut depth: i32 = 0;
    for c in path.split(['/', '\\']) {
        match c {
            "" | "." => {}
            ".." => {
                depth -= 1;
                if depth < 0 {
                    return Err(outside_error());
                }
            }
            _ => depth += 1,
        }
    }

    Ok(Lexical {
        absolute,
        drive: None,
        components,
    })
}

/// An absolute Windows disk path: `letter:` then a separator, then the rest.
///
/// Returns `(drive_with_colon, rest_after_separator)`. Drive-*relative* spellings
/// (`C:foo`, `C:`) are deliberately not matched — those stay with
/// [`has_drive_or_unc_prefix`].
fn windows_absolute_disk(path: &str) -> Option<(String, &str)> {
    let b = path.as_bytes();
    if b.len() >= 3 && b[0].is_ascii_alphabetic() && b[1] == b':' && (b[2] == b'\\' || b[2] == b'/')
    {
        Some((path[..2].to_string(), &path[3..]))
    } else {
        None
    }
}

/// True for anything that names somewhere else *by spelling*: `C:\x`, `C:foo`, `C:`,
/// `\\server\share`, `//server/share`, `\/server`, `\x` (BND-02, BND-11).
///
/// Two rules, and the difference matters:
///
/// * **Rule 1 - the prefix.** A single ASCII letter followed by `:` is a drive, whatever the
///   third byte is. On Windows `C:foo` is not a file named `C:foo`; it is a path relative to
///   the current directory *of drive C*, a directory the process picked, not the caller. The
///   older test (`X:` plus a separator) let `C:foo` and `C:.:\WiWi` through to the
///   filesystem, which answered `io_error` where an absent path answers `outside_workspace` -
///   one question, two answers, and an attacker learns which. Any two leading separators in
///   any mixture are a UNC path: `\\` is what a Windows caller writes, but a path that came
///   through JSON, a shell or a URL parser arrives as `//ser/share` or `\/ser`.
/// * **Rule 2 - any component with a colon.** That is `has_colon_component`, checked
///   separately, because it is an NTFS alternate data stream and not a location.
///
/// One leading `/` is deliberately *not* here: that is an ordinary absolute path, and refusing
/// it would refuse every absolute path an agent legitimately receives.
fn has_drive_or_unc_prefix(path: &str) -> bool {
    let b = path.as_bytes();
    // A single leading backslash is Windows' root-relative spelling (`\Windows`).
    if b.first() == Some(&b'\\') {
        return true;
    }
    // Rule 1(a): a drive letter, with anything - or nothing - after the colon.
    if b.len() >= 2 && b[0].is_ascii_alphabetic() && b[1] == b':' {
        return true;
    }
    // Rule 1(b): two leading separators in any mixture.
    matches!(b.first(), Some(&b'/')) && matches!(b.get(1), Some(&b'/') | Some(&b'\\'))
}

/// True for a `\\?\` or `\\.\` extended-length / device prefix **anywhere** in the string,
/// not only at the front (BND-11, BND-24).
///
/// The operator-root rule gets an exception for the verbatim *disk* form, because
/// `canonicalize` itself produces it on Windows and a root has to survive its own
/// canonicalisation. That exception has no meaning here. An agent-supplied relative path is
/// never the output of a canonicalisation - the prefix can only ever be input - so there is no
/// shape for which a caller genuinely needs it, and refusing it costs nothing.
///
/// The prefix is checked *anywhere*, not just at the start, and that is the half the
/// position-only rule above misses. `\\?\` means "the rest of this string is not normalised":
/// the double separator stops `..` collapsing and stops trailing dots being stripped, so a
/// `..` or `.` placed after it is passed through to the object manager as a real parent or
/// current-directory reference. `a/\\?\../..` is therefore an escape spelled as an ordinary
/// relative path, and the only place that sees it is here.
///
/// This also costs something on Unix, like the colon rule: a file literally named `\\?\x`
/// becomes unreachable. Same trade, same reason, and the same reasoning - see `TOOLS.md`.
fn has_extended_length_prefix(path: &str) -> bool {
    // `\\?\` and `\\.\`, each accepting the mixed separator spellings a JSON payload or a URL
    // parser can produce (`\/`, `/\`, `//`). The `?`/`.` is what distinguishes the two
    // prefixes; a bare `\\` is plain UNC and is already rule 1(b) when it leads the string.
    let bytes = path.as_bytes();
    bytes.windows(4).any(|w| {
        (w[0] == b'\\' || w[0] == b'/')
            && (w[1] == b'\\' || w[1] == b'/')
            && (w[2] == b'?' || w[2] == b'.')
            && (w[3] == b'\\' || w[3] == b'/')
    })
}

/// True for any component carrying a `:`: an NTFS alternate data stream.
///
/// `file.txt:stream` addresses bytes that are not in the file the agent can reason about, and
/// on Windows it is a *write* target that no `stat` mentions. Refused on every platform, so
/// the rule is testable on Linux, at the price that a Unix file whose name contains a colon is
/// unreachable. That price is paid deliberately: one rule that behaves identically everywhere
/// is worth more than a Unix-shaped hole that only Windows clients can feel.
fn has_colon_component(path: &str) -> bool {
    path.split(['/', '\\']).any(|c| c.contains(':'))
}

/// True for a single path component that Windows cannot name the way the caller wrote it, or
/// that names something other than the file it looks like (BND-11, T-04).
///
/// Purely a string function, on every platform, so it is unit-testable on Linux and the rule
/// it feeds is one rule rather than a platform fork of one. It is *consulted* only under
/// `cfg!(windows)` — see `lexically_normalise` — because on Unix `aux.rs` and `con.go` are
/// ordinary file names that real projects use, and refusing them would be refusing the code
/// base rather than defending it.
///
/// Three classes, and each of them is a way for the string the boundary checked and the string
/// the filesystem opened to be *different names*:
///
/// 1. **Characters Windows rejects outright** — `<` `>` `"` `|` `?` `*` and the C0/C1 control
///    characters. A component carrying one of these never exists: `open(">>")` fails with
///    `ERROR_INVALID_NAME`, which `std::io` reports as an I/O error, while an absent path
///    reports not-found. Left alone, that difference is a probe oracle in its purest form —
///    the error code *tells the attacker whether a name is illegal*, and the fuzzer found it
///    (`resolve_read(">>")` answered `io_error` on the Windows job).
/// 2. **A trailing `.` or space.** Windows silently strips them: `foo.` and `foo` are the same
///    file, and `foo ` is `foo`. So a path the boundary judged to be one file is opened as
///    another — the direct analogue of Unix's trailing-slash aliasing, and the reason a
///    protected-name or extension check on the spelled form can be walked straight past. The
///    components `.` and `..` are excluded: they are path *grammar*, they are consumed before
///    any component list exists, and they are already the subject of the traversal rule.
///    `...` is **not** excluded — it is neither `.` nor `..`, it is an ordinary name, and
///    Windows strips its trailing dot, so it belongs here. That is a decision, not an
///    oversight: it keeps the rule stated once ("a trailing dot is stripped, except for the
///    two components that are not names") instead of as a growing list of exceptions.
/// 3. **Reserved device names**, in any case, with or without an extension, and with or without
///    a trailing dot or space: `CON` `PRN` `AUX` `NUL` `COM1`–`COM9` `LPT1`–`LPT9`, plus the
///    superscript-digit spellings Windows also accepts (`COM¹`, `LPT³`). `CON.txt` and `con.`
///    are not files, they are the console device — a write goes to a character device and a
///    read blocks forever, and the boundary's whole job is that the thing it approved is the
///    thing that got opened. Note this is about the component *name*: `com0`, `com10` and
///    `COM` are ordinary files, and so are `console` and `conf`; only the exact base name,
///    up to the first dot, is reserved.
///
/// Note what is deliberately **not** here: the length limit, the 8.3 short-name form, and the
/// case-insensitivity. Those are properties of a path that has been *resolved* by the
/// filesystem rather than of the string, and the boundary already re-checks containment and
/// the protected list against the canonical form, so they are handled where they can be
/// measured rather than guessed at.
fn windows_name_hazard(component: &str) -> bool {
    // Rule 1: a character Windows will not accept in a file name at all.
    if component
        .chars()
        .any(|c| matches!(c, '<' | '>' | '"' | '|' | '?' | '*') || c.is_control())
    {
        return true;
    }
    // Rule 2: a trailing dot or space, which Windows strips. `.` and `..` are path grammar, not
    // names, and are resolved by the caller before they reach here.
    if component != "."
        && component != ".."
        && component
            .chars()
            .next_back()
            .is_some_and(|c| c == '.' || c.is_whitespace())
    {
        return true;
    }
    // Rule 3: a reserved device name. Everything after the first dot is the "extension" and is
    // ignored by Windows when matching the device, so `con.txt` is the console.
    let base = component.split('.').next().unwrap_or(component);
    // A trailing space is stripped before the device name is compared, so `con ` is `con`.
    let base = base.trim_end();
    is_reserved_device_name(base)
}

/// Reserved Windows device names, compared case-insensitively.
///
/// The superscript forms are here because Windows accepts them: `COM¹`, `COM²` and `COM³` are
/// the same devices as `COM1`–`COM3`, and so are `LPT¹`–`LPT³`.
fn is_reserved_device_name(base: &str) -> bool {
    const NAMES: &[&str] = &["CON", "PRN", "AUX", "NUL"];
    if NAMES.iter().any(|n| base.eq_ignore_ascii_case(n)) {
        return true;
    }
    // `COM1`–`COM9` / `LPT1`–`LPT9`, plus the three superscript spellings. What must *not*
    // match is `com0`, `com10`, `COM` or `communicate` - ordinary file names in any real
    // project - so the digit has to be the whole remainder, not a prefix of it.
    //
    // `to_ascii_uppercase` folds `com` to `COM` and leaves a superscript digit alone, since
    // it has no ASCII case. That is why the digits are matched by an explicit list rather than
    // by a `1..=9` range, and it is why the comparison can be a plain `strip_prefix`.
    for prefix in ["COM", "LPT"] {
        // The fold is bound, not inlined: the borrow of the owned upper-case string has to
        // outlive the comparison that reads through it.
        let upper = base.to_ascii_uppercase();
        let Some(rest) = upper.strip_prefix(prefix) else {
            continue;
        };
        let mut digits = rest.chars();
        let one_digit = matches!(
            digits.next(),
            Some('1'..='9' | '\u{b9}' | '\u{b2}' | '\u{b3}')
        );
        if one_digit && digits.next().is_none() {
            return true;
        }
    }
    false
}

/// `/`, `C:\`, `\\server\share`. Always called on a CANONICAL path, so a relative
/// spelling such as `.` or `proj` is never mistaken for a root.
fn is_filesystem_root(p: &Path) -> bool {
    p.parent().is_none()
        || matches!(
            p.components().next_back(),
            Some(Component::RootDir) | Some(Component::Prefix(_))
        )
}

/// Component-wise containment. Never a string prefix (`/tmp/ab` vs `/tmp/abc`).
fn is_under(path: &Path, root: &Path) -> bool {
    path.starts_with(root)
}

/// Same directory, tolerant of a trailing separator and of a missing trailing component.
fn same_dir(a: &Path, b: &Path) -> bool {
    match (a.canonicalize(), b.canonicalize()) {
        (Ok(x), Ok(y)) => x == y,
        _ => false,
    }
}

fn home_dir() -> Option<PathBuf> {
    // Windows runners and desktop installs set USERPROFILE, not HOME. Prefer that there so
    // CFG-07's "refuse the home directory as a root" is not silently skipped on the one
    // platform where agents are most likely to pass it.
    #[cfg(windows)]
    {
        std::env::var_os("USERPROFILE")
            .or_else(|| std::env::var_os("HOME"))
            .map(PathBuf::from)
            .filter(|p| !p.as_os_str().is_empty())
    }
    #[cfg(not(windows))]
    {
        std::env::var_os("HOME")
            .map(PathBuf::from)
            .filter(|p| !p.as_os_str().is_empty())
    }
}

/// `/`-separated relative display path. Read roots are shown as `@root<N>/relative`
/// (OUT-02): an absolute path never reaches an agent.
fn display_rel(canonical: &Path, root: &Path, root_index: usize) -> String {
    let rel = canonical
        .strip_prefix(root)
        .map(|p| p.to_string_lossy().replace('\\', "/"))
        .unwrap_or_default();
    if root_index == 0 {
        if rel.is_empty() { ".".to_string() } else { rel }
    } else {
        format!("@root{root_index}/{rel}")
    }
}

/// The one wording for "this is not a directory". Exposed inside the crate because the
/// walker has to tell this apart from every other listing failure (a start that is a
/// regular file is a legal walk start, everything else is an error) without matching on
/// arbitrary text of its own. Contains no path.
///
/// Produced by the unix `openat` path and by the Windows `read_dir` path when the start is
/// a regular file — both so `walk` can treat a file start as legal.
pub(crate) const NOT_A_DIRECTORY: &str = "The path is not a directory.";

/// Map a failed directory open without echoing the OS message (which can carry foreign
/// text). "Not a directory" gets the one shared wording; every other errno goes through
/// the same mapping as a failed file open, so a symlink swapped in for the directory and a
/// directory that vanished are worded the same way whether we were listing or opening.
#[cfg(unix)]
fn listing_error(code: rustix::io::Errno) -> ToolError {
    if code.raw_os_error() == rustix::io::Errno::NOTDIR.raw_os_error() {
        return ToolError::new(
            ErrorCode::IoError,
            NOT_A_DIRECTORY,
            "Point the listing at a directory.",
        );
    }
    open_error(code)
}

/// The one outside-workspace refusal (BND-18): no path, no existence hint.
pub(crate) fn outside_error() -> ToolError {
    ToolError::new(ErrorCode::OutsideWorkspace, OUTSIDE_MESSAGE, OUTSIDE_NEXT)
}

fn failed(what: &str) -> ToolError {
    ToolError::new(
        ErrorCode::IoError,
        format!("The path {what}."),
        "Check the path and the permissions of its directories.",
    )
}

fn config_error(what: &str, next: &str) -> ToolError {
    ToolError::new(ErrorCode::InvalidArgs, what, next)
}

fn unsupported(what: &str) -> ToolError {
    ToolError::new(
        ErrorCode::UnsupportedTarget,
        what,
        "Pick a regular, single-linked, writable file that is not on the protected list.",
    )
}

fn protected_error(what: &str, next: &str) -> ToolError {
    ToolError::new(ErrorCode::ProtectedPath, what, next)
}

/// The refusal for "the target's parent directory is no longer the directory the workspace
/// names" (SEC-FIX 2 / F-01, invariant 2).
///
/// One wording for both ways it is discovered — the entry no longer opens (it was replaced by a
/// symlink) and it opens to a different inode (it was replaced by another directory) — because
/// from the caller's side those are the same situation: the plan it verified no longer describes
/// where the write would land. It repeats nothing about the directory's own metadata (F-04).
#[cfg(unix)]
fn parent_moved_error() -> ToolError {
    ToolError::new(
        ErrorCode::IoError,
        "The target's directory changed while the write was in progress.",
        "Re-read the file and rebuild the plan; nothing was written.",
    )
}

/// Open the root directory itself as a descriptor, so later opens are relative to the
/// directory we validated instead of to its name (T-03). `O_NOFOLLOW` because a root must be
/// a real directory, `O_DIRECTORY` so a file is refused here rather than at read time.
#[cfg(unix)]
fn open_root_dir(dir: &Path) -> Result<rustix::fd::OwnedFd, ToolError> {
    rustix::fs::openat(
        rustix::fs::CWD,
        dir,
        rustix::fs::OFlags::RDONLY
            | rustix::fs::OFlags::DIRECTORY
            | rustix::fs::OFlags::NOFOLLOW
            | rustix::fs::OFlags::CLOEXEC,
        rustix::fs::Mode::empty(),
    )
    .map_err(|_| {
        config_error(
            "A root directory could not be opened.",
            "Check that the root exists and is readable.",
        )
    })
}

/// Flags for the final component of an open. Non-blocking is what turns a FIFO from a hang
/// into a refusal (BND-22); no-follow is what stops a link swapped in for the file itself.
#[cfg(unix)]
fn last_component_flags() -> rustix::fs::OFlags {
    rustix::fs::OFlags::NOFOLLOW | rustix::fs::OFlags::NONBLOCK
}

/// Open `relative` beneath `root_fd`, refusing to follow any symlink and refusing to leave
/// the root (T-03).
#[cfg(unix)]
fn open_beneath(
    root_fd: &rustix::fd::OwnedFd,
    relative: &Path,
    oflags: rustix::fs::OFlags,
) -> Result<rustix::fd::OwnedFd, rustix::io::Errno> {
    #[cfg(target_os = "linux")]
    {
        use rustix::fs::ResolveFlags;
        let resolve =
            ResolveFlags::BENEATH | ResolveFlags::NO_SYMLINKS | ResolveFlags::NO_MAGICLINKS;
        match rustix::fs::openat2(
            root_fd,
            relative,
            oflags,
            rustix::fs::Mode::empty(),
            resolve,
        ) {
            Ok(fd) => return Ok(fd),
            // Kernels before 5.6 have no `openat2` at all. The component walk below is the
            // same guarantee written out by hand, so fall back to it rather than fail.
            // Only ENOSYS falls back: EINVAL and EPERM are real answers and are reported.
            Err(rustix::io::Errno::NOSYS) => {}
            Err(e) => return Err(e),
        }
    }
    walk_components(root_fd, relative, oflags)
}

/// POSIX-only fallback for platforms without `openat2`: descend one component at a time,
/// each with `O_NOFOLLOW | O_DIRECTORY`, and open the last component with the caller's
/// flags. No step can be a symlink and no step can leave the directory we are already in.
#[cfg(unix)]
fn walk_components(
    root_fd: &rustix::fd::OwnedFd,
    relative: &Path,
    oflags: rustix::fs::OFlags,
) -> Result<rustix::fd::OwnedFd, rustix::io::Errno> {
    use rustix::fs::OFlags;
    let mut dir = root_fd.try_clone().map_err(|_| rustix::io::Errno::BADF)?;
    let mut comps = relative.components().peekable();
    if comps.peek().is_none() {
        return Err(rustix::io::Errno::NOENT);
    }
    while let Some(c) = comps.next() {
        let name = match c {
            std::path::Component::Normal(n) => n,
            _ => return Err(rustix::io::Errno::INVAL),
        };
        let flags = if comps.peek().is_none() {
            oflags | OFlags::RDONLY
        } else {
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC
        };
        dir = rustix::fs::openat(&dir, name, flags, rustix::fs::Mode::empty())?;
    }
    Ok(dir)
}

/// True for the errnos that mean "this node is not something you read as a file".
///
/// Linux answers `ENXIO` when a unix socket is opened, macOS answers `EOPNOTSUPP`, and
/// device nodes add `ENODEV`. They are the same situation and must be reported the same
/// way: the caller counts skipped special files from this wording (OUT-07), and it must not
/// have to know which kernel said what. `ENOTSUP` and `EOPNOTSUPP` are the same value on
/// both kernels, so this compares values instead of matching patterns (two identical
/// patterns would be an unreachable arm).
#[cfg(unix)]
fn is_special_node_errno(code: rustix::io::Errno) -> bool {
    let raw = code.raw_os_error();
    raw == rustix::io::Errno::NXIO.raw_os_error()
        || raw == rustix::io::Errno::NODEV.raw_os_error()
        || raw == rustix::io::Errno::OPNOTSUPP.raw_os_error()
        || raw == rustix::io::Errno::NOTSUP.raw_os_error()
}

/// The refusal for a node that cannot be read as a regular file. One message for every
/// platform and every errno, because the caller keys off it to count skipped files. Used by
/// both platforms' read path, so it is not gated.
fn special_file_error() -> ToolError {
    ToolError::new(
        ErrorCode::IoError,
        "The path is a special file, not a regular file.",
        "Point the read at a regular file; special files are skipped.",
    )
}

/// Map an `open` failure without echoing the OS message: it can carry foreign text.
/// `ELOOP` means the final component became a symlink between resolve and open, which is
/// exactly the race BND-07 is about, so it says so; `ENOENT` is the honest answer when the
/// file disappeared on its own.
#[cfg(unix)]
fn open_error(code: rustix::io::Errno) -> ToolError {
    if is_special_node_errno(code) {
        return special_file_error();
    }
    let raw = code.raw_os_error();
    if raw == rustix::io::Errno::LOOP.raw_os_error() {
        return ToolError::new(
            ErrorCode::IoError,
            "The path was replaced by a symlink while it was being opened.",
            "Retry the read; a write target must never be reached through a symlink.",
        );
    }
    if raw == rustix::io::Errno::NOENT.raw_os_error() {
        return ToolError::new(
            ErrorCode::NotFound,
            "Path does not exist.",
            "Check the spelling of the path.",
        );
    }
    failed("could not be opened")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The code a refusal carries, without asking `Lexical` - which has no `Debug` and no
    /// reason to grow one for a test - to describe itself.
    fn refusal_code(result: Result<Lexical, ToolError>) -> ErrorCode {
        match result {
            Ok(l) => panic!(
                "expected a refusal, got a resolved path: {:?}",
                l.components
            ),
            Err(e) => e.code,
        }
    }

    #[test]
    fn lexical_normalisation() {
        let l = lexically_normalise("src/./a//b").unwrap();
        assert_eq!(l.components, ["src", "a", "b"]);
        assert!(!l.absolute);
        assert!(lexically_normalise("/a/b").unwrap().absolute);
        assert!(lexically_normalise("src\\..\\..\\x").is_err());
        // Absolute disk paths are accepted lexically; containment decides later.
        let win = lexically_normalise("C:\\x").unwrap();
        assert!(win.absolute);
        assert_eq!(win.drive.as_deref(), Some("C:"));
        assert_eq!(win.components, ["x"]);
        // Drive-relative and extended-length forms stay refused.
        assert!(lexically_normalise("C:x").is_err());
        assert!(lexically_normalise("\\\\?\\C:\\x").is_err());
        // ...including the verbatim *disk* form, which the operator-root check allows. That
        // exception is for `--workspace` / `--read-root`; an agent path is never
        // canonicalisation output, so the prefix there is only ever input.
        assert!(lexically_normalise("\\\\?\\UNC\\srv\\s").is_err());
        // And *anywhere* in the path, not only at the front. `\\?\` means "what follows is
        // not normalised", so a `..` behind it stays a real parent reference and the escape
        // analysis above never gets to collapse it - `a/\\?\../..` is an escape spelled as an
        // ordinary relative path.
        assert!(
            lexically_normalise("a/\\\\?\\b").is_err(),
            "an extended-length prefix mid-path must be refused"
        );
        assert!(
            lexically_normalise("a/\\\\?\\..").is_err(),
            "`..` behind an extended-length prefix must not survive normalisation"
        );
        assert!(lexically_normalise("dir\\\\.\\x").is_err());
        // The mixed separator spellings a JSON payload or URL parser produces, in the third
        // position as well as the fourth.
        assert!(lexically_normalise("a/\\\\/?\\b").is_err());
        assert!(lexically_normalise("a/\\\\?/b").is_err());
        assert!(lexically_normalise("a\\\\?/../x").is_err());
        assert!(lexically_normalise("a/../b").unwrap().components == ["b"]);
    }

    #[test]
    fn control_and_invisible_characters_are_refused() {
        assert!(check_input_chars("a\nb").is_err());
        assert!(check_input_chars("a\u{202e}b").is_err());
        assert!(check_input_chars("a\u{feff}b").is_err());
        assert!(check_input_chars("src/a.rs").is_ok());
    }

    #[test]
    fn limits_are_enforced_before_any_io() {
        let long = "a".repeat(5000);
        assert_eq!(
            check_size(&long, &Limits::default()).unwrap_err().code,
            ErrorCode::LimitExceeded
        );
        let deep = vec!["d"; 300].join("/");
        assert_eq!(
            check_size(&deep, &Limits::default()).unwrap_err().code,
            ErrorCode::LimitExceeded
        );
    }

    /// Every "not a file" errno has to reach the caller with the one special-file wording,
    /// on whichever kernel it came from. macOS answers `EOPNOTSUPP` for a unix socket where
    /// Linux answers `ENXIO`; both must be counted as a skipped special file (OUT-07).
    #[cfg(unix)]
    #[test]
    fn open_errno_classification() {
        for e in [
            rustix::io::Errno::NXIO,
            rustix::io::Errno::NODEV,
            rustix::io::Errno::OPNOTSUPP,
            rustix::io::Errno::NOTSUP,
        ] {
            let err = open_error(e);
            assert_eq!(err.code, ErrorCode::IoError, "{e:?}");
            assert!(
                err.message.contains("special file"),
                "{e:?} must be reported as a special file, got {:?}",
                err.message
            );
        }
        // A link swapped in for the file is its own story (BND-07).
        let loop_err = open_error(rustix::io::Errno::LOOP);
        assert!(loop_err.message.contains("symlink"), "{loop_err}");
        // A file that vanished says so.
        assert_eq!(
            open_error(rustix::io::Errno::NOENT).code,
            ErrorCode::NotFound
        );
        // Permission and every other errno stay generic: they are not a special file.
        for e in [
            rustix::io::Errno::ACCESS,
            rustix::io::Errno::PERM,
            rustix::io::Errno::ISDIR,
            rustix::io::Errno::NAMETOOLONG,
        ] {
            let err = open_error(e);
            assert!(
                !err.message.contains("special file"),
                "{e:?} must not be counted as a special file"
            );
        }
    }

    #[test]
    fn outside_message_never_names_a_path() {
        let e = outside_error();
        assert_eq!(e.code, ErrorCode::OutsideWorkspace);
        assert!(!e.message.contains('/'));
    }

    /// Rule 1(a) and 1(c): *any* `letter:` prefix names a drive, whatever follows the colon.
    ///
    /// The load-bearing case is the one a `X:\`-shaped test never sees: on Windows `C:foo` is
    /// not a file called `C:foo`, it is a path *relative to the current directory of drive C*,
    /// which the process - not the agent - chose. Judged only on the third byte it slips
    /// through to the filesystem and comes back as `io_error`, while a path that is merely
    /// absent comes back as `outside_workspace`: two different answers for one question,
    /// which is a probe oracle (BND-18).
    #[test]
    fn any_drive_relative_prefix_is_refused_whatever_the_third_byte_is() {
        // Absolute disk paths (`C:\x`, `C:/x`) are NOT in this list: they are accepted
        // lexically and judged by containment (same as Unix `/tmp/ws/a.rs`). What stays
        // refused here is drive-*relative* — no separator after the colon.
        for path in ["C:foo", "C:", "c:.:\\x", "C:.:\\WiWi", "z:x", "C:..\\x"] {
            assert!(
                has_drive_or_unc_prefix(path),
                "{path:?} names a drive and must be refused"
            );
            assert_eq!(
                refusal_code(lexically_normalise(path)),
                ErrorCode::OutsideWorkspace,
                "{path:?} must be refused as outside, never as io_error"
            );
        }
    }

    /// Absolute Windows disk paths parse as absolute and keep their drive; containment, not
    /// the string layer, decides whether they are inside a root.
    #[test]
    fn an_absolute_windows_disk_path_normalises_as_absolute() {
        for path in ["C:\\proj\\a.rs", "C:/proj/a.rs", "z:\\x"] {
            let l = lexically_normalise(path).unwrap_or_else(|e| {
                panic!("{path:?} must parse as an absolute disk path, got {e}")
            });
            assert!(l.absolute, "{path:?}");
            assert!(
                l.drive.as_deref().is_some_and(|d| d.ends_with(':')),
                "{path:?} must carry its drive"
            );
        }
        // Traversal above the drive root is still an escape.
        assert_eq!(
            refusal_code(lexically_normalise("C:\\..\\Windows")),
            ErrorCode::OutsideWorkspace
        );
    }

    /// Rule 1(b) and 1(c): two leading separators in any mixture are a UNC path, and one
    /// leading backslash is a root-relative path. A single leading *slash* is an ordinary
    /// absolute Unix path and stays perfectly legal.
    ///
    /// The mixed spellings matter because they are what a caller produces by accident: `\\`
    /// is what Windows callers write, but a JSON payload that went through a shell, a
    /// browser or a URL parser arrives as `//ser/share` or the near-invisible `\/ser`.
    #[test]
    fn unc_and_root_relative_prefixes_are_refused_in_every_spelling() {
        for path in [
            "\\\\ser\\share",
            "//ser/share",
            "\\/ser/share",
            "/\\ser",
            "\\\\?\\C:\\x",
            "\\\\.\\pipe\\x",
            "\\x",
        ] {
            assert!(
                has_drive_or_unc_prefix(path),
                "{path:?} is a UNC or root-relative path and must be refused"
            );
            assert_eq!(
                refusal_code(lexically_normalise(path)),
                ErrorCode::OutsideWorkspace,
                "{path:?} must be refused as outside"
            );
        }
        // A plain absolute path is not a UNC path, and must still resolve.
        let l = lexically_normalise("/a/b").unwrap();
        assert!(l.absolute);
    }

    /// Rule 2: a `:` in any component is an NTFS alternate data stream.
    ///
    /// `file.txt:stream` reads bytes that are not in the file an agent can reason about, and
    /// on Windows it is a *write* target that no `stat` ever mentions. It is refused on every
    /// platform even though `a:b` is a legal file name on Unix: one rule, and the price -
    /// a Unix file with a colon in its name is unreachable - is paid knowingly.
    #[test]
    fn a_colon_inside_any_component_is_refused() {
        for path in [
            "dir/file.txt:stream",
            "dir/a:b/c",
            "a/b:c",
            "file.txt:stream",
            "x:stream",
            "a\\b:c",
        ] {
            assert!(
                has_colon_component(path),
                "{path:?} carries a colon in a component and must be refused"
            );
            assert_eq!(
                refusal_code(lexically_normalise(path)),
                ErrorCode::OutsideWorkspace,
                "{path:?} must be refused as outside"
            );
        }
    }

    /// Where rule 1 and rule 2 stop, spelled out, because they overlap and it would be easy to
    /// merge them by accident. Two independent reasons to refuse a colon:
    ///
    /// * rule 1 looks at the **first two bytes only** and accepts any single ASCII letter,
    ///   so `C:foo` is a drive and `a:b` is a drive - there is no such thing as a Unix file
    ///   called `a:b` that an agent may name;
    /// * rule 2 looks at **every component**, so it fires on `ab:c` and `1:2` as well.
    ///
    /// Neither rule is relaxed by the other: `ab:c` and `1:2` are not drives, and they are
    /// still refused, as `outside_workspace`, by rule 2.
    #[test]
    fn the_two_colon_rules_have_different_boundaries() {
        // Not a drive: a digit, and two letters. Both still carry a colon in a component.
        for path in ["1:2", "ab:c"] {
            assert!(
                !has_drive_or_unc_prefix(path),
                "{path:?} is not a drive prefix: the byte before the colon is not one letter"
            );
            assert!(
                has_colon_component(path),
                "{path:?} still has a colon in a component"
            );
            assert_eq!(
                refusal_code(lexically_normalise(path)),
                ErrorCode::OutsideWorkspace
            );
        }
        // A single letter before the colon is a drive, wherever it appears - `a:b` included,
        // and whether or not a rule 2 reading of the same string also applies.
        for path in ["a:b", "C:x", "z:x"] {
            assert!(
                has_drive_or_unc_prefix(path),
                "{path:?} is a drive prefix and rule 1 claims it first"
            );
            assert!(has_colon_component(path));
        }
        // No colon anywhere: legal, and none of the rules may touch them.
        for path in ["abc", "a/b", "./x", "dir/file.txt", "/abs/x", "C"] {
            assert!(!has_drive_or_unc_prefix(path), "{path:?}");
            assert!(!has_colon_component(path), "{path:?}");
            assert!(
                lexically_normalise(path).is_ok(),
                "{path:?} must stay legal"
            );
        }
    }

    /// Rule 3, from the other end: a network, device or drive-relative path must be refused
    /// *as a root* by the string layer, before `canonicalize` can reach the network or a drive
    /// the caller never named. On Linux these spellings are merely non-existent names, so the
    /// refusal cannot have come from the filesystem - which is what makes this testable here
    /// and the Windows CI check a second opinion rather than the only one.
    #[test]
    fn a_network_or_device_path_is_refused_as_a_root() {
        for path in [
            "\\\\ser\\share",
            "//ser/share",
            "\\/ser/share",
            "/\\ser",
            "\\\\.\\pipe\\x",
            // `\\?\C:\x` moved out of this list: it is a *local* volume, and it is what
            // `canonicalize` returns on Windows for an ordinary `C:\...` root. The other
            // namespaces under the same prefix stay, and they are the dangerous ones -
            // `\\?\` means "hand this to the object manager verbatim", so what follows it
            // can be a UNC location, the block-device namespace, or a volume GUID.
            "\\\\?\\UNC\\srv\\share",
            "\\\\?\\unc\\srv\\share",
            "\\\\?\\GLOBALROOT\\Device\\HarddiskVolumeShadowCopy1",
            "\\\\?\\Volume{12345678-1234-1234-1234-123456789abc}\\x",
            "\\\\?\\pipe\\x",
            "\\\\?\\C:",
            "\\\\?\\Cx",
            // The device namespace outright, never allowed.
            "\\\\.\\C:\\x",
            "\\\\.\\pipe\\x",
            "C:proj",
            "C:",
            "c:.:\\WiWi",
        ] {
            let err = refuse_network_path(std::path::Path::new(path)).unwrap_err();
            assert_eq!(
                err.code,
                ErrorCode::InvalidArgs,
                "{path:?} must be an invalid_args refusal"
            );
            assert!(
                err.message
                    .to_lowercase()
                    .contains("network, device and drive-relative"),
                "{path:?}: the message must name the one reason, got {:?}",
                err.message
            );
            // And the same check as a workspace root, which must not reach canonicalize
            // either. The *code* cannot be the evidence: a root that does not exist is also
            // invalid_args, so this would pass with the string check deleted. The message is
            // the evidence - only the string check can produce it, and canonicalisation
            // never gets to speak.
            let root_err = check_root(std::path::Path::new(path), RootKind::Workspace).unwrap_err();
            assert_eq!(
                root_err.code,
                ErrorCode::InvalidArgs,
                "{path:?} must be refused before the filesystem is consulted"
            );
            assert!(
                root_err
                    .message
                    .to_lowercase()
                    .contains("network, device and drive-relative"),
                "{path:?} must be refused by the string rule, not by canonicalisation: {:?}",
                root_err.message
            );
        }
        // A drive with a separator after the colon is an ordinary absolute path, and `C:\x`
        // is the most ordinary workspace root on Windows: it must not be caught here.
        // So is its *verbatim* spelling `\\?\C:\x`, which is a local volume - and the form
        // `canonicalize` itself hands back on Windows, so refusing it would make the most
        // common root on that platform unusable. Any letter, either separator, any case.
        for path in [
            "C:\\x",
            "/tmp",
            "proj",
            "\\\\?\\C:\\x",
            "\\\\?\\c:/x",
            "\\\\?\\Z:\\Users\\me",
        ] {
            assert!(
                refuse_network_path(std::path::Path::new(path)).is_ok(),
                "{path:?} is an ordinary root spelling, not a network or drive-relative one"
            );
        }
    }

    /// The Windows illegal-name classes, as a pure string function, so the table is testable on
    /// Linux even though the *rule* is only consulted on Windows (BND-11).
    ///
    /// Three separate tests, one per rule, because the acceptance criterion for this ticket is a
    /// mutation proof for each: deleting any one rule has to turn exactly its own test red and
    /// leave the other two green. A single table would pass with two of the three rules deleted.
    #[test]
    fn rule_one_a_character_windows_cannot_name_in_a_file() {
        for component in [
            // The case the fuzzer found: `>>` answers `io_error` on Windows and not-found for
            // a merely absent name, which is the probe oracle the rule exists to prevent.
            ">>",
            "a<b",
            "a?b",
            "a*b",
            "\"x\"",
            "a|b",
            // Control characters, which `check_input_chars` also refuses - this is the
            // belt-and-braces for a component reached some other way.
            "a\u{0}b",
            "a\u{1b}[0m",
            "a\u{7f}",
        ] {
            assert!(
                windows_name_hazard(component),
                "{component:?} contains a character Windows rejects outright"
            );
        }
        // Ordinary names, which must not be swept up: `>` is legal on Unix (and note that
        // `a->b` above is *not* - `>` is one of the seven Windows rejects outright, wherever it
        // sits), and non-ASCII names are accepted by Windows; it rejects only those seven.
        for component in [
            "a",
            "a.rs",
            "a.txt",
            "src/main.rs",
            "日本語.rs",
            "a-b",
            "a=b",
        ] {
            assert!(
                !windows_name_hazard(component),
                "{component:?} is a name Windows accepts"
            );
        }
    }

    #[test]
    fn rule_two_a_trailing_dot_or_space_windows_strips() {
        for component in ["foo.", "foo ", "foo..", "foo. ", ".hidden.", "a. "] {
            assert!(
                windows_name_hazard(component),
                "{component:?} ends in a dot or space, which Windows silently strips - it is a \
                 different file than the one that was spelled"
            );
        }
        // `.` and `..` are excluded: they are path grammar, consumed by the traversal rule
        // before any component list exists, and not names.
        assert!(!windows_name_hazard("."), "\".\" is grammar, not a name");
        assert!(!windows_name_hazard(".."), "\"..\" is grammar, not a name");
        // `...` is a *name* by this decision, and not `.` or `..`, and Windows does strip its
        // trailing dot - so it is a hazard. This is the one entry the ticket leaves to us and
        // the choice is recorded here so the next reader knows it was a choice, not an
        // oversight: treating it as a name keeps the rule "a trailing dot is stripped, except
        // for the two components that are not names", instead of a list of exceptions.
        assert!(
            windows_name_hazard("..."),
            "`...` is a name, and its dot is stripped"
        );
        // A dot in the middle is perfectly fine.
        assert!(
            !windows_name_hazard("a.b"),
            "an interior dot is part of the name"
        );
        assert!(
            !windows_name_hazard(".gitignore"),
            "a leading dot is part of the name"
        );
    }

    #[test]
    fn rule_three_a_reserved_device_name_in_any_spelling() {
        for component in [
            // The four fixed names, any case, and with an "extension" - Windows ignores
            // everything after the first dot when it matches a device, so `con.txt` is the
            // console, not a file called `con.txt`.
            "CON",
            "con",
            "Con.txt",
            "CON.txt",
            "prn",
            "AUX",
            "nul",
            "NUL.log",
            // A trailing dot or space makes it a device name too, which is why rule 3 also
            // has to look past the strip rule 2 removes.
            "nul.",
            "con ",
            // COM1-COM9 / LPT1-LPT9, any case, with or without an extension.
            "COM1",
            "com9.x",
            "LPT3",
            "lpt1 ",
            "LPT9.log",
            "com5",
            // The superscript spellings Windows also accepts: same devices as COM1-COM3.
            "COM\u{b9}",
            "COM\u{b2}",
            "COM\u{b3}",
            "lpt\u{b9}",
            "com\u{b2}.txt",
        ] {
            assert!(
                windows_name_hazard(component),
                "{component:?} is a reserved device name on Windows"
            );
        }
        // The near misses. The first four are the trap for a `starts_with` implementation and
        // really are legal everywhere. The rest are legal **on Unix only** - and the pure
        // function still says `true` for them, which is the point of the split: the function
        // reports the Windows hazard and `cfg!(windows)` alone decides whether to act on it.
        // `aux.rs` and `con.go` are named here because they are the names a Rust project
        // really has; on Windows they collide with the devices, so refusing them there is
        // correct rather than a bug, and they cannot also appear in the legal set above.
        for component in [
            "console",
            "conf",
            "com0",
            "com10",
            "communicate",
            "lpt",
            "null",
        ] {
            assert!(
                !windows_name_hazard(component),
                "{component:?} is an ordinary file name, not a device"
            );
        }
    }

    /// The three rules are consulted together, per component, and each has to hold for every
    /// component of a path - a hazard anywhere is a hazard. Also the guard on the platform
    /// fork: the *function* is total on every platform, and only `lexically_normalise` decides
    /// to ask.
    #[test]
    fn the_rules_are_consulted_per_component_and_the_call_is_gated_on_windows() {
        // `cfg!` is deliberately used at the call site, not `#[cfg]`: this test exists to keep
        // it that way. If someone converts the call to `#[cfg(windows)]`, this file stops
        // compiling on Linux and the gating is stated as a fact rather than a behaviour.
        let src = include_str!("boundary.rs");
        let call = src
            .lines()
            .find(|l| l.contains("any(windows_name_hazard)"))
            .expect("lexically_normalise must still consult windows_name_hazard");
        assert!(
            call.contains("cfg!(windows)"),
            "the consultation must be gated with `cfg!(windows)` so the code is compiled and \
             linted on every platform: {call:?}"
        );
        // And the gate really is a no-op on this platform, which is the other half: on Unix the
        // names below are legal files and must resolve, not be refused.
        let on_windows = cfg!(windows);
        let refused: Vec<&str> = ["con.go", "aux.rs", "nul", ">>"]
            .into_iter()
            .filter(|c| on_windows && windows_name_hazard(c))
            .collect();
        if !on_windows {
            assert!(
                refused.is_empty(),
                "on Unix the gate is off, so none of these may be refused: {refused:?}"
            );
            for legal in ["con.go", "aux.rs", "com10", "a.b"] {
                assert!(
                    lexically_normalise(legal).is_ok(),
                    "{legal:?} is an ordinary Unix file name and must resolve"
                );
            }
        }
    }

    // ---- reprove_write_containment: the second line of defence on the write path ------------

    // `std::fs` is imported inside each unix-only helper rather than once at module scope: every
    // use below sits behind a `#[cfg(unix)]`, so a module-level import is unused on Windows and
    // `-D warnings` turns that into a build failure.
    /// A boundary over a fresh temporary workspace, plus a path guaranteed to be outside it.
    ///
    /// `outside` is a sibling of the workspace root inside the same temp directory, so it
    /// canonicalises successfully — which matters, because a path that cannot be canonicalised is
    /// refused by the same `outside_workspace` code for a different reason and would make this
    /// test pass without proving anything.
    #[cfg(unix)]
    fn boundary_with_an_outside_sibling() -> (tempfile::TempDir, Boundary, PathBuf) {
        use std::fs;

        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("ws");
        fs::create_dir(&root).unwrap();
        let outside = dir.path().join("outside");
        fs::create_dir(&outside).unwrap();
        let target = outside.join("secret.txt");
        fs::write(&target, b"not yours\n").unwrap();
        let boundary = Boundary::new(BoundaryConfig {
            root,
            state_dir: None,
            limits: Limits::default(),
            read_roots: Vec::new(),
            extra_protected: Vec::new(),
        })
        .unwrap();
        (dir, boundary, target)
    }

    /// A hand-built [`ResolvedPath`] whose `abs` points outside the workspace is refused.
    ///
    /// `ResolvedPath` is a **public struct with public fields**, so any caller can construct one —
    /// or keep one across a workspace that has since moved. `atomic_replace` therefore takes this
    /// type and must not trust it: [`Boundary::reprove_write_containment`] re-derives containment
    /// from `abs` alone, and this is the test that says so.
    ///
    /// The assertion is on the **error code**, deliberately. The wording of the refusal is pinned
    /// elsewhere (`boundary_prop::check_outside_err`, and BND-18's one constant text); asserting
    /// the text here would mean this test goes red for a copy edit and stays green for a security
    /// regression. Short-circuiting the function to `Ok(abs.to_path_buf())` is what this catches,
    /// and it is the mutation that leaves the message-text test green.
    #[cfg(unix)]
    #[test]
    fn reprove_write_containment_refuses_a_hand_built_path_pointing_outside() {
        let (_dir, boundary, outside_target) = boundary_with_an_outside_sibling();

        // Built by hand, exactly as an untrusted or stale caller would build it: `rel` claims a
        // tidy in-workspace name, `abs` says otherwise. Only `abs` may be believed.
        let forged = ResolvedPath {
            rel: "src/a.rs".to_string(),
            abs: outside_target.clone(),
        };

        let e = boundary
            .reprove_write_containment(&forged.abs)
            .expect_err("a path outside the workspace must be refused");

        assert_eq!(
            e.code,
            ErrorCode::OutsideWorkspace,
            "the error CODE is the containment property; the text is pinned elsewhere: {}",
            e.message
        );

        // The refusal must not leak the path that was refused (BND-18): an outside refusal is
        // deliberately identical whether or not the target exists.
        assert!(
            !e.message.contains("outside") && !e.message.contains("secret.txt"),
            "the refusal must not confirm what is out there: {} / {}",
            e.message,
            e.next
        );
    }

    /// The same hand-built path, but pointing *inside*: the check must not refuse everything.
    ///
    /// Without this, the test above would also pass if `reprove_write_containment` returned
    /// `outside_error()` unconditionally — a check that refuses all writes is not a check, it is
    /// an outage with a security-shaped excuse.
    #[cfg(unix)]
    #[test]
    fn reprove_write_containment_accepts_a_path_inside_and_returns_it_canonical() {
        use std::fs;

        let (_dir, boundary, _) = boundary_with_an_outside_sibling();
        let inside = boundary.root.join("src").join("a.rs");
        fs::create_dir_all(inside.parent().unwrap()).unwrap();
        fs::write(&inside, b"fn a() {}\n").unwrap();

        let forged = ResolvedPath {
            rel: "src/a.rs".to_string(),
            abs: inside.clone(),
        };
        let proven = boundary
            .reprove_write_containment(&forged.abs)
            .expect("a path inside the workspace must be accepted");

        // The return value is what the caller then writes to, so it must be the *canonical* path
        // — not the input echoed back. Echoing back would reintroduce whatever traversal the
        // caller put in `abs`.
        assert_eq!(proven, inside.canonicalize().unwrap());
        assert!(proven.is_absolute());
    }

    /// A traversal spelling that lands back inside is accepted; one that lands outside is not.
    ///
    /// `canonicalize` happens before `is_under`, so `..` cannot be used to smuggle a write out of
    /// the workspace — and cannot be used to fake one in either, which is the case the previous
    /// test's "canonical, not echoed" assertion covers.
    #[cfg(unix)]
    #[test]
    fn reprove_write_containment_decides_after_canonicalisation() {
        let (_dir, boundary, outside_target) = boundary_with_an_outside_sibling();
        let ws = boundary.root.clone();
        let escaping = ws.join("..").join("outside").join("secret.txt");
        assert!(
            escaping.canonicalize().is_ok(),
            "the escaping spelling must resolve to a real file, or the case proves nothing"
        );
        assert_eq!(
            boundary
                .reprove_write_containment(&escaping)
                .expect_err("a path that leaves the workspace must be refused")
                .code,
            ErrorCode::OutsideWorkspace
        );
        assert_eq!(
            boundary
                .reprove_write_containment(&outside_target)
                .expect_err("so must the direct spelling")
                .code,
            ErrorCode::OutsideWorkspace
        );
    }
}
