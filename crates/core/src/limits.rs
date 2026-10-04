//! Central limits with compiled-in hard maxima (docs/CONFIGURATION.md, EDIT-MODEL "Limits").

use crate::error::{ErrorCode, ToolError};

/// Hard maximum for [`Limits::max_file_bytes`]: 16 MiB.
pub const MAX_FILE_BYTES_HARD: u64 = 16 * 1024 * 1024;
/// Hard maximum for [`Limits::max_output_bytes`]: 256 KiB.
pub const MAX_OUTPUT_BYTES_HARD: u64 = 256 * 1024;
/// Hard maximum for [`Limits::max_results`].
pub const MAX_RESULTS_HARD: u64 = 1000;
/// Hard maximum for [`Limits::max_scan_files`].
pub const MAX_SCAN_FILES_HARD: u64 = 50_000;
/// Hard maximum for [`Limits::parse_timeout_ms`]: 30 s.
pub const PARSE_TIMEOUT_MS_HARD: u64 = 30_000;
/// Hard maximum for [`Limits::parse_max_depth`].
pub const PARSE_MAX_DEPTH_HARD: u64 = 4096;
/// Hard maximum for [`Limits::parse_max_nodes`].
pub const PARSE_MAX_NODES_HARD: u64 = 20_000_000;
/// Hard maximum for [`Limits::call_timeout_ms`]: 2 min.
pub const CALL_TIMEOUT_MS_HARD: u64 = 120_000;
/// Hard maximum for [`Limits::plan_ttl_minutes`]: one day.
pub const PLAN_TTL_MINUTES_HARD: u64 = 1440;
/// Hard maximum for [`Limits::plan_max_files`].
pub const PLAN_MAX_FILES_HARD: u64 = 500;
/// Hard maximum for [`Limits::plan_max_edits`].
pub const PLAN_MAX_EDITS_HARD: u64 = 5000;
/// Hard maximum for [`Limits::plan_max_changed_bytes`]: 8 MiB.
pub const PLAN_MAX_CHANGED_BYTES_HARD: u64 = 8 * 1024 * 1024;
/// Hard maximum for [`Limits::plan_max_store_mib`].
pub const PLAN_MAX_STORE_MIB_HARD: u64 = 1024;
/// Hard maximum for [`Limits::plan_max_plans`].
pub const PLAN_MAX_PLANS_HARD: u64 = 1000;
/// Hard maximum for [`Limits::plan_max_plans_per_process`].
pub const PLAN_MAX_PLANS_PER_PROCESS_HARD: u64 = 200;
/// Hard maximum for [`Limits::journal_max_plan_mib`].
pub const JOURNAL_MAX_PLAN_MIB_HARD: u64 = 128;
/// Hard maximum for [`Limits::journal_retention_days`].
pub const JOURNAL_RETENTION_DAYS_HARD: u64 = 90;
/// Hard maximum for [`Limits::journal_max_total_mib`].
pub const JOURNAL_MAX_TOTAL_MIB_HARD: u64 = 4096;
/// Hard maximum for [`Limits::note_max_bytes`].
pub const NOTE_MAX_BYTES_HARD: u64 = 4096;
/// Hard maximum for [`Limits::path_max_bytes`].
pub const PATH_MAX_BYTES_HARD: u64 = 4096;
/// Hard maximum for [`Limits::path_max_depth`]: 256 path components.
///
/// This is the compiled-in ceiling on the one **tunable** path-policy knob. It is chosen so
/// that a 256-component path still fits inside `PATH_MAX_BYTES_HARD` (4096 bytes) once each
/// component and its separator are counted, so the byte ceiling remains the binding
/// constraint for realistic component names and the two limits cannot contradict each other.
/// It sits four times above the documented default of 64, which is already well past any
/// real source tree, so an operator raising it is widening an allowance rather than
/// disabling a guard. Beyond it the value has no meaning to call: the check counts `/`
/// separators in a string that is already capped at 4096 bytes.
///
/// Every OTHER limit in this file is a **resource ceiling**, and is deliberately NOT
/// tunable beyond its maximum — see [`Limits::validate`].
pub const PATH_MAX_DEPTH_HARD: u64 = 256;

