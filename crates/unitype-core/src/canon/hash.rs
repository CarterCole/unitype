//! Content addressing: what gives a type an identity no one has to assign.
//!
//! A hash is taken over the canonical bytes of an *identity form* — the
//! declaration with everything that is not the type itself removed. Two systems
//! that have never exchanged a schema can then compare 32 bytes and know
//! whether they hold the same type.
//!
//! # What identity excludes
//!
//! Documentation and metadata are stripped, because prose is not structure.
//! Parameter names are stripped from signatures, because `sort(xs)` and
//! `sort(items)` state the same proposition. A signature's body is stripped
//! too: a proposition does not change when someone finds a better proof of it.
//!
//! # Domains
//!
//! Each kind of hash is computed in its own domain, so a type and a signature
//! can never collide even if their encodings coincide:
//!
//! - [`local_hash`] — a type expression, with references left as the names they
//!   were written as. "Local" because it does not chase a reference to see what
//!   it points at.
//! - [`decl_hash`] — a declaration as authored, names included. Cheap, and what
//!   you want for change detection.
//! - [`sig_hash`] — the identity of a proposition, which is what an
//!   implementation registers itself against.
//!
//! A fully name-erased structural hash, which requires resolving references
//! across a whole workspace, is built on these primitives by the checker.

use std::collections::HashMap;
use std::fmt;
use std::str::FromStr;

use serde::de::Error as _;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::canon::encode::canonical_bytes;
use crate::canon::normalize::normalize_type;
use crate::ident::Ident;
use crate::ir::{
    AliasDef, EnumDef, Field, FunctionSig, InterfaceDef, StructDef, Type, TypeDef, TypeParamDecl,
    Variant,
};

/// Domain for hashing a standalone type expression.
const DOMAIN_LOCAL: &str = "unitype local v1";
/// Domain for hashing a declaration as authored.
const DOMAIN_DECL: &str = "unitype decl v1";
/// Domain for hashing a proposition.
const DOMAIN_SIG: &str = "unitype sig v1";

/// The prefix that marks a content address in text.
pub const TYPE_ID_PREFIX: &str = "ut1:";

/// A content address: the hash of something's canonical identity form.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct TypeId([u8; 32]);

impl TypeId {
    /// Wraps raw digest bytes.
    pub fn from_bytes(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    /// The raw digest bytes.
    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }

    /// The first twelve hex digits, for display where the full address would
    /// crowd everything else out.
    pub fn short(&self) -> String {
        self.to_hex()[..12].to_string()
    }

    fn to_hex(self) -> String {
        self.0.iter().map(|b| format!("{b:02x}")).collect()
    }
}

/// Hashes `bytes` in the given domain.
fn digest(domain: &str, bytes: &[u8]) -> TypeId {
    TypeId(
        *blake3::Hasher::new_derive_key(domain)
            .update(bytes)
            .finalize()
            .as_bytes(),
    )
}

impl fmt::Display for TypeId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{TYPE_ID_PREFIX}{}", self.to_hex())
    }
}

impl fmt::Debug for TypeId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{self}")
    }
}

/// Why text could not be read as a content address.
#[derive(thiserror::Error, Debug, Clone, PartialEq, Eq)]
#[error("`{0}` is not a content address of the form `ut1:<64 hex digits>`")]
pub struct TypeIdParseError(String);

impl FromStr for TypeId {
    type Err = TypeIdParseError;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let hex = s
            .strip_prefix(TYPE_ID_PREFIX)
            .ok_or_else(|| TypeIdParseError(s.to_string()))?;
        if hex.len() != 64 {
            return Err(TypeIdParseError(s.to_string()));
        }
        let mut bytes = [0u8; 32];
        for (i, byte) in bytes.iter_mut().enumerate() {
            *byte = u8::from_str_radix(&hex[i * 2..i * 2 + 2], 16)
                .map_err(|_| TypeIdParseError(s.to_string()))?;
        }
        Ok(TypeId(bytes))
    }
}

impl Serialize for TypeId {
    fn serialize<S: Serializer>(&self, ser: S) -> Result<S::Ok, S::Error> {
        ser.serialize_str(&self.to_string())
    }
}

impl<'de> Deserialize<'de> for TypeId {
    fn deserialize<D: Deserializer<'de>>(de: D) -> Result<Self, D::Error> {
        let s = String::deserialize(de)?;
        s.parse().map_err(D::Error::custom)
    }
}

/// The identity of a type expression, with references left as written.
///
/// Normalizes first, so a caller cannot accidentally hash a non-canonical form.
pub fn local_hash(t: &Type) -> TypeId {
    digest(DOMAIN_LOCAL, &canonical_bytes(&identity_type(t)))
}

/// The identity of a declaration as authored, prose removed but names kept.
pub fn decl_hash(def: &TypeDef) -> TypeId {
    digest(DOMAIN_DECL, &canonical_bytes(&identity_def(def)))
}

