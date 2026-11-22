//! Request / response shapes, error codes and version negotiation.

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

/// The protocol version this build speaks.
pub const PROTOCOL_MAJOR: u32 = 1;
pub const PROTOCOL_MINOR: u32 = 0;

/// Largest text an edit may carry (UTF-8 bytes).
pub const MAX_EDIT_TEXT_BYTES: usize = 32 * 1024;
pub const MAX_BEFORE_CHARS: usize = 1_200;
pub const MAX_AFTER_CHARS: usize = 200;
pub const MAX_PENDING_HANDLES: usize = 2;
/// How long a range handle stays usable.
pub const HANDLE_LIFETIME_MS: u64 = 120_000;
const MAX_ID_BYTES: usize = 64;
const MAX_STRING_PARAM_BYTES: usize = 256;

/// Whether a failed request may have changed the document.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Effect {
    NotApplied,
    Unknown,
}

impl Effect {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::NotApplied => "not_applied",
            Self::Unknown => "unknown",
        }
    }
}

/// The closed set of error codes. A response carries the code and the effect
/// only: never document text, paths or exception messages.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub enum ErrorCode {
    InvalidRequest,
    DeadlineExceeded,
    Busy,
    UnsupportedVersion,
    UnsupportedDocument,
    StaleTarget,
    NotEditable,
    StaleCursor,
    StaleVersion,
    InProgress,
    OperationConflict,
    RangeChanged,
    RangeUnverifiable,
    RangeExpired,
    /// A fixed code for every authentication refusal.
    Unauthorized,
    /// An unexpected failure inside Hane. The effect is `unknown` unless the
    /// request was refused before reaching the editor.
    Internal,
}

impl ErrorCode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::InvalidRequest => "INVALID_REQUEST",
            Self::DeadlineExceeded => "DEADLINE_EXCEEDED",
            Self::Busy => "BUSY",
            Self::UnsupportedVersion => "UNSUPPORTED_VERSION",
            Self::UnsupportedDocument => "UNSUPPORTED_DOCUMENT",
            Self::StaleTarget => "STALE_TARGET",
            Self::NotEditable => "NOT_EDITABLE",
            Self::StaleCursor => "STALE_CURSOR",
            Self::StaleVersion => "STALE_VERSION",
            Self::InProgress => "IN_PROGRESS",
            Self::OperationConflict => "OPERATION_CONFLICT",
            Self::RangeChanged => "RANGE_CHANGED",
            Self::RangeUnverifiable => "RANGE_UNVERIFIABLE",
            Self::RangeExpired => "RANGE_EXPIRED",
            Self::Unauthorized => "UNAUTHORIZED",
            Self::Internal => "INTERNAL_ERROR",
        }
    }
}

/// A failure the link reports to the client.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct LinkError {
    pub code: ErrorCode,
    pub effect: Effect,
}

impl LinkError {
    pub const fn new(code: ErrorCode, effect: Effect) -> Self {
        Self { code, effect }
    }

    /// The edit did not happen.
    pub const fn not_applied(code: ErrorCode) -> Self {
        Self::new(code, Effect::NotApplied)
    }

    pub const fn unknown(code: ErrorCode) -> Self {
        Self::new(code, Effect::Unknown)
    }
}

pub type LinkResult = Result<Value, LinkError>;

/// A parsed request frame. `id` is echoed verbatim (a number or a short string).
#[derive(Clone, Debug)]
pub struct Request {
    pub id: Value,
    pub method: String,
    pub params: Value,
}

/// Why a frame is not a request. A syntax error closes the connection; a
/// well-formed JSON value with a wrong shape only fails that request.
#[derive(Debug, Eq, PartialEq)]
pub enum ParseError {
    Syntax,
    Shape { id: Value },
}

