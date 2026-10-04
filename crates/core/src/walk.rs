//! Directory walking through the boundary, with ignore rules (docs/CONFIGURATION.md `[ignore]`,
//! SECURITY-MODEL T-31; tests LMT-05, BND-22, BND-23, OUT-07).
//!
//! There is NO second way to read a directory: listing goes through [`Boundary::read_dir`], and
//! ignore files are read through [`Boundary::open_read`] like any other file. A symlinked
//! `.gitignore` that points outside the root is therefore never read.

use crate::boundary::{Boundary, ResolvedPath};
use crate::error::{ErrorCode, ToolError};
use std::sync::Arc;

/// Hard ceiling on how many entries ONE directory listing may return.
///
/// `Limits` has no per-directory entry count and inventing one in the configuration would
/// mean a new knob nobody asked for, so the ceiling lives here as a constant. It is
/// deliberately far above any real source directory (the largest ones in the wild are
/// tens of thousands of files) and far below what would hurt: 200k entries cost a few
/// megabytes of `Vec`, while an unbounded listing of a directory an attacker controls
/// would let a single tool call consume all the memory the server has (LMT-05).
///
/// Exceeding it is an error, not a silent truncation: a truncated listing would be
/// indistinguishable from a complete one, and every caller must be able to trust that
/// what it got is everything there is.
const MAX_DIR_ENTRIES_HARD: usize = 200_000;

/// What a directory entry is, decided WITHOUT following links.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntryKind {
    /// A regular file.
    File,
    /// A directory.
    Dir,
    /// A symbolic link (never followed).
    Symlink,
    /// FIFO, socket, device or anything else.
    Other,
}

/// One entry of a directory listing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DirEntryInfo {
    /// File name only (no separators).
    pub name: String,
    /// Kind, from `lstat` semantics.
    pub kind: EntryKind,
}

/// A set of gitignore-style rules from ONE ignore file, relative to the directory holding it.
///
/// Supported subset (exact, tested): blank lines and `#` comments; `!` negation; leading `/`
/// anchors to the rule's directory; a `/` in the middle also anchors; trailing `/` matches
/// directories only; `*` and `?` stay inside one path component; `**/` (leading), `/**`
/// (trailing) and `/**/` (middle) match any number of components; the LAST matching rule wins;
/// `[...]` classes and backslash escapes are NOT supported (treated as literal characters).
/// Matching is case-sensitive. Patterns without any `/` match at any depth.
#[derive(Debug, Clone, Default)]
pub struct IgnoreRules {
    /// Rules that need a pattern comparison, in file order. Everything that is a plain
    /// single-component NAME lives in `names` instead, so that a 100k-line ignore file of
    /// plain names costs one hash lookup per path component instead of a scan.
    patterns: Vec<Rule>,
    /// Plain single-component names, and the LAST rule for each spelling. Two slots, because
    /// `build` and `build/` are different rules that both answer to the name `build`: which
    /// one decides depends on whether the path being tested is a directory.
    names: std::collections::HashMap<String, NameRules>,
}

/// Which rule decides for one plain name, and when.
#[derive(Debug, Clone, Copy, Default)]
struct NameRules {
    /// Last rule for this name that matches files as well as directories.
    plain: Option<Hit>,
    /// Last rule for this name that matches directories only.
    dir_only: Option<Hit>,
}

/// A rule and where it sat in the file. The position is what "the last matching rule wins"
/// compares, across both halves of the index.
#[derive(Debug, Clone, Copy)]
struct Hit {
    order: u32,
    negated: bool,
}

/// One parsed rule.
#[derive(Debug, Clone)]
struct Rule {
    /// Position in the file; the LAST matching rule decides.
    order: u32,
    /// `!` prefix: this rule re-includes what an earlier rule ignored.
    negated: bool,
    /// Trailing `/`: matches directories only.
    dir_only: bool,
    /// How the pattern is compared.
    shape: Shape,
}

/// What a pattern is compared against: one component anywhere, or the whole component list
/// from the rule's own directory down.
#[derive(Debug, Clone)]
enum Shape {
    /// No `/` in the pattern: compared against EACH single component, at any depth.
    OneComponent(Vec<u8>),
    /// Leading `/` or an inner `/`: compared against the whole relative path, anchored at
    /// the directory holding the ignore file.
    Anchored {
        /// The pattern split on `/`, empty components folded away.
        segments: Vec<Segment>,
        /// The pattern ended in `/**`, which matches what is INSIDE the directory and not the
        /// directory itself: at least one component has to be left over.
        tail_needs_one: bool,
    },
}

/// One component of an anchored pattern.
#[derive(Debug, Clone)]
enum Segment {
    /// Exact bytes, including a `?` or `*` that cannot occur here.
    Literal(Vec<u8>),
    /// `*` and `?` wildcards that stay inside this component.
    Glob(Vec<u8>),
    /// `**`: any number of components, including none.
    DoubleStar,
}

