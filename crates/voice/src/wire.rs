//! The `voice/1` messages: what the engine sends Hane (`hello`, the segment
//! events) and what Hane sends it (`hello.ok`, `capture.*`).
//!
//! Frames are the 1.0 link's `u32 LE + UTF-8 JSON`. Every incoming message is
//! checked here, before anything reaches the editor: a message that does not
//! fit the contract is an error and the connection is closed. Text and tokens
//! never appear in an error.

use hane_editor::{
    EndStatus, MAX_SEGMENT_BYTES, PolishMode, PolishResult, RawStatus, VoiceEnvelope, VoiceEvent,
};
use serde_json::{Map, Value, json};

/// The one protocol both sides must name. There is no version negotiation.
pub const PROTOCOL: &str = "voice/1";

/// Why a message was refused. A fixed description, never the content.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct WireError(pub &'static str);

impl std::fmt::Display for WireError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.0)
    }
}

/// The engine's first message.
pub struct Hello {
    pub token: String,
    pub pid: u32,
}

fn object(body: &[u8]) -> Result<Map<String, Value>, WireError> {
    match serde_json::from_slice::<Value>(body) {
        Ok(Value::Object(map)) => Ok(map),
        _ => Err(WireError("frame is not a JSON object")),
    }
}

fn text<'a>(map: &'a Map<String, Value>, key: &str) -> Option<&'a str> {
    map.get(key)?.as_str()
}

/// A positive JSON integer (`1`, not `1.0`, not a string).
fn number(map: &Map<String, Value>, key: &str) -> Option<u64> {
    map.get(key)?.as_u64()
}

fn is_session_id(value: &str) -> bool {
    value.len() == 32 && value.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// A reason code: the fixed lowercase identifiers of the contract. Only the
/// shape is checked so a code added within `voice/1` is still shown; the
/// text is limited so it is safe to put on screen.
fn is_code(value: &str) -> bool {
    (1..=64).contains(&value.len())
        && value.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
}

pub fn parse_hello(body: &[u8]) -> Result<Hello, WireError> {
    let map = object(body)?;
    if text(&map, "type") != Some("hello") {
        return Err(WireError("first message is not hello"));
    }
    if text(&map, "protocol") != Some(PROTOCOL) {
        return Err(WireError("protocol mismatch"));
    }
    let token = text(&map, "token").ok_or(WireError("hello has no token"))?;
    let pid = number(&map, "pid")
        .and_then(|pid| u32::try_from(pid).ok())
        .ok_or(WireError("hello has no pid"))?;
    Ok(Hello {
        token: token.to_owned(),
        pid,
    })
}

pub fn hello_ok() -> Vec<u8> {
    json!({"type": "hello.ok", "protocol": PROTOCOL})
        .to_string()
        .into_bytes()
}

/// The text taken around the caret when recording starts (a fixed snapshot).
/// Sent only when the user allowed document context for polishing.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Context {
    pub before: String,
    pub after: String,
}

pub fn capture_begin(session_id: &str, context: Option<&Context>) -> Vec<u8> {
    let mut message = json!({"type": "capture.begin", "sessionId": session_id, "options": {}});
    if let Some(context) = context {
        message["context"] = json!({"before": context.before, "after": context.after});
    }
    message.to_string().into_bytes()
}

pub fn capture_stop(session_id: &str) -> Vec<u8> {
    json!({"type": "capture.stop", "sessionId": session_id})
        .to_string()
        .into_bytes()
}

pub fn capture_cancel(session_id: &str) -> Vec<u8> {
    json!({"type": "capture.cancel", "sessionId": session_id})
        .to_string()
        .into_bytes()
}

fn segment_text(map: &Map<String, Value>) -> Result<String, WireError> {
    let value = text(map, "text").ok_or(WireError("segment has no text"))?;
    if value.is_empty() {
        return Err(WireError("segment text is empty"));
    }
    if value.len() > MAX_SEGMENT_BYTES {
        return Err(WireError("segment text is too long"));
    }
    Ok(value.to_owned())
}

fn code(map: &Map<String, Value>) -> Result<String, WireError> {
    match text(map, "code") {
        Some(value) if is_code(value) => Ok(value.to_owned()),
        _ => Err(WireError("missing or malformed reason code")),
    }
}

