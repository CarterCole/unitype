//! Universes: named worlds of types and propositions.

use serde::{Deserialize, Serialize};

use crate::ident::Ident;
use crate::ir::defs::{FunctionSig, TypeDef};
use crate::meta::Metadata;

/// A named scope of type declarations and function signatures.
///
/// A universe is whatever boundary matters to you — one service, one vendor's
/// API, one version of a schema. Names are unique within a universe and mean
/// nothing outside it: two universes may both declare `User` without any
/// relationship between them, and establishing one takes an explicit link.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Universe {
    /// The universe's name, unique within its workspace.
    pub name: Ident,
    /// Prose documentation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub doc: Option<String>,
    /// The types this universe declares.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub types: Vec<TypeDef>,
    /// The propositions this universe states.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub functions: Vec<FunctionSig>,
    /// Non-identifying annotations.
    #[serde(default, skip_serializing_if = "Metadata::is_empty")]
    pub meta: Metadata,
}

impl Universe {
    /// An empty universe.
    pub fn new(name: Ident) -> Self {
        Self {
            name,
            doc: None,
            types: Vec::new(),
            functions: Vec::new(),
            meta: Metadata::default(),
        }
    }

    /// Looks up a declaration by name.
    pub fn type_def(&self, name: &Ident) -> Option<&TypeDef> {
        self.types.iter().find(|d| d.name() == name)
    }

    /// Looks up a signature by name.
    pub fn function(&self, name: &Ident) -> Option<&FunctionSig> {
        self.functions.iter().find(|f| &f.name == name)
    }

    /// Reads a universe from JSON, expanding the input shorthand first.
    pub fn from_json(text: &str) -> Result<Self, crate::ir::LoadError> {
        let mut json: serde_json::Value = serde_json::from_str(text)?;
        crate::ir::sugar::desugar_universe(&mut json)?;
        Ok(serde_json::from_value(json)?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_universe_reads_from_shorthand() {
        let u = Universe::from_json(
            r#"{
                "name": "demo",
                "types": [
                    {"k": "struct", "name": "Shape", "fields": [{"name": "id", "ty": "str"}]},
                    {"k": "struct", "name": "Circle", "extends": "Shape",
                     "fields": [{"name": "radius", "ty": "float"}]}
                ],
                "functions": [
                    {"name": "sort", "params": [{"name": "xs", "ty": "[int]"}], "ret": "[int]"}
                ]
            }"#,
        )
        .expect("reads");

        assert_eq!(u.types.len(), 2);
        let circle = u
            .type_def(&Ident::new("Circle").unwrap())
            .and_then(TypeDef::as_struct)
            .expect("Circle is a struct");
        assert_eq!(
            circle.extends.as_ref().map(|r| r.name.as_str()),
            Some("Shape")
        );

        let sort = u.function(&Ident::new("sort").unwrap()).expect("sort");
        assert_eq!(sort.ret, crate::ir::Type::I64.list());
    }
}
