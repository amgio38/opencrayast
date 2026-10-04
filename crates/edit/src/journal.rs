//! The journal model and the recovery decision (docs/EDIT-MODEL.md "Journal", "Undo",
//! "Recovery"; invariants E-6, E-8, E-9, E-13, E-14). Everything here is a **pure function of
//! values**: no file is touched. The shell reads the manifest and the current file hashes,
//! asks this module what to do, and does exactly that. Keeping the decision pure is what lets
//! the crash behaviour be tested exhaustively (`tests/journal_model_spec.rs` injects a crash
//! between every pair of steps of apply, undo and recovery).
//!
//! The one rule behind all of it (E-14): **classify every file first, act only if the whole
//! set is consistent.** A file is classified by its *content hash*, never by the manifest's
//! `progress` counter, because a crash can fall between a rename and the update of that
//! counter; the counter is informational. The same rule is why `prepared` — the state whose whole
//! claim is "no target was touched" — is now checked against those hashes instead of believed
//! (E-16): the state field is an unauthenticated byte pair on disk, the content hashes are not.

use crate::plan::{PATH_MAX_BYTES, Plan};
use opencrayast_core::error::{ErrorCode, ToolError};
use opencrayast_core::hash::{ContentHash, is_full_plan_id};
use serde_json::Value;

/// Largest accepted serialised manifest (`plan_corrupt` before parsing).
const MAX_MANIFEST_BYTES: usize = 4 * 1024 * 1024;
/// Maximum JSON nesting depth for [`Manifest::parse`].
const MAX_JSON_DEPTH: usize = 8;
/// Hard maximum number of files in a journal.
const MAX_JOURNAL_FILES: usize = 500;

/// Where a journal is in its life. Terminal states never change again.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum JournalState {
    /// Originals are saved; no target has been touched.
    Prepared,
    /// Renames are in progress (some targets may be new, some old).
    Writing,
    /// All targets were replaced and the apply completed. Terminal for apply; undo may start.
    Applied,
    /// An undo is in progress.
    Undoing,
    /// An apply was abandoned and every target is back to its original. Terminal.
    RolledBack,
    /// An undo completed and every target is back to its original. Terminal.
    Undone,
}

impl JournalState {
    /// The wire name: `prepared`, `writing`, `applied`, `undoing`, `rolled_back`, `undone`.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Prepared => "prepared",
            Self::Writing => "writing",
            Self::Applied => "applied",
            Self::Undoing => "undoing",
            Self::RolledBack => "rolled_back",
            Self::Undone => "undone",
        }
    }

    /// Inverse of [`JournalState::as_str`]; `None` for anything else (exact, lowercase).
    pub fn parse(s: &str) -> Option<JournalState> {
        match s {
            "prepared" => Some(Self::Prepared),
            "writing" => Some(Self::Writing),
            "applied" => Some(Self::Applied),
            "undoing" => Some(Self::Undoing),
            "rolled_back" => Some(Self::RolledBack),
            "undone" => Some(Self::Undone),
            _ => None,
        }
    }

    /// `RolledBack` and `Undone` are terminal. (`Applied` is terminal for *apply* but an undo
    /// may still start from it, so it is not terminal here: a journal in `Applied` is kept.)
    pub fn is_terminal(self) -> bool {
        matches!(self, Self::RolledBack | Self::Undone)
    }

    /// The legal transitions, exactly this table and nothing else (a state may not "move" to
    /// itself; the shell rewrites a manifest in place when only `progress` changes):
    ///
    /// | from \ to | Prepared | Writing | Applied | Undoing | RolledBack | Undone |
    /// |---|---|---|---|---|---|---|
    /// | Prepared | – | yes | no | no | yes | no |
    /// | Writing | no | – | yes | no | yes | no |
    /// | Applied | no | no | – | yes | no | no |
    /// | Undoing | no | no | yes (abort an undo back to the applied state) | – | no | yes |
    /// | RolledBack | no | no | no | no | – | no |
    /// | Undone | no | no | no | no | no | – |
    pub fn can_become(self, to: JournalState) -> bool {
        use JournalState::*;
        matches!(
            (self, to),
            (Prepared, Writing)
                | (Prepared, RolledBack)
                | (Writing, Applied)
                | (Writing, RolledBack)
                | (Applied, Undoing)
                | (Undoing, Applied)
                | (Undoing, Undone)
        )
    }
}

