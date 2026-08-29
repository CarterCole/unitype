//! Named declarations: the types a universe defines and the propositions it
//! states.

use std::fmt;

use serde::{Deserialize, Serialize};

use crate::ident::{Ident, QualifiedName};
use crate::ir::expr::Expr;
use crate::ir::ty::{Field, Type, TypeRef};
use crate::meta::Metadata;

/// A generic parameter introduced by a declaration.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct TypeParamDecl {
    /// The parameter's name, referred to by [`Type::Param`].
    pub name: Ident,
    /// Interfaces every argument must satisfy.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub bounds: Vec<TypeRef>,
}

impl TypeParamDecl {
    /// An unbounded parameter.
    pub fn new(name: Ident) -> Self {
        Self {
            name,
            bounds: Vec::new(),
        }
    }
}

/// A nominal product type, optionally extending one parent and satisfying any
/// number of interfaces.
///
/// Fields keep the order they were declared in. Unlike an anonymous record,
/// that order is semantic: it drives positional constructors and field layout
/// in generated code.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct StructDef {
    /// The declared name.
    pub name: Ident,
    /// Prose documentation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub doc: Option<String>,
    /// Generic parameters.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub params: Vec<TypeParamDecl>,
    /// The single parent struct, which must be declared in the same universe.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub extends: Option<TypeRef>,
    /// Interfaces this struct claims to satisfy.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub implements: Vec<TypeRef>,
    /// Fields declared here, not counting inherited ones.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub fields: Vec<Field>,
    /// Non-identifying annotations.
    #[serde(default, skip_serializing_if = "Metadata::is_empty")]
    pub meta: Metadata,
}

/// A field contract that structs can claim to satisfy.
///
/// Interfaces carry fields only in v1; behavioral conformance against declared
/// function signatures is a later addition that does not disturb these
/// declarations.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct InterfaceDef {
    /// The declared name.
    pub name: Ident,
    /// Prose documentation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub doc: Option<String>,
    /// Generic parameters.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub params: Vec<TypeParamDecl>,
    /// Interfaces this one refines. Unlike structs, interfaces may extend
    /// several, since no layout has to be reconciled.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub extends: Vec<TypeRef>,
    /// Required fields.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub fields: Vec<Field>,
    /// Non-identifying annotations.
    #[serde(default, skip_serializing_if = "Metadata::is_empty")]
    pub meta: Metadata,
}

/// One alternative of an [`EnumDef`].
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Variant {
    /// The variant's tag, as it appears on the wire.
    pub name: Ident,
    /// What the variant carries; [`Type::Unit`] for a bare tag.
    #[serde(default = "unit_type", skip_serializing_if = "is_unit_type")]
    pub payload: Type,
    /// Prose documentation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub doc: Option<String>,
}

fn unit_type() -> Type {
    Type::Unit
}

fn is_unit_type(t: &Type) -> bool {
    matches!(t, Type::Unit)
}

impl Variant {
    /// A variant carrying nothing.
    pub fn unit(name: Ident) -> Self {
        Self {
            name,
            payload: Type::Unit,
            doc: None,
        }
    }
}

/// A nominal tagged sum.
///
/// Distinct from [`Type::Union`], which is untagged and structural: an enum has
/// a fixed wire tag and a nominal identity, so adding a variant with the same
/// shape as an existing one is still a distinct alternative.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct EnumDef {
    /// The declared name.
    pub name: Ident,
    /// Prose documentation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub doc: Option<String>,
    /// Generic parameters.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub params: Vec<TypeParamDecl>,
    /// The alternatives.
    pub variants: Vec<Variant>,
    /// Non-identifying annotations.
    #[serde(default, skip_serializing_if = "Metadata::is_empty")]
    pub meta: Metadata,
}

/// A transparent name for another type.
///
/// An alias is only a name: it is expanded before hashing and before subtyping,
/// so it shares its target's identity rather than having one of its own.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AliasDef {
    /// The declared name.
    pub name: Ident,
    /// Prose documentation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub doc: Option<String>,
    /// Generic parameters.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub params: Vec<TypeParamDecl>,
    /// The type this name stands for.
    pub target: Type,
    /// Non-identifying annotations.
    #[serde(default, skip_serializing_if = "Metadata::is_empty")]
    pub meta: Metadata,
}

