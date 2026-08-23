//! The JSON encoding of [`Value`].
//!
//! JSON-native shapes are used wherever they exist. The four cases JSON has no
//! shape for are written as single-key objects whose key starts with `$`:
//!
//! | Value            | JSON                                |
//! |------------------|-------------------------------------|
//! | `Bytes`          | `{"$bytes": "<base64>"}`            |
//! | `Tuple`          | `{"$tuple": [...]}`                 |
//! | `Variant`        | `{"$tag": "Name", "value": ...}`    |
//! | `Map`            | `{"$map": [[k, v], ...]}`           |
//!
//! `value` is omitted from a variant carrying nothing. A plain JSON object
//! always decodes to [`Value::Record`]; a map with string keys is recognized
//! later, when a value is read against a known type.

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use indexmap::IndexMap;
use serde::de::Error as _;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::Value as Json;

use super::Value;

/// The escape key marking a byte string.
pub(crate) const KEY_BYTES: &str = "$bytes";
/// The escape key marking a tuple.
pub(crate) const KEY_TUPLE: &str = "$tuple";
/// The escape key marking a tagged variant.
pub(crate) const KEY_TAG: &str = "$tag";
/// The payload key accompanying [`KEY_TAG`].
pub(crate) const KEY_VALUE: &str = "value";
/// The escape key marking a map with arbitrary keys.
pub(crate) const KEY_MAP: &str = "$map";

/// Why a JSON document could not be read as a [`Value`].
#[derive(thiserror::Error, Debug, Clone, PartialEq, Eq)]
pub enum ValueDecodeError {
    /// A number was outside the range this system represents exactly.
    #[error("number `{0}` is not representable as i64 or f64")]
    UnrepresentableNumber(String),
    /// A `$bytes` payload was not valid base64.
    #[error("`$bytes` payload is not valid base64: {0}")]
    BadBase64(String),
    /// An escape object was malformed.
    #[error("malformed `{key}` escape: {detail}")]
    BadEscape {
        /// The escape key involved.
        key: &'static str,
        /// What was wrong with it.
        detail: String,
    },
    /// A float that JSON cannot carry reached the encoder.
    #[error("`{0}` cannot be encoded as JSON")]
    NonFiniteFloat(String),
}

impl Value {
    /// Encodes this value as JSON.
    ///
    /// Fails only on a non-finite float, which JSON cannot represent and which
    /// this system refuses to approximate.
    pub fn to_json(&self) -> Result<Json, ValueDecodeError> {
        Ok(match self {
            Value::Null => Json::Null,
            Value::Bool(b) => Json::Bool(*b),
            Value::Int(i) => Json::Number((*i).into()),
            Value::Float(f) => {
                let n = serde_json::Number::from_f64(*f)
                    .ok_or_else(|| ValueDecodeError::NonFiniteFloat(f.to_string()))?;
                Json::Number(n)
            }
            Value::Str(s) => Json::String(s.clone()),
            Value::Bytes(b) => single(KEY_BYTES, Json::String(BASE64.encode(b))),
            Value::List(items) => {
                Json::Array(items.iter().map(Value::to_json).collect::<Result<_, _>>()?)
            }
            Value::Tuple(items) => single(
                KEY_TUPLE,
                Json::Array(items.iter().map(Value::to_json).collect::<Result<_, _>>()?),
            ),
            Value::Record(fields) => {
                let mut obj = serde_json::Map::with_capacity(fields.len());
                for (k, v) in fields {
                    obj.insert(k.clone(), v.to_json()?);
                }
                Json::Object(obj)
            }
            Value::Map(entries) => {
                let mut out = Vec::with_capacity(entries.len());
                for (k, v) in entries {
                    out.push(Json::Array(vec![k.to_json()?, v.to_json()?]));
                }
                single(KEY_MAP, Json::Array(out))
            }
            Value::Variant { tag, value } => {
                let mut obj = serde_json::Map::new();
                obj.insert(KEY_TAG.to_string(), Json::String(tag.clone()));
                if !matches!(**value, Value::Null) {
                    obj.insert(KEY_VALUE.to_string(), value.to_json()?);
                }
                Json::Object(obj)
            }
        })
    }