/// One file of a journal. `orig/<n>` holds the original bytes of file `n` (same order).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JournalFile {
    /// Workspace-relative path (a request, never an authority: re-resolved at use).
    pub path: String,
    /// Hash of the original (and of `orig/<n>`).
    pub pre_hash: ContentHash,
    /// Hash of the file after the plan.
    pub post_hash: ContentHash,
}

/// The journal manifest.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Manifest {
    /// The full plan id this journal belongs to.
    pub plan_id: String,
    /// [`ContentHash`] of the canonical bytes of the plan this journal belongs to — the same value
    /// the plan id is derived from, so it also names the plan content (E-2).
    ///
    /// **Why this exists.** The state field below is the *only* witness for "`prepared` means no
    /// target was touched", and a bare field on disk is not evidence: `manifest.json` is rewritten
    /// in place with no MAC, no monotonic counter and nothing binding it to the plan, so one
    /// byte-pair of corruption that keeps the shape and the mode turns a half-applied `writing`
    /// journal back into `prepared`, and recovery then reports a successful rollback while the
    /// edited files stay edited and the journal goes terminal. Binding the manifest to the plan
    /// content means a rewrite must *change this field too*, which no shape-preserving accident
    /// does on its own, and every state transition re-derives it from the plan bytes the caller
    /// must supply (E-16).
    pub plan_digest: ContentHash,
    /// The workspace (`w-` + 32 lowercase hex).
    pub workspace_id: String,
    /// Current state.
    pub state: JournalState,
    /// Files, strictly ascending by `path` (byte order), no duplicates, at least one.
    pub files: Vec<JournalFile>,
    /// Files renamed so far (apply) or restored so far (undo). **Informational only.**
    pub progress: u64,
    /// Creation time (clock seconds).
    pub created_at: u64,
    /// Last update time (clock seconds).
    pub updated_at: u64,
}