impl IgnoreRules {
    /// Parse the text of an ignore file.
    ///
    /// Line handling, in order: `\r\n` and `\n` both end a line, trailing whitespace is not
    /// part of the pattern, an empty line and a line starting with `#` are skipped, a
    /// leading `!` negates, and a rule whose body is empty afterwards (a bare `!`, a bare
    /// `/`) is skipped rather than treated as a match-everything rule.
    pub fn parse(text: &str) -> Self {
        let mut patterns = Vec::new();
        let mut names = std::collections::HashMap::new();
        for (index, raw) in text.split('\n').enumerate() {
            let line = raw.strip_suffix('\r').unwrap_or(raw).trim_end();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let (negated, body) = match line.strip_prefix('!') {
                Some(rest) => (true, rest),
                None => (false, line),
            };
            let dir_only = body.ends_with('/');
            // EVERY trailing separator, not just one: `build//` names the same directory as
            // `build/`, so it has to mean the same thing rather than turning into a rule
            // with an empty component in it. (A leading separator is handled separately,
            // below, because for that one the position is the whole meaning.)
            let body = body.trim_end_matches('/');
            // A leading `/` is the explicit spelling of anchoring to the directory of this
            // ignore file; a `/` anywhere else anchors it the same way, because reaching
            // several components is only possible from the top. Both are decided BEFORE the
            // leading one is removed, since its position is the whole meaning.
            let anchored = body.starts_with('/') || body.contains('/');
            let body = body.strip_prefix('/').unwrap_or(body);
            // `/`, `//`, `!`, `!/` all end up here: no pattern left to match.
            if body.is_empty() {
                continue;
            }
            // A pattern made of nothing but two or more asterisks (`**`, `****`) matches
            // NOTHING here. Read as a wildcard it would match every name at every depth, and
            // read as a path form it is a malformed `**`. git calls consecutive asterisks
            // invalid; there is no single right reading, and this tool resolves the ambiguity
            // in the direction that shows the agent MORE of the tree rather than less. A
            // single `*` is left alone: "every name here" is a rule people really write.
            let stars = body.bytes().filter(|b| *b == b'*').count();
            if stars == body.len() && stars > 1 {
                continue;
            }
            let order = index as u32;
            let rule = Rule {
                order,
                negated,
                dir_only,
                shape: if anchored {
                    let tail_needs_one = body.ends_with("/**") && body.len() > 3;
                    Shape::Anchored {
                        segments: body
                            .split('/')
                            .filter(|s| !s.is_empty())
                            .map(segment)
                            .collect(),
                        tail_needs_one,
                    }
                } else {
                    Shape::OneComponent(body.as_bytes().to_vec())
                },
            };
            // The plain-name fast path only covers "one component, no wildcard": anything
            // else stays in `patterns` where it is compared with the glob matcher.
            if let Shape::OneComponent(pat) = &rule.shape
                && !pat.contains(&b'*')
                && !pat.contains(&b'?')
                && let Ok(name) = std::str::from_utf8(pat)
            {
                let hit = Hit { order, negated };
                let slot: &mut NameRules = names.entry(name.to_string()).or_default();
                let target = if dir_only {
                    &mut slot.dir_only
                } else {
                    &mut slot.plain
                };
                // Keep the LAST rule for this spelling: an earlier one can never be the
                // deciding rule, because the later one matches the same paths and wins.
                if target.is_none_or(|old| old.order < order) {
                    *target = Some(hit);
                }
                continue;
            }
            patterns.push(rule);
        }
        Self { patterns, names }
    }

    /// Is `rel` (path relative to the directory the rules belong to, `/`-separated, no leading
    /// `/`) ignored? `is_dir` says whether it names a directory. Returns `Some(true)` if the
    /// last matching rule ignores it, `Some(false)` if the last matching rule is a negation,
    /// `None` if no rule matches.
    pub fn matches(&self, rel: &str, is_dir: bool) -> Option<bool> {
        let components: Vec<&str> = rel.split('/').filter(|c| !c.is_empty()).collect();
        // Nothing to match: an empty or all-separators path names no component at all.
        if components.is_empty() {
            return None;
        }
        let mut best: Option<Hit> = None;
        for c in &components {
            let Some(slot) = self.names.get(*c) else {
                continue;
            };
            let candidate = if is_dir {
                match (slot.plain, slot.dir_only) {
                    (Some(a), Some(b)) => Some(if b.order > a.order { b } else { a }),
                    (a, b) => a.or(b),
                }
            } else {
                slot.plain
            };
            if candidate.is_some_and(|hit| best.is_none_or(|b| hit.order > b.order)) {
                best = candidate;
            }
        }
        for rule in &self.patterns {
            if rule.matches(&components, is_dir) && best.is_none_or(|b| rule.order > b.order) {
                best = Some(Hit {
                    order: rule.order,
                    negated: rule.negated,
                });
            }
        }
        best.map(|hit| !hit.negated)
    }
}

