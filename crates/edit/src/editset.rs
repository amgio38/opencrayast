//! Edit sets (EDIT-MODEL invariant E-1). The shell validates every edit set it receives, whichever
//! executor produced it, and applies it only through this module.

use opencrayast_core::error::{ErrorCode, ToolError};
use opencrayast_core::limits::Limits;

/// Replace byte range `[start, end)` of a file with `replacement`. `start == end` is an insertion.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Edit {
    /// First byte replaced.
    pub start: usize,
    /// One past the last byte replaced.
    pub end: usize,
    /// The new text.
    pub replacement: String,
}

/// Bytes **inserted** by `edits` — the sum of each `replacement.len()`.
///
/// This is the `+A` half of the reviewer's `+A −B bytes` line (docs/TOOLS.md,
/// docs/EDIT-MODEL.md). It does **not** include removed bytes. Saturating; ranges
/// are not validated here.
pub fn bytes_added(edits: &[Edit]) -> u64 {
    let mut total = 0u64;
    for e in edits {
        total = total.saturating_add(e.replacement.len() as u64);
    }
    total
}

/// Bytes **removed** by `edits` — the sum of each `[start, end)` length.
///
/// This is the `−B` half of the reviewer's `+A −B bytes` line. It does **not**
/// include inserted bytes. Saturating; ranges are not validated here.
pub fn bytes_removed(edits: &[Edit]) -> u64 {
    let mut total = 0u64;
    for e in edits {
        let removed = e.end.saturating_sub(e.start) as u64;
        total = total.saturating_add(removed);
    }
    total
}

/// Inserted **plus** removed bytes — the quantity `limits.plan_max_changed_bytes`
/// bounds (EDIT-MODEL "Changed bytes per plan (inserted + removed)").
///
/// Derived from [`bytes_added`] + [`bytes_removed`]; not a third stored truth.
/// Saturating; ranges are not validated here.
pub fn changed_bytes(edits: &[Edit]) -> u64 {
    bytes_added(edits).saturating_add(bytes_removed(edits))
}

fn invalid_edit(message: impl Into<String>, next: impl Into<String>) -> ToolError {
    ToolError::new(ErrorCode::InvalidEdit, message, next)
}

fn limit_exceeded(message: impl Into<String>, next: impl Into<String>) -> ToolError {
    ToolError::new(ErrorCode::LimitExceeded, message, next)
}

/// Structural checks shared by [`validate_edits`] and [`apply_edits`] (no limits, no empty-set).
fn check_structure(source: &str, edits: &[Edit]) -> Result<(), ToolError> {
    let len = source.len();
    for (i, e) in edits.iter().enumerate() {
        if e.start > e.end {
            return Err(invalid_edit(
                format!("edit {i} has start {} greater than end {}", e.start, e.end),
                "Swap or fix the range so start ≤ end.",
            ));
        }
        if e.end > len {
            return Err(invalid_edit(
                format!("edit {i} ends at {} past the source length {len}", e.end),
                "Keep every edit range within the file.",
            ));
        }
        if !source.is_char_boundary(e.start) {
            return Err(invalid_edit(
                format!(
                    "edit {i} starts at byte {} inside a UTF-8 character",
                    e.start
                ),
                "Place start and end on character boundaries.",
            ));
        }
        if !source.is_char_boundary(e.end) {
            return Err(invalid_edit(
                format!("edit {i} ends at byte {} inside a UTF-8 character", e.end),
                "Place start and end on character boundaries.",
            ));
        }
    }

    // Overlap and duplicate-insertion checks use original indices in messages.
    for a in 0..edits.len() {
        for b in (a + 1)..edits.len() {
            let ea = &edits[a];
            let eb = &edits[b];
            let a_ins = ea.start == ea.end;
            let b_ins = eb.start == eb.end;
            if a_ins && b_ins && ea.start == eb.start {
                return Err(invalid_edit(
                    format!("edits {a} and {b} are both insertions at byte {}", ea.start),
                    "Keep at most one insertion at each position.",
                ));
            }
            // Classical overlap; insertions at the boundary of a replace do not overlap.
            if ea.start < eb.end && eb.start < ea.end {
                return Err(invalid_edit(
                    format!(
                        "edits {a} [{}, {}) and {b} [{}, {}) overlap",
                        ea.start, ea.end, eb.start, eb.end
                    ),
                    "Make edit ranges disjoint (touching ends are fine).",
                ));
            }
        }
    }
    Ok(())
}

/// Validate an edit set against `source`. The edits may be given in any order.
///
/// ## Failure semantics (decision table; exact codes)
///
/// | Condition | Result |
/// |---|---|
/// | `start > end` | `invalid_edit` |
/// | `end > source.len()` | `invalid_edit` |
/// | `start` or `end` not on a UTF-8 character boundary of `source` | `invalid_edit` |
/// | two edits overlap (`a.start < b.end && b.start < a.end` after sorting by start) | `invalid_edit` |
/// | an insertion strictly inside another edit's replaced range (`o.start < i.start < o.end`) | `invalid_edit` (it is an overlap) |
/// | two insertions (`start == end`) at the same position | `invalid_edit` (their order would be ambiguous) |
/// | an insertion at the start or end of a non-empty replaced range | allowed |
/// | `edits.len() > limits.plan_max_edits` | `limit_exceeded` |
/// | `changed_bytes(edits) > limits.plan_max_changed_bytes` | `limit_exceeded` |
/// | empty `edits` | `invalid_edit` (a plan with no change is not a plan) |
///
/// Checks run in the order listed, so the first applicable row decides the code, and the message
/// names the offending edit by index and range but never includes source text. Total: arbitrary
/// `usize` values never panic or overflow (EDT-01).
pub fn validate_edits(source: &str, edits: &[Edit], limits: &Limits) -> Result<(), ToolError> {
    check_structure(source, edits)?;
    let n = edits.len() as u64;
    if n > limits.plan_max_edits {
        return Err(limit_exceeded(
            format!(
                "edit set has {n} edits, above plan_max_edits ({})",
                limits.plan_max_edits
            ),
            "Split the plan or raise plan_max_edits.",
        ));
    }
    let changed = changed_bytes(edits);
    if changed > limits.plan_max_changed_bytes {
        return Err(limit_exceeded(
            format!(
                "edit set changes {changed} bytes, above plan_max_changed_bytes ({})",
                limits.plan_max_changed_bytes
            ),
            "Shrink replacements or raise plan_max_changed_bytes.",
        ));
    }
    if edits.is_empty() {
        return Err(invalid_edit(
            "edit set is empty",
            "A plan must contain at least one edit.",
        ));
    }
    Ok(())
}

/// Apply a valid edit set and return the new text. Input order does not matter. Runs the same
/// range, overlap and boundary checks as [`validate_edits`] (but not the limits), so it can
/// never produce garbage for a bad set; it returns `invalid_edit` instead.
///
/// Two insertions at one position are refused (see above). The result is deterministic and
/// equals the concatenation of untouched slices and replacements in position order.
pub fn apply_edits(source: &str, edits: &[Edit]) -> Result<String, ToolError> {
    check_structure(source, edits)?;
    let mut order: Vec<usize> = (0..edits.len()).collect();
    order.sort_by_key(|&i| (edits[i].start, edits[i].end));
    let mut out = String::new();
    let mut pos = 0usize;
    for i in order {
        let e = &edits[i];
        out.push_str(&source[pos..e.start]);
        out.push_str(&e.replacement);
        pos = e.end;
    }
    out.push_str(&source[pos..]);
    Ok(out)
}
