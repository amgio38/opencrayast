//! The plan model: a self-contained record of byte-range edits with the hashes of every file
//! before and after (docs/EDIT-MODEL.md "Plan format (version 1)"; invariants E-1, E-2, E-11).
//!
//! A plan has exactly one byte representation, the **canonical form**, and its id is the
//! content hash of those bytes. Everything that decides what is written or what a reviewer is
//! told is inside the canonical bytes; nothing run-varying is (the creation time, expiry and
//! producing binary live in the unhashed envelope, which is not part of this module).

use crate::editset::{Edit, changed_bytes};
use opencrayast_core::error::{ErrorCode, ToolError};
use opencrayast_core::hash::{ContentHash, plan_id};
use opencrayast_core::limits::Limits;
use serde_json::Value;

/// The plan `format` this build reads and writes. Other values are refused, never guessed at.
pub const PLAN_FORMAT: u32 = 1;
/// The version of the edit-generation rules (not of the binary). Other values are refused.
pub const ENGINE_FORMAT: u32 = 1;
/// A serialised plan larger than this is refused before it is parsed (`plan_corrupt`). It is
/// the worst case of the hard maxima: 8 MiB changed bytes, each of which can take six bytes of
/// JSON escape, plus slack for the structure.
pub const MAX_PLAN_BYTES: usize = 64 * 1024 * 1024;
/// Longest `request.summary` in bytes.
pub const SUMMARY_MAX_BYTES: usize = 256;
/// Longest stored path in bytes.
pub const PATH_MAX_BYTES: usize = 4096;
/// Maximum JSON nesting depth accepted by [`Plan::parse`].
const MAX_JSON_DEPTH: usize = 16;

/// What was asked for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlanRequest {
    /// `"rewrite"` or `"symbol"` (free text up to 32 bytes of `[a-z_]`; unknown kinds are refused
    /// by [`Plan::check`]).
    pub kind: String,
    /// One line describing the change, at most [`SUMMARY_MAX_BYTES`] bytes.
    pub summary: String,
    /// Optional caller note, at most `limits.note_max_bytes` bytes. Part of the hashed content.
    pub note: Option<String>,
}

/// The edits for one file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlanFile {
    /// Workspace-relative path, `/`-separated. A stored path is a request, never an authority.
    pub path: String,
    /// Language id, or `"text"` for none.
    pub language: String,
    /// Hash of the file before the plan.
    pub pre_hash: ContentHash,
    /// Size in bytes before.
    pub pre_size: u64,
    /// Syntax errors before.
    pub pre_errors: u64,
    /// Hash of the file after all its edits.
    pub post_hash: ContentHash,
    /// Size in bytes after.
    pub post_size: u64,
    /// Syntax errors after.
    pub post_errors: u64,
    /// The edits, strictly ascending by `start`, non-overlapping.
    pub edits: Vec<Edit>,
}

/// A plan.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Plan {
    /// Must equal [`PLAN_FORMAT`].
    pub format: u32,
    /// The workspace this plan is bound to (`w-` + 32 hex), E-11.
    pub workspace_id: String,
    /// Must equal [`ENGINE_FORMAT`].
    pub engine_format: u32,
    /// What was asked for.
    pub request: PlanRequest,
    /// Files, strictly ascending by `path` (byte order), no duplicates.
    pub files: Vec<PlanFile>,
}

fn plan_corrupt(message: impl Into<String>, next: impl Into<String>) -> ToolError {
    ToolError::new(ErrorCode::PlanCorrupt, message, next)
}

fn limit_exceeded(message: impl Into<String>, next: impl Into<String>) -> ToolError {
    ToolError::new(ErrorCode::LimitExceeded, message, next)
}

fn write_u64(out: &mut Vec<u8>, n: u64) {
    let mut buf = itoa_buf(n);
    out.extend_from_slice(buf.make_ascii());
}

/// Minimal itoa without pulling a crate: decimal, no sign, no leading zeros.
struct ItoaBuf {
    buf: [u8; 20],
    start: usize,
}