/// Checks one frame body and turns it into the editor's event. Unknown event
/// types, a missing field or a text where none is allowed are refused.
pub fn parse_event(body: &[u8]) -> Result<VoiceEnvelope, WireError> {
    let map = object(body)?;
    let session_id = text(&map, "sessionId").ok_or(WireError("event has no session id"))?;
    if !is_session_id(session_id) {
        return Err(WireError("malformed session id"));
    }
    let event_no = number(&map, "eventNo")
        .filter(|no| *no >= 1)
        .ok_or(WireError("malformed event number"))?;
    let seq = || {
        number(&map, "seq")
            .filter(|seq| *seq >= 1)
            .ok_or(WireError("malformed segment number"))
    };
    let event = match text(&map, "type") {
        Some("session.start") => VoiceEvent::SessionStart,
        Some("segment.raw") => {
            let seq = seq()?;
            let status = match text(&map, "status") {
                Some("ok") => {
                    let polish = match text(&map, "polish") {
                        Some("pending") => PolishMode::Pending,
                        Some("off") => PolishMode::Off,
                        _ => return Err(WireError("malformed polish mode")),
                    };
                    RawStatus::Ok {
                        text: segment_text(&map)?,
                        polish,
                    }
                }
                Some("silent") if !map.contains_key("text") => RawStatus::Silent,
                Some("failed") if !map.contains_key("text") => RawStatus::Failed {
                    code: code(&map)?,
                },
                _ => return Err(WireError("malformed raw status")),
            };
            VoiceEvent::SegmentRaw { seq, status }
        }
        Some("segment.polished") => {
            let seq = seq()?;
            let result = match text(&map, "status") {
                Some("ok") => PolishResult::Ok(segment_text(&map)?),
                Some("failed") if !map.contains_key("text") => PolishResult::Failed(code(&map)?),
                _ => return Err(WireError("malformed polish status")),
            };
            VoiceEvent::SegmentPolished { seq, result }
        }
        Some("session.end") => {
            let last_seq = map
                .get("lastSeq")
                .and_then(Value::as_u64)
                .ok_or(WireError("malformed lastSeq"))?;
            let status = match text(&map, "status") {
                Some("completed") => EndStatus::Completed,
                Some("cancelled") => EndStatus::Cancelled,
                Some("failed") => EndStatus::Failed { code: code(&map)? },
                _ => return Err(WireError("malformed end status")),
            };
            VoiceEvent::SessionEnd { status, last_seq }
        }
        _ => return Err(WireError("unknown event type")),
    };
    Ok(VoiceEnvelope {
        session_id: session_id.to_owned(),
        event_no,
        event,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const ID: &str = "a162d4cb5747474d9cf7fcaf1833dabb";

    fn event(body: Value) -> Result<VoiceEnvelope, WireError> {
        parse_event(body.to_string().as_bytes())
    }

    #[test]
    fn parses_the_documented_raw_segment() {
        let parsed = parse_event(
            r#"{"type":"segment.raw","sessionId":"a162d4cb5747474d9cf7fcaf1833dabb","eventNo":3,"seq":2,"status":"ok","text":"つぎの章です","polish":"pending"}"#
                .as_bytes(),
        )
        .unwrap();
        assert_eq!(parsed.event_no, 3);
        assert_eq!(
            parsed.event,
            VoiceEvent::SegmentRaw {
                seq: 2,
                status: RawStatus::Ok {
                    text: "つぎの章です".into(),
                    polish: PolishMode::Pending
                }
            }
        );
    }

    #[test]
    fn parses_every_event_type() {
        let start = event(json!({"type":"session.start","sessionId":ID,"eventNo":1})).unwrap();
        assert_eq!(start.event, VoiceEvent::SessionStart);
        let silent = event(
            json!({"type":"segment.raw","sessionId":ID,"eventNo":2,"seq":1,"status":"silent"}),
        )
        .unwrap();
        assert_eq!(
            silent.event,
            VoiceEvent::SegmentRaw {
                seq: 1,
                status: RawStatus::Silent
            }
        );
        let failed = event(json!({"type":"segment.polished","sessionId":ID,"eventNo":3,"seq":1,"status":"failed","code":"timeout"})).unwrap();
        assert_eq!(
            failed.event,
            VoiceEvent::SegmentPolished {
                seq: 1,
                result: PolishResult::Failed("timeout".into())
            }
        );
        let end = event(json!({"type":"session.end","sessionId":ID,"eventNo":4,"status":"failed","lastSeq":1,"code":"microphone_lost"})).unwrap();
        assert_eq!(
            end.event,
            VoiceEvent::SessionEnd {
                status: EndStatus::Failed {
                    code: "microphone_lost".into()
                },
                last_seq: 1
            }
        );
    }

    #[test]
    fn refuses_what_does_not_fit_the_contract() {
        let base = || json!({"type":"segment.raw","sessionId":ID,"eventNo":1,"seq":1,"status":"ok","text":"a","polish":"off"});
        assert!(event(base()).is_ok());
        let mut cases = Vec::new();
        for (key, value) in [
            ("eventNo", json!(0)),
            ("eventNo", json!(1.5)),
            ("eventNo", json!("1")),
            ("eventNo", json!(-1)),
            ("seq", json!(0)),
            ("sessionId", json!("ABCDEF")),
            ("sessionId", json!(ID.to_uppercase())),
            ("text", json!("")),
            ("text", json!("あ".repeat(MAX_SEGMENT_BYTES))),
            ("polish", json!("maybe")),
            ("status", json!("done")),
            ("type", json!("segment.unknown")),
        ] {
            let mut message = base();
            message[key] = value;
            cases.push(message);
        }
        for message in cases {
            assert!(event(message.clone()).is_err(), "{message}");
        }
        // Text on a silent segment, a failure without a code, and a code with
        // free text are all refused.
        assert!(event(json!({"type":"segment.raw","sessionId":ID,"eventNo":1,"seq":1,"status":"silent","text":"a"})).is_err());
        assert!(event(json!({"type":"segment.raw","sessionId":ID,"eventNo":1,"seq":1,"status":"failed"})).is_err());
        assert!(event(json!({"type":"segment.raw","sessionId":ID,"eventNo":1,"seq":1,"status":"failed","code":"本文 secret"})).is_err());
        assert!(event(json!({"type":"session.end","sessionId":ID,"eventNo":1,"status":"completed"})).is_err());
        assert!(parse_event(b"[1]").is_err());
        assert!(parse_event(b"not json").is_err());
    }

    #[test]
    fn hello_has_exactly_the_agreed_shape() {
        let hello = parse_hello(br#"{"type":"hello","protocol":"voice/1","token":"t","pid":42}"#)
            .unwrap();
        assert_eq!((hello.token.as_str(), hello.pid), ("t", 42));
        for bad in [
            br#"{"type":"hello","protocol":"voice/2","token":"t","pid":42}"#.as_slice(),
            br#"{"type":"hello","protocol":"voice/1","token":7,"pid":42}"#,
            br#"{"type":"hello","protocol":"voice/1","token":"t","pid":"42"}"#,
            br#"{"type":"hello","protocol":"voice/1","pid":42}"#,
            br#"{"type":"session.start"}"#,
        ] {
            assert!(parse_hello(bad).is_err());
        }
        let reply: Value = serde_json::from_slice(&hello_ok()).unwrap();
        assert_eq!(reply, json!({"type":"hello.ok","protocol":"voice/1"}));
    }

    #[test]
    fn capture_messages_carry_the_session_and_only_the_agreed_fields() {
        let begin: Value = serde_json::from_slice(&capture_begin(
            ID,
            Some(&Context {
                before: "前".into(),
                after: "後".into(),
            }),
        ))
        .unwrap();
        assert_eq!(begin["type"], "capture.begin");
        assert_eq!(begin["context"], json!({"before":"前","after":"後"}));
        let bare: Value = serde_json::from_slice(&capture_begin(ID, None)).unwrap();
        assert!(bare.get("context").is_none());
        for (body, kind) in [
            (capture_stop(ID), "capture.stop"),
            (capture_cancel(ID), "capture.cancel"),
        ] {
            let value: Value = serde_json::from_slice(&body).unwrap();
            assert_eq!(value, json!({"type": kind, "sessionId": ID}));
        }
    }
}
