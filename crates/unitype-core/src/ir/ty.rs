//! The symbolic type representation.
//!
//! Every node serializes as a JSON object carrying a `"k"` discriminator, so a
//! type is always a self-describing literal. See [`crate::canon`] for the
//! normalization rules that make one semantic type have exactly one such
//! serialization, and [`crate::ir::sugar`] for the shorthand accepted on input.

use serde::{Deserialize, Serialize};

use crate::ident::Ident;

/// The largest magnitude of integer that a float holds exactly.
///
/// Above this, consecutive integers stop being distinguishable as floats.
const MAX_EXACT_INTEGER_IN_F64: i64 = 1 << 53;

/// Whether an integer survives being a float.
///
/// Note that a round-trip cast is *not* a valid test for this: converting an
/// out-of-range float back to an integer saturates, so `i64::MAX` appears to
/// round-trip when it does not.
pub fn fits_f64_exactly(i: i64) -> bool {
    (-MAX_EXACT_INTEGER_IN_F64..=MAX_EXACT_INTEGER_IN_F64).contains(&i)
}

/// A singleton value promoted to a type.
///
/// Singleton types are what make a union discriminable: a field typed
/// `Literal("circle")` pins that variant's tag. Floats are excluded — they have
/// no exact equality worth building type identity on.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LiteralValue {
    /// A specific boolean.
    Bool(bool),
    /// A specific integer.
    Int(i64),
    /// A specific string.
    Str(String),
}

impl LiteralValue {
    /// The primitive type that subsumes this literal.
    pub fn base(&self) -> Type {
        match self {
            LiteralValue::Bool(_) => Type::Bool,
            LiteralValue::Int(_) => Type::I64,
            LiteralValue::Str(_) => Type::String,
        }
    }
}

/// A reference to a named declaration, with any generic arguments applied.
///
/// `universe` is optional on input, where it means "the universe this appears
/// in". Canonical form always states it, so a canonical type carries its
/// meaning independent of where it was found.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct TypeRef {
    /// The declaring universe; `None` means the enclosing one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub universe: Option<Ident>,
    /// The declared name.
    pub name: Ident,
    /// Generic arguments, empty when the target takes no parameters.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub args: Vec<Type>,
}

impl TypeRef {
    /// A reference with no universe stated and no generic arguments.
    pub fn local(name: Ident) -> Self {
        Self {
            universe: None,
            name,
            args: Vec::new(),
        }
    }

    /// A reference naming its universe explicitly.
    pub fn qualified(universe: Ident, name: Ident) -> Self {
        Self {
            universe: Some(universe),
            name,
            args: Vec::new(),
        }
    }
}

/// One named field of a record or struct.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Field {
    /// The field name, as it appears on the wire.
    pub name: Ident,
    /// The field's type.
    pub ty: Type,
    /// Prose documentation, carried through codegen but excluded from identity.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub doc: Option<String>,
}

impl Field {
    /// An undocumented field.
    pub fn new(name: Ident, ty: Type) -> Self {
        Self {
            name,
            ty,
            doc: None,
        }
    }
}

/// A type.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "k", rename_all = "snake_case")]
pub enum Type {
    /// The empty type: no value inhabits it, and it is below every type.
    Never,
    /// The unknown type: every value inhabits it, and it is above every type.
    Any,
    /// Exactly one value, written `null`.
    Unit,
    /// A boolean.
    Bool,
    /// A 32-bit signed integer.
    I32,
    /// A 64-bit signed integer.
    I64,
    /// A 64-bit float. Not-a-number and the infinities are rejected at the
    /// boundary rather than encoded, since JSON cannot carry them.
    F64,
    /// A UTF-8 string.
    String,
    /// An opaque byte string, base64 on the wire.
    Bytes,
    /// A single value promoted to a type.
    Literal {
        /// The inhabiting value.
        value: LiteralValue,
    },
    /// A value that may be absent. Absent, `null`, and "field missing" are one
    /// state, deliberately: it is the only one all target languages share.
    Optional {
        /// The type when present.
        of: Box<Type>,
    },
    /// An ordered sequence.
    List {
        /// The element type.
        of: Box<Type>,
    },
    /// An association from keys to values.
    Map {
        /// The key type, restricted to string and integer types.
        key: Box<Type>,
        /// The value type.
        value: Box<Type>,
    },
    /// A fixed-length heterogeneous sequence, of arity two or more.
    Tuple {
        /// The element types, positionally.
        items: Vec<Type>,
    },
    /// An anonymous structural record.
    Record {
        /// The fields, sorted by name in canonical form.
        fields: Vec<Field>,
    },
    /// An untagged union of alternatives.
    Union {
        /// The alternatives, flattened, deduplicated, and ordered in canonical
        /// form.
        members: Vec<Type>,
    },
    /// A function type. Structural and positional: parameter *names* live on
    /// the declaration, not the type, so two signatures that differ only in
    /// parameter naming state the same proposition.
    Function {
        /// Parameter types, positionally.
        params: Vec<Type>,
        /// The result type.
        ret: Box<Type>,
    },
    /// A value that will be available later.
    ///
    /// Nesting is preserved rather than collapsed: `Future<Future<T>>` is a
    /// distinct type from `Future<T>`, because it is distinct in Rust. Targets
    /// that auto-flatten handle it in their own backend.
    Future {
        /// The eventual type.
        of: Box<Type>,
    },
    /// A reference to a named declaration.
    Ref(TypeRef),
    /// An occurrence of a generic parameter bound by the enclosing declaration.
    Param {
        /// The parameter's name.
        name: Ident,
    },
}

