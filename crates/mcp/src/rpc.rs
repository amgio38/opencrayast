//! JSON-RPC 2.0 envelopes for MCP (no batching).

use serde_json::{Map, Value, json};

pub const PARSE_ERROR: i64 = -32700;
pub const INVALID_REQUEST: i64 = -32600;
pub const METHOD_NOT_FOUND: i64 = -32601;
pub const INVALID_PARAMS: i64 = -32602;

/// A decoded client message.
#[derive(Debug)]
pub enum Incoming {
    /// Request with an id (must be answered).
    Request {
        id: Value,
        method: String,
        params: Value,
    },
    /// Notification (no id; never answered).
    Notification { method: String, params: Value },
}

/// Why a byte slice is not a usable message.
#[derive(Debug)]
pub enum Fault {
    /// Not JSON / not a UTF-8 object.
    Parse,
    /// JSON array (batching) or missing required fields.
    Invalid(Option<Value>),
}

/// Parse one line into a request or notification.
pub fn parse_incoming(bytes: &[u8]) -> Result<Incoming, Fault> {
    let text = std::str::from_utf8(bytes).map_err(|_| Fault::Parse)?;
    let value: Value = serde_json::from_str(text).map_err(|_| Fault::Parse)?;
    if value.is_array() {
        return Err(Fault::Invalid(None));
    }
    let obj = match value.as_object() {
        Some(o) => o,
        None => return Err(Fault::Invalid(None)),
    };
    if obj.get("jsonrpc").and_then(Value::as_str) != Some("2.0") {
        return Err(Fault::Invalid(obj.get("id").cloned()));
    }
    let method = match obj.get("method").and_then(Value::as_str) {
        Some(m) => m.to_string(),
        None => return Err(Fault::Invalid(obj.get("id").cloned())),
    };
    let params = obj.get("params").cloned().unwrap_or(Value::Null);
    match obj.get("id") {
        None => Ok(Incoming::Notification { method, params }),
        Some(id) if id.is_null() => Err(Fault::Invalid(Some(Value::Null))),
        Some(id) => Ok(Incoming::Request {
            id: id.clone(),
            method,
            params,
        }),
    }
}

pub fn error_response(id: Value, code: i64, message: &str) -> String {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": { "code": code, "message": message }
    })
    .to_string()
}

pub fn result_response(id: Value, result: Value) -> String {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "result": result
    })
    .to_string()
}

pub fn tool_result(id: Value, text: &str, is_error: bool) -> String {
    result_response(
        id,
        json!({
            "content": [{ "type": "text", "text": text }],
            "isError": is_error
        }),
    )
}

/// `params` as an object map; `null` / missing means empty.
pub fn params_map(params: &Value) -> Result<&Map<String, Value>, ()> {
    match params {
        Value::Null => {
            // Empty object borrowed from a static null-as-empty helper below.
            Ok(empty_map())
        }
        Value::Object(m) => Ok(m),
        _ => Err(()),
    }
}

fn empty_map() -> &'static Map<String, Value> {
    use std::sync::OnceLock;
    static EMPTY: OnceLock<Map<String, Value>> = OnceLock::new();
    EMPTY.get_or_init(Map::new)
}
