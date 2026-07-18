//! Wire (de)serialization for timestamps, matching RSCTF's global
//! `DateTimeOffsetJsonConverter` (`Utils/JsonSerializerContext.cs`):
//!
//!   * WRITE — every `DateTimeOffset` is a JSON **number**: Unix **milliseconds**.
//!   * READ  — a JSON number is `FromUnixTimeMilliseconds`; a string falls back
//!     to ISO-8601. So the client sends millis and we must accept them (its
//!     forms POST `start: 1735689600000`), and we must emit millis so numeric
//!     date math on the client (`@format uint64` fields) works.
//!
//! Apply with `#[serde(with = "crate::utils::datetime::millis")]` on a
//! `DateTime<Utc>` field, or `...::millis_opt` on an `Option<DateTime<Utc>>`.

use chrono::{DateTime, TimeZone, Utc};
use serde::{Deserialize, Deserializer, Serializer};

/// A timestamp on the wire: a millis number (canonical) or an ISO-8601 string
/// (tolerated on input, exactly as RSCTF's converter does).
#[derive(Deserialize)]
#[serde(untagged)]
enum WireTs {
    Millis(i64),
    Str(String),
}

fn to_utc<E: serde::de::Error>(w: WireTs) -> Result<DateTime<Utc>, E> {
    match w {
        WireTs::Millis(ms) => Utc
            .timestamp_millis_opt(ms)
            .single()
            .ok_or_else(|| E::custom("timestamp out of range")),
        WireTs::Str(s) => DateTime::parse_from_rfc3339(&s)
            .map(|dt| dt.with_timezone(&Utc))
            .map_err(E::custom),
    }
}

/// `DateTime<Utc>` <-> Unix milliseconds.
pub mod millis {
    use super::*;

    pub fn serialize<S: Serializer>(dt: &DateTime<Utc>, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_i64(dt.timestamp_millis())
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<DateTime<Utc>, D::Error> {
        super::to_utc(WireTs::deserialize(d)?)
    }
}

/// `Option<DateTime<Utc>>` <-> Unix milliseconds (or `null`).
pub mod millis_opt {
    use super::*;

    pub fn serialize<S: Serializer>(dt: &Option<DateTime<Utc>>, s: S) -> Result<S::Ok, S::Error> {
        match dt {
            Some(d) => s.serialize_some(&d.timestamp_millis()),
            None => s.serialize_none(),
        }
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Option<DateTime<Utc>>, D::Error> {
        match Option::<WireTs>::deserialize(d)? {
            Some(w) => super::to_utc(w).map(Some),
            None => Ok(None),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::{Deserialize, Serialize};

    #[derive(Serialize, Deserialize, PartialEq, Debug)]
    struct T {
        #[serde(with = "millis")]
        a: DateTime<Utc>,
        #[serde(with = "millis_opt")]
        b: Option<DateTime<Utc>>,
    }

    #[test]
    fn serializes_as_millis_number() {
        let t = T {
            a: Utc.timestamp_millis_opt(1_735_689_600_000).unwrap(),
            b: None,
        };
        assert_eq!(
            serde_json::to_string(&t).unwrap(),
            r#"{"a":1735689600000,"b":null}"#
        );
    }

    #[test]
    fn accepts_millis_number_and_iso_string() {
        // The client POSTs millis numbers (the form-break we are fixing).
        let n: T = serde_json::from_str(r#"{"a":1735689600000,"b":1735776000000}"#).unwrap();
        assert_eq!(n.a.timestamp_millis(), 1_735_689_600_000);
        assert_eq!(n.b.unwrap().timestamp_millis(), 1_735_776_000_000);
        // ISO strings still accepted (RSCTF's Read fallback).
        let s: T = serde_json::from_str(r#"{"a":"2025-01-01T00:00:00Z","b":null}"#).unwrap();
        assert_eq!(s.a.timestamp_millis(), 1_735_689_600_000);
    }
}
