//! Values: the data that types describe.
//!
//! [`Value`] is a superset of JSON. The extra cases — bytes, tuples, tagged
//! variants, and maps with non-string keys — are what let a value from one
//! language survive the trip to another intact, and each has an unambiguous
//! JSON encoding built from a `$`-prefixed key. Identifiers cannot begin with
//! `$`, so no user field name can ever collide with one.

mod infer;
pub(crate) mod json;
mod lift;
mod read;
mod unify;
mod validate;

use indexmap::IndexMap;

pub use infer::{InferOptions, infer, infer_many, infer_many_with, infer_with};
pub use json::{ValueDecodeError, ValueParseError};
pub use lift::{Lifted, lift};
pub use read::ReadError;
pub use unify::{unify, unify_all};
pub use validate::{ValueError, ValuePath, ValueSeg};

/// A value.
#[derive(Clone, Debug, PartialEq)]
pub enum Value {
    /// Absence, and the single inhabitant of [`crate::ir::Type::Unit`].
    ///
    /// One state covers "explicitly null", "not present", and "none". That
    /// collapse is deliberate: it is the distinction every target language
    /// agrees on.
    Null,
    /// A boolean.
    Bool(bool),
    /// A 64-bit signed integer, kept distinct from [`Value::Float`] so that
    /// integer identity survives a round trip.
    Int(i64),
    /// A 64-bit float. Not-a-number and the infinities are rejected at the
    /// encoding boundary rather than silently corrupted.
    Float(f64),
    /// A UTF-8 string.
    Str(String),
    /// Opaque bytes.
    Bytes(Vec<u8>),
    /// An ordered sequence.
    List(Vec<Value>),
    /// A fixed-length heterogeneous sequence.
    Tuple(Vec<Value>),
    /// A record, keyed by field name in the order encountered.
    Record(IndexMap<String, Value>),
    /// An association with keys that need not be strings.
    Map(Vec<(Value, Value)>),
    /// One alternative of a tagged sum.
    Variant {
        /// The variant's tag.
        tag: String,
        /// The payload; [`Value::Null`] for a bare tag.
        value: Box<Value>,
    },
}

impl Value {
    /// A record built from name/value pairs.
    pub fn record<K: Into<String>>(fields: impl IntoIterator<Item = (K, Value)>) -> Value {
        Value::Record(fields.into_iter().map(|(k, v)| (k.into(), v)).collect())
    }

    /// A variant carrying a payload.
    pub fn variant(tag: impl Into<String>, value: Value) -> Value {
        Value::Variant {
            tag: tag.into(),
            value: Box::new(value),
        }
    }

    /// A variant carrying nothing.
    pub fn unit_variant(tag: impl Into<String>) -> Value {
        Value::variant(tag, Value::Null)
    }

    /// A short name for this value's shape, for diagnostics.
    pub fn kind_name(&self) -> &'static str {
        match self {
            Value::Null => "null",
            Value::Bool(_) => "bool",
            Value::Int(_) => "int",
            Value::Float(_) => "float",
            Value::Str(_) => "string",
            Value::Bytes(_) => "bytes",
            Value::List(_) => "list",
            Value::Tuple(_) => "tuple",
            Value::Record(_) => "record",
            Value::Map(_) => "map",
            Value::Variant { .. } => "variant",
        }
    }

    /// The string this value holds, if it holds one.
    pub fn as_str(&self) -> Option<&str> {
        match self {
            Value::Str(s) => Some(s),
            _ => None,
        }
    }

    /// The integer this value holds, if it holds one.
    pub fn as_int(&self) -> Option<i64> {
        match self {
            Value::Int(i) => Some(*i),
            _ => None,
        }
    }

    /// One field of a record.
    pub fn get(&self, field: &str) -> Option<&Value> {
        match self {
            Value::Record(m) => m.get(field),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn record_helper_preserves_insertion_order() {
        let v = Value::record([("b", Value::Int(2)), ("a", Value::Int(1))]);
        let Value::Record(m) = &v else {
            panic!("expected a record");
        };
        assert_eq!(m.keys().collect::<Vec<_>>(), ["b", "a"]);
    }

    #[test]
    fn integers_and_floats_are_distinct_values() {
        assert_ne!(Value::Int(1), Value::Float(1.0));
    }
}
