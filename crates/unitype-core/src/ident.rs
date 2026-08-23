//! Validated identifiers and universe-qualified names.
//!
//! Identifiers are ASCII-only in v1. This is deliberate: identifiers feed the
//! canonical byte encoding that content hashes are computed over, and Unicode
//! normalization forms would make "the same" name hash differently depending on
//! how it was typed. Relaxing this later stays hash-compatible, because every
//! identifier valid today remains valid and encodes identically.

use std::fmt;
use std::str::FromStr;

use serde::de::{Error as DeError, Unexpected};
use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// Failure to construct an [`Ident`] or [`QualifiedName`].
#[derive(thiserror::Error, Debug, Clone, PartialEq, Eq)]
pub enum IdentError {
    /// The identifier had no characters.
    #[error("identifier must not be empty")]
    Empty,
    /// The first character was not an ASCII letter or underscore.
    #[error("identifier `{0}` must start with an ASCII letter or underscore")]
    BadStart(String),
    /// Some character was outside `[A-Za-z0-9_]`.
    #[error("identifier `{0}` may only contain ASCII letters, digits, and underscores")]
    BadChar(String),
    /// A qualified name was not of the form `universe::name`.
    #[error("`{0}` is not a qualified name of the form `universe::name`")]
    NotQualified(String),
}

/// A validated identifier: `[A-Za-z_][A-Za-z0-9_]*`.
///
/// The leading `$` used by the value wire format is excluded by construction,
/// so escape keys such as `$tag` can never collide with a user field name.
#[derive(Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Ident(String);

impl Ident {
    /// Validates `s` and wraps it.
    pub fn new(s: impl Into<String>) -> Result<Self, IdentError> {
        let s = s.into();
        let mut chars = s.chars();
        let Some(first) = chars.next() else {
            return Err(IdentError::Empty);
        };
        if !(first.is_ascii_alphabetic() || first == '_') {
            return Err(IdentError::BadStart(s));
        }
        if !chars.all(|c| c.is_ascii_alphanumeric() || c == '_') {
            return Err(IdentError::BadChar(s));
        }
        Ok(Self(s))
    }

    /// Wraps `s` without validating it.
    ///
    /// Reserved for identifiers this crate generates itself and knows to be
    /// well-formed. Prefer [`Ident::new`] for anything derived from input.
    pub(crate) fn new_unchecked(s: impl Into<String>) -> Self {
        Self(s.into())
    }

    /// An identifier from a literal the caller knows is well-formed.
    ///
    /// For names a program writes into its own source, where a failure would be
    /// a bug rather than bad input. Anything from outside goes through
    /// [`Ident::new`].
    pub fn literal(s: &'static str) -> Self {
        debug_assert!(Ident::new(s).is_ok(), "`{s}` is not a valid identifier");
        Self(s.to_string())
    }

    /// The identifier as written.
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// A case- and separator-folded key, for matching fields whose names differ
    /// only by language convention: `userId` and `user_id` both fold to `userid`.
    ///
    /// Used only by bridge derivation, never by hashing or subtyping.
    pub fn fold(&self) -> String {
        self.0
            .chars()
            .filter(|c| *c != '_')
            .flat_map(char::to_lowercase)
            .collect()
    }
}

impl fmt::Display for Ident {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl fmt::Debug for Ident {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:?}", self.0)
    }
}

impl FromStr for Ident {
    type Err = IdentError;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ident::new(s)
    }
}

impl Serialize for Ident {
    fn serialize<S: Serializer>(&self, ser: S) -> Result<S::Ok, S::Error> {
        ser.serialize_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for Ident {
    fn deserialize<D: Deserializer<'de>>(de: D) -> Result<Self, D::Error> {
        let s = String::deserialize(de)?;
        Ident::new(s).map_err(|e| D::Error::custom(e.to_string()))
    }
}

/// A name qualified by the universe that declares it, written `universe::name`.
#[derive(Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct QualifiedName {
    /// The declaring universe.
    pub universe: Ident,
    /// The declared name.
    pub name: Ident,
}

impl QualifiedName {
    /// Builds a qualified name from its two parts.
    pub fn new(universe: Ident, name: Ident) -> Self {
        Self { universe, name }
    }
}

impl fmt::Display for QualifiedName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}::{}", self.universe, self.name)
    }
}

impl fmt::Debug for QualifiedName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{self}")
    }
}

impl FromStr for QualifiedName {
    type Err = IdentError;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let (u, n) = s
            .split_once("::")
            .ok_or_else(|| IdentError::NotQualified(s.to_string()))?;
        Ok(Self {
            universe: Ident::new(u)?,
            name: Ident::new(n)?,
        })
    }
}

impl Serialize for QualifiedName {
    fn serialize<S: Serializer>(&self, ser: S) -> Result<S::Ok, S::Error> {
        ser.serialize_str(&self.to_string())
    }
}

impl<'de> Deserialize<'de> for QualifiedName {
    fn deserialize<D: Deserializer<'de>>(de: D) -> Result<Self, D::Error> {
        let s = String::deserialize(de)?;
        s.parse().map_err(|_| {
            D::Error::invalid_value(Unexpected::Str(&s), &"a qualified name `universe::name`")
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_well_formed_identifiers() {
        for s in ["a", "_x", "User", "user_id", "f0"] {
            assert!(Ident::new(s).is_ok(), "{s} should be valid");
        }
    }

    #[test]
    fn rejects_malformed_identifiers() {
        assert_eq!(Ident::new(""), Err(IdentError::Empty));
        assert!(matches!(Ident::new("0x"), Err(IdentError::BadStart(_))));
        assert!(matches!(Ident::new("$tag"), Err(IdentError::BadStart(_))));
        assert!(matches!(Ident::new("a-b"), Err(IdentError::BadChar(_))));
        assert!(matches!(Ident::new("naïve"), Err(IdentError::BadChar(_))));
    }

    #[test]
    fn folding_bridges_naming_conventions() {
        let camel = Ident::new("userId").unwrap();
        let snake = Ident::new("user_id").unwrap();
        assert_eq!(camel.fold(), snake.fold());
        assert_eq!(camel.fold(), "userid");
    }

    #[test]
    fn qualified_names_round_trip_through_text() {
        let q: QualifiedName = "geo::Point".parse().unwrap();
        assert_eq!(q.universe.as_str(), "geo");
        assert_eq!(q.name.as_str(), "Point");
        assert_eq!(q.to_string(), "geo::Point");
        assert!("Point".parse::<QualifiedName>().is_err());
    }
}
