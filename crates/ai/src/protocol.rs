//! Wire-level message shapes for the Codex App Server stdio JSON-RPC-like protocol.
//!
//! The protocol has no `jsonrpc` version field. A line is a request when it
//! carries both `id` and `method`, a notification when it carries `method`
//! without `id`, and a response when it carries `id` without `method`. This
//! module never treats "has an id" alone as "is a response", because the
//! server can also send `id` + `method` for its own requests to the client.

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// A JSON-RPC style request id. The protocol allows both string and integer
/// ids, so both must round-trip without coercion.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(untagged)]
pub enum RequestId {
    Number(i64),
    String(String),
}

/// An error object as carried in a response's `error` field.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ErrorObject {
    pub code: i64,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
}

impl ErrorObject {
    pub fn method_not_found(method: &str) -> Self {
        ErrorObject {
            code: -32601,
            message: format!("method not found: {method}"),
            data: None,
        }
    }
}

#[derive(Debug, Deserialize)]
struct RawMessage {
    #[serde(default)]
    id: Option<RequestId>,
    #[serde(default)]
    method: Option<String>,
    #[serde(default)]
    params: Option<Value>,
    #[serde(default)]
    result: Option<Value>,
    #[serde(default)]
    error: Option<ErrorObject>,
}

/// A parsed, classified incoming line.
#[derive(Debug, Clone)]
pub enum IncomingMessage {
    /// A response to a request we previously sent.
    Response {
        id: RequestId,
        outcome: Result<Value, ErrorObject>,
    },
    /// A one-way message from the server that expects no reply.
    Notification {
        method: String,
        params: Option<Value>,
    },
    /// A request the server sent to us, which must be answered.
    ServerRequest {
        id: RequestId,
        method: String,
        params: Option<Value>,
    },
}

#[derive(Debug)]
pub enum ParseError {
    Json(serde_json::Error),
    Malformed(&'static str),
}

impl std::fmt::Display for ParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ParseError::Json(e) => write!(f, "invalid JSON: {e}"),
            ParseError::Malformed(reason) => write!(f, "malformed message: {reason}"),
        }
    }
}

impl std::error::Error for ParseError {}

pub fn parse_incoming(line: &str) -> Result<IncomingMessage, ParseError> {
    let raw: RawMessage = serde_json::from_str(line).map_err(ParseError::Json)?;
    match (raw.id, raw.method) {
        (Some(id), Some(method)) => Ok(IncomingMessage::ServerRequest {
            id,
            method,
            params: raw.params,
        }),
        (None, Some(method)) => Ok(IncomingMessage::Notification {
            method,
            params: raw.params,
        }),
        (Some(id), None) => {
            let outcome = match raw.error {
                Some(err) => Err(err),
                None => Ok(raw.result.unwrap_or(Value::Null)),
            };
            Ok(IncomingMessage::Response { id, outcome })
        }
        (None, None) => Err(ParseError::Malformed("message has neither id nor method")),
    }
}

fn id_to_value(id: &RequestId) -> Value {
    serde_json::to_value(id).unwrap_or(Value::Null)
}

pub fn encode_request(id: &RequestId, method: &str, params: Option<Value>) -> String {
    let mut map = serde_json::Map::new();
    map.insert("id".to_string(), id_to_value(id));
    map.insert("method".to_string(), Value::String(method.to_string()));
    if let Some(params) = params {
        map.insert("params".to_string(), params);
    }
    Value::Object(map).to_string()
}

pub fn encode_notification(method: &str, params: Option<Value>) -> String {
    let mut map = serde_json::Map::new();
    map.insert("method".to_string(), Value::String(method.to_string()));
    if let Some(params) = params {
        map.insert("params".to_string(), params);
    }
    Value::Object(map).to_string()
}

pub fn encode_response_ok(id: &RequestId, result: Value) -> String {
    let mut map = serde_json::Map::new();
    map.insert("id".to_string(), id_to_value(id));
    map.insert("result".to_string(), result);
    Value::Object(map).to_string()
}

pub fn encode_response_err(id: &RequestId, error: ErrorObject) -> String {
    let mut map = serde_json::Map::new();
    map.insert("id".to_string(), id_to_value(id));
    map.insert(
        "error".to_string(),
        serde_json::to_value(error).unwrap_or(Value::Null),
    );
    Value::Object(map).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_response_with_integer_id() {
        let msg = parse_incoming(r#"{"id":1,"result":{"ok":true}}"#).unwrap();
        match msg {
            IncomingMessage::Response { id, outcome } => {
                assert_eq!(id, RequestId::Number(1));
                assert_eq!(outcome.unwrap()["ok"], Value::Bool(true));
            }
            other => panic!("unexpected classification: {other:?}"),
        }
    }

    #[test]
    fn classifies_response_with_string_id_and_error() {
        let msg =
            parse_incoming(r#"{"id":"abc","error":{"code":-32000,"message":"boom"}}"#).unwrap();
        match msg {
            IncomingMessage::Response { id, outcome } => {
                assert_eq!(id, RequestId::String("abc".to_string()));
                let err = outcome.unwrap_err();
                assert_eq!(err.code, -32000);
                assert_eq!(err.message, "boom");
            }
            other => panic!("unexpected classification: {other:?}"),
        }
    }

    #[test]
    fn classifies_notification_without_id() {
        let msg = parse_incoming(r#"{"method":"session/event","params":{"n":1}}"#).unwrap();
        match msg {
            IncomingMessage::Notification { method, params } => {
                assert_eq!(method, "session/event");
                assert_eq!(params.unwrap()["n"], 1);
            }
            other => panic!("unexpected classification: {other:?}"),
        }
    }

    #[test]
    fn classifies_server_request_with_id_and_method() {
        let msg =
            parse_incoming(r#"{"id":"srv-1","method":"approveCommand","params":{}}"#).unwrap();
        match msg {
            IncomingMessage::ServerRequest { id, method, .. } => {
                assert_eq!(id, RequestId::String("srv-1".to_string()));
                assert_eq!(method, "approveCommand");
            }
            other => panic!("unexpected classification: {other:?}"),
        }
    }

    #[test]
    fn rejects_message_without_id_or_method() {
        let err = parse_incoming(r#"{"result":{}}"#).unwrap_err();
        assert!(matches!(err, ParseError::Malformed(_)));
    }

    #[test]
    fn null_id_is_treated_as_absent() {
        let msg = parse_incoming(r#"{"id":null,"method":"session/event"}"#).unwrap();
        assert!(matches!(msg, IncomingMessage::Notification { .. }));
    }

    #[test]
    fn encodes_request_with_string_id() {
        let id = RequestId::String("abc".to_string());
        let line = encode_request(&id, "initialize", None);
        let value: Value = serde_json::from_str(&line).unwrap();
        assert_eq!(value["id"], "abc");
        assert_eq!(value["method"], "initialize");
        assert!(value.get("params").is_none());
    }

    #[test]
    fn encodes_response_err() {
        let id = RequestId::Number(7);
        let line = encode_response_err(&id, ErrorObject::method_not_found("x/y"));
        let value: Value = serde_json::from_str(&line).unwrap();
        assert_eq!(value["id"], 7);
        assert_eq!(value["error"]["code"], -32601);
    }
}