/// A named type declaration.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "k", rename_all = "snake_case")]
pub enum TypeDef {
    /// A nominal product type.
    Struct(StructDef),
    /// A field contract.
    Interface(InterfaceDef),
    /// A nominal tagged sum.
    Enum(EnumDef),
    /// A transparent name.
    Alias(AliasDef),
}

impl TypeDef {
    /// The declared name.
    pub fn name(&self) -> &Ident {
        match self {
            TypeDef::Struct(d) => &d.name,
            TypeDef::Interface(d) => &d.name,
            TypeDef::Enum(d) => &d.name,
            TypeDef::Alias(d) => &d.name,
        }
    }

    /// The generic parameters this declaration introduces.
    pub fn params(&self) -> &[TypeParamDecl] {
        match self {
            TypeDef::Struct(d) => &d.params,
            TypeDef::Interface(d) => &d.params,
            TypeDef::Enum(d) => &d.params,
            TypeDef::Alias(d) => &d.params,
        }
    }

    /// Prose documentation.
    pub fn doc(&self) -> Option<&str> {
        match self {
            TypeDef::Struct(d) => d.doc.as_deref(),
            TypeDef::Interface(d) => d.doc.as_deref(),
            TypeDef::Enum(d) => d.doc.as_deref(),
            TypeDef::Alias(d) => d.doc.as_deref(),
        }
    }

    /// The kind token used in the wire form, for diagnostics.
    pub fn kind_name(&self) -> &'static str {
        match self {
            TypeDef::Struct(_) => "struct",
            TypeDef::Interface(_) => "interface",
            TypeDef::Enum(_) => "enum",
            TypeDef::Alias(_) => "alias",
        }
    }

    /// The struct this declaration is, if it is one.
    pub fn as_struct(&self) -> Option<&StructDef> {
        match self {
            TypeDef::Struct(d) => Some(d),
            _ => None,
        }
    }

    /// The interface this declaration is, if it is one.
    pub fn as_interface(&self) -> Option<&InterfaceDef> {
        match self {
            TypeDef::Interface(d) => Some(d),
            _ => None,
        }
    }
}

/// One named parameter of a [`FunctionSig`].
///
/// The name is for humans and for generated code; it is not part of the
/// proposition, so renaming a parameter does not change the signature's
/// identity.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct NamedParam {
    /// The parameter's name.
    pub name: Ident,
    /// The parameter's type.
    pub ty: Type,
    /// Prose documentation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub doc: Option<String>,
}

impl NamedParam {
    /// An undocumented parameter.
    pub fn new(name: Ident, ty: Type) -> Self {
        Self {
            name,
            ty,
            doc: None,
        }
    }
}

/// A proposition: a named function type that implementations prove.
///
/// A signature says *what* is computed, never *how*. Implementations supply
/// the how, and there may be many with different costs. When `body` is present
/// the signature carries its own constructive proof, which both runs directly
/// and transpiles into every target language.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct FunctionSig {
    /// The declared name.
    pub name: Ident,
    /// Prose documentation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub doc: Option<String>,
    /// Generic parameters.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub params_decl: Vec<TypeParamDecl>,
    /// The parameters, in order.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub params: Vec<NamedParam>,
    /// The result type.
    pub ret: Type,
    /// A signature this one strengthens.
    ///
    /// A proof of the stronger proposition is also a proof of the weaker one,
    /// so implementations of a refinement are candidates when the refined
    /// signature is called.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refines: Option<QualifiedName>,
    /// A constructive proof written in the expression IR.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub body: Option<Expr>,
    /// Non-identifying annotations.
    #[serde(default, skip_serializing_if = "Metadata::is_empty")]
    pub meta: Metadata,
}

impl FunctionSig {
    /// The structural function type this signature states, with parameter names
    /// dropped. This is the proposition itself.
    pub fn function_type(&self) -> Type {
        Type::Function {
            params: self.params.iter().map(|p| p.ty.clone()).collect(),
            ret: Box::new(self.ret.clone()),
        }
    }
}