/// Classify one component of an anchored pattern.
fn segment(raw: &str) -> Segment {
    if raw == "**" {
        Segment::DoubleStar
    } else if raw.contains('*') || raw.contains('?') {
        Segment::Glob(raw.as_bytes().to_vec())
    } else {
        Segment::Literal(raw.as_bytes().to_vec())
    }
}

impl Rule {
    /// Does this rule match the path, whose components are already split?
    fn matches(&self, components: &[&str], is_dir: bool) -> bool {
        // A `dir_only` rule never matches a file, whatever the rest of the pattern says.
        if self.dir_only && !is_dir {
            return false;
        }
        match &self.shape {
            Shape::OneComponent(pat) => components.iter().any(|c| glob_match(pat, c.as_bytes())),
            Shape::Anchored {
                segments,
                tail_needs_one,
            } => anchored_match(segments, components, *tail_needs_one),
        }
    }
}

/// Match an anchored pattern against a whole component list.
///
/// A fill-over-the-grid match, so `**` cannot make this exponential: the state is "which
/// pattern components can have consumed which number of path components", and every cell is
/// written once. Cost is `O(pattern components * path components)` comparisons, each of
/// which is a linear glob match on one component.
fn anchored_match(segments: &[Segment], components: &[&str], tail_needs_one: bool) -> bool {
    let rows = segments.len();
    let cols = components.len();
    let cell = |i: usize, j: usize| i * (cols + 1) + j;
    let mut reach = vec![false; (rows + 1) * (cols + 1)];
    reach[cell(0, 0)] = true;
    for i in 0..rows {
        for j in 0..=cols {
            if !reach[cell(i, j)] {
                continue;
            }
            match &segments[i] {
                // Zero components, or take one more and stay on this `**`.
                Segment::DoubleStar => {
                    reach[cell(i + 1, j)] = true;
                    if j < cols {
                        reach[cell(i, j + 1)] = true;
                    }
                }
                seg => {
                    if j < cols && component_matches(seg, components[j].as_bytes()) {
                        reach[cell(i + 1, j + 1)] = true;
                    }
                }
            }
        }
    }
    if tail_needs_one {
        // Everything but the trailing `**` matched, and at least one component is left for
        // it to swallow. `out/**` covers what is inside `out`, not `out` itself.
        (0..cols).any(|j| reach[cell(rows - 1, j)])
    } else {
        reach[cell(rows, cols)]
    }
}

/// One component against one anchored-pattern segment.
fn component_matches(seg: &Segment, component: &[u8]) -> bool {
    match seg {
        Segment::Literal(lit) => lit == component,
        Segment::Glob(pat) => glob_match(pat, component),
        // A `**` is handled by the grid, never here.
        Segment::DoubleStar => false,
    }
}

/// Match one component against one pattern: `?` is any single character, `*` is any run of
/// characters WITHIN the component (never across a `/`, which is not even in the input
/// here), and everything else - including `[`, `]` and `\` - is a literal.
///
/// The state is "after this much pattern, which prefixes of the component are still
/// possible", advanced one pattern byte at a time. A `*` opens every remaining prefix and a
/// literal or `?` shifts the row. Filling a row instead of backtracking is what keeps a
/// pattern like `a*a*a*...*b` - sixty stars - polynomial: a backtracking matcher would try
/// every way of splitting the run of `a`s and take exponential time on a long one.
fn glob_match(pattern: &[u8], text: &[u8]) -> bool {
    let mut dp = vec![false; text.len() + 1];
    dp[0] = true;
    for p in pattern {
        match p {
            b'*' => {
                for j in 1..=text.len() {
                    dp[j] |= dp[j - 1];
                }
            }
            b'?' => {
                for j in (1..=text.len()).rev() {
                    dp[j] = dp[j - 1];
                }
                dp[0] = false;
            }
            _ => {
                for j in (1..=text.len()).rev() {
                    dp[j] = dp[j - 1] && *p == text[j - 1];
                }
                dp[0] = false;
            }
        }
    }
    dp[text.len()]
}

/// Options of a walk.
#[derive(Debug, Clone)]
pub struct WalkOptions {
    /// Stop after this many files; the result says `truncated`.
    pub max_files: u64,
    /// Honour `.gitignore` files found while walking (read through the boundary).
    pub respect_gitignore: bool,
    /// Extra ignore globs (same syntax as one ignore file), relative to the walk's start.
    pub extra_ignore: Vec<String>,
}

impl Default for WalkOptions {
    fn default() -> Self {
        Self {
            max_files: 5000,
            respect_gitignore: true,
            extra_ignore: Vec::new(),
        }
    }
}

