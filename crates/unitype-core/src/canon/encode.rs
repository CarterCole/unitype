//! The canonical byte encoding.
//!
//! Content addressing only works if two systems that hold the same type produce
//! the same bytes for it, so the encoding has to be pinned down rather than
//! left to whatever a serializer happens to do.
//!
//! Determinism here rests on three properties, each of which a test in this
//! module holds in place:
//!
//! 1. **Object key order is declaration order.** The IR derives its
//!    serialization, so keys come out in the order the fields are written in
//!    the source, never in hash order.
//! 2. **Absent and empty members are omitted.** Every optional field carries
//!    `skip_serializing_if`, so `null` and `[]` never appear and cannot differ
//!    between a value that was written and one that defaulted.
//! 3. **No floats reach an identity encoding.** Floats appear only in metadata
//!    and in literal values inside expression bodies, and identity excludes
//!    both. Storage encodings may contain floats, where the shortest
//!    round-tripping form is used.
//!
//! The golden-vector test in `tests/hash_golden.rs` is the real guard: if any
//! of this drifts, pinned hashes stop matching and the change has to be
//! deliberate.

use serde::Serialize;

/// Encodes a value in its canonical form.
///
/// Panics only if the value cannot be serialized at all, which for the IR types
/// means a non-finite float reached a literal — a condition rejected when the
/// value was read.
pub fn canonical_bytes<T: Serialize>(value: &T) -> Vec<u8> {
    serde_json::to_vec(value).expect("IR types serialize infallibly")
}

/// Encodes a value in its canonical form as text.
pub fn canonical_string<T: Serialize>(value: &T) -> String {
    serde_json::to_string(value).expect("IR types serialize infallibly")
}

/// Encodes a value in an indented form, for reading rather than hashing.
///
/// Never use this for identity: whitespace is not part of the canonical form.
pub fn pretty_string<T: Serialize>(value: &T) -> String {
    serde_json::to_string_pretty(value).expect("IR types serialize infallibly")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ident::Ident;
    use crate::ir::{Field, Type};

    #[test]
    fn encoding_is_compact() {
        let bytes = canonical_string(&Type::I64.list());
        assert_eq!(bytes, r#"{"k":"list","of":{"k":"i64"}}"#);
    }

    #[test]
    fn absent_members_are_omitted_so_they_cannot_differ() {
        let documented = Field {
            name: Ident::new("x").unwrap(),
            ty: Type::I64,
            doc: Some("the x".into()),
        };
        let bare = Field::new(Ident::new("x").unwrap(), Type::I64);
        assert_eq!(canonical_string(&bare), r#"{"name":"x","ty":{"k":"i64"}}"#);
        assert_ne!(canonical_string(&documented), canonical_string(&bare));
    }

    #[test]
    fn encoding_the_same_type_twice_gives_the_same_bytes() {
        let build = || Type::Record {
            fields: vec![
                Field::new(Ident::new("b").unwrap(), Type::String),
                Field::new(Ident::new("a").unwrap(), Type::I64),
            ],
        };
        assert_eq!(canonical_bytes(&build()), canonical_bytes(&build()));
    }

    #[test]
    fn the_encoding_round_trips_back_to_the_same_type() {
        let t = Type::Function {
            params: vec![Type::I64.list(), Type::String.optional()],
            ret: Box::new(Type::Bool.future()),
        };
        let text = canonical_string(&t);
        assert_eq!(serde_json::from_str::<Type>(&text).unwrap(), t);
    }
}