impl Type {
    /// Wraps `self` in [`Type::Optional`].
    pub fn optional(self) -> Type {
        Type::Optional { of: Box::new(self) }
    }

    /// A list of `self`.
    pub fn list(self) -> Type {
        Type::List { of: Box::new(self) }
    }

    /// A future of `self`.
    pub fn future(self) -> Type {
        Type::Future { of: Box::new(self) }
    }

    /// A map from `key` to `self`.
    pub fn map_from(self, key: Type) -> Type {
        Type::Map {
            key: Box::new(key),
            value: Box::new(self),
        }
    }

    /// A reference to a name in the enclosing universe.
    pub fn local_ref(name: &str) -> Type {
        Type::Ref(TypeRef::local(Ident::new_unchecked(name)))
    }

    /// A string literal type.
    pub fn lit_str(s: &str) -> Type {
        Type::Literal {
            value: LiteralValue::Str(s.to_string()),
        }
    }

    /// The discriminator token used in the wire form, for diagnostics.
    pub fn kind_name(&self) -> &'static str {
        match self {
            Type::Never => "never",
            Type::Any => "any",
            Type::Unit => "unit",
            Type::Bool => "bool",
            Type::I32 => "i32",
            Type::I64 => "i64",
            Type::F64 => "f64",
            Type::String => "string",
            Type::Bytes => "bytes",
            Type::Literal { .. } => "literal",
            Type::Optional { .. } => "optional",
            Type::List { .. } => "list",
            Type::Map { .. } => "map",
            Type::Tuple { .. } => "tuple",
            Type::Record { .. } => "record",
            Type::Union { .. } => "union",
            Type::Function { .. } => "function",
            Type::Future { .. } => "future",
            Type::Ref(_) => "ref",
            Type::Param { .. } => "param",
        }
    }

    /// True for the primitive and lattice-extreme types, which have no children.
    pub fn is_leaf(&self) -> bool {
        matches!(
            self,
            Type::Never
                | Type::Any
                | Type::Unit
                | Type::Bool
                | Type::I32
                | Type::I64
                | Type::F64
                | Type::String
                | Type::Bytes
                | Type::Literal { .. }
                | Type::Param { .. }
        )
    }

    /// Whether this type is legal as a [`Type::Map`] key.
    ///
    /// Restricted so that a map is representable as a JSON object in every
    /// target language.
    pub fn is_valid_map_key(&self) -> bool {
        match self {
            Type::String | Type::I32 | Type::I64 => true,
            Type::Literal { value } => !matches!(value, LiteralValue::Bool(_)),
            Type::Union { members } => members.iter().all(Type::is_valid_map_key),
            _ => false,
        }
    }

    /// Visits every immediate child type.
    pub fn children(&self) -> Vec<&Type> {
        match self {
            t if t.is_leaf() => Vec::new(),
            Type::Optional { of } | Type::List { of } | Type::Future { of } => vec![of.as_ref()],
            Type::Map { key, value } => vec![key.as_ref(), value.as_ref()],
            Type::Tuple { items } | Type::Union { members: items } => items.iter().collect(),
            Type::Record { fields } => fields.iter().map(|f| &f.ty).collect(),
            Type::Function { params, ret } => {
                params.iter().chain(std::iter::once(ret.as_ref())).collect()
            }
            Type::Ref(r) => r.args.iter().collect(),
            _ => Vec::new(),
        }
    }

    /// Rebuilds this node with `children` in place of its current children,
    /// preserving everything else. The slice must have the length that
    /// [`Type::children`] returns.
    pub(crate) fn with_children(&self, mut children: Vec<Type>) -> Type {
        debug_assert_eq!(children.len(), self.children().len());
        match self {
            t if t.is_leaf() => self.clone(),
            Type::Optional { .. } => Type::Optional {
                of: Box::new(children.remove(0)),
            },
            Type::List { .. } => Type::List {
                of: Box::new(children.remove(0)),
            },
            Type::Future { .. } => Type::Future {
                of: Box::new(children.remove(0)),
            },
            Type::Map { .. } => {
                let value = children.pop().expect("map has two children");
                let key = children.pop().expect("map has two children");
                Type::Map {
                    key: Box::new(key),
                    value: Box::new(value),
                }
            }
            Type::Tuple { .. } => Type::Tuple { items: children },
            Type::Union { .. } => Type::Union { members: children },
            Type::Record { fields } => Type::Record {
                fields: fields
                    .iter()
                    .zip(children)
                    .map(|(f, ty)| Field {
                        name: f.name.clone(),
                        ty,
                        doc: f.doc.clone(),
                    })
                    .collect(),
            },
            Type::Function { .. } => {
                let ret = children.pop().expect("function has a return type");
                Type::Function {
                    params: children,
                    ret: Box::new(ret),
                }
            }
            Type::Ref(r) => Type::Ref(TypeRef {
                universe: r.universe.clone(),
                name: r.name.clone(),
                args: children,
            }),
            _ => self.clone(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn types_round_trip_through_json() {
        let t = Type::Record {
            fields: vec![
                Field::new(Ident::new("id").unwrap(), Type::I64),
                Field::new(Ident::new("tags").unwrap(), Type::String.list().optional()),
            ],
        };
        let j = serde_json::to_string(&t).unwrap();
        assert_eq!(serde_json::from_str::<Type>(&j).unwrap(), t);
    }

    #[test]
    fn leaf_types_carry_only_a_discriminator() {
        assert_eq!(serde_json::to_string(&Type::I64).unwrap(), r#"{"k":"i64"}"#);
        assert_eq!(
            serde_json::to_string(&Type::Never).unwrap(),
            r#"{"k":"never"}"#
        );
    }

    #[test]
    fn refs_omit_absent_universe_and_empty_args() {
        let t = Type::local_ref("User");
        assert_eq!(
            serde_json::to_string(&t).unwrap(),
            r#"{"k":"ref","name":"User"}"#
        );
    }

    #[test]
    fn rebuilding_a_node_from_its_children_is_the_identity() {
        let cases = vec![
            Type::I64.list().optional(),
            Type::Map {
                key: Box::new(Type::String),
                value: Box::new(Type::Bool),
            },
            Type::Function {
                params: vec![Type::I64, Type::String],
                ret: Box::new(Type::Bool.future()),
            },
            Type::Record {
                fields: vec![Field::new(Ident::new("a").unwrap(), Type::F64)],
            },
            Type::Tuple {
                items: vec![Type::I32, Type::Bytes],
            },
        ];
        for t in cases {
            let kids = t.children().into_iter().cloned().collect();
            assert_eq!(t.with_children(kids), t, "rebuilding {t:?} changed it");
        }
    }

    #[test]
    fn the_integers_a_float_holds_exactly_stop_at_two_to_the_53() {
        assert!(fits_f64_exactly(0));
        assert!(fits_f64_exactly(9_007_199_254_740_992));
        assert!(fits_f64_exactly(-9_007_199_254_740_992));
        assert!(!fits_f64_exactly(9_007_199_254_740_993));
        assert!(
            !fits_f64_exactly(i64::MAX),
            "a round-trip cast saturates here, which is why it cannot be the test"
        );
    }

    #[test]
    fn map_keys_are_restricted_to_object_representable_types() {
        assert!(Type::String.is_valid_map_key());
        assert!(Type::I64.is_valid_map_key());
        assert!(!Type::F64.is_valid_map_key());
        assert!(!Type::Bool.is_valid_map_key());
        assert!(!Type::I64.list().is_valid_map_key());
    }
}
