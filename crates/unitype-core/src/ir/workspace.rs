//! Workspaces: a set of universes and the links between them.

use serde::{Deserialize, Serialize};

use crate::ident::{Ident, QualifiedName};
use crate::ir::defs::{FunctionSig, TypeDef};
use crate::ir::sugar::SugarError;
use crate::ir::universe::Universe;
use crate::link::Link;

/// The schema version this crate reads and writes.
pub const SCHEMA_VERSION: &str = "1.0";

/// Why a workspace document could not be read.
#[derive(thiserror::Error, Debug)]
pub enum LoadError {
    /// The text was not valid JSON, or did not match the schema.
    #[error("invalid workspace document: {0}")]
    Json(#[from] serde_json::Error),
    /// Shorthand in the document could not be expanded.
    #[error(transparent)]
    Sugar(#[from] SugarError),
    /// The document declared a schema version this crate does not read.
    #[error("unsupported schema version `{found}`; this build reads `{expected}`")]
    Version {
        /// The version the document declared.
        found: String,
        /// The version this build understands.
        expected: &'static str,
    },
}

/// Everything loaded together: several universes and the links between them.
///
/// A workspace is the unit that gets checked, because a link is only meaningful
/// when both of its endpoints are present.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Workspace {
    /// The schema version of this document.
    #[serde(default = "default_version")]
    pub version: String,
    /// The universes in scope.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub universes: Vec<Universe>,
    /// Declared correspondences between types in different universes.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub links: Vec<Link>,
}

fn default_version() -> String {
    SCHEMA_VERSION.to_string()
}

impl Default for Workspace {
    fn default() -> Self {
        Self {
            version: default_version(),
            universes: Vec::new(),
            links: Vec::new(),
        }
    }
}

impl Workspace {
    /// A workspace holding the given universes and no links.
    pub fn of(universes: impl IntoIterator<Item = Universe>) -> Self {
        Self {
            universes: universes.into_iter().collect(),
            ..Self::default()
        }
    }

    /// Reads a workspace from JSON, expanding the input shorthand first.
    pub fn from_json(text: &str) -> Result<Self, LoadError> {
        let mut json: serde_json::Value = serde_json::from_str(text)?;
        crate::ir::sugar::desugar_workspace(&mut json)?;
        let ws: Workspace = serde_json::from_value(json)?;
        if ws.version != SCHEMA_VERSION {
            return Err(LoadError::Version {
                found: ws.version,
                expected: SCHEMA_VERSION,
            });
        }
        Ok(ws)
    }

    /// Looks up a universe by name.
    pub fn universe(&self, name: &Ident) -> Option<&Universe> {
        self.universes.iter().find(|u| &u.name == name)
    }

    /// Looks up a type declaration by its qualified name.
    pub fn type_def(&self, name: &QualifiedName) -> Option<&TypeDef> {
        self.universe(&name.universe)?.type_def(&name.name)
    }

    /// Looks up a signature by its qualified name.
    pub fn function(&self, name: &QualifiedName) -> Option<&FunctionSig> {
        self.universe(&name.universe)?.function(&name.name)
    }

    /// Looks up a link by name.
    pub fn link(&self, name: &Ident) -> Option<&Link> {
        self.links.iter().find(|l| &l.name == name)
    }

    /// Every declaration in the workspace, paired with its qualified name.
    pub fn iter_defs(&self) -> impl Iterator<Item = (QualifiedName, &TypeDef)> {
        self.universes.iter().flat_map(|u| {
            u.types
                .iter()
                .map(move |d| (QualifiedName::new(u.name.clone(), d.name().clone()), d))
        })
    }

    /// Every signature in the workspace, paired with its qualified name.
    pub fn iter_functions(&self) -> impl Iterator<Item = (QualifiedName, &FunctionSig)> {
        self.universes.iter().flat_map(|u| {
            u.functions
                .iter()
                .map(move |f| (QualifiedName::new(u.name.clone(), f.name.clone()), f))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_empty_document_reads_as_an_empty_workspace() {
        let ws = Workspace::from_json("{}").expect("reads");
        assert_eq!(ws.version, SCHEMA_VERSION);
        assert!(ws.universes.is_empty());
    }

    #[test]
    fn a_future_schema_version_is_refused_rather_than_guessed_at() {
        let err = Workspace::from_json(r#"{"version":"9.9"}"#).unwrap_err();
        assert!(matches!(err, LoadError::Version { .. }), "{err}");
    }

    #[test]
    fn declarations_are_reachable_by_qualified_name() {
        let ws = Workspace::from_json(
            r#"{"universes":[
                {"name":"geo","types":[{"k":"struct","name":"Point",
                 "fields":[{"name":"x","ty":"float"},{"name":"y","ty":"float"}]}]}
            ]}"#,
        )
        .expect("reads");
        let q: QualifiedName = "geo::Point".parse().unwrap();
        assert!(ws.type_def(&q).is_some());
        assert!(ws.type_def(&"geo::Missing".parse().unwrap()).is_none());
        assert_eq!(ws.iter_defs().count(), 1);
    }
}
