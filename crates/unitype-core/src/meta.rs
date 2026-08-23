//! Vocabulary shared with the runtime and codegen crates: free-form metadata
//! and the complexity classes that distinguish one proof of a proposition from
//! another.

use std::fmt;

use indexmap::IndexMap;
use serde::{Deserialize, Serialize};

/// Free-form annotations carried alongside a declaration.
///
/// Metadata is preserved through canonical serialization but **excluded from
/// identity**: two declarations that differ only in metadata have the same
/// content hash. Anything that should change a type's identity belongs in the
/// type, not here.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Metadata(pub IndexMap<String, serde_json::Value>);

impl Metadata {
    /// True when no annotations are present (canonical form omits it entirely).
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// Looks up one annotation.
    pub fn get(&self, key: &str) -> Option<&serde_json::Value> {
        self.0.get(key)
    }
}

/// An asymptotic complexity class.
///
/// Declared, not measured: this is the cost a proof *claims*, and the
/// dispatcher orders candidate proofs by it. [`BigO::Custom`] covers classes
/// this enum does not name and deliberately ranks below every named class, so
/// an unquantified claim never outranks a stated one.
///
/// The wire form is always a plain string, so a manifest reads
/// `"time": "o_n_log_n"` and an unnamed class reads as its own prose.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum BigO {
    /// Constant.
    O1,
    /// Logarithmic.
    OLogN,
    /// Linear.
    ON,
    /// Linearithmic.
    ONLogN,
    /// Quadratic.
    ON2,
    /// Cubic.
    ON3,
    /// Exponential.
    OExp,
    /// A class this enum does not name, described in prose.
    Custom(String),
}

/// The wire token for each named class. Frozen: these appear in manifests.
const BIG_O_TOKENS: [(&str, BigO); 7] = [
    ("o1", BigO::O1),
    ("o_log_n", BigO::OLogN),
    ("o_n", BigO::ON),
    ("o_n_log_n", BigO::ONLogN),
    ("o_n2", BigO::ON2),
    ("o_n3", BigO::ON3),
    ("o_exp", BigO::OExp),
];

impl BigO {
    /// The frozen wire token, or the prose of a [`BigO::Custom`].
    pub fn token(&self) -> &str {
        match self {
            BigO::O1 => "o1",
            BigO::OLogN => "o_log_n",
            BigO::ON => "o_n",
            BigO::ONLogN => "o_n_log_n",
            BigO::ON2 => "o_n2",
            BigO::ON3 => "o_n3",
            BigO::OExp => "o_exp",
            BigO::Custom(s) => s,
        }
    }

    /// Parses a wire token, falling back to [`BigO::Custom`].
    pub fn from_token(s: &str) -> Self {
        BIG_O_TOKENS
            .iter()
            .find(|(tok, _)| *tok == s)
            .map(|(_, c)| c.clone())
            .unwrap_or_else(|| BigO::Custom(s.to_string()))
    }
}

impl Serialize for BigO {
    fn serialize<S: serde::Serializer>(&self, ser: S) -> Result<S::Ok, S::Error> {
        ser.serialize_str(self.token())
    }
}

impl<'de> Deserialize<'de> for BigO {
    fn deserialize<D: serde::Deserializer<'de>>(de: D) -> Result<Self, D::Error> {
        Ok(BigO::from_token(&String::deserialize(de)?))
    }
}

impl BigO {
    /// Ordering rank, ascending in cost. [`BigO::Custom`] has no rank.
    pub fn rank(&self) -> Option<u8> {
        Some(match self {
            BigO::O1 => 0,
            BigO::OLogN => 1,
            BigO::ON => 2,
            BigO::ONLogN => 3,
            BigO::ON2 => 4,
            BigO::ON3 => 5,
            BigO::OExp => 6,
            BigO::Custom(_) => return None,
        })
    }

    /// Sort key placing unranked classes after every ranked one.
    pub fn sort_key(&self) -> u8 {
        self.rank().unwrap_or(u8::MAX)
    }
}

impl fmt::Display for BigO {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            BigO::O1 => f.write_str("O(1)"),
            BigO::OLogN => f.write_str("O(log n)"),
            BigO::ON => f.write_str("O(n)"),
            BigO::ONLogN => f.write_str("O(n log n)"),
            BigO::ON2 => f.write_str("O(n^2)"),
            BigO::ON3 => f.write_str("O(n^3)"),
            BigO::OExp => f.write_str("O(2^n)"),
            BigO::Custom(s) => write!(f, "{s}"),
        }
    }
}

/// The cost profile a proof claims for itself.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ImplProfile {
    /// Time complexity.
    pub time: BigO,
    /// Space complexity, beyond the input.
    pub space: BigO,
    /// Prose qualification of the claim.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notes: Option<String>,
}

impl ImplProfile {
    /// A profile with no prose note.
    pub fn new(time: BigO, space: BigO) -> Self {
        Self {
            time,
            space,
            notes: None,
        }
    }
}

impl fmt::Display for ImplProfile {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "time {} / space {}", self.time, self.space)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn named_classes_rank_ascending_by_cost() {
        assert!(BigO::ONLogN.sort_key() < BigO::ON2.sort_key());
        assert!(BigO::O1.sort_key() < BigO::OLogN.sort_key());
    }

    #[test]
    fn unquantified_claims_rank_last() {
        assert_eq!(BigO::Custom("amortized".into()).rank(), None);
        assert!(BigO::Custom("amortized".into()).sort_key() > BigO::OExp.sort_key());
    }

    #[test]
    fn complexity_round_trips_through_json() {
        for c in [BigO::ONLogN, BigO::O1, BigO::Custom("O(n!)".into())] {
            let j = serde_json::to_string(&c).unwrap();
            assert_eq!(serde_json::from_str::<BigO>(&j).unwrap(), c);
        }
    }

    #[test]
    fn complexity_wire_tokens_are_frozen() {
        assert_eq!(
            serde_json::to_string(&BigO::ONLogN).unwrap(),
            r#""o_n_log_n""#
        );
        assert_eq!(serde_json::to_string(&BigO::O1).unwrap(), r#""o1""#);
        assert_eq!(serde_json::to_string(&BigO::ON2).unwrap(), r#""o_n2""#);
    }

    #[test]
    fn unknown_tokens_survive_as_prose() {
        let parsed: BigO = serde_json::from_str(r#""O(n!)""#).unwrap();
        assert_eq!(parsed, BigO::Custom("O(n!)".into()));
        assert_eq!(serde_json::to_string(&parsed).unwrap(), r#""O(n!)""#);
    }
}