/// The identity of a proposition.
///
/// Parameter names, documentation, metadata, the declared name, any body, and
/// any `refines` relationship are all excluded: what remains is the function
/// type and the shape of its generic parameters.
pub fn sig_hash(sig: &FunctionSig) -> TypeId {
    digest(DOMAIN_SIG, &canonical_bytes(&sig_identity(sig)))
}

/// A proposition's identity form.
#[derive(Serialize)]
struct SigIdentity {
    /// Generic parameters, renamed to their positions.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    params: Vec<TypeParamDecl>,
    /// The function type, with the same renaming applied.
    ty: Type,
}

fn sig_identity(sig: &FunctionSig) -> SigIdentity {
    let renames = positional_renames(&sig.params_decl);
    SigIdentity {
        params: sig
            .params_decl
            .iter()
            .enumerate()
            .map(|(i, decl)| TypeParamDecl {
                name: positional_name(i),
                bounds: decl
                    .bounds
                    .iter()
                    .map(|b| {
                        let Type::Ref(r) =
                            rename_params(&identity_type(&Type::Ref(b.clone())), &renames)
                        else {
                            unreachable!("renaming a reference yields a reference");
                        };
                        r
                    })
                    .collect(),
            })
            .collect(),
        ty: rename_params(&identity_type(&sig.function_type()), &renames),
    }
}

/// `_0`, `_1`, and so on: the name a generic parameter has once only its
/// position matters.
fn positional_name(index: usize) -> Ident {
    Ident::new_unchecked(format!("_{index}"))
}

fn positional_renames(decls: &[TypeParamDecl]) -> HashMap<Ident, Ident> {
    decls
        .iter()
        .enumerate()
        .map(|(i, d)| (d.name.clone(), positional_name(i)))
        .collect()
}

/// Rewrites every generic parameter occurrence according to `renames`.
fn rename_params(t: &Type, renames: &HashMap<Ident, Ident>) -> Type {
    if renames.is_empty() {
        return t.clone();
    }
    let kids: Vec<Type> = t
        .children()
        .iter()
        .map(|c| rename_params(c, renames))
        .collect();
    let node = t.with_children(kids);
    match node {
        Type::Param { name } => Type::Param {
            name: renames.get(&name).cloned().unwrap_or(name),
        },
        other => other,
    }
}

/// A type in identity form: canonical, with field documentation removed.
fn identity_type(t: &Type) -> Type {
    strip_type_docs(&normalize_type(t))
}

fn strip_type_docs(t: &Type) -> Type {
    let kids: Vec<Type> = t.children().iter().map(|c| strip_type_docs(c)).collect();
    let node = t.with_children(kids);
    match node {
        Type::Record { fields } => Type::Record {
            fields: fields.into_iter().map(strip_field_doc).collect(),
        },
        other => other,
    }
}

fn strip_field_doc(f: Field) -> Field {
    Field {
        name: f.name,
        ty: f.ty,
        doc: None,
    }
}

