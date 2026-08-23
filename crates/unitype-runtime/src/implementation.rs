//! A proof: something that answers a proposition.
//!
//! An implementation is not the code — it is the claim that some code, in some
//! environment, answers a particular signature, together with what that costs.
//! The code itself lives wherever the environment says.
//!
//! A signature may have any number of these. That is the arrangement the whole
//! system is built around: `sort` can be proved by a Rust mergesort and a
//! Python bubble sort at the same time, and choosing between them at the moment
//! of the call is a policy decision rather than an architectural one.

use std::fmt;

use serde::{Deserialize, Serialize};
use unitype_core::canon::TypeId;
use unitype_core::{BigO, Ident, ImplProfile, Metadata, QualifiedName};

/// What an implementation is called.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ImplId(pub String);

impl ImplId {
    /// Names an implementation.
    pub fn new(id: impl Into<String>) -> Self {
        Self(id.into())
    }

    /// The name as text.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for ImplId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl From<&str> for ImplId {
    fn from(s: &str) -> Self {
        Self::new(s)
    }
}

/// A claim that something proves a proposition.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Implementation {
    /// What this implementation is called.
    pub id: ImplId,
    /// The proposition it claims to prove.
    pub proves: QualifiedName,
    /// The address of that proposition when this was registered.
    ///
    /// Present, it catches an implementation that was written against a
    /// signature which has since changed — a proof of something that is no
    /// longer being asked.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sig_hash: Option<TypeId>,
    /// The environment it runs in.
    pub environment: Ident,
    /// What to invoke there. What this means is the environment's business: a
    /// key for a compiled-in function, a name the harness dispatches on.
    pub entry: String,
    /// What it claims to cost.
    pub cost: ImplProfile,
    /// Anything worth knowing about it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notes: Option<String>,
    /// Non-identifying annotations.
    #[serde(default, skip_serializing_if = "Metadata::is_empty")]
    pub meta: Metadata,
}

impl Implementation {
    /// An implementation with a cost and nothing else said about it.
    pub fn new(
        id: impl Into<ImplId>,
        proves: QualifiedName,
        environment: Ident,
        entry: impl Into<String>,
        cost: ImplProfile,
    ) -> Self {
        Self {
            id: id.into(),
            proves,
            sig_hash: None,
            environment,
            entry: entry.into(),
            cost,
            notes: None,
            meta: Metadata::default(),
        }
    }

    /// How this implementation sorts against another, cheapest first.
    ///
    /// Time, then space, then name — the last only so that two runs of the same
    /// workspace agree about which of two equals comes first.
    pub fn cost_key(&self) -> (u8, u8, &str) {
        (
            self.cost.time.sort_key(),
            self.cost.space.sort_key(),
            self.id.as_str(),
        )
    }

    /// The same, weighing memory ahead of time.
    pub fn memory_key(&self) -> (u8, u8, &str) {
        (
            self.cost.space.sort_key(),
            self.cost.time.sort_key(),
            self.id.as_str(),
        )
    }
}

/// A set of implementations declared together, as a file or a message.
///
/// This is what "registering as an execution source" looks like written down: a
/// module announcing which propositions it can prove and where.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Manifest {
    /// The format version.
    #[serde(default = "default_version")]
    pub version: String,
    /// Environments this manifest introduces.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub environments: Vec<crate::environment::Environment>,
    /// The proofs on offer.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub implementations: Vec<Implementation>,
}

/// The manifest format this build reads and writes.
pub const MANIFEST_VERSION: &str = "1.0";

fn default_version() -> String {
    MANIFEST_VERSION.to_string()
}

impl Default for Manifest {
    fn default() -> Self {
        Self {
            version: default_version(),
            environments: Vec::new(),
            implementations: Vec::new(),
        }
    }
}

/// A cost profile for something that does no real work.
///
/// Rearranging fields is constant in both, which is what a derived bridge does.
pub fn trivial_cost() -> ImplProfile {
    ImplProfile {
        time: BigO::O1,
        space: BigO::O1,
        notes: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn impl_with(id: &str, time: BigO, space: BigO) -> Implementation {
        Implementation::new(
            id,
            "demo::sort".parse().unwrap(),
            Ident::new("here").unwrap(),
            id,
            ImplProfile::new(time, space),
        )
    }

    #[test]
    fn the_cheaper_proof_sorts_first() {
        let merge = impl_with("merge", BigO::ONLogN, BigO::ON);
        let bubble = impl_with("bubble", BigO::ON2, BigO::O1);
        assert!(
            merge.cost_key() < bubble.cost_key(),
            "asked for speed, mergesort wins"
        );
        assert!(
            bubble.memory_key() < merge.memory_key(),
            "asked for memory, bubble sort wins"
        );
    }

    #[test]
    fn a_proof_that_will_not_say_what_it_costs_sorts_last() {
        let stated = impl_with("stated", BigO::OExp, BigO::OExp);
        let vague = impl_with("vague", BigO::Custom("depends".into()), BigO::O1);
        assert!(
            stated.cost_key() < vague.cost_key(),
            "a stated cost, however bad, beats no answer"
        );
    }

    #[test]
    fn equally_good_proofs_are_ordered_the_same_way_every_time() {
        let a = impl_with("alpha", BigO::ON, BigO::ON);
        let b = impl_with("beta", BigO::ON, BigO::ON);
        assert!(a.cost_key() < b.cost_key());
    }

    #[test]
    fn a_manifest_round_trips_through_json() {
        let manifest = Manifest {
            version: MANIFEST_VERSION.to_string(),
            environments: vec![crate::environment::Environment::subprocess(
                "py",
                "python",
                "python3",
                &["-u"],
            )],
            implementations: vec![impl_with("bubble", BigO::ON2, BigO::O1)],
        };
        let text = serde_json::to_string(&manifest).unwrap();
        assert_eq!(serde_json::from_str::<Manifest>(&text).unwrap(), manifest);
    }

    #[test]
    fn a_manifest_reads_the_way_someone_would_write_one() {
        let manifest: Manifest = serde_json::from_str(
            r#"{
                "environments": [
                    {"id":"py","language":"python",
                     "reach":{"k":"subprocess","command":"python3","args":["impls.py"]}}],
                "implementations": [
                    {"id":"demo.sort.py-bubble","proves":"demo::sort","environment":"py",
                     "entry":"bubble_sort",
                     "cost":{"time":"o_n2","space":"o1","notes":"in place"}}]
            }"#,
        )
        .expect("reads");

        assert_eq!(manifest.version, MANIFEST_VERSION);
        assert_eq!(manifest.implementations[0].cost.time, BigO::ON2);
        assert_eq!(manifest.environments[0].reach.kind(), "subprocess");
    }
}