/// The outcome of a walk. Everything skipped is COUNTED, never silent (OUT-07).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct WalkResult {
    /// Regular files found, sorted by `rel` (byte order), each already resolved for reading.
    pub files: Vec<ResolvedPath>,
    /// Entries skipped by ignore rules, by the built-in VCS directories, or because they sit
    /// below the depth ceiling. In other words: everything the walk deliberately did not
    /// ENTER. It is not a count of files that were never seen - a whole ignored subtree
    /// counts once, for the directory that was not entered.
    pub skipped_ignored: usize,
    /// Symbolic links skipped (never followed).
    pub skipped_links: usize,
    /// FIFOs, sockets, devices, names that are not valid UTF-8, ignore files that could not be
    /// used, and directories that could not be listed at all.
    pub skipped_special: usize,
    /// True if `max_files` stopped the walk, or if the walk hit its own entry ceiling
    /// (a directory it refused to enter counts against `skipped_special` and is not a
    /// reason to fail).
    pub truncated: bool,
}

impl Boundary {
    /// List a directory that `resolve_read` already accepted, with the project's own entry
    /// ceiling of 200,000.
    ///
    /// This is `read_dir_limited` with the project's own ceiling of 200,000 entries; the
    /// ceiling is the only difference, so there is one listing path and one place where the
    /// limit is decided.
    pub fn read_dir(&self, dir: &ResolvedPath) -> Result<Vec<DirEntryInfo>, ToolError> {
        self.read_dir_limited(dir, MAX_DIR_ENTRIES_HARD)
    }

    /// List a directory that `resolve_read` already accepted, refusing a directory with more
    /// than `max_entries` entries. Opened relative to the pinned root descriptor with no
    /// symlink followed (same mechanism as `open_read`); entries are returned in byte-sorted
    /// name order; `.` and `..` are never included; a name that is not valid UTF-8 is
    /// returned as `EntryKind::Other` with a lossy name (so the walker counts it in
    /// `skipped_special` rather than silently dropping it).
    ///
    /// A directory with EXACTLY `max_entries` entries is fine; the refusal happens on the
    /// first entry beyond it, so the boundary is not paid for by off-by-one arithmetic.
    ///
    /// Over the limit it is [`ErrorCode::LimitExceeded`], never a shorter list: a truncated
    /// listing is indistinguishable from a complete one, and a caller that cannot tell the
    /// difference will happily read a partial tree as if it were all of it (LMT-05).
    pub fn read_dir_limited(
        &self,
        dir: &ResolvedPath,
        max_entries: usize,
    ) -> Result<Vec<DirEntryInfo>, ToolError> {
        // Windows counterpart. `std::fs::read_dir` is the portable listing, and the kind comes from
        // `symlink_metadata` on each entry, so a link is reported as `Symlink` and never followed —
        // the same `lstat` semantics the unix arm gets from `d_type`/`fstatat`.
        //
        // One behavioural difference is deliberate and worth stating: an entry whose name is not
        // valid UTF-8 cannot exist on Windows (names are UTF-16), so the lossy-name branch the
        // unix arm needs has no counterpart here, and no entry is ever reported undecodable.
        #[cfg(windows)]
        {
            let full = dir.abs.as_path();
            // A regular file is a legal walk *start* (preview of one path). Unix gets
            // `NOT_A_DIRECTORY` from `O_DIRECTORY`; here `read_dir` on a file is a different
            // OS error, so classify first and return the shared wording `walk` matches on.
            match std::fs::metadata(full) {
                Ok(md) if md.is_file() => {
                    return Err(ToolError::new(
                        ErrorCode::IoError,
                        crate::boundary::NOT_A_DIRECTORY,
                        "Point the listing at a directory.",
                    ));
                }
                Ok(md) if !md.is_dir() => return Err(list_failed()),
                Err(_) => return Err(list_failed()),
                Ok(_) => {}
            }
            let mut out: Vec<DirEntryInfo> = Vec::new();
            let entries = std::fs::read_dir(full).map_err(|_| list_failed())?;
            for entry in entries {
                let entry = entry.map_err(|_| list_failed())?;
                if out.len() == max_entries {
                    // Stop the moment the limit is exceeded, as on unix.
                    return Err(too_many_entries(max_entries));
                }
                let name = entry.file_name().to_string_lossy().into_owned();
                let kind = match std::fs::symlink_metadata(entry.path()) {
                    Ok(md) => {
                        let ft = md.file_type();
                        if ft.is_symlink() {
                            EntryKind::Symlink
                        } else if ft.is_dir() {
                            EntryKind::Dir
                        } else if ft.is_file() {
                            EntryKind::File
                        } else {
                            EntryKind::Other
                        }
                    }
                    // Removed underneath us: not a reason to fail the whole listing, and the
                    // walker counts it (same rule as the unix arm).
                    Err(_) => EntryKind::Other,
                };
                out.push(DirEntryInfo { name, kind });
            }
            out.sort_by(|a, b| a.name.as_bytes().cmp(b.name.as_bytes()));
            Ok(out)
        }

        #[cfg(unix)]
        {
            let mut stream =
                rustix::fs::Dir::new(self.open_dir_for_listing(dir)?).map_err(|_| list_failed())?;
            // A second handle on the SAME directory, used only to `stat` an entry whose
            // `d_type` the filesystem left unknown. It is a `dup`, so it shares the stream's
            // read offset - harmless, because `fstatat` never moves an offset, and it is
            // dropped with the stream at the end of the call. It has to be a separate handle
            // because the iteration borrows the stream mutably while the stat needs a live
            // descriptor.
            let probe = stream
                .fd()
                .map_err(|_| list_failed())?
                .try_clone_to_owned()
                .map_err(|_| list_failed())?;

            let mut out: Vec<DirEntryInfo> = Vec::new();
            while let Some(entry) = stream.read() {
                let entry = entry.map_err(|_| list_failed())?;
                let raw = entry.file_name().to_bytes();
                // The stream reports these two; they are not entries of the directory.
                if raw == b"." || raw == b".." {
                    continue;
                }
                if out.len() == max_entries {
                    // Stop reading the moment the limit is exceeded, rather than after
                    // filling a buffer we have already promised not to keep.
                    return Err(too_many_entries(max_entries));
                }
                // An undecodable name is kept, never dropped: it becomes `Other` so the
                // walker counts it in `skipped_special` (OUT-07). Losing it would make the
                // counts lie about what is on disk.
                let (name, decodable) = match std::str::from_utf8(raw) {
                    Ok(name) => (name.to_string(), true),
                    Err(_) => (String::from_utf8_lossy(raw).into_owned(), false),
                };
                let kind = if decodable {
                    entry_kind(&entry, &probe, raw)
                } else {
                    EntryKind::Other
                };
                out.push(DirEntryInfo { name, kind });
            }
            // Byte order of the name, not of any locale collation: the result has to be the
            // same on every machine, and the walk sorts whole paths the same way.
            out.sort_by(|a, b| a.name.as_bytes().cmp(b.name.as_bytes()));
            Ok(out)
        }
    }
}