pub fn parse_request(body: &[u8]) -> Result<Request, ParseError> {
    let value: Value = serde_json::from_slice(body).map_err(|_| ParseError::Syntax)?;
    let Value::Object(mut object) = value else {
        return Err(ParseError::Syntax);
    };
    let id = match object.remove("id") {
        Some(id @ Value::Number(_)) => id,
        Some(Value::String(id)) if id.len() <= MAX_ID_BYTES => Value::String(id),
        _ => return Err(ParseError::Shape { id: Value::Null }),
    };
    let Some(Value::String(method)) = object.remove("method") else {
        return Err(ParseError::Shape { id });
    };
    if method.len() > MAX_ID_BYTES {
        return Err(ParseError::Shape { id });
    }
    let params = object
        .remove("params")
        .unwrap_or_else(|| Value::Object(Map::new()));
    if !params.is_object() {
        return Err(ParseError::Shape { id });
    }
    Ok(Request { id, method, params })
}

pub fn ok_response(id: &Value, result: Value) -> Vec<u8> {
    serde_json::to_vec(&json!({ "id": id, "result": result })).expect("a JSON value serializes")
}

pub fn error_response(id: &Value, error: LinkError) -> Vec<u8> {
    serde_json::to_vec(&json!({
        "id": id,
        "error": { "code": error.code.as_str(), "effect": error.effect.as_str() },
    }))
    .expect("a JSON value serializes")
}

/// `major.minor`, as the wire carries it: `{"major":1,"minor":0}`.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
pub struct Version {
    pub major: u32,
    pub minor: u32,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HelloParams {
    pub token: String,
    pub min_version: Version,
    pub max_version: Version,
}

/// Picks the protocol version both sides speak: the same major, the lower of
/// the two minors, and never below what the client requires. A major mismatch
/// is incompatible.
pub fn negotiate(client_min: Version, client_max: Version) -> Option<Version> {
    if client_min > client_max
        || client_max.major < PROTOCOL_MAJOR
        || client_min.major > PROTOCOL_MAJOR
    {
        return None;
    }
    // A client that accepts a newer major still speaks ours if its range spans it.
    let client_minor = if client_max.major == PROTOCOL_MAJOR {
        client_max.minor
    } else {
        u32::MAX
    };
    #[allow(
        clippy::unnecessary_min_or_max,
        reason = "the minor is 0 for now; the rule must keep holding once it grows"
    )]
    let minor = PROTOCOL_MINOR.min(client_minor);
    let agreed = Version {
        major: PROTOCOL_MAJOR,
        minor,
    };
    (agreed >= client_min).then_some(agreed)
}

/// What the server offers after authentication.
pub fn hello_result(agreed: Version, instance_id: &str) -> Value {
    json!({
        "protocol": agreed,
        "instanceId": instance_id,
        "features": ["context.get", "edit.insert", "edit.replace"],
        "limits": {
            "maxFrameBytes": crate::frame::MAX_FRAME_BYTES,
            "maxEditTextBytes": MAX_EDIT_TEXT_BYTES,
            "maxBeforeChars": MAX_BEFORE_CHARS,
            "maxAfterChars": MAX_AFTER_CHARS,
            "handleLifetimeMs": HANDLE_LIFETIME_MS,
        },
    })
}

// ---- method parameters -------------------------------------------------

/// The window a request is aimed at: the native handle captured at the start
/// of recording. It is checked against Hane's registered windows.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq)]
#[serde(transparent)]
pub struct WindowHandle(pub i64);

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ContextGetParams {
    pub hwnd: WindowHandle,
    #[serde(default)]
    pub before_chars: Option<usize>,
    #[serde(default)]
    pub after_chars: Option<usize>,
    #[serde(default)]
    pub expected_document_id: Option<String>,
    #[serde(default)]
    pub expected_revision: Option<String>,
    #[serde(default)]
    pub include_snapshot: bool,
    #[serde(default)]
    pub pending_range_handles: Vec<String>,
}

impl ContextGetParams {
    pub fn before_chars(&self) -> usize {
        self.before_chars
            .unwrap_or(MAX_BEFORE_CHARS)
            .min(MAX_BEFORE_CHARS)
    }