/// Every limit the tool enforces. `Default` gives the documented defaults.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Limits {
    /// Max bytes read from one file (default 4 MiB, hard max 16 MiB).
    pub max_file_bytes: u64,
    /// Max bytes of one tool output (default 64 KiB, hard max 256 KiB).
    pub max_output_bytes: u64,
    /// Max results returned (default 200, hard max 1000).
    pub max_results: u64,
    /// Max files scanned per call (default 5000, hard max 50000).
    pub max_scan_files: u64,
    /// Parse wall-clock budget in ms (default 2000, hard max 30000).
    pub parse_timeout_ms: u64,
    /// Parse depth budget (default 512, hard max 4096).
    pub parse_max_depth: u64,
    /// Parse node budget (default 2_000_000, hard max 20_000_000).
    pub parse_max_nodes: u64,
    /// Whole-call wall clock in ms (default 10000, hard max 120000).
    pub call_timeout_ms: u64,
    /// Plan time to live in minutes (default 15, hard max 1440).
    pub plan_ttl_minutes: u64,
    /// Files per plan (default 50, hard max 500).
    pub plan_max_files: u64,
    /// Edits per plan (default 500, hard max 5000).
    pub plan_max_edits: u64,
    /// Changed bytes per plan (default 1 MiB, hard max 8 MiB).
    pub plan_max_changed_bytes: u64,
    /// Plan store size in MiB (default 64, hard max 1024).
    pub plan_max_store_mib: u64,
    /// Plans per workspace (default 100, hard max 1000).
    pub plan_max_plans: u64,
    /// Unexpired plans per server process (default 25, hard max 200).
    pub plan_max_plans_per_process: u64,
    /// Total original bytes journaled per plan in MiB (default 64, hard max 128).
    pub journal_max_plan_mib: u64,
    /// Retained terminal journals, days (default 7, hard max 90).
    pub journal_retention_days: u64,
    /// Retained terminal journals, total MiB (default 256, hard max 4096).
    pub journal_max_total_mib: u64,
    /// Max plan note length in bytes (default 1024, hard max 4096).
    pub note_max_bytes: u64,
    /// Max path length in bytes (default 4096, hard max 4096).
    pub path_max_bytes: u64,
    /// Max path depth in components (default 64, hard max 256).
    pub path_max_depth: u64,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_file_bytes: 4 * 1024 * 1024,
            max_output_bytes: 64 * 1024,
            max_results: 200,
            max_scan_files: 5000,
            parse_timeout_ms: 2000,
            parse_max_depth: 512,
            parse_max_nodes: 2_000_000,
            call_timeout_ms: 10_000,
            plan_ttl_minutes: 15,
            plan_max_files: 50,
            plan_max_edits: 500,
            plan_max_changed_bytes: 1024 * 1024,
            plan_max_store_mib: 64,
            plan_max_plans: 100,
            plan_max_plans_per_process: 25,
            journal_max_plan_mib: 64,
            journal_retention_days: 7,
            journal_max_total_mib: 256,
            note_max_bytes: 1024,
            path_max_bytes: 4096,
            path_max_depth: 64,
        }
    }
}