/// The kind of one entry, from `lstat` semantics and never by following a link.
///
/// `d_type` is what the filesystem already knows and costs nothing; it is `DT_UNKNOWN` on
/// some filesystems (and for every entry on a few), so those fall back to one `fstatat`
/// relative to the directory handle with `AT_SYMLINK_NOFOLLOW`. An entry that cannot even
/// be stat'ed has just been removed underneath us, which is not a reason to fail the whole
/// listing: it becomes `Other` and the walker counts it.
#[cfg(unix)]
fn entry_kind(
    entry: &rustix::fs::DirEntry,
    dir_fd: &rustix::fd::OwnedFd,
    name: &[u8],
) -> EntryKind {
    match entry.file_type() {
        rustix::fs::FileType::RegularFile => EntryKind::File,
        rustix::fs::FileType::Directory => EntryKind::Dir,
        rustix::fs::FileType::Symlink => EntryKind::Symlink,
        rustix::fs::FileType::Unknown => stat_kind(dir_fd, name).unwrap_or(EntryKind::Other),
        // FIFO, socket, character or block device: nothing here reads it as a file.
        _ => EntryKind::Other,
    }
}

/// `fstatat(dir_fd, name, AT_SYMLINK_NOFOLLOW)`, reduced to a kind. `None` when the entry is
/// gone or the name cannot be spelled as a C string (which a directory entry never is).
#[cfg(unix)]
fn stat_kind(dir_fd: &rustix::fd::OwnedFd, name: &[u8]) -> Option<EntryKind> {
    let name = std::ffi::CString::new(name).ok()?;
    let st = rustix::fs::statat(dir_fd, &name, rustix::fs::AtFlags::SYMLINK_NOFOLLOW).ok()?;
    Some(match rustix::fs::FileType::from_raw_mode(st.st_mode) {
        rustix::fs::FileType::RegularFile => EntryKind::File,
        rustix::fs::FileType::Directory => EntryKind::Dir,
        rustix::fs::FileType::Symlink => EntryKind::Symlink,
        _ => EntryKind::Other,
    })
}

fn list_failed() -> ToolError {
    ToolError::new(
        ErrorCode::IoError,
        "The directory could not be listed.",
        "Check the permissions of the directory.",
    )
}

fn too_many_entries(limit: usize) -> ToolError {
    ToolError::new(
        ErrorCode::LimitExceeded,
        format!("The directory has more than {limit} entries."),
        "List a subdirectory of it instead of the whole directory.",
    )
}

/// Directory names that mark version-control metadata. Never entered, at any depth, and no
/// ignore rule can bring them back: they are not "content", they are the repository.
///
/// [`crate::statedir::LEGACY_WORKSPACE_STATE_DIR_NAME`] is in the list for a different reason and
/// with a deliberately blunt rule. The tool's state used to live at `<workspace>/.opencrayast`,
/// and it now lives under the platform state directory — but a tree checked out with an older
/// build still has one, and an ordinary workspace walk must not be a way to read the tool's own
/// plans, journals, undo backups, plan ids and before/after hashes back through `ast_get`. It is
/// skipped by **name at any depth**, exactly like a VCS directory, and no `.gitignore` entry can
/// bring it back.
///
/// This is deliberately *not* the same check as refusing to *adopt* the directory: an existing
/// `.opencrayast` directory that a user keeps is still refused by [`crate::statedir::
/// ensure_state_dir`], because that function verifies rather than trusts. The walker does not
/// verify anything — it just refuses to descend.
const VCS_DIRS: [&str; 5] = [
    ".git",
    ".hg",
    ".svn",
    ".bzr",
    crate::statedir::LEGACY_WORKSPACE_STATE_DIR_NAME,
];