    /// Reads a value out of JSON.
    ///
    /// This is the untyped reading: a JSON object becomes a record, and numbers
    /// become integers or floats as written. Reading against a known type,
    /// which can recognize maps and enums, happens during validation.
    pub fn from_json(json: &Json) -> Result<Value, ValueDecodeError> {
        Ok(match json {
            Json::Null => Value::Null,
            Json::Bool(b) => Value::Bool(*b),
            Json::Number(n) => {
                if let Some(i) = n.as_i64() {
                    Value::Int(i)
                } else if let Some(f) = n.as_f64() {
                    Value::Float(f)
                } else {
                    return Err(ValueDecodeError::UnrepresentableNumber(n.to_string()));
                }
            }
            Json::String(s) => Value::Str(s.clone()),
            Json::Array(items) => Value::List(
                items
                    .iter()
                    .map(Value::from_json)
                    .collect::<Result<_, _>>()?,
            ),
            Json::Object(obj) => return object_from_json(obj),
        })
    }

    /// Encodes this value as a JSON string.
    pub fn to_json_string(&self) -> Result<String, ValueDecodeError> {
        Ok(self.to_json()?.to_string())
    }

    /// Reads a value from JSON text.
    pub fn parse_json(text: &str) -> Result<Value, ValueParseError> {
        let json: Json = serde_json::from_str(text)?;
        Ok(Value::from_json(&json)?)
    }
}

