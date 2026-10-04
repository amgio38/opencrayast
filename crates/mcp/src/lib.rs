//! MCP stdio transport (ARCHITECTURE.md): JSON-RPC framing, limits, logging setup.
//!
//! Tool logic lives in `opencrayast-tools`. This crate only reads/writes NDJSON on
//! stdio, enforces a per-message size cap, and dispatches to the tool registry.

#![allow(missing_docs)]

mod dispatch;
mod editstate;
mod rpc;
mod server;
mod transport;

pub use server::{ServerConfig, serve};
pub use transport::{MAX_MESSAGE_BYTES, ReadOutcome, WriteOutcome, read_message, write_message};

/// Protocol revision we speak (MCP).
pub const PROTOCOL_VERSION: &str = "2025-06-18";

/// Name in `initialize.serverInfo`.
pub const SERVER_NAME: &str = "opencrayast-mcp";