impl Limits {
    /// Every field paired with its hard maximum, in declaration order.
    ///
    /// The pattern below is an **exhaustive** destructure of [`Limits`] (no `..`).
    /// Adding a field to the struct without naming it here is a compile error — that is
    /// the gate that keeps a limit from shipping without a hard maximum (CFG-04).
    /// The array length must then grow with the new row; a forgotten row leaves an
    /// unused binding the compiler also flags.
    pub fn table(&self) -> [(&'static str, u64, u64); 21] {
        let Limits {
            max_file_bytes,
            max_output_bytes,
            max_results,
            max_scan_files,
            parse_timeout_ms,
            parse_max_depth,
            parse_max_nodes,
            call_timeout_ms,
            plan_ttl_minutes,
            plan_max_files,
            plan_max_edits,
            plan_max_changed_bytes,
            plan_max_store_mib,
            plan_max_plans,
            plan_max_plans_per_process,
            journal_max_plan_mib,
            journal_retention_days,
            journal_max_total_mib,
            note_max_bytes,
            path_max_bytes,
            path_max_depth,
        } = self;
        [
            ("max_file_bytes", *max_file_bytes, MAX_FILE_BYTES_HARD),
            ("max_output_bytes", *max_output_bytes, MAX_OUTPUT_BYTES_HARD),
            ("max_results", *max_results, MAX_RESULTS_HARD),
            ("max_scan_files", *max_scan_files, MAX_SCAN_FILES_HARD),
            ("parse_timeout_ms", *parse_timeout_ms, PARSE_TIMEOUT_MS_HARD),
            ("parse_max_depth", *parse_max_depth, PARSE_MAX_DEPTH_HARD),
            ("parse_max_nodes", *parse_max_nodes, PARSE_MAX_NODES_HARD),
            ("call_timeout_ms", *call_timeout_ms, CALL_TIMEOUT_MS_HARD),
            ("plan_ttl_minutes", *plan_ttl_minutes, PLAN_TTL_MINUTES_HARD),
            ("plan_max_files", *plan_max_files, PLAN_MAX_FILES_HARD),
            ("plan_max_edits", *plan_max_edits, PLAN_MAX_EDITS_HARD),
            (
                "plan_max_changed_bytes",
                *plan_max_changed_bytes,
                PLAN_MAX_CHANGED_BYTES_HARD,
            ),
            (
                "plan_max_store_mib",
                *plan_max_store_mib,
                PLAN_MAX_STORE_MIB_HARD,
            ),
            ("plan_max_plans", *plan_max_plans, PLAN_MAX_PLANS_HARD),
            (
                "plan_max_plans_per_process",
                *plan_max_plans_per_process,
                PLAN_MAX_PLANS_PER_PROCESS_HARD,
            ),
            (
                "journal_max_plan_mib",
                *journal_max_plan_mib,
                JOURNAL_MAX_PLAN_MIB_HARD,
            ),
            (
                "journal_retention_days",
                *journal_retention_days,
                JOURNAL_RETENTION_DAYS_HARD,
            ),
            (
                "journal_max_total_mib",
                *journal_max_total_mib,
                JOURNAL_MAX_TOTAL_MIB_HARD,
            ),
            ("note_max_bytes", *note_max_bytes, NOTE_MAX_BYTES_HARD),
            ("path_max_bytes", *path_max_bytes, PATH_MAX_BYTES_HARD),
            ("path_max_depth", *path_max_depth, PATH_MAX_DEPTH_HARD),
        ]
    }

    /// Raise `path_max_depth` to the compiled-in ceiling, leaving every other limit alone.
    ///
    /// Returns `true` when a value was above `PATH_MAX_DEPTH_HARD` and got clamped, so the
    /// caller can report it. This is the *rewrite* form, for a caller that needs the
    /// operator's request resolved into a [`Limits`] it can hand out. The *read* form is
    /// [`Self::clamped_path_max_depth`], which every enforcement point uses.
    pub fn clamp_path_max_depth(&mut self) -> bool {
        let clamped = self.clamped_path_max_depth();
        let changed = clamped != self.path_max_depth;
        self.path_max_depth = clamped;
        changed
    }

    /// The `path_max_depth` an operator **actually gets**: the configured value if it is
    /// legal, otherwise [`PATH_MAX_DEPTH_HARD`].
    ///
    /// This is the single function that answers "what is the effective depth ceiling", so
    /// the boundary, the walker and every refusal message read one value and cannot disagree
    /// with each other. Reading the raw `path_max_depth` field instead is reading the
    /// operator's *request*; an operator who asked for 999999 and is silently served 999999
    /// would have defeated the guard entirely, which is the whole reason the clamp exists.
    pub fn clamped_path_max_depth(&self) -> u64 {
        self.path_max_depth.min(PATH_MAX_DEPTH_HARD)
    }

    /// True when the operator's requested `path_max_depth` exceeded the hard ceiling and
    /// was therefore clamped. Diagnostic only — enforcement does not branch on it.
    pub fn path_max_depth_was_clamped(&self) -> bool {
        self.path_max_depth > PATH_MAX_DEPTH_HARD
    }

    /// Reject a value of zero, and clamp every value above its hard maximum (CFG-04).
    ///
    /// **Two different behaviours, on purpose, and this is the split the operator ruling
    /// draws.**
    ///
    /// A **resource ceiling** — bytes, output size, result counts, plan and journal
    /// sizes, scan and time budgets — is **not** switchable off by an operator. A value
    /// above the hard maximum is a mistake or a tamper, so `validate` **refuses** it:
    /// the configuration does not load, the process does not start on a silently reduced
    /// ceiling, and the error names the field and the maximum. A ceiling that quietly
    /// became a smaller ceiling is a guard the operator cannot see.
    /// A **resource ceiling** — bytes, output size, result counts, plan and journal
    /// sizes, scan and time budgets — is **not** switchable off by an operator. A value
    /// above the hard maximum is a mistake or a tamper, so `validate` **refuses** it:
    /// the configuration does not load, the process does not start on a silently reduced
    /// ceiling, and the error names the field and the maximum. A ceiling that quietly
    /// became a smaller ceiling is a guard the operator cannot see.
    ///
    /// `validate` itself does **not** clamp. Clamping happens at the enforcement point via
    /// [`Self::clamped_path_max_depth`], which is where the value is actually read — so a
    /// caller that reads the raw `path_max_depth` field is visibly reading the
    /// *requested* value, and the boundary, the walker and every check reach the *effective*
    /// one through the single function below.
    ///
    /// The first offending field in declaration order is reported, so the same bad
    /// configuration always produces the same message.
    pub fn validate(&self) -> Result<(), ToolError> {
        for (name, value, hard) in self.table() {
            if value == 0 {
                return Err(ToolError::new(
                    ErrorCode::InvalidArgs,
                    format!("limits.{name} must be at least 1"),
                    format!("Set limits.{name} to a value from 1 to {hard}."),
                ));
            }
            if value > hard && name != "path_max_depth" {
                return Err(ToolError::new(
                    ErrorCode::InvalidArgs,
                    format!("limits.{name} is {value}, above the hard maximum {hard}"),
                    format!("Lower limits.{name} to {hard} or less."),
                ));
            }
        }
        Ok(())
    }
}