/// Why JSON text could not be read as a [`Value`].
#[derive(thiserror::Error, Debug)]
pub enum ValueParseError {
    /// The text was not valid JSON.
    #[error("invalid JSON: {0}")]
    Syntax(#[from] serde_json::Error),
    /// The JSON was valid but did not describe a value.
    #[error(transparent)]
    Decode(#[from] ValueDecodeError),
}

fn single(key: &str, value: Json) -> Json {
    let mut obj = serde_json::Map::with_capacity(1);
    obj.insert(key.to_string(), value);
    Json::Object(obj)
}

fn object_from_json(obj: &serde_json::Map<String, Json>) -> Result<Value, ValueDecodeError> {
    if let Some(b64) = obj.get(KEY_BYTES) {
        expect_only(obj, &[KEY_BYTES], KEY_BYTES)?;
        let s = b64.as_str().ok_or_else(|| ValueDecodeError::BadEscape {
            key: KEY_BYTES,
            detail: "payload must be a base64 string".into(),
        })?;
        let bytes = BASE64
            .decode(s)
            .map_err(|e| ValueDecodeError::BadBase64(e.to_string()))?;
        return Ok(Value::Bytes(bytes));
    }

    if let Some(items) = obj.get(KEY_TUPLE) {
        expect_only(obj, &[KEY_TUPLE], KEY_TUPLE)?;
        let items = items
            .as_array()
            .ok_or_else(|| ValueDecodeError::BadEscape {
                key: KEY_TUPLE,
                detail: "payload must be an array".into(),
            })?;
        return Ok(Value::Tuple(
            items
                .iter()
                .map(Value::from_json)
                .collect::<Result<_, _>>()?,
        ));
    }

    if let Some(entries) = obj.get(KEY_MAP) {
        expect_only(obj, &[KEY_MAP], KEY_MAP)?;
        let entries = entries
            .as_array()
            .ok_or_else(|| ValueDecodeError::BadEscape {
                key: KEY_MAP,
                detail: "payload must be an array of [key, value] pairs".into(),
            })?;
        let mut out = Vec::with_capacity(entries.len());
        for entry in entries {
            let pair = entry.as_array().filter(|p| p.len() == 2).ok_or_else(|| {
                ValueDecodeError::BadEscape {
                    key: KEY_MAP,
                    detail: "each entry must be a two-element array".into(),
                }
            })?;
            out.push((Value::from_json(&pair[0])?, Value::from_json(&pair[1])?));
        }
        return Ok(Value::Map(out));
    }

    if let Some(tag) = obj.get(KEY_TAG) {
        expect_only(obj, &[KEY_TAG, KEY_VALUE], KEY_TAG)?;
        let tag = tag.as_str().ok_or_else(|| ValueDecodeError::BadEscape {
            key: KEY_TAG,
            detail: "tag must be a string".into(),
        })?;
        let payload = match obj.get(KEY_VALUE) {
            Some(v) => Value::from_json(v)?,
            None => Value::Null,
        };
        return Ok(Value::variant(tag, payload));
    }

    let mut fields = IndexMap::with_capacity(obj.len());
    for (k, v) in obj {
        fields.insert(k.clone(), Value::from_json(v)?);
    }
    Ok(Value::Record(fields))
}

/// Rejects stray keys alongside an escape, so a typo is reported rather than
/// silently dropped.
fn expect_only(
    obj: &serde_json::Map<String, Json>,
    allowed: &[&str],
    key: &'static str,
) -> Result<(), ValueDecodeError> {
    if let Some(extra) = obj.keys().find(|k| !allowed.contains(&k.as_str())) {
        return Err(ValueDecodeError::BadEscape {
            key,
            detail: format!("unexpected key `{extra}` alongside the escape"),
        });
    }
    Ok(())
}

impl Serialize for Value {
    fn serialize<S: Serializer>(&self, ser: S) -> Result<S::Ok, S::Error> {
        self.to_json()
            .map_err(serde::ser::Error::custom)?
            .serialize(ser)
    }
}

impl<'de> Deserialize<'de> for Value {
    fn deserialize<D: Deserializer<'de>>(de: D) -> Result<Self, D::Error> {
        let json = Json::deserialize(de)?;
        Value::from_json(&json).map_err(D::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn round_trip(v: Value) {
        let json = v.to_json().expect("encodes");
        let back = Value::from_json(&json).expect("decodes");
        assert_eq!(back, v, "round trip changed {v:?} (via {json})");
    }

    #[test]
    fn json_native_values_round_trip() {
        round_trip(Value::Null);
        round_trip(Value::Bool(true));
        round_trip(Value::Int(-7));
        round_trip(Value::Float(1.5));
        round_trip(Value::Str("hi".into()));
        round_trip(Value::List(vec![Value::Int(1), Value::Str("a".into())]));
        round_trip(Value::record([("x", Value::Int(1))]));
    }

    #[test]
    fn extended_values_round_trip_through_escapes() {
        round_trip(Value::Bytes(vec![0, 1, 2, 255]));
        round_trip(Value::Tuple(vec![Value::Int(1), Value::Bool(false)]));
        round_trip(Value::unit_variant("Red"));
        round_trip(Value::variant(
            "Circle",
            Value::record([("r", Value::Float(2.0))]),
        ));
        round_trip(Value::Map(vec![(Value::Int(1), Value::Str("one".into()))]));
    }

    #[test]
    fn escape_encodings_are_frozen() {
        assert_eq!(
            Value::Bytes(vec![1, 2]).to_json_string().unwrap(),
            r#"{"$bytes":"AQI="}"#
        );
        assert_eq!(
            Value::unit_variant("Red").to_json_string().unwrap(),
            r#"{"$tag":"Red"}"#
        );
        assert_eq!(
            Value::variant("N", Value::Int(1)).to_json_string().unwrap(),
            r#"{"$tag":"N","value":1}"#
        );
    }

    #[test]
    fn integers_do_not_become_floats_in_transit() {
        let v = Value::parse_json("1").unwrap();
        assert_eq!(v, Value::Int(1));
        assert_eq!(Value::parse_json("1.0").unwrap(), Value::Float(1.0));
    }

    #[test]
    fn non_finite_floats_are_refused_rather_than_approximated() {
        assert!(matches!(
            Value::Float(f64::NAN).to_json(),
            Err(ValueDecodeError::NonFiniteFloat(_))
        ));
        assert!(matches!(
            Value::Float(f64::INFINITY).to_json(),
            Err(ValueDecodeError::NonFiniteFloat(_))
        ));
    }

    #[test]
    fn a_typo_beside_an_escape_is_reported() {
        let json: Json = serde_json::from_str(r#"{"$tag":"A","valu":1}"#).unwrap();
        assert!(matches!(
            Value::from_json(&json),
            Err(ValueDecodeError::BadEscape { key: KEY_TAG, .. })
        ));
    }

    #[test]
    fn plain_objects_are_records() {
        let json: Json = serde_json::from_str(r#"{"a":1,"b":2}"#).unwrap();
        assert!(matches!(Value::from_json(&json), Ok(Value::Record(_))));
    }
}