/// A proposition, written the way it would be said.
///
/// [`FunctionSig::function_type`] is the proposition's identity and drops
/// parameter names on purpose. This keeps them, because a person reading
/// `(xs: [i64]) -> [i64]` learns something `([i64]) -> [i64]` does not tell
/// them — and someone about to pass arguments positionally learns it in the
/// order they need it.
impl fmt::Display for FunctionSig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.name)?;
        if !self.params_decl.is_empty() {
            let mut first = true;
            write!(f, "<")?;
            for decl in &self.params_decl {
                if !std::mem::take(&mut first) {
                    write!(f, ", ")?;
                }
                write!(f, "{}", decl.name)?;
                for (n, bound) in decl.bounds.iter().enumerate() {
                    write!(f, "{} {bound}", if n == 0 { ":" } else { " +" })?;
                }
            }
            write!(f, ">")?;
        }
        write!(f, "(")?;
        for (n, param) in self.params.iter().enumerate() {
            if n > 0 {
                write!(f, ", ")?;
            }
            write!(f, "{}: {}", param.name, param.ty)?;
        }
        write!(f, ") -> {}", self.ret)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id(s: &str) -> Ident {
        Ident::new(s).unwrap()
    }

    #[test]
    fn a_proposition_reads_the_way_it_would_be_said() {
        let sig = FunctionSig {
            name: id("sort"),
            doc: None,
            params_decl: vec![],
            params: vec![NamedParam::new(id("xs"), Type::I64.list())],
            ret: Type::I64.list(),
            refines: None,
            body: None,
            meta: Metadata::default(),
        };
        assert_eq!(sig.to_string(), "sort(xs: [i64]) -> [i64]");

        // Names are what this adds, so it must not be the same as the identity.
        assert_eq!(sig.function_type().to_string(), "([i64]) -> [i64]");
    }

    #[test]
    fn a_generic_proposition_says_what_it_is_over() {
        let sig = FunctionSig {
            name: id("first"),
            doc: None,
            params_decl: vec![TypeParamDecl {
                name: id("T"),
                bounds: vec![TypeRef::local(id("Ord")), TypeRef::local(id("Show"))],
            }],
            params: vec![NamedParam::new(
                id("xs"),
                Type::Param { name: id("T") }.list(),
            )],
            ret: Type::Param { name: id("T") }.optional(),
            refines: None,
            body: None,
            meta: Metadata::default(),
        };
        assert_eq!(sig.to_string(), "first<T: Ord + Show>(xs: [T]) -> T?");
    }

    #[test]
    fn declarations_round_trip_through_json() {
        let d = TypeDef::Struct(StructDef {
            name: id("Circle"),
            doc: None,
            params: vec![],
            extends: Some(TypeRef::local(id("Shape"))),
            implements: vec![],
            fields: vec![Field::new(id("radius"), Type::F64)],
            meta: Metadata::default(),
        });
        let j = serde_json::to_string(&d).unwrap();
        assert!(j.starts_with(r#"{"k":"struct""#), "{j}");
        assert_eq!(serde_json::from_str::<TypeDef>(&j).unwrap(), d);
    }

    #[test]
    fn bare_variants_omit_their_unit_payload() {
        let v = Variant::unit(id("Red"));
        assert_eq!(serde_json::to_string(&v).unwrap(), r#"{"name":"Red"}"#);
        assert_eq!(
            serde_json::from_str::<Variant>(r#"{"name":"Red"}"#).unwrap(),
            v
        );
    }

    #[test]
    fn a_signature_states_a_function_type_without_parameter_names() {
        let sig = FunctionSig {
            name: id("sort"),
            doc: None,
            params_decl: vec![],
            params: vec![NamedParam::new(id("xs"), Type::I64.list())],
            ret: Type::I64.list(),
            refines: None,
            body: None,
            meta: Metadata::default(),
        };
        let renamed = FunctionSig {
            params: vec![NamedParam::new(id("items"), Type::I64.list())],
            ..sig.clone()
        };
        assert_eq!(sig.function_type(), renamed.function_type());
    }
}