/// The ignore file looked for in every directory walked.
const IGNORE_FILE: &str = ".gitignore";

/// An ignore file larger than this is not used at all, and counts once as a skipped special
/// entry. A megabyte of ignore rules is already absurd; a multi-gigabyte one is a way to make
/// every walk read the whole file, and rules past the point where the tree is fully ignored
/// cannot change the outcome.
const IGNORE_FILE_MAX_BYTES: u64 = 1024 * 1024;

/// Hard ceiling on the entries ONE walk will look at, whatever `max_files` says.
///
/// `max_files` bounds the RESULT, not the work: a walk cannot stop at `max_files` files and
/// still promise the first `max_files` in sorted order, because it does not know what it has
/// not seen yet. So the files are collected, sorted and then cut, which means the collection
/// itself has to be bounded - otherwise a tree with ten million files under a single
/// `max_files: 10` request would allocate until the process died (LMT-05).
///
/// One million entries is a few tens of megabytes of `ResolvedPath`, far past any real
/// repository and far below anything that hurts. Hitting it is reported as `truncated`, never
/// as a complete walk.
const WALK_ENTRY_CEILING: usize = 1_000_000;

/// One directory waiting to be listed, with the rules that apply inside it.
struct Frame {
    /// The directory itself, as the boundary resolved it.
    dir: ResolvedPath,
    /// Workspace-relative path of that directory, with the walk root as `""`. Every path the
    /// walk produces is workspace-relative (OUT-02), so this is also what `rel` is built from.
    rel: String,
    /// How many components below the walk start this directory sits.
    depth: u64,
    /// Rule sets that apply here and below, outermost first: the caller's `extra_ignore`,
    /// then the `.gitignore` of each directory on the way down.
    rules: Vec<RuleSet>,
}

/// One ignore file's rules plus the directory they are relative to.
///
/// Cloneable so that handing the rules down to a child directory is a refcount bump: a deep
/// tree must not copy the rules of every ancestor into every frame.
#[derive(Clone)]
struct RuleSet {
    /// Workspace-relative path of the directory holding the ignore file; `""` is the walk
    /// root. `matches` wants a path relative to THAT directory, not to the workspace.
    base: String,
    /// The parsed rules.
    rules: Arc<IgnoreRules>,
}