fn itoa_buf(mut n: u64) -> ItoaBuf {
    let mut buf = [b'0'; 20];
    let mut i = 20;
    if n == 0 {
        return ItoaBuf { buf, start: 19 };
    }
    while n > 0 {
        i -= 1;
        buf[i] = b'0' + (n % 10) as u8;
        n /= 10;
    }
    ItoaBuf { buf, start: i }
}

impl ItoaBuf {
    fn make_ascii(&mut self) -> &[u8] {
        &self.buf[self.start..]
    }
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

fn write_hash(out: &mut Vec<u8>, h: &ContentHash) {
    write_str(out, &h.to_string());
}

impl Plan {
    /// The canonical bytes (UTF-8 JSON). Exact rules, golden-tested:
    /// - object keys in ascending byte order at every level: plan = `engine_format`, `files`,
    ///   `format`, `request`, `workspace_id`; request = `kind`, `note` (omitted when `None`),
    ///   `summary`; file = `edits`, `language`, `path`, `post_errors`, `post_hash`, `post_size`,
    ///   `pre_errors`, `pre_hash`, `pre_size`; edit = `end`, `replacement`, `start`;
    /// - no whitespace anywhere; numbers are non-negative integers in decimal without sign,
    ///   exponent, fraction or leading zeros; hashes are the `sha256:<64 lowercase hex>` form;
    /// - strings: `"` and `\` are escaped as `\"` and `\\`; U+0008, U+000C, U+000A, U+000D,
    ///   U+0009 as `\b`, `\f`, `\n`, `\r`, `\t`; every other code point below U+0020 as `\u00xx`
    ///   with lowercase hex; everything else (including `/`, U+007F and all non-ASCII) is written
    ///   as its raw UTF-8;
    /// - arrays keep their order (files and edits are stored sorted, see [`Plan::check`]).
    ///
    /// Total and deterministic: the same value gives the same bytes on every platform.
    pub fn canonical_bytes(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(256);
        out.push(b'{');
        // engine_format, files, format, request, workspace_id
        out.extend_from_slice(br#""engine_format":"#);
        write_u64(&mut out, u64::from(self.engine_format));
        out.extend_from_slice(br#","files":["#);
        for (i, f) in self.files.iter().enumerate() {
            if i > 0 {
                out.push(b',');
            }
            write_file(&mut out, f);
        }
        out.extend_from_slice(br#"],"format":"#);
        write_u64(&mut out, u64::from(self.format));
        out.extend_from_slice(br#","request":"#);
        write_request(&mut out, &self.request);
        out.extend_from_slice(br#","workspace_id":"#);
        write_str(&mut out, &self.workspace_id);
        out.push(b'}');
        out
    }

    /// The plan id: [`opencrayast_core::hash::plan_id`] of [`Plan::canonical_bytes`] (E-2).
    pub fn id(&self) -> String {
        plan_id(&self.canonical_bytes())
    }

    /// Read a plan from bytes that must be **exactly** its canonical form.
    ///
    /// ## Failure semantics (every row is `plan_corrupt`, message names the problem class and
    /// never quotes the input)
    ///
    /// | Input | Result |
    /// |---|---|
    /// | longer than [`MAX_PLAN_BYTES`] | refused before parsing |
    /// | not UTF-8, not JSON, trailing bytes after the value, nesting deeper than 16 | refused |
    /// | a missing key, an unknown key, a duplicate key, a value of the wrong type | refused |
    /// | a number that is negative, fractional, has an exponent, or exceeds `u64` / `u32` | refused |
    /// | a hash that is not exactly the lowercase `sha256:<64 hex>` form | refused |
    /// | `format` or `engine_format` other than the supported values | refused |
    /// | valid JSON whose re-serialisation ([`Plan::canonical_bytes`]) differs from the input (whitespace, key order, escapes, `1.0`, `A`) | refused |
    /// | otherwise | `Ok`, and then [`Plan::check`] is run with `limits`; its error is returned |
    ///
    /// Total: arbitrary bytes never panic, overflow the stack or allocate more than a small
    /// multiple of the input (EDT-20 fuzz target).
    pub fn parse(bytes: &[u8], limits: &Limits) -> Result<Plan, ToolError> {
        if bytes.len() > MAX_PLAN_BYTES {
            return Err(plan_corrupt(
                "plan document exceeds MAX_PLAN_BYTES",
                "Shrink the plan or raise the limit.",
            ));
        }
        // UTF-8 gate (also rejects a leading BOM as non-canonical later via reserialise).
        if std::str::from_utf8(bytes).is_err() {
            return Err(plan_corrupt(
                "plan document is not UTF-8",
                "Store the plan as UTF-8 canonical JSON.",
            ));
        }
        let value: Value = serde_json::from_slice(bytes).map_err(|_| {
            plan_corrupt(
                "plan document is not valid JSON",
                "Store the plan as a single JSON object.",
            )
        })?;
        if json_depth(&value) > MAX_JSON_DEPTH {
            return Err(plan_corrupt(
                "plan document nests deeper than 16",
                "Flatten the plan structure.",
            ));
        }
        let plan = plan_from_value(&value)?;
        // Total gate: every non-canonical spelling (whitespace, key order, escapes, 1.0,
        // duplicate keys, …) fails this equality.
        if plan.canonical_bytes() != bytes {
            return Err(plan_corrupt(
                "plan document is not in canonical form",
                "Re-serialise with Plan::canonical_bytes before storing.",
            ));
        }
        plan.check(limits)?;
        Ok(plan)
    }

    /// [`Plan::parse`], and in addition the recomputed [`Plan::id`] must equal `expected_id`
    /// (E-2, EDT-02): a stored plan whose bytes were altered is `plan_corrupt` even if the altered
    /// bytes are a perfectly valid plan.
    pub fn parse_named(
        expected_id: &str,
        bytes: &[u8],
        limits: &Limits,
    ) -> Result<Plan, ToolError> {
        let plan = Self::parse(bytes, limits)?;
        if plan.id() != expected_id {
            return Err(plan_corrupt(
                "plan id does not match the canonical bytes",
                "Refuse the stored plan; its content was altered (E-2).",
            ));
        }
        Ok(plan)
    }

    /// Structural validation against `limits`. Checks run in this order, the first failing row
    /// decides the code; messages name the file index / path class, never file content.
    ///
    /// | Condition | Code |
    /// |---|---|
    /// | `format != PLAN_FORMAT` or `engine_format != ENGINE_FORMAT` | `plan_corrupt` |
    /// | `workspace_id` is not `w-` + 32 lowercase hex | `plan_corrupt` |
    /// | `request.kind` not in {`rewrite`, `symbol`}; `summary` empty, over [`SUMMARY_MAX_BYTES`], or containing a control character | `plan_corrupt` |
    /// | `request.note` longer than `limits.note_max_bytes` | `limit_exceeded` |
    /// | no files | `plan_corrupt` |
    /// | more files than `limits.plan_max_files` | `limit_exceeded` |
    /// | a path that is empty, over [`PATH_MAX_BYTES`], absolute (`/…`, `X:…`, `\\…`), contains `\`, NUL or another control character, or has an empty, `.` or `..` component | `plan_corrupt` |
    /// | paths not strictly ascending in byte order (unsorted or duplicate) | `plan_corrupt` |
    /// | a file with no edits | `plan_corrupt` |
    /// | edits not strictly ascending / overlapping / `start > end` / two insertions at one position / an insertion strictly inside another edit (same rules as `validate_edits`, without a source) | `plan_corrupt` |
    /// | more edits in total than `limits.plan_max_edits` | `limit_exceeded` |
    /// | total [`crate::changed_bytes`] over `limits.plan_max_changed_bytes` | `limit_exceeded` |
    /// | `post_size != pre_size - removed + inserted` (checked arithmetic; also when it would underflow) or any edit end > `pre_size` | `plan_corrupt` |
    pub fn check(&self, limits: &Limits) -> Result<(), ToolError> {
        if self.format != PLAN_FORMAT || self.engine_format != ENGINE_FORMAT {
            return Err(plan_corrupt(
                "plan format or engine_format is unsupported",
                "Use PLAN_FORMAT 1 and ENGINE_FORMAT 1.",
            ));
        }
        if !is_workspace_id(&self.workspace_id) {
            return Err(plan_corrupt(
                "workspace_id is not w- plus 32 lowercase hex",
                "Bind the plan to a real workspace id (E-11).",
            ));
        }
        if self.request.kind != "rewrite" && self.request.kind != "symbol" {
            return Err(plan_corrupt(
                "request.kind is not rewrite or symbol",
                "Use kind rewrite or symbol.",
            ));
        }
        if self.request.summary.is_empty()
            || self.request.summary.len() > SUMMARY_MAX_BYTES
            || self.request.summary.chars().any(|c| (c as u32) < 0x20)
        {
            return Err(plan_corrupt(
                "request.summary is empty, too long, or contains a control character",
                "Keep summary to one line of at most 256 bytes.",
            ));
        }
        // Why: clippy collapsible_if; keep a single gate for note length.
        if self
            .request
            .note
            .as_ref()
            .is_some_and(|n| n.len() as u64 > limits.note_max_bytes)
        {
            return Err(limit_exceeded(
                "request.note exceeds note_max_bytes",
                "Shorten the note or raise note_max_bytes.",
            ));
        }
        if self.files.is_empty() {
            return Err(plan_corrupt(
                "plan has no files",
                "A plan must list at least one file.",
            ));
        }
        if self.files.len() as u64 > limits.plan_max_files {
            return Err(limit_exceeded(
                "plan has more files than plan_max_files",
                "Split the plan or raise plan_max_files.",
            ));
        }
        for (i, f) in self.files.iter().enumerate() {
            if let Err(reason) = path_ok(&f.path) {
                return Err(plan_corrupt(
                    format!("file {i} path is {reason}"),
                    "Use a workspace-relative normalised path.",
                ));
            }
            if i > 0 && self.files[i - 1].path.as_bytes() >= f.path.as_bytes() {
                return Err(plan_corrupt(
                    "file paths are not strictly ascending in byte order",
                    "Sort files by path and remove duplicates.",
                ));
            }
            if f.edits.is_empty() {
                return Err(plan_corrupt(
                    format!("file {i} has no edits"),
                    "Every file in a plan must have at least one edit.",
                ));
            }
            check_edits_structure(i, &f.edits)?;
            check_post_size(i, f)?;
        }
        let total_edits: u64 = self.files.iter().map(|f| f.edits.len() as u64).sum();
        if total_edits > limits.plan_max_edits {
            return Err(limit_exceeded(
                "plan has more edits than plan_max_edits",
                "Split the plan or raise plan_max_edits.",
            ));
        }
        let mut total_changed = 0u64;
        for f in &self.files {
            total_changed = total_changed.saturating_add(changed_bytes(&f.edits));
        }
        if total_changed > limits.plan_max_changed_bytes {
            return Err(limit_exceeded(
                "plan changes more bytes than plan_max_changed_bytes",
                "Shrink replacements or raise plan_max_changed_bytes.",
            ));
        }
        Ok(())
    }

    /// E-11: `wrong_workspace` unless the plan is bound to `workspace_id`.
    pub fn check_workspace(&self, workspace_id: &str) -> Result<(), ToolError> {
        if self.workspace_id != workspace_id {
            return Err(ToolError::new(
                ErrorCode::WrongWorkspace,
                "plan is bound to a different workspace",
                "Refuse to apply a plan from another workspace (E-11).",
            ));
        }
        Ok(())
    }
}

fn write_request(out: &mut Vec<u8>, r: &PlanRequest) {
    out.push(b'{');
    out.extend_from_slice(br#""kind":"#);
    write_str(out, &r.kind);
    if let Some(note) = &r.note {
        out.extend_from_slice(br#","note":"#);
        write_str(out, note);
    }
    out.extend_from_slice(br#","summary":"#);
    write_str(out, &r.summary);
    out.push(b'}');
}

fn write_file(out: &mut Vec<u8>, f: &PlanFile) {
    out.push(b'{');
    out.extend_from_slice(br#""edits":["#);
    for (i, e) in f.edits.iter().enumerate() {
        if i > 0 {
            out.push(b',');
        }
        write_edit(out, e);
    }
    out.extend_from_slice(br#"],"language":"#);
    write_str(out, &f.language);
    out.extend_from_slice(br#","path":"#);
    write_str(out, &f.path);
    out.extend_from_slice(br#","post_errors":"#);
    write_u64(out, f.post_errors);
    out.extend_from_slice(br#","post_hash":"#);
    write_hash(out, &f.post_hash);
    out.extend_from_slice(br#","post_size":"#);
    write_u64(out, f.post_size);
    out.extend_from_slice(br#","pre_errors":"#);
    write_u64(out, f.pre_errors);
    out.extend_from_slice(br#","pre_hash":"#);
    write_hash(out, &f.pre_hash);
    out.extend_from_slice(br#","pre_size":"#);
    write_u64(out, f.pre_size);
    out.push(b'}');
}

fn write_edit(out: &mut Vec<u8>, e: &Edit) {
    out.push(b'{');
    out.extend_from_slice(br#""end":"#);
    write_u64(out, e.end as u64);
    out.extend_from_slice(br#","replacement":"#);
    write_str(out, &e.replacement);
    out.extend_from_slice(br#","start":"#);
    write_u64(out, e.start as u64);
    out.push(b'}');
}

fn json_depth(v: &Value) -> usize {
    match v {
        Value::Array(a) => 1 + a.iter().map(json_depth).max().unwrap_or(0),
        Value::Object(m) => 1 + m.values().map(json_depth).max().unwrap_or(0),
        _ => 1,
    }
}

/// Look up a key after [`require_keys`] (or an equivalent presence check). Never panics.
fn map_get<'a>(obj: &'a serde_json::Map<String, Value>, key: &str) -> Result<&'a Value, ToolError> {
    obj.get(key).ok_or_else(|| {
        plan_corrupt(
            "plan object is missing a required key",
            "Include every required plan field.",
        )
    })
}

fn plan_from_value(v: &Value) -> Result<Plan, ToolError> {
    let obj = expect_object(v, "plan")?;
    require_keys(
        obj,
        &[
            "engine_format",
            "files",
            "format",
            "request",
            "workspace_id",
        ],
    )?;
    let engine_format = expect_u32(map_get(obj, "engine_format")?, "engine_format")?;
    let format = expect_u32(map_get(obj, "format")?, "format")?;
    let workspace_id = expect_string(map_get(obj, "workspace_id")?, "workspace_id")?.to_string();
    let request = request_from_value(map_get(obj, "request")?)?;
    let files_v = expect_array(map_get(obj, "files")?, "files")?;
    let mut files = Vec::with_capacity(files_v.len());
    for (i, fv) in files_v.iter().enumerate() {
        files.push(file_from_value(fv, i)?);
    }
    Ok(Plan {
        format,
        workspace_id,
        engine_format,
        request,
        files,
    })
}

fn request_from_value(v: &Value) -> Result<PlanRequest, ToolError> {
    let obj = expect_object(v, "request")?;
    // note is optional
    let keys: Vec<&str> = obj.keys().map(String::as_str).collect();
    for k in &keys {
        if *k != "kind" && *k != "note" && *k != "summary" {
            return Err(plan_corrupt(
                "request has an unknown key",
                "Use only kind, note, summary.",
            ));
        }
    }
    if !obj.contains_key("kind") || !obj.contains_key("summary") {
        return Err(plan_corrupt(
            "request is missing a required key",
            "request needs kind and summary.",
        ));
    }
    let kind = expect_string(map_get(obj, "kind")?, "kind")?.to_string();
    let summary = expect_string(map_get(obj, "summary")?, "summary")?.to_string();
    let note = match obj.get("note") {
        None => None,
        Some(n) => Some(expect_string(n, "note")?.to_string()),
    };
    Ok(PlanRequest {
        kind,
        summary,
        note,
    })
}

fn file_from_value(v: &Value, index: usize) -> Result<PlanFile, ToolError> {
    let obj = expect_object(v, "file")?;
    require_keys(
        obj,
        &[
            "edits",
            "language",
            "path",
            "post_errors",
            "post_hash",
            "post_size",
            "pre_errors",
            "pre_hash",
            "pre_size",
        ],
    )?;
    let path = expect_string(map_get(obj, "path")?, "path")?.to_string();
    let language = expect_string(map_get(obj, "language")?, "language")?.to_string();
    let pre_hash = expect_hash(map_get(obj, "pre_hash")?, "pre_hash")?;
    let pre_size = expect_u64(map_get(obj, "pre_size")?, "pre_size")?;
    let pre_errors = expect_u64(map_get(obj, "pre_errors")?, "pre_errors")?;
    let post_hash = expect_hash(map_get(obj, "post_hash")?, "post_hash")?;
    let post_size = expect_u64(map_get(obj, "post_size")?, "post_size")?;
    let post_errors = expect_u64(map_get(obj, "post_errors")?, "post_errors")?;
    let edits_v = expect_array(map_get(obj, "edits")?, "edits")?;
    let mut edits = Vec::with_capacity(edits_v.len());
    for (j, ev) in edits_v.iter().enumerate() {
        edits.push(edit_from_value(ev, index, j)?);
    }
    Ok(PlanFile {
        path,
        language,
        pre_hash,
        pre_size,
        pre_errors,
        post_hash,
        post_size,
        post_errors,
        edits,
    })
}

fn edit_from_value(v: &Value, file: usize, edit: usize) -> Result<Edit, ToolError> {
    let obj = expect_object(v, "edit")?;
    require_keys(obj, &["end", "replacement", "start"])?;
    let start = expect_usize(map_get(obj, "start")?, "start", file, edit)?;
    let end = expect_usize(map_get(obj, "end")?, "end", file, edit)?;
    let replacement = expect_string(map_get(obj, "replacement")?, "replacement")?.to_string();
    Ok(Edit {
        start,
        end,
        replacement,
    })
}

fn expect_object<'a>(
    v: &'a Value,
    what: &str,
) -> Result<&'a serde_json::Map<String, Value>, ToolError> {
    v.as_object().ok_or_else(|| {
        plan_corrupt(
            format!("{what} must be a JSON object"),
            "Fix the plan document structure.",
        )
    })
}

fn expect_array<'a>(v: &'a Value, what: &str) -> Result<&'a Vec<Value>, ToolError> {
    v.as_array().ok_or_else(|| {
        plan_corrupt(
            format!("{what} must be a JSON array"),
            "Fix the plan document structure.",
        )
    })
}

fn expect_string<'a>(v: &'a Value, what: &str) -> Result<&'a str, ToolError> {
    v.as_str().ok_or_else(|| {
        plan_corrupt(
            format!("{what} must be a string"),
            "Fix the plan document types.",
        )
    })
}

fn expect_hash(v: &Value, what: &str) -> Result<ContentHash, ToolError> {
    let s = expect_string(v, what)?;
    ContentHash::parse(s).ok_or_else(|| {
        plan_corrupt(
            format!("{what} is not a lowercase sha256 digest"),
            "Use the sha256:<64 lowercase hex> form.",
        )
    })
}

/// Accept only JSON numbers that are exact non-negative integers in range (via f64 mantissa check
/// is insufficient alone; the canonical-bytes gate rejects `1.0` / `05` / `5e0`). Here we still
/// refuse negatives, non-integers and values that do not fit the target width when read back.
fn expect_u64(v: &Value, what: &str) -> Result<u64, ToolError> {
    match v {
        Value::Number(n) => {
            if let Some(u) = n.as_u64() {
                Ok(u)
            } else {
                Err(plan_corrupt(
                    format!("{what} is not a non-negative integer in u64 range"),
                    "Use a decimal integer without sign, fraction or exponent.",
                ))
            }
        }
        _ => Err(plan_corrupt(
            format!("{what} must be a number"),
            "Fix the plan document types.",
        )),
    }
}

fn expect_u32(v: &Value, what: &str) -> Result<u32, ToolError> {
    let u = expect_u64(v, what)?;
    u32::try_from(u)
        .map_err(|_| plan_corrupt(format!("{what} exceeds u32"), "Use a smaller integer."))
}

fn expect_usize(v: &Value, what: &str, file: usize, edit: usize) -> Result<usize, ToolError> {
    let u = expect_u64(v, what)?;
    usize::try_from(u).map_err(|_| {
        plan_corrupt(
            format!("file {file} edit {edit} {what} exceeds usize"),
            "Use a smaller byte offset.",
        )
    })
}

fn require_keys(obj: &serde_json::Map<String, Value>, required: &[&str]) -> Result<(), ToolError> {
    for k in obj.keys() {
        if !required.contains(&k.as_str()) {
            return Err(plan_corrupt(
                "plan object has an unknown key",
                "Remove unknown keys from the plan.",
            ));
        }
    }
    for k in required {
        if !obj.contains_key(*k) {
            return Err(plan_corrupt(
                "plan object is missing a required key",
                "Include every required plan field.",
            ));
        }
    }
    Ok(())
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
    // Drive letter: `X:` or `x:` at the start.
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

fn check_edits_structure(file: usize, edits: &[Edit]) -> Result<(), ToolError> {
    for (i, e) in edits.iter().enumerate() {
        if e.start > e.end {
            return Err(plan_corrupt(
                format!("file {file} edit {i} has start greater than end"),
                "Fix the edit ranges.",
            ));
        }
    }
    for i in 1..edits.len() {
        let prev = &edits[i - 1];
        let cur = &edits[i];
        if (prev.start, prev.end) >= (cur.start, cur.end) {
            return Err(plan_corrupt(
                format!("file {file} edits are not strictly ascending"),
                "Sort edits by start then end.",
            ));
        }
    }
    for a in 0..edits.len() {
        for b in (a + 1)..edits.len() {
            let ea = &edits[a];
            let eb = &edits[b];
            if ea.start == ea.end && eb.start == eb.end && ea.start == eb.start {
                return Err(plan_corrupt(
                    format!("file {file} has two insertions at the same position"),
                    "Keep at most one insertion per position.",
                ));
            }
            if ea.start < eb.end && eb.start < ea.end {
                return Err(plan_corrupt(
                    format!("file {file} edits {a} and {b} overlap"),
                    "Make edit ranges disjoint (touching ends are fine).",
                ));
            }
        }
    }
    Ok(())
}

fn check_post_size(file: usize, f: &PlanFile) -> Result<(), ToolError> {
    let mut removed: u64 = 0;
    let mut inserted: u64 = 0;
    for (i, e) in f.edits.iter().enumerate() {
        if e.end as u64 > f.pre_size {
            return Err(plan_corrupt(
                format!("file {file} edit {i} ends past pre_size"),
                "Keep every edit within the pre-image size.",
            ));
        }
        let r = (e.end - e.start) as u64;
        removed = removed.checked_add(r).ok_or_else(|| {
            plan_corrupt(
                format!("file {file} removed-byte total overflows"),
                "Reduce the edit set.",
            )
        })?;
        inserted = inserted
            .checked_add(e.replacement.len() as u64)
            .ok_or_else(|| {
                plan_corrupt(
                    format!("file {file} inserted-byte total overflows"),
                    "Reduce the replacements.",
                )
            })?;
    }
    let expected = f
        .pre_size
        .checked_sub(removed)
        .and_then(|x| x.checked_add(inserted));
    if expected != Some(f.post_size) {
        return Err(plan_corrupt(
            format!("file {file} post_size is inconsistent with its edits"),
            "Set post_size to pre_size - removed + inserted.",
        ));
    }
    Ok(())
}
