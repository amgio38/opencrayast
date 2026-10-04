//! L3 edit layer: the pure parts of the edit model (docs/EDIT-MODEL.md) live here first —
//! edit-set validation and application, rewrite-template expansion, overlap resolution. The
//! plan model, store, preview, apply, journal, undo and recovery are added on top in later
//! tickets. Nothing in this module touches the filesystem.

pub mod apply;
pub mod capability;
pub mod editset;
mod encoding_gate;
mod fsutil;
pub mod journal;
pub mod jstore;
pub mod plan;
pub mod rewrite;
pub mod store;
pub mod template;
pub mod undo;

pub use apply::{
    ApplyContext, ApplyResult, Fault, FaultAction, NoFault, Recovered, Step, StepKind, apply,
    journal_of, recover,
};
pub use capability::{WriteCap, policy};
pub use editset::{Edit, apply_edits, bytes_added, bytes_removed, changed_bytes, validate_edits};

// Write-mode specs need `policy::enable_writes` (`pub(crate)`), so they live as
// in-crate unit tests rather than `tests/` integration tests. External callers mint
// via `WriteCap::mint(&WritePermission)` from parsed Settings (WCAP-1).
#[cfg(all(test, unix))]
#[path = "spec/apply_spec.rs"]
mod apply_spec;
#[cfg(all(test, unix))]
#[path = "spec/edit11_encoding_gate_spec.rs"]
mod edit11_encoding_gate_spec;
#[cfg(all(test, unix))]
#[path = "spec/edit7_extra_spec.rs"]
mod edit7_extra_spec;
#[cfg(all(test, unix))]
#[path = "spec/lmt07_binary_edit_spec.rs"]
mod lmt07_binary_edit_spec;
#[cfg(all(test, unix))]
#[path = "spec/sec_audit_write_poc.rs"]
mod sec_audit_write_poc;
#[cfg(all(test, unix))]
#[path = "spec/secfix4_write_cap_spec.rs"]
mod secfix4_write_cap_spec;
#[cfg(all(test, unix))]
#[path = "spec/undo_spec.rs"]
mod undo_spec;
pub use journal::{
    FileClass, JournalFile, JournalState, Manifest, Recovery, classify, plan_recovery, plan_undo,
};
pub use jstore::JournalStore;
pub use plan::{
    ENGINE_FORMAT, MAX_PLAN_BYTES, PATH_MAX_BYTES, PLAN_FORMAT, Plan, PlanFile, PlanRequest,
    SUMMARY_MAX_BYTES,
};
pub use rewrite::{RewriteOutcome, RewriteRequest, Span, rewrite_file};
pub use store::{Clock, PlanMeta, PlanStore, PlanSummary, SystemClock, UseGuard};
pub use template::{ExpandOptions, expand_template, indent_of_line, resolve_overlaps};
pub use undo::{UndoResult, undo};

pub mod preview;
pub use preview::{
    Diff, DiffLine, DiffLineKind, EditRequest, FileDiff, Hunk, PreviewContext, PreviewOutcome,
    RiskSummary, SkipReason, SkippedFile, SymbolOp, preview,
};