impl Manifest {
    /// Canonical JSON, the same discipline as `Plan::canonical_bytes`: keys ascending at every
    /// level (`created_at`, `files`, `plan_digest`, `plan_id`, `progress`, `state`, `updated_at`,
    /// `workspace_id`; file = `path`, `post_hash`, `pre_hash`), no whitespace, integers only,
    /// the same string escaping, hashes as `sha256:<64 lowercase hex>`, `state` as its wire name.
    pub fn canonical_bytes(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(256);
        out.push(b'{');
        out.extend_from_slice(br#""created_at":"#);
        write_u64(&mut out, self.created_at);
        out.extend_from_slice(br#","files":["#);
        for (i, f) in self.files.iter().enumerate() {
            if i > 0 {
                out.push(b',');
            }
            write_file(&mut out, f);
        }
        out.extend_from_slice(br#"],"plan_digest":"#);
        write_str(&mut out, &self.plan_digest.to_string());
        out.extend_from_slice(br#","plan_id":"#);
        write_str(&mut out, &self.plan_id);
        out.extend_from_slice(br#","progress":"#);
        write_u64(&mut out, self.progress);
        out.extend_from_slice(br#","state":"#);
        write_str(&mut out, self.state.as_str());
        out.extend_from_slice(br#","updated_at":"#);
        write_u64(&mut out, self.updated_at);
        out.extend_from_slice(br#","workspace_id":"#);
        write_str(&mut out, &self.workspace_id);
        out.push(b'}');
        out
    }

    /// Read a manifest that must be **exactly** its canonical form; every defect is
    /// `plan_corrupt` (a missing/unknown/duplicate key, wrong type, bad hash, unknown state,
    /// non-canonical bytes, trailing data, more than 4 MiB of input, nesting deeper than 8).
    /// Then [`Manifest::check`] runs. Total: arbitrary bytes never panic.
    ///
    /// `plan_digest` is checked for *shape* only here, because the digest of the plan can only be
    /// recomputed by a caller that holds the plan bytes ([`Manifest::check_plan_digest`]).
    pub fn parse(bytes: &[u8]) -> Result<Manifest, ToolError> {
        if bytes.len() > MAX_MANIFEST_BYTES {
            return Err(corrupt(
                "manifest exceeds the maximum size",
                "Shrink the journal manifest.",
            ));
        }
        if std::str::from_utf8(bytes).is_err() {
            return Err(corrupt(
                "manifest is not UTF-8",
                "Store the manifest as UTF-8 canonical JSON.",
            ));
        }
        let value: Value = serde_json::from_slice(bytes).map_err(|_| {
            corrupt(
                "manifest is not valid JSON",
                "Store the manifest as a single JSON object.",
            )
        })?;
        if json_depth(&value) > MAX_JSON_DEPTH {
            return Err(corrupt(
                "manifest nests deeper than 8",
                "Flatten the manifest structure.",
            ));
        }
        let m = manifest_from_value(&value)?;
        if m.canonical_bytes() != bytes {
            return Err(corrupt(
                "manifest is not in canonical form",
                "Re-serialise with Manifest::canonical_bytes before storing.",
            ));
        }
        m.check()?;
        Ok(m)
    }

    /// Structural validation; every failure is `plan_corrupt`. `plan_id` must be a full plan id,
    /// `workspace_id` well-formed, at least one and at most 500 files, paths valid under the same
    /// rules as plan paths (relative, `/`-separated, no `.`/`..`/empty components, no control
    /// characters, at most 4096 bytes), strictly ascending, `progress <= files.len()`,
    /// `updated_at >= created_at`.
    pub fn check(&self) -> Result<(), ToolError> {
        if self.plan_digest == ContentHash::of(b"") {
            // Shape is verified by `ContentHash::parse` in `manifest_from_value`; this is only
            // the one digest that cannot belong to a plan, so refusing it here keeps the "digest
            // is a function of real content" claim true rather than merely well-spelled.
            return Err(corrupt(
                "plan_digest is the digest of no plan",
                "Rewrite the manifest with the digest of the plan's canonical bytes.",
            ));
        }
        if !is_full_plan_id(&self.plan_id) {
            return Err(corrupt(
                "plan_id is not a full plan id",
                "Use a full p-<26 base32> plan id.",
            ));
        }
        if !is_workspace_id(&self.workspace_id) {
            return Err(corrupt(
                "workspace_id is not w- plus 32 lowercase hex",
                "Bind the journal to a real workspace id.",
            ));
        }
        if self.files.is_empty() {
            return Err(corrupt(
                "manifest has no files",
                "A journal must list at least one file.",
            ));
        }
        if self.files.len() > MAX_JOURNAL_FILES {
            return Err(corrupt(
                "manifest has more than 500 files",
                "Split the plan or raise the journal file cap.",
            ));
        }
        for (i, f) in self.files.iter().enumerate() {
            if let Err(reason) = path_ok(&f.path) {
                return Err(corrupt(
                    format!("file {i} path is {reason}"),
                    "Use a workspace-relative normalised path.",
                ));
            }
            if i > 0 && self.files[i - 1].path.as_bytes() >= f.path.as_bytes() {
                return Err(corrupt(
                    "file paths are not strictly ascending in byte order",
                    "Sort files by path and remove duplicates.",
                ));
            }
        }
        if self.progress > self.files.len() as u64 {
            return Err(corrupt(
                "progress exceeds the number of files",
                "Keep progress at most files.len().",
            ));
        }
        if self.updated_at < self.created_at {
            return Err(corrupt(
                "updated_at is before created_at",
                "Timestamps must be non-decreasing.",
            ));
        }
        Ok(())
    }

    /// Check that this manifest still belongs to the plan it names, in full:
    ///
    /// 1. `plan_digest` must be the digest of `plan.canonical_bytes()`, and that digest is the
    ///    value the plan id is derived from, so this also proves the two belong together (E-2).
    /// 2. **Every** journal file must equal the corresponding plan file: same paths, in the same
    ///    order, with the same `pre_hash` and `post_hash`.
    ///
    /// Part 2 is not implied by part 1 and is not optional. The `files` array is *copied out of*
    /// the plan, so the digest of the plan says nothing about the array as it currently reads; a
    /// rewrite of one `post_hash` to any other well-formed value leaves `plan_digest` intact and
    /// passes a digest-only check, then makes every subsequent classification unsatisfiable —
    /// `diverged` for a file that is in fact the plan's own post-image, and an undo that can never
    /// succeed for a reason no hash in the journal can resolve. The array is the *only* witness
    /// that the hashes recovery reasons about are the plan's hashes, so it is checked against the
    /// plan on every transition that acts on them.
    ///
    /// Any mismatch is `plan_corrupt`: the journal is refused, nothing is written, and — for a
    /// non-terminal journal — [`crate::recover`] refuses to act on it rather than trusting the
    /// state field. Call this **before** every state transition the caller makes, including the
    /// terminal ones.
    pub fn check_bound_to(&self, plan: &Plan) -> Result<(), ToolError> {
        let bytes = plan.canonical_bytes();
        let digest = ContentHash::of(&bytes);
        if digest != self.plan_digest {
            return Err(corrupt(
                "manifest plan_digest does not match the plan",
                "Refuse the journal; its manifest is not the one written for this plan.",
            ));
        }
        if self.files.len() != plan.files.len() {
            return Err(corrupt(
                "manifest file list does not match the plan",
                "Refuse the journal; the hashes recovery would use are not the plan's.",
            ));
        }
        for (i, (jf, pf)) in self.files.iter().zip(plan.files.iter()).enumerate() {
            if jf.path != pf.path || jf.pre_hash != pf.pre_hash || jf.post_hash != pf.post_hash {
                return Err(corrupt(
                    format!("manifest entry {i} does not match the plan"),
                    "Refuse the journal; the hashes recovery would use are not the plan's.",
                ));
            }
        }
        Ok(())
    }
}

/// How one file relates to the journal, decided by hash alone.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileClass {
    /// Equals `post_hash` (checked first, so a file whose pre and post hashes are equal is `Post`).
    Post,
    /// Equals `pre_hash`.
    Pre,
    /// Anything else, including a file that is missing (`None`).
    Other,
}

impl FileClass {
    fn as_str(self) -> &'static str {
        match self {
            Self::Post => "post",
            Self::Pre => "pre",
            Self::Other => "other",
        }
    }
}

/// Classify a file by its current content hash (`None` = missing or unreadable).
pub fn classify(current: Option<&ContentHash>, file: &JournalFile) -> FileClass {
    match current {
        Some(h) if h == &file.post_hash => FileClass::Post,
        Some(h) if h == &file.pre_hash => FileClass::Pre,
        _ => FileClass::Other,
    }
}

/// What the shell must do for a journal found in a non-terminal state (or asked to recover).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Recovery {
    /// Nothing to do (terminal states, and `Applied`).
    Nothing,
    /// `Prepared`, **and verified against the filesystem**: every target still hashes to its
    /// `pre_hash`. No workspace file is written; the journal is marked `rolled_back`.
    MarkRolledBack,
    /// `Writing`, or `Prepared` whose claim turned out to be false: restore the files at these
    /// indexes from `orig/<n>` (each is `Post`; the original is checked against `pre_hash` before
    /// it is written), then mark `RolledBack`. Indexes ascending. Files already `Pre` are not
    /// rewritten. May be empty (every file is already `Pre`).
    RestoreThenRolledBack(Vec<usize>),
    /// `Undoing`: restore the files at these indexes from `orig/<n>` (each is `Post`), then mark
    /// `Undone`. Indexes ascending. May be empty.
    RestoreThenUndone(Vec<usize>),
}

/// Decide the recovery action. `current[i]` is the current hash of `files[i]` (`None` = missing).
///
/// | State | Classification of all files | Result |
/// |---|---|---|
/// | `Applied`, `RolledBack`, `Undone` | any | `Nothing` (terminal; nothing is inspected) |
/// | `Prepared` | every file `Pre` | `MarkRolledBack` — **verified, not assumed** (see below) |
/// | `Prepared` | any file `Post` | `RestoreThenRolledBack(indexes of the Post files)`: the manifest says nothing was touched, and the filesystem says otherwise, so the originals are written back rather than thrown away |
/// | `Prepared` | at least one `Other` | `Err(diverged)`: `diverged` listing **every** file as `path: pre\|post\|other`, nothing is changed |
/// | `Writing` | every file `Pre` or `Post` | `RestoreThenRolledBack(indexes of the Post files)` |
/// | `Undoing` | every file `Pre` or `Post` | `RestoreThenUndone(indexes of the Post files)` |
/// | `Writing` or `Undoing` | at least one `Other` | `Err(diverged)`: message lists **every** file as `path: pre\|post\|other`, nothing is to be changed |
///
/// **Why `Prepared` is decided from the hashes and not from the field alone.** `prepared` is a
/// *claim*, not evidence: the state lives in one unauthenticated field of a file that is rewritten
/// in place, so a single byte-pair of corruption — or a torn write on a filesystem that reorders
/// or zero-fills — can turn a half-applied `writing` journal back into `prepared`. Trusting that
/// field is how two edited files survive a "successful" rollback and a terminal journal, with
/// nothing left that will ever restore them. The claim is cheap to check and the filesystem is the
/// authority, so this function checks it: the classification already in hand is what makes
/// `prepared` trustworthy, and when the claim is *false* the plan's own direction (`rolled_back`)
/// is still the right one, taken from the restored originals instead of from the field.
///
/// `current.len() != m.files.len()` is `internal` (a caller bug). Total and deterministic.
/// Idempotent by construction: the action for the state *after* performing it is `Nothing`.
pub fn plan_recovery(m: &Manifest, current: &[Option<ContentHash>]) -> Result<Recovery, ToolError> {
    use JournalState::*;
    match m.state {
        Applied | RolledBack | Undone => return Ok(Recovery::Nothing),
        Prepared | Writing | Undoing => {}
    }
    if current.len() != m.files.len() {
        return Err(ToolError::new(
            ErrorCode::Internal,
            "recovery hash vector length does not match the manifest",
            "Pass one current hash (or None) per journal file.",
        ));
    }
    let classes: Vec<FileClass> = m
        .files
        .iter()
        .zip(current.iter())
        .map(|(f, c)| classify(c.as_ref(), f))
        .collect();
    if classes.contains(&FileClass::Other) {
        return Err(diverged_listing(&m.files, &classes));
    }
    let posts: Vec<usize> = classes
        .iter()
        .enumerate()
        .filter_map(|(i, c)| (*c == FileClass::Post).then_some(i))
        .collect();
    match m.state {
        // The claim held: every target is byte-for-byte what it was before, so there is nothing to
        // restore and nothing to check beyond that.
        Prepared if posts.is_empty() => Ok(Recovery::MarkRolledBack),
        // The claim was false: restore from `orig/<n>` and end in `rolled_back`, the same
        // direction the field named and the same executor `Writing` uses — so a forged or torn
        // `prepared` cannot make recovery claim a rollback it did not perform.
        Prepared | Writing => Ok(Recovery::RestoreThenRolledBack(posts)),
        Undoing => Ok(Recovery::RestoreThenUndone(posts)),
        _ => Err(ToolError::new(
            ErrorCode::Internal,
            "recovery reached an unexpected journal state",
            "Report this as a defect.",
        )),
    }
}

/// Pre-check for a user-requested undo (EDIT-MODEL "Undo" steps 1–3): the state must be
/// `Applied` (else `invalid_args` naming the state; `Undoing` is handled by [`plan_recovery`]),
/// and **every** file must be `Post`; if any is not, `diverged` listing every file as in
/// [`plan_recovery`] and nothing is to be written. On success returns all indexes `0..n`.
pub fn plan_undo(m: &Manifest, current: &[Option<ContentHash>]) -> Result<Vec<usize>, ToolError> {
    if m.state != JournalState::Applied {
        return Err(ToolError::new(
            ErrorCode::InvalidArgs,
            format!(
                "undo requires journal state applied, got {}",
                m.state.as_str()
            ),
            "Undo only an applied plan; use recovery for an interrupted undo.",
        ));
    }
    if current.len() != m.files.len() {
        return Err(ToolError::new(
            ErrorCode::Internal,
            "undo hash vector length does not match the manifest",
            "Pass one current hash (or None) per journal file.",
        ));
    }
    let classes: Vec<FileClass> = m
        .files
        .iter()
        .zip(current.iter())
        .map(|(f, c)| classify(c.as_ref(), f))
        .collect();
    if classes.iter().any(|c| *c != FileClass::Post) {
        return Err(diverged_listing(&m.files, &classes));
    }
    Ok((0..m.files.len()).collect())
}

fn diverged_listing(files: &[JournalFile], classes: &[FileClass]) -> ToolError {
    // Why list every file: E-8 / E-14 — a person must see the full classification; never quote
    // file contents.
    let mut parts = Vec::with_capacity(files.len());
    for (f, c) in files.iter().zip(classes.iter()) {
        parts.push(format!("{}: {}", f.path, c.as_str()));
    }
    ToolError::new(
        ErrorCode::Diverged,
        format!("files diverged from the journal: {}", parts.join(", ")),
        "Resolve the named files by hand, then retry recovery or undo.",
    )
}

fn corrupt(message: impl Into<String>, next: impl Into<String>) -> ToolError {
    ToolError::new(ErrorCode::PlanCorrupt, message, next)
}

fn write_file(out: &mut Vec<u8>, f: &JournalFile) {
    out.push(b'{');
    out.extend_from_slice(br#""path":"#);
    write_str(out, &f.path);
    out.extend_from_slice(br#","post_hash":"#);
    write_str(out, &f.post_hash.to_string());
    out.extend_from_slice(br#","pre_hash":"#);
    write_str(out, &f.pre_hash.to_string());
    out.push(b'}');
}

fn write_u64(out: &mut Vec<u8>, n: u64) {
    out.extend_from_slice(n.to_string().as_bytes());
}

fn write_str(out: &mut Vec<u8>, s: &str) {
    out.push(b'"');
    for ch in s.chars() {
        match ch {
            '"' => out.extend_from_slice(br#"\""#),
            '\\' => out.extend_from_slice(br#"\\"#),
            '\u{0008}' => out.extend_from_slice(br#"\b"#),
            '\u{000C}' => out.extend_from_slice(br#"\f"#),
            '\n' => out.extend_from_slice(br#"\n"#),
            '\r' => out.extend_from_slice(br#"\r"#),
            '\t' => out.extend_from_slice(br#"\t"#),
            c if (c as u32) < 0x20 => {
                let n = c as u32;
                let hex = [
                    b'\\',
                    b'u',
                    b'0',
                    b'0',
                    hex_digit((n >> 4) as u8),
                    hex_digit((n & 0xf) as u8),
                ];
                out.extend_from_slice(&hex);
            }
            c => {
                let mut buf = [0u8; 4];
                out.extend_from_slice(c.encode_utf8(&mut buf).as_bytes());
            }
        }
    }
    out.push(b'"');
}

fn hex_digit(n: u8) -> u8 {
    if n < 10 { b'0' + n } else { b'a' + (n - 10) }
}

fn json_depth(v: &Value) -> usize {
    match v {
        Value::Array(a) => 1 + a.iter().map(json_depth).max().unwrap_or(0),
        Value::Object(m) => 1 + m.values().map(json_depth).max().unwrap_or(0),
        _ => 1,
    }
}

fn is_workspace_id(s: &str) -> bool {
    let Some(hex) = s.strip_prefix("w-") else {
        return false;
    };
    hex.len() == 32 && hex.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

fn path_ok(path: &str) -> Result<(), &'static str> {
    if path.is_empty() {
        return Err("empty");
    }
    if path.len() > PATH_MAX_BYTES {
        return Err("too long");
    }
    if path.starts_with('/') || path.starts_with('\\') {
        return Err("absolute");
    }
    let b = path.as_bytes();
    if b.len() >= 2 && b[1] == b':' && b[0].is_ascii_alphabetic() {
        return Err("absolute");
    }
    if path.contains('\\') || path.chars().any(|c| (c as u32) < 0x20) {
        return Err("contains a backslash or control character");
    }
    for comp in path.split('/') {
        if comp.is_empty() || comp == "." || comp == ".." {
            return Err("has an empty, '.' or '..' component");
        }
    }
    Ok(())
}

fn map_get<'a>(obj: &'a serde_json::Map<String, Value>, key: &str) -> Result<&'a Value, ToolError> {
    obj.get(key).ok_or_else(|| {
        corrupt(
            "manifest is missing a required key",
            "Include every required manifest field.",
        )
    })
}

fn require_keys(obj: &serde_json::Map<String, Value>, required: &[&str]) -> Result<(), ToolError> {
    for k in obj.keys() {
        if !required.contains(&k.as_str()) {
            return Err(corrupt(
                "manifest has an unknown key",
                "Remove unknown keys from the manifest.",
            ));
        }
    }
    for k in required {
        if !obj.contains_key(*k) {
            return Err(corrupt(
                "manifest is missing a required key",
                "Include every required manifest field.",
            ));
        }
    }
    Ok(())
}

fn expect_object<'a>(
    v: &'a Value,
    what: &str,
) -> Result<&'a serde_json::Map<String, Value>, ToolError> {
    v.as_object().ok_or_else(|| {
        corrupt(
            format!("{what} must be a JSON object"),
            "Fix the manifest structure.",
        )
    })
}

fn expect_array<'a>(v: &'a Value, what: &str) -> Result<&'a Vec<Value>, ToolError> {
    v.as_array().ok_or_else(|| {
        corrupt(
            format!("{what} must be a JSON array"),
            "Fix the manifest structure.",
        )
    })
}

fn expect_string<'a>(v: &'a Value, what: &str) -> Result<&'a str, ToolError> {
    v.as_str().ok_or_else(|| {
        corrupt(
            format!("{what} must be a string"),
            "Fix the manifest types.",
        )
    })
}

fn expect_u64(v: &Value, what: &str) -> Result<u64, ToolError> {
    match v {
        Value::Number(n) => n.as_u64().ok_or_else(|| {
            corrupt(
                format!("{what} is not a non-negative integer"),
                "Use a decimal integer without sign, fraction or exponent.",
            )
        }),
        _ => Err(corrupt(
            format!("{what} must be a number"),
            "Fix the manifest types.",
        )),
    }
}

fn expect_hash(v: &Value, what: &str) -> Result<ContentHash, ToolError> {
    let s = expect_string(v, what)?;
    ContentHash::parse(s).ok_or_else(|| {
        corrupt(
            format!("{what} is not a lowercase sha256 digest"),
            "Use the sha256:<64 lowercase hex> form.",
        )
    })
}

fn manifest_from_value(v: &Value) -> Result<Manifest, ToolError> {
    let obj = expect_object(v, "manifest")?;
    require_keys(
        obj,
        &[
            "created_at",
            "files",
            "plan_digest",
            "plan_id",
            "progress",
            "state",
            "updated_at",
            "workspace_id",
        ],
    )?;
    let created_at = expect_u64(map_get(obj, "created_at")?, "created_at")?;
    let files_v = expect_array(map_get(obj, "files")?, "files")?;
    let mut files = Vec::with_capacity(files_v.len());
    for fv in files_v {
        files.push(file_from_value(fv)?);
    }
    let plan_id = expect_string(map_get(obj, "plan_id")?, "plan_id")?.to_string();
    let plan_digest = expect_hash(map_get(obj, "plan_digest")?, "plan_digest")?;
    let progress = expect_u64(map_get(obj, "progress")?, "progress")?;
    let state_s = expect_string(map_get(obj, "state")?, "state")?;
    let state = JournalState::parse(state_s).ok_or_else(|| {
        corrupt(
            "state is not a known journal state",
            "Use prepared, writing, applied, undoing, rolled_back or undone.",
        )
    })?;
    let updated_at = expect_u64(map_get(obj, "updated_at")?, "updated_at")?;
    let workspace_id = expect_string(map_get(obj, "workspace_id")?, "workspace_id")?.to_string();
    Ok(Manifest {
        plan_id,
        plan_digest,
        workspace_id,
        state,
        files,
        progress,
        created_at,
        updated_at,
    })
}

fn file_from_value(v: &Value) -> Result<JournalFile, ToolError> {
    let obj = expect_object(v, "file")?;
    require_keys(obj, &["path", "post_hash", "pre_hash"])?;
    let path = expect_string(map_get(obj, "path")?, "path")?.to_string();
    let post_hash = expect_hash(map_get(obj, "post_hash")?, "post_hash")?;
    let pre_hash = expect_hash(map_get(obj, "pre_hash")?, "pre_hash")?;
    Ok(JournalFile {
        path,
        pre_hash,
        post_hash,
    })
}