/// A declaration in identity form: canonical types, no prose, no annotations.
fn identity_def(def: &TypeDef) -> TypeDef {
    match def {
        TypeDef::Struct(d) => TypeDef::Struct(StructDef {
            name: d.name.clone(),
            doc: None,
            params: d.params.clone(),
            extends: d.extends.clone(),
            implements: d.implements.clone(),
            fields: d
                .fields
                .iter()
                .map(|f| Field {
                    name: f.name.clone(),
                    ty: identity_type(&f.ty),
                    doc: None,
                })
                .collect(),
            meta: Default::default(),
        }),
        TypeDef::Interface(d) => TypeDef::Interface(InterfaceDef {
            name: d.name.clone(),
            doc: None,
            params: d.params.clone(),
            extends: d.extends.clone(),
            fields: d
                .fields
                .iter()
                .map(|f| Field {
                    name: f.name.clone(),
                    ty: identity_type(&f.ty),
                    doc: None,
                })
                .collect(),
            meta: Default::default(),
        }),
        TypeDef::Enum(d) => TypeDef::Enum(EnumDef {
            name: d.name.clone(),
            doc: None,
            params: d.params.clone(),
            variants: d
                .variants
                .iter()
                .map(|v| Variant {
                    name: v.name.clone(),
                    payload: identity_type(&v.payload),
                    doc: None,
                })
                .collect(),
            meta: Default::default(),
        }),
        TypeDef::Alias(d) => TypeDef::Alias(AliasDef {
            name: d.name.clone(),
            doc: None,
            params: d.params.clone(),
            target: identity_type(&d.target),
            meta: Default::default(),
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir::NamedParam;
    use crate::meta::Metadata;

    fn id(s: &str) -> Ident {
        Ident::new(s).unwrap()
    }

    fn sig(name: &str, params: Vec<NamedParam>, ret: Type) -> FunctionSig {
        FunctionSig {
            name: id(name),
            doc: None,
            params_decl: vec![],
            params,
            ret,
            refines: None,
            body: None,
            meta: Metadata::default(),
        }
    }

    #[test]
    fn a_content_address_round_trips_through_text() {
        let h = local_hash(&Type::I64);
        let text = h.to_string();
        assert!(text.starts_with("ut1:"));
        assert_eq!(text.len(), 4 + 64);
        assert_eq!(text.parse::<TypeId>().unwrap(), h);
        assert_eq!(h.short().len(), 12);
    }

    #[test]
    fn malformed_addresses_are_refused() {
        assert!("nope".parse::<TypeId>().is_err());
        assert!("ut1:abc".parse::<TypeId>().is_err());
        assert!("ut1:".parse::<TypeId>().is_err());
        // Right length, but not hex.
        assert!(format!("ut1:{}", "z".repeat(64)).parse::<TypeId>().is_err());
    }

    #[test]
    fn types_written_differently_but_meaning_the_same_share_an_address() {
        let a = Type::Union {
            members: vec![Type::I64, Type::String],
        };
        let b = Type::Union {
            members: vec![Type::String, Type::I64, Type::Never],
        };
        assert_eq!(local_hash(&a), local_hash(&b));
    }

    #[test]
    fn genuinely_different_types_have_different_addresses() {
        assert_ne!(local_hash(&Type::I32), local_hash(&Type::I64));
        assert_ne!(local_hash(&Type::I64.list()), local_hash(&Type::I64));
        assert_ne!(
            local_hash(&Type::I64.future()),
            local_hash(&Type::I64.optional())
        );
    }

    #[test]
    fn prose_does_not_change_what_a_type_is() {
        let documented = Type::Record {
            fields: vec![Field {
                name: id("x"),
                ty: Type::I64,
                doc: Some("how far along".into()),
            }],
        };
        let bare = Type::Record {
            fields: vec![Field::new(id("x"), Type::I64)],
        };
        assert_eq!(local_hash(&documented), local_hash(&bare));
    }

    #[test]
    fn annotations_do_not_change_what_a_declaration_is() {
        let mut meta = Metadata::default();
        meta.0.insert("owner".into(), serde_json::json!("platform"));
        let annotated = TypeDef::Alias(AliasDef {
            name: id("Id"),
            doc: Some("an identifier".into()),
            params: vec![],
            target: Type::String,
            meta,
        });
        let bare = TypeDef::Alias(AliasDef {
            name: id("Id"),
            doc: None,
            params: vec![],
            target: Type::String,
            meta: Metadata::default(),
        });
        assert_eq!(decl_hash(&annotated), decl_hash(&bare));
    }

    #[test]
    fn renaming_a_parameter_does_not_change_the_proposition() {
        let a = sig(
            "sort",
            vec![NamedParam::new(id("xs"), Type::I64.list())],
            Type::I64.list(),
        );
        let b = sig(
            "arrange",
            vec![NamedParam::new(id("items"), Type::I64.list())],
            Type::I64.list(),
        );
        assert_eq!(sig_hash(&a), sig_hash(&b));
    }

    #[test]
    fn finding_a_proof_does_not_change_the_proposition() {
        let bare = sig("f", vec![], Type::I64);
        let proved = FunctionSig {
            body: Some(crate::ir::Expr::lit(crate::value::Value::Int(1))),
            doc: Some("now with a proof".into()),
            ..bare.clone()
        };
        assert_eq!(sig_hash(&bare), sig_hash(&proved));
    }

    #[test]
    fn changing_a_parameter_type_does_change_the_proposition() {
        let a = sig("f", vec![NamedParam::new(id("x"), Type::I32)], Type::I64);
        let b = sig("f", vec![NamedParam::new(id("x"), Type::I64)], Type::I64);
        assert_ne!(sig_hash(&a), sig_hash(&b));
    }

    #[test]
    fn generic_parameters_are_identified_by_position_not_name() {
        let make = |param: &str| FunctionSig {
            name: id("identity"),
            doc: None,
            params_decl: vec![TypeParamDecl::new(id(param))],
            params: vec![NamedParam::new(id("v"), Type::Param { name: id(param) })],
            ret: Type::Param { name: id(param) },
            refines: None,
            body: None,
            meta: Metadata::default(),
        };
        assert_eq!(sig_hash(&make("T")), sig_hash(&make("U")));
    }

    #[test]
    fn the_three_domains_do_not_collide() {
        // The same bytes hashed in different domains must not coincide.
        let t = Type::String;
        let d = TypeDef::Alias(AliasDef {
            name: id("S"),
            doc: None,
            params: vec![],
            target: Type::String,
            meta: Metadata::default(),
        });
        let s = sig("f", vec![], Type::String);
        let ids = [local_hash(&t), decl_hash(&d), sig_hash(&s)];
        for (i, a) in ids.iter().enumerate() {
            for b in &ids[i + 1..] {
                assert_ne!(a, b);
            }
        }
    }
}
