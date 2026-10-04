//! LMT-07 moved in-crate. See `crates/edit/src/spec/lmt07_binary_edit_spec.rs`.
//!
//! These tests need WRITE MODE: they apply a plan. SEC-FIX 4 made the write capability
//! mintable only from inside the crate, so an integration test can no longer construct a
//! write context at all. See the header of `sec_audit_poc.rs` for the full reasoning.