    pub fn after_chars(&self) -> usize {
        self.after_chars
            .unwrap_or(MAX_AFTER_CHARS)
            .min(MAX_AFTER_CHARS)
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SelectionBytes {
    pub anchor_byte: usize,
    pub active_byte: usize,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct InsertParams {
    pub operation_id: String,
    pub deadline_tick: String,
    pub hwnd: WindowHandle,
    pub document_id: String,
    pub expected_revision: String,
    pub expected_selection: SelectionBytes,
    pub text: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ReplaceParams {
    pub operation_id: String,
    pub deadline_tick: String,
    pub hwnd: WindowHandle,
    pub document_id: String,
    pub range_handle: String,
    pub inserted_revision: String,
    pub expected_text_hash: String,
    pub replacement: String,
}

/// A request the editor side carries out.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum HostRequest {
    ContextGet(ContextGetParams),
    Insert(InsertParams),
    Replace(ReplaceParams),
}

fn invalid() -> LinkError {
    LinkError::not_applied(ErrorCode::InvalidRequest)
}

fn is_token(value: &str, max: usize) -> bool {
    !value.is_empty()
        && value.len() <= max
        && value.bytes().all(|byte| {
            byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_' || byte == b'.'
        })
}

fn is_decimal(value: &str) -> bool {
    !value.is_empty() && value.len() <= 20 && value.bytes().all(|byte| byte.is_ascii_digit())
}

fn is_hex_hash(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn bounded(value: &str) -> bool {
    !value.is_empty() && value.len() <= MAX_STRING_PARAM_BYTES
}

impl HostRequest {
    /// Parses and validates the parameters of `method` before anything is
    /// allocated for, or applied to, an editor. `None` for a method this
    /// service does not know.
    pub fn parse(method: &str, params: Value) -> Option<Result<Self, LinkError>> {
        Some(match method {
            "context.get" => parse_context_get(params),
            "edit.insert" => parse_insert(params),
            "edit.replace" => parse_replace(params),
            _ => return None,
        })
    }

    /// The idempotency key of a mutation.
    pub fn operation_id(&self) -> Option<&str> {
        match self {
            Self::ContextGet(_) => None,
            Self::Insert(params) => Some(&params.operation_id),
            Self::Replace(params) => Some(&params.operation_id),
        }
    }

    /// The absolute `GetTickCount64` deadline of a mutation.
    pub fn deadline_tick(&self) -> Option<u64> {
        match self {
            Self::ContextGet(_) => None,
            Self::Insert(params) => params.deadline_tick.parse().ok(),
            Self::Replace(params) => params.deadline_tick.parse().ok(),
        }
    }

    pub fn method(&self) -> &'static str {
        match self {
            Self::ContextGet(_) => "context.get",
            Self::Insert(_) => "edit.insert",
            Self::Replace(_) => "edit.replace",
        }
    }

    /// Canonical bytes of the request, for telling a resend (same operation id,
    /// same content) from a conflicting reuse of the id.
    pub fn fingerprint(&self) -> String {
        let canonical = match self {
            Self::ContextGet(_) => Value::Null,
            Self::Insert(params) => json!([
                params.deadline_tick,
                params.hwnd.0,
                params.document_id,
                params.expected_revision,
                [
                    params.expected_selection.anchor_byte,
                    params.expected_selection.active_byte
                ],
                params.text,
            ]),
            Self::Replace(params) => json!([
                params.deadline_tick,
                params.hwnd.0,
                params.document_id,
                params.range_handle,
                params.inserted_revision,
                params.expected_text_hash,
                params.replacement,
            ]),
        };
        crate::auth::sha256_hex(format!("{}\n{canonical}", self.method()).as_bytes())
    }
}

fn parse_context_get(params: Value) -> Result<HostRequest, LinkError> {
    let parsed: ContextGetParams = serde_json::from_value(params).map_err(|_| invalid())?;
    if parsed.pending_range_handles.len() > MAX_PENDING_HANDLES
        || !parsed
            .pending_range_handles
            .iter()
            .all(|handle| bounded(handle))
        || parsed
            .expected_document_id
            .as_deref()
            .is_some_and(|id| !bounded(id))
        || parsed
            .expected_revision
            .as_deref()
            .is_some_and(|revision| !is_decimal(revision))
    {
        return Err(invalid());
    }
    // A snapshot is a read of one exact version of one exact document.
    if parsed.include_snapshot
        && (parsed.expected_document_id.is_none() || parsed.expected_revision.is_none())
    {
        return Err(invalid());
    }
    Ok(HostRequest::ContextGet(parsed))
}

fn parse_insert(params: Value) -> Result<HostRequest, LinkError> {
    let parsed: InsertParams = serde_json::from_value(params).map_err(|_| invalid())?;
    if !is_token(&parsed.operation_id, MAX_ID_BYTES)
        || !is_decimal(&parsed.deadline_tick)
        || parsed.deadline_tick.parse::<u64>().is_err()
        || !bounded(&parsed.document_id)
        || !is_decimal(&parsed.expected_revision)
        || parsed.text.is_empty()
        || parsed.text.len() > MAX_EDIT_TEXT_BYTES
    {
        return Err(invalid());
    }
    Ok(HostRequest::Insert(parsed))
}

fn parse_replace(params: Value) -> Result<HostRequest, LinkError> {
    let parsed: ReplaceParams = serde_json::from_value(params).map_err(|_| invalid())?;
    if !is_token(&parsed.operation_id, MAX_ID_BYTES)
        || !is_decimal(&parsed.deadline_tick)
        || parsed.deadline_tick.parse::<u64>().is_err()
        || !bounded(&parsed.document_id)
        || !bounded(&parsed.range_handle)
        || !is_decimal(&parsed.inserted_revision)
        || !is_hex_hash(&parsed.expected_text_hash)
        || parsed.replacement.is_empty()
        || parsed.replacement.len() > MAX_EDIT_TEXT_BYTES
    {
        return Err(invalid());
    }
    Ok(HostRequest::Replace(parsed))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(major: u32, minor: u32) -> Version {
        Version { major, minor }
    }

    #[test]
    fn negotiation_follows_the_major_and_takes_the_lower_minor() {
        assert_eq!(negotiate(v(1, 0), v(1, 0)), Some(v(1, 0)));
        // A newer client minor is spoken at our minor.
        assert_eq!(negotiate(v(1, 0), v(1, 7)), Some(v(1, 0)));
        // A client range that spans several majors still lands on ours.
        assert_eq!(negotiate(v(1, 0), v(2, 3)), Some(v(1, 0)));
        // A client that needs a newer minor than ours cannot be served.
        assert_eq!(negotiate(v(1, 1), v(1, 2)), None);
        assert_eq!(negotiate(v(2, 0), v(2, 1)), None);
        assert_eq!(negotiate(v(0, 1), v(0, 9)), None);
        assert_eq!(negotiate(v(1, 2), v(1, 0)), None);
    }

    #[test]
    fn a_request_must_be_a_json_object_with_a_method() {
        assert_eq!(parse_request(b"not json").unwrap_err(), ParseError::Syntax);
        assert_eq!(parse_request(b"[1]").unwrap_err(), ParseError::Syntax);
        assert_eq!(
            parse_request(br#"{"id":3}"#).unwrap_err(),
            ParseError::Shape { id: json!(3) }
        );
        assert_eq!(
            parse_request(br#"{"method":"x"}"#).unwrap_err(),
            ParseError::Shape { id: Value::Null }
        );
        assert_eq!(
            parse_request(br#"{"id":1,"method":"x","params":[]}"#).unwrap_err(),
            ParseError::Shape { id: json!(1) }
        );
        let request = parse_request(br#"{"id":"a","method":"hello","unknown":1}"#).unwrap();
        assert_eq!(request.method, "hello");
        assert_eq!(request.id, json!("a"));
    }

    #[test]
    fn error_responses_carry_only_a_code_and_an_effect() {
        let body = error_response(&json!(7), LinkError::unknown(ErrorCode::InProgress));
        let value: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(
            value,
            json!({"id":7,"error":{"code":"IN_PROGRESS","effect":"unknown"}})
        );
    }

    fn insert_params() -> Value {
        json!({
            "operationId": "op-1", "deadlineTick": "123456", "hwnd": 4242,
            "documentId": "doc", "expectedRevision": "9",
            "expectedSelection": {"anchorByte": 1, "activeByte": 1},
            "text": "こんにちは",
            "futureField": {"ignored": true},
        })
    }

    #[test]
    fn insert_params_validate_and_unknown_fields_are_skipped() {
        let parsed = HostRequest::parse("edit.insert", insert_params())
            .unwrap()
            .unwrap();
        assert_eq!(parsed.operation_id(), Some("op-1"));
        assert_eq!(parsed.deadline_tick(), Some(123_456));
        for (key, bad) in [
            ("operationId", json!("")),
            ("operationId", json!("has space")),
            ("deadlineTick", json!("12x")),
            ("deadlineTick", json!(12)),
            ("expectedRevision", json!("-1")),
            ("text", json!("")),
            ("text", json!("あ".repeat(MAX_EDIT_TEXT_BYTES))),
        ] {
            let mut params = insert_params();
            params[key] = bad;
            let error = HostRequest::parse("edit.insert", params)
                .unwrap()
                .unwrap_err();
            assert_eq!(
                error,
                LinkError::not_applied(ErrorCode::InvalidRequest),
                "{key}"
            );
        }
        let mut missing = insert_params();
        missing.as_object_mut().unwrap().remove("hwnd");
        assert!(HostRequest::parse("edit.insert", missing).unwrap().is_err());
        assert!(HostRequest::parse("edit.nope", insert_params()).is_none());
    }

    #[test]
    fn an_edit_text_at_the_limit_is_accepted_and_one_byte_more_is_not() {
        let mut params = insert_params();
        params["text"] = json!("a".repeat(MAX_EDIT_TEXT_BYTES));
        assert!(
            HostRequest::parse("edit.insert", params.clone())
                .unwrap()
                .is_ok()
        );
        params["text"] = json!("a".repeat(MAX_EDIT_TEXT_BYTES + 1));
        assert!(HostRequest::parse("edit.insert", params).unwrap().is_err());
    }

    #[test]
    fn context_params_clamp_the_windows_and_require_a_version_for_a_snapshot() {
        let params = json!({"hwnd": 1, "beforeChars": 99999, "afterChars": 5});
        let HostRequest::ContextGet(parsed) =
            HostRequest::parse("context.get", params).unwrap().unwrap()
        else {
            panic!("context.get");
        };
        assert_eq!(parsed.before_chars(), MAX_BEFORE_CHARS);
        assert_eq!(parsed.after_chars(), 5);
        let bare = json!({"hwnd": 1});
        let HostRequest::ContextGet(parsed) =
            HostRequest::parse("context.get", bare).unwrap().unwrap()
        else {
            panic!("context.get");
        };
        assert_eq!((parsed.before_chars(), parsed.after_chars()), (1_200, 200));
        assert!(
            HostRequest::parse("context.get", json!({"hwnd": 1, "includeSnapshot": true}))
                .unwrap()
                .is_err()
        );
        assert!(
            HostRequest::parse(
                "context.get",
                json!({"hwnd": 1, "pendingRangeHandles": ["a","b","c"]})
            )
            .unwrap()
            .is_err()
        );
    }

    #[test]
    fn the_fingerprint_ignores_nothing_that_changes_the_edit() {
        let base = HostRequest::parse("edit.insert", insert_params())
            .unwrap()
            .unwrap();
        let same = HostRequest::parse("edit.insert", insert_params())
            .unwrap()
            .unwrap();
        assert_eq!(base.fingerprint(), same.fingerprint());
        let mut other = insert_params();
        other["text"] = json!("さようなら");
        let other = HostRequest::parse("edit.insert", other).unwrap().unwrap();
        assert_ne!(base.fingerprint(), other.fingerprint());
        let mut other = insert_params();
        other["expectedRevision"] = json!("10");
        let other = HostRequest::parse("edit.insert", other).unwrap().unwrap();
        assert_ne!(base.fingerprint(), other.fingerprint());
    }
}
