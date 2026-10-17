use std::collections::HashSet;
use std::fmt;
use std::result::Result as StdResult;

use ring::digest::{Context, SHA256, digest};
use serde::de::{self, Deserialize, DeserializeOwned, Deserializer, MapAccess, SeqAccess, Visitor};

use crate::error::{ErrorCode, Result, UpdateError};

pub(crate) fn hex_encode(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push(HEX[usize::from(byte >> 4)] as char);
        out.push(HEX[usize::from(byte & 0x0f)] as char);
    }
    out
}

pub(crate) fn sha256_hex(bytes: &[u8]) -> String {
    hex_encode(digest(&SHA256, bytes).as_ref())
}

pub(crate) fn sha256_context() -> Context {
    Context::new(&SHA256)
}

pub(crate) fn is_lower_hex(text: &str, len: usize) -> bool {
    text.len() == len && text.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

pub(crate) fn is_sha256_hex(text: &str) -> bool {
    is_lower_hex(text, 64)
}

pub(crate) fn malformed(detail: impl Into<String>) -> UpdateError {
    UpdateError::new(ErrorCode::MalformedMetadata, detail)
}

/// Parses `YYYY-MM-DDTHH:MM:SSZ` (UTC only, no fraction) into Unix seconds.
pub(crate) fn parse_rfc3339_utc(text: &str) -> Option<i64> {
    let bytes = text.as_bytes();
    if bytes.len() != 20
        || bytes[4] != b'-'
        || bytes[7] != b'-'
        || bytes[10] != b'T'
        || bytes[13] != b':'
        || bytes[16] != b':'
        || bytes[19] != b'Z'
    {
        return None;
    }
    let field = |from: usize, to: usize| -> Option<i64> {
        let part = text.get(from..to)?;
        if part.bytes().all(|b| b.is_ascii_digit()) {
            part.parse().ok()
        } else {
            None
        }
    };
    let year = field(0, 4)?;
    let month = field(5, 7)?;
    let day = field(8, 10)?;
    let hour = field(11, 13)?;
    let minute = field(14, 16)?;
    let second = field(17, 19)?;
    if !(1..=12).contains(&month) || hour > 23 || minute > 59 || second > 59 {
        return None;
    }
    if !(1..=days_in_month(year, month)).contains(&day) {
        return None;
    }
    Some(days_from_civil(year, month, day) * 86_400 + hour * 3_600 + minute * 60 + second)
}

fn is_leap(year: i64) -> bool {
    (year % 4 == 0 && year % 100 != 0) || year % 400 == 0
}

fn days_in_month(year: i64, month: i64) -> i64 {
    match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        _ if is_leap(year) => 29,
        _ => 28,
    }
}

/// Days since 1970-01-01 for a proleptic Gregorian date.
fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let y = if month <= 2 { year - 1 } else { year };
    let era = (if y >= 0 { y } else { y - 399 }) / 400;
    let yoe = y - era * 400;
    let mp = (month + 9) % 12;
    let doy = (153 * mp + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// Accepts any JSON value but fails on a duplicate object key anywhere.
struct StrictValue;

impl<'de> Deserialize<'de> for StrictValue {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> StdResult<Self, D::Error> {
        deserializer.deserialize_any(StrictVisitor)
    }
}

struct StrictVisitor;

impl<'de> Visitor<'de> for StrictVisitor {
    type Value = StrictValue;

    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("a JSON value")
    }

    fn visit_bool<E: de::Error>(self, _v: bool) -> StdResult<StrictValue, E> {
        Ok(StrictValue)
    }

    fn visit_i64<E: de::Error>(self, _v: i64) -> StdResult<StrictValue, E> {
        Ok(StrictValue)
    }

    fn visit_u64<E: de::Error>(self, _v: u64) -> StdResult<StrictValue, E> {
        Ok(StrictValue)
    }

    fn visit_f64<E: de::Error>(self, _v: f64) -> StdResult<StrictValue, E> {
        Ok(StrictValue)
    }

    fn visit_str<E: de::Error>(self, _v: &str) -> StdResult<StrictValue, E> {
        Ok(StrictValue)
    }

    fn visit_unit<E: de::Error>(self) -> StdResult<StrictValue, E> {
        Ok(StrictValue)
    }

    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> StdResult<StrictValue, A::Error> {
        while seq.next_element::<StrictValue>()?.is_some() {}
        Ok(StrictValue)
    }

    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> StdResult<StrictValue, A::Error> {
        let mut seen: HashSet<String> = HashSet::new();
        while let Some(key) = map.next_key::<String>()? {
            if !seen.insert(key.clone()) {
                return Err(de::Error::custom(format!("duplicate key {key:?}")));
            }
            map.next_value::<StrictValue>()?;
        }
        Ok(StrictValue)
    }
}

/// Rejects invalid JSON and duplicate object keys (anywhere in the document).
pub(crate) fn reject_duplicate_keys(bytes: &[u8]) -> Result<()> {
    serde_json::from_slice::<StrictValue>(bytes)
        .map(|_| ())
        .map_err(|err| malformed(format!("invalid JSON: {err}")))
}

/// Strict JSON -> `serde_json::Value` (no duplicate keys).
pub(crate) fn parse_strict_value(bytes: &[u8]) -> Result<serde_json::Value> {
    reject_duplicate_keys(bytes)?;
    serde_json::from_slice(bytes).map_err(|err| malformed(format!("invalid JSON: {err}")))
}

/// Strict JSON -> typed value. Unknown fields are rejected by the types'
/// `deny_unknown_fields`; this adds the duplicate-key rejection.
pub(crate) fn parse_strict<T: DeserializeOwned>(bytes: &[u8]) -> Result<T> {
    let value = parse_strict_value(bytes)?;
    serde_json::from_value(value).map_err(|err| malformed(format!("invalid document: {err}")))
}