/// Walk `start` (a file or a directory previously accepted by `resolve_read`) and collect the
/// regular files beneath it, deterministically (sorted by relative path bytes).
///
/// - the built-in VCS directories (`.git`, `.hg`, `.svn`, `.bzr`) are never entered;
/// - symlinks are never followed and are counted in `skipped_links`;
/// - an ignored directory is not entered (a negation cannot re-include below it);
/// - `.gitignore` files are read with `open_read` (so a symlinked one is refused, not followed);
///   rules of a nested `.gitignore` apply only beneath its directory;
/// - if `start` is a regular file the result is exactly that file.
///
/// The result is `max_files` files, sorted, cut from the front: the walk collects, sorts and
/// then truncates, because a walk cannot know which files come first without having seen all
/// of them. The collection is bounded by [`WALK_ENTRY_CEILING`] instead, so the memory a walk
/// can take is bounded whatever `max_files` says. Two walks of the same unchanged tree return
/// equal results, entry for entry.
pub fn walk(
    boundary: &Boundary,
    start: &ResolvedPath,
    opts: &WalkOptions,
) -> Result<WalkResult, ToolError> {
    // The kind of the start decides the kind of walk, and the boundary is the only thing
    // allowed to ask the disk: a listing that works means it is a directory, and the "not a
    // directory" refusal means it may still be a regular file - which is a legal start.
    let first_listing = match boundary.read_dir(start) {
        Ok(entries) => entries,
        Err(e) if is_not_a_directory(&e) => {
            // Anything else (a FIFO, a socket, a permission error) is reported by `open_read`,
            // which words a special file properly (BND-22).
            boundary.open_read(start).map_err(|_| e)?;
            return Ok(WalkResult {
                files: vec![start.clone()],
                ..WalkResult::default()
            });
        }
        Err(e) => return Err(e),
    };

    // The ceiling comes from the boundary, not from a freshly built default: the operator's
    // `limits.path_max_depth` has to bound the walk, and asking the boundary is the only
    // place that value lives (SECFIX5-01, SECFIX5-03).
    //
    // `clamped_path_max_depth`, not the raw field: `path_max_depth` is the one tunable knob,
    // so an above-ceiling request resolves to `PATH_MAX_DEPTH_HARD` here instead of letting
    // the walk run unbounded.
    let max_depth = boundary.limits().clamped_path_max_depth();
    // The walk root: `.` for the workspace root (every path is already relative to it), the
    // subdirectory or `@root1/...` label otherwise.
    let base = if start.rel == "." {
        String::new()
    } else {
        start.rel.clone()
    };

    let mut rules: Vec<RuleSet> = Vec::new();
    if !opts.extra_ignore.is_empty() {
        rules.push(RuleSet {
            base: base.clone(),
            rules: Arc::new(IgnoreRules::parse(&opts.extra_ignore.join("\n"))),
        });
    }

    let mut result = WalkResult::default();
    let mut files: Vec<ResolvedPath> = Vec::new();
    let mut pending: Option<Vec<DirEntryInfo>> = Some(first_listing);
    let mut stack = vec![Frame {
        dir: start.clone(),
        rel: base.clone(),
        // The depth ceiling is counted in WORKSPACE-RELATIVE components, so it starts at the
        // start's own depth and not at zero. Measuring from the start would let a walk that
        // begins deep return paths deeper than `path_max_depth`, which `resolve_read` refuses
        // - the walk would hand back files the boundary then cannot open.
        depth: rel_depth(&base),
        rules,
    }];
    let mut examined = 0usize;
    let mut hit_ceiling = false;

    // An explicit stack, not recursion: the depth ceiling is 64, but a recursive walk would
    // still be one stack frame per level on a thread the caller does not own, and the shape
    // of this loop (push the children, pop the next) is the same either way.
    'outer: while let Some(frame) = stack.pop() {
        let entries = match pending.take() {
            Some(entries) => entries,
            None => match boundary.read_dir(&frame.dir) {
                Ok(entries) => entries,
                Err(e) => {
                    // A directory that cannot be listed is counted, not fatal: one unreadable
                    // directory must not make the whole workspace unreadable, and OUT-07 says
                    // the count is what tells the caller it happened. Anything that is not one
                    // of the three "the tree changed under us" answers - including
                    // `LimitExceeded`, which must NOT be swallowed into a result that looks
                    // complete - is passed on unchanged.
                    if !count_unlistable(&e) {
                        return Err(e);
                    }
                    continue;
                }
            },
        };

        // The rules that apply to THIS directory's entries: everything from above, plus this
        // directory's own `.gitignore`. Deeper files are appended later and win, which is how
        // a nested ignore file overrides the one above it.
        let mut here = frame.rules.clone();
        if opts.respect_gitignore {
            for entry in entries.iter().filter(|e| e.name == IGNORE_FILE) {
                // Only a regular file is an ignore file. A symlinked one is skipped and
                // counted as a link by the entry loop below, and its content is never read -
                // that is the whole point of reading it through the boundary (BND-23).
                if entry.kind != EntryKind::File {
                    continue;
                }
                let path = child_path(&frame, &entry.name);
                match read_ignore_file(boundary, &path) {
                    Ok(text) => here.push(RuleSet {
                        base: frame.rel.clone(),
                        rules: Arc::new(IgnoreRules::parse(&text)),
                    }),
                    // A link in place of the ignore file refuses as "outside" (the uniform
                    // refusal, BND-18), and that is still a link as far as the count goes.
                    Err(e) if e.code == ErrorCode::OutsideWorkspace => result.skipped_links += 1,
                    Err(_) => result.skipped_special += 1,
                }
            }
        }

        let mut children: Vec<Frame> = Vec::new();
        for entry in &entries {
            if examined == WALK_ENTRY_CEILING {
                hit_ceiling = true;
                break 'outer;
            }
            examined += 1;
            let rel = join_rel(&frame.rel, &entry.name);
            let ignored = is_ignored(&here, &rel, entry.kind == EntryKind::Dir);
            match entry.kind {
                EntryKind::Symlink => result.skipped_links += 1,
                EntryKind::Other => result.skipped_special += 1,
                EntryKind::File => {
                    if ignored {
                        result.skipped_ignored += 1;
                    } else {
                        files.push(ResolvedPath {
                            rel,
                            abs: frame.dir.abs.join(&entry.name),
                        });
                    }
                }
                EntryKind::Dir => {
                    if VCS_DIRS.contains(&entry.name.as_str()) || ignored {
                        // An ignored directory is not entered, which is also why a negation
                        // further down cannot re-include something inside it: nothing down
                        // there is ever looked at.
                        result.skipped_ignored += 1;
                    } else if frame.depth + 1 >= max_depth {
                        // Below the depth ceiling. `depth` is the number of path components
                        // of the directory being listed, so a file inside a child would have
                        // `depth + 2` components: refusing to enter at `depth + 1 >=
                        // max_depth` leaves exactly `max_depth` components reachable, which
                        // is what `resolve_read` allows for any other path (BND-20). Counted,
                        // not silently dropped, and never entered.
                        result.skipped_ignored += 1;
                    } else {
                        children.push(Frame {
                            dir: child_path(&frame, &entry.name),
                            rel,
                            depth: frame.depth + 1,
                            rules: here.clone(),
                        });
                    }
                }
            }
        }
        // Reversed on the way onto the stack, so the directories are visited in sorted
        // order. The result is sorted at the end anyway; this only makes the ORDER in which
        // the entry ceiling is hit reproducible.
        stack.extend(children.into_iter().rev());
    }

    files.sort_by(|a, b| a.rel.as_bytes().cmp(b.rel.as_bytes()));
    let keep = usize::try_from(opts.max_files).unwrap_or(usize::MAX);
    result.truncated = hit_ceiling || files.len() > keep;
    files.truncate(keep);
    result.files = files;
    Ok(result)
}

