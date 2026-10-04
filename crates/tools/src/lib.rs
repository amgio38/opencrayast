//! L4 tools: the read-tool handlers (`ast_info`, `ast_outline`, `ast_get`) that turn a request
//! into bounded, deterministic text (docs/TOOLS.md). The MCP and CLI layers only deserialize
//! arguments into the structs here and print what comes back.
//!
//! Rules that hold for every handler (tests OUT-01..07, LMT-04, LMT-05):
//! - every path goes through the [`opencrayast_core::boundary::Boundary`], every file is opened
//!   with `open_read`, every directory is listed with `read_dir` (no second way in);
//! - names, paths and signatures are shown through `render::escape_inline`, source through
//!   `render::fenced_block`; the number of escaped characters is reported, never hidden;
//! - output is capped at `limits.max_output_bytes` and says what was cut and how to narrow;
//! - results are sorted and identical for identical inputs.

pub mod bench;
pub mod context;
pub mod edit;
pub mod explain;
pub mod get;
pub mod info;
pub mod outline;
pub mod registry;
pub mod search;
mod source;

pub use context::{ConfigSource, Mode, ToolContext};
pub use edit::{
    ApplyArgs, EditTools, PlanListArgs, PlanShowArgs, PreviewArgs, RecoverArgs, UndoArgs,
    ast_edit_apply, ast_edit_preview, ast_plan_list, ast_plan_show, ast_recover, ast_undo,
};
pub use explain::{ExplainArgs, ast_explain_pattern};
pub use get::{GetArgs, ast_get};
pub use info::ast_info;
/// Re-exported so the shells (MCP, CLI) can mint the write capability without depending on
/// `opencrayast-edit` directly. Minting still needs a `WritePermission` from parsed
/// configuration; this re-export widens *reach*, not *authority* (WCAP-1).
pub use opencrayast_edit::WriteCap;
/// The plan store, journal store and system clock, re-exported for the same reason as
/// [`WriteCap`]: the shells must open the stores an [`EditTools`] borrows, and they should reach
/// them through this crate rather than taking a direct `opencrayast-edit` dependency.
///
/// This keeps `opencrayast-mcp`'s dependency list exactly as the layering table records it
/// (`opencrayast-tools`, `opencrayast-core`): the stores are *reachable*, not a new edge. Opening
/// a store does not grant anything — it creates a `state/ws-<id>/plans/` directory and nothing
/// else, and the write capability for the three write tools still has to be minted by the shell
/// from parsed configuration, exactly as [`WriteCap`] requires.
pub use opencrayast_edit::{Clock, JournalStore, PlanStore, SystemClock};
pub use outline::{OutlineArgs, ast_outline};
pub use registry::{
    ToolAnnotations, ToolEntry, WRITE_TOOL_NAMES, find_tool, is_write_tool_name, tools_catalog,
    tools_for_mode,
};
pub use search::{SEARCH_STEP_BUDGET, SearchArgs, ast_search};