/// Is `rel` ignored by any rule set that applies to it? Deeper rule sets decide first, which
/// is what makes a nested `.gitignore` override the one above it.
fn is_ignored(rules: &[RuleSet], rel: &str, is_dir: bool) -> bool {
    rules
        .iter()
        .rev()
        .find_map(|set| set.rules.matches(rel_from(&set.base, rel), is_dir))
        .is_some_and(|ignored| ignored)
}

/// `rel` as the rules of `base` see it: relative to the directory holding their ignore file,
/// not to the workspace. Falls back to `rel` itself if the two do not line up, which cannot
/// happen for a walk but must not turn into an empty path if it ever did.
fn rel_from<'a>(base: &'a str, rel: &'a str) -> &'a str {
    if base.is_empty() {
        return rel;
    }
    let rest = rel.strip_prefix(base).unwrap_or(rel);
    if base.ends_with('/') {
        rest
    } else {
        rest.strip_prefix('/').unwrap_or(rel)
    }
}

/// A workspace-relative path for an entry of `frame`. `frame.rel` is `""` for the walk root,
/// which is exactly what makes the entry's own name the whole path, and a read-root label
/// already ends in its own separator, which must not be doubled.
fn join_rel(frame_rel: &str, name: &str) -> String {
    if frame_rel.is_empty() {
        name.to_string()
    } else if frame_rel.ends_with('/') {
        format!("{frame_rel}{name}")
    } else {
        format!("{frame_rel}/{name}")
    }
}

/// The resolved path of an entry of `frame`.
fn child_path(frame: &Frame, name: &str) -> ResolvedPath {
    ResolvedPath {
        rel: join_rel(&frame.rel, name),
        abs: frame.dir.abs.join(name),
    }
}

/// Read one ignore file through the boundary, which is what stops a `.gitignore` that is a
/// symlink - or a planted FIFO - from deciding what the walk skips (BND-22, BND-23).
fn read_ignore_file(boundary: &Boundary, path: &ResolvedPath) -> Result<String, ToolError> {
    use std::io::Read;
    let (file, _id) = boundary.open_read(path)?;
    let mut buf = Vec::new();
    // One byte past the ceiling is enough to know it is over, and never reads a file an
    // attacker made huge into memory.
    let read = file
        .take(IGNORE_FILE_MAX_BYTES + 1)
        .read_to_end(&mut buf)
        .map_err(|_| ignore_unusable())?;
    if read as u64 > IGNORE_FILE_MAX_BYTES {
        return Err(ignore_unusable());
    }
    // A file that is not UTF-8 is not a rule set. Silently applying the readable prefix would
    // be worse than not applying it: the caller would get a result it cannot explain.
    String::from_utf8(buf).map_err(|_| ignore_unusable())
}

fn ignore_unusable() -> ToolError {
    ToolError::new(
        ErrorCode::IoError,
        "An ignore file could not be used.",
        "It is too large or not valid UTF-8; its rules were not applied.",
    )
}

/// How many path components a workspace-relative label has: `""` (the workspace root) and
/// `.` are 0, `src` is 1, and a read-root label (`@root1/...`) counts only what is below the
/// label, because that is what `path_max_depth` counts on the path a caller would pass back.
fn rel_depth(rel: &str) -> u64 {
    let below_label = match rel.split_once('/') {
        Some((label, rest)) if label.starts_with("@root") => rest,
        _ => rel,
    };
    below_label
        .split('/')
        .filter(|c| !c.is_empty() && *c != ".")
        .count() as u64
}

/// True for the listing failures that mean "the tree changed while we were walking it", and
/// which are counted instead of failing the walk. `LimitExceeded` is deliberately not one of
/// them: a refused directory must not come back as a shorter result.
fn count_unlistable(e: &ToolError) -> bool {
    matches!(
        e.code,
        ErrorCode::OutsideWorkspace | ErrorCode::NotFound | ErrorCode::IoError
    )
}

/// True for the one "this is not a directory" refusal — the signal that a walk start is a
/// regular file rather than a directory that failed to open.
fn is_not_a_directory(e: &ToolError) -> bool {
    e.message == crate::boundary::NOT_A_DIRECTORY
}
