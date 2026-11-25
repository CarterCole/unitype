//! The symbolic type representation.
//!
//! Every node serializes as a JSON object carrying a `"k"` discriminator, so a
//! type is always a self-describing literal. See [`crate::canon`] for the
//! normalization rules that make one semantic type have exactly one such
//! serialization, and [`crate::ir::sugar`] for the shorthand accepted on input.

use std::fmt;

use serde::{Deserialize, Serialize};

use crate::ident::Ident;
use crate::ir::functor::Functor;

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

/// A range something must lie in, both ends inclusive and either end open.
///
/// One shape, used wherever a type bounds something: the value of an integer,
/// the number of items in a list, the bytes in a string. Written `0..255` as
/// shorthand, `4` when both ends are the same, `..8` and `1..` when one is
/// open.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Bounds {
    /// The least allowed, or no lower bound.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min: Option<i64>,
    /// The greatest allowed, or no upper bound.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max: Option<i64>,
}

impl Bounds {
    /// Between `min` and `max`, inclusive.
    pub fn between(min: i64, max: i64) -> Self {
        Self {
            min: Some(min),
            max: Some(max),
        }
    }

    /// Exactly `n`.
    pub fn exactly(n: i64) -> Self {
        Self::between(n, n)
    }

    /// No more than `n`.
    pub fn at_most(n: i64) -> Self {
        Self {
            min: None,
            max: Some(n),
        }
    }

    /// No fewer than `n`.
    pub fn at_least(n: i64) -> Self {
        Self {
            min: Some(n),
            max: None,
        }
    }

    /// True when neither end is given, which bounds nothing.
    pub fn is_open(&self) -> bool {
        self.min.is_none() && self.max.is_none()
    }

    /// True when the two ends are given the wrong way round, admitting nothing.
    pub fn is_empty(&self) -> bool {
        matches!((self.min, self.max), (Some(a), Some(b)) if a > b)
    }

    /// Whether `n` lies within.
    pub fn contains(&self, n: i64) -> bool {
        self.min.is_none_or(|a| a <= n) && self.max.is_none_or(|b| n <= b)
    }

    /// Whether everything within `self` is within `outer`.
    ///
    /// An open end of `self` fits only an open end of `outer`: there is no
    /// finite bound that admits every integer.
    pub fn within(&self, outer: &Bounds) -> bool {
        let low = match (outer.min, self.min) {
            (None, _) => true,
            (Some(_), None) => false,
            (Some(o), Some(s)) => o <= s,
        };
        let high = match (outer.max, self.max) {
            (None, _) => true,
            (Some(_), None) => false,
            (Some(o), Some(s)) => s <= o,
        };
        low && high
    }
}

impl fmt::Display for Bounds {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match (self.min, self.max) {
            (Some(a), Some(b)) if a == b => write!(f, "{a}"),
            (Some(a), Some(b)) => write!(f, "{a}..{b}"),
            (Some(a), None) => write!(f, "{a}.."),
            (None, Some(b)) => write!(f, "..{b}"),
            (None, None) => f.write_str(".."),
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
    /// A higher-order parameter applied to arguments: `M<A>`, where `M` stands
    /// for a unary type constructor such as a list.
    ///
    /// Only a parameter declared with an arity of one can be applied, and it is
    /// applied to exactly one argument. Substituting a [`Type::Constructor`] for
    /// `M` gives that constructor's application: `M<A>` with `M` bound to
    /// `list` is `[A]`. Until then the application is opaque, equal only to
    /// itself, as a bare parameter is.
    Apply {
        /// The higher-order parameter being applied.
        name: Ident,
        /// The arguments it is applied to.
        args: Vec<Type>,
    },
    /// A unary type constructor, passed as the argument of a higher-order
    /// parameter: `list` in `Monad<list>`.
    ///
    /// It is not a type that values have. It is only meaningful as the argument
    /// for a parameter of arity one, and is refused anywhere else.
    Constructor {
        /// Which constructor it is.
        functor: Functor,
    },
    /// A type with bounds on its values or its length.
    ///
    /// The values are the inner type's values that lie within the bounds, and
    /// so is the wire form: `i32[0..255]` is written as the integer it is. What
    /// may be bounded depends on the type. An integer's `value`; the `len` of a
    /// list, a map, or bytes, counting items, entries, or bytes; and a string's
    /// `len` in Unicode scalar values and `size` in UTF-8 bytes, which differ,
    /// and which each language's own `length` counts differently.
    Bounded {
        /// The type being bounded.
        of: Box<Type>,
        /// Where an integer's value must lie.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        value: Option<Bounds>,
        /// How many items, entries, bytes, or scalar values there must be.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        len: Option<Bounds>,
        /// How many bytes a string must take in UTF-8.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        size: Option<Bounds>,
        /// What a string must match in full, as a regular expression. A
        /// restriction class for strings: `str[match "[a-z]+"]`.
        #[serde(default, rename = "match", skip_serializing_if = "Option::is_none")]
        pattern: Option<String>,
        /// The window in which a future must deliver its value, in
        /// milliseconds from the call that produced it: `future<T>[wait 100..5000]`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        wait: Option<Bounds>,
    },
}

/// The restrictions a bounded type carries, gathered while one is built.
#[derive(Default)]
struct Restrictions {
    value: Option<Bounds>,
    len: Option<Bounds>,
    size: Option<Bounds>,
    pattern: Option<String>,
    wait: Option<Bounds>,
}

impl fmt::Display for Type {
    /// Writes a type the way a person would write it.
    ///
    /// The same shorthand the reader accepts, so that what an error message
    /// shows can be pasted straight back into a universe. Canonical JSON is for
    /// hashing and for machines; this is for reading.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Type::Never => f.write_str("never"),
            Type::Any => f.write_str("any"),
            Type::Unit => f.write_str("unit"),
            Type::Bool => f.write_str("bool"),
            Type::I32 => f.write_str("i32"),
            Type::I64 => f.write_str("i64"),
            Type::F64 => f.write_str("f64"),
            Type::String => f.write_str("str"),
            Type::Bytes => f.write_str("bytes"),

            Type::Literal { value } => match value {
                LiteralValue::Bool(b) => write!(f, "{b}"),
                LiteralValue::Int(i) => write!(f, "{i}"),
                LiteralValue::Str(s) => write!(f, "{s:?}"),
            },

            // A union or a function has to be bracketed here, or `A | B?` would
            // read as a union with an optional rather than an optional union.
            Type::Optional { of } => match of.as_ref() {
                loose @ (Type::Union { .. } | Type::Function { .. }) => write!(f, "({loose})?"),
                tight => write!(f, "{tight}?"),
            },
            Type::List { of } => write!(f, "[{of}]"),
            Type::Future { of } => write!(f, "future<{of}>"),
            Type::Map { key, value } => write!(f, "map<{key}, {value}>"),

            Type::Tuple { items } => {
                write!(f, "({})", join(items, ", "))
            }
            Type::Union { members } => f.write_str(&join(members, " | ")),
            Type::Record { fields } => {
                let inner: Vec<String> = fields
                    .iter()
                    .map(|field| format!("{}: {}", field.name, field.ty))
                    .collect();
                write!(f, "{{{}}}", inner.join(", "))
            }
            Type::Function { params, ret } => {
                write!(f, "({}) -> {ret}", join(params, ", "))
            }

            Type::Ref(r) => write!(f, "{r}"),
            Type::Param { name } => write!(f, "{name}"),
            Type::Apply { name, args } => write!(f, "{name}<{}>", join(args, ", ")),
            Type::Constructor { functor } => f.write_str(match functor {
                Functor::List => "list",
                Functor::Optional => "optional",
                Functor::Future => "future",
            }),

            // The bounds go where the thing they bound is said: a list's length
            // inside its brackets, an integer's range after it, and a string's
            // two lengths each under its own word.
            Type::Bounded {
                of,
                value,
                len,
                size,
                pattern,
                wait,
            } => match of.as_ref() {
                Type::List { of: item } => match len {
                    Some(len) => write!(f, "[{item}; {len}]"),
                    None => write!(f, "[{item}]"),
                },
                Type::String => {
                    let mut parts = Vec::new();
                    if let Some(len) = len {
                        parts.push(format!("len {len}"));
                    }
                    if let Some(size) = size {
                        parts.push(format!("size {size}"));
                    }
                    if let Some(pattern) = pattern {
                        // Quoted the way JSON quotes, so a pattern with a
                        // comma or a bracket reads back as one value.
                        let quoted = serde_json::to_string(pattern).unwrap_or_default();
                        parts.push(format!("match {quoted}"));
                    }
                    if parts.is_empty() {
                        write!(f, "{of}")
                    } else {
                        write!(f, "{of}[{}]", parts.join(", "))
                    }
                }
                Type::Future { of: item } => match wait {
                    Some(window) => write!(f, "future<{item}>[wait {window}]"),
                    None => write!(f, "{of}"),
                },
                inner => match value.as_ref().or(len.as_ref()) {
                    Some(bounds) => write!(f, "{inner}[{bounds}]"),
                    None => write!(f, "{inner}"),
                },
            },
        }
    }
}

/// A reference, written the way it was written.
///
/// Unqualified when it names something in the universe it sits in, which is
/// how the declaration itself reads.
impl fmt::Display for TypeRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if let Some(universe) = &self.universe {
            write!(f, "{universe}::")?;
        }
        write!(f, "{}{}", self.name, args(&self.args))
    }
}

fn join(types: &[Type], separator: &str) -> String {
    types
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join(separator)
}

fn args(applied: &[Type]) -> String {
    if applied.is_empty() {
        String::new()
    } else {
        format!("<{}>", join(applied, ", "))
    }
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

    /// `self` with its value bounded: `i32[0..255]`.
    ///
    /// On a type that is already bounded, this sets the value bounds and keeps
    /// the rest, so bounds of different kinds build up one node.
    pub fn valued(self, bounds: Bounds) -> Type {
        self.bounded(|r| r.value = Some(bounds))
    }

    /// `self` with its length bounded: `[T; 4]`, `bytes[16]`, `str[len 1..63]`.
    pub fn counted(self, bounds: Bounds) -> Type {
        self.bounded(|r| r.len = Some(bounds))
    }

    /// `self` with its size in UTF-8 bytes bounded: `str[size ..253]`.
    pub fn sized(self, bounds: Bounds) -> Type {
        self.bounded(|r| r.size = Some(bounds))
    }

    /// `self`, a string, restricted to the strings `pattern` matches in full:
    /// `str[match "[a-z]+"]`.
    pub fn matching(self, pattern: impl Into<String>) -> Type {
        let pattern = pattern.into();
        self.bounded(|r| r.pattern = Some(pattern))
    }

    /// `self`, a future, with the window its value must arrive in:
    /// `future<T>[wait 100..5000]`.
    pub fn waiting(self, window: Bounds) -> Type {
        self.bounded(|r| r.wait = Some(window))
    }

    /// What a future of this type holds: `T` for `future<T>`, and the same for
    /// a future restricted to a window, which is still a future. `None` for
    /// anything that is not one.
    pub fn future_of(&self) -> Option<&Type> {
        match self.unbounded() {
            Type::Future { of } => Some(of),
            _ => None,
        }
    }

    fn bounded(self, set: impl FnOnce(&mut Restrictions)) -> Type {
        let (of, mut r) = match self {
            Type::Bounded {
                of,
                value,
                len,
                size,
                pattern,
                wait,
            } => (
                of,
                Restrictions {
                    value,
                    len,
                    size,
                    pattern,
                    wait,
                },
            ),
            other => (Box::new(other), Restrictions::default()),
        };
        set(&mut r);
        Type::Bounded {
            of,
            value: r.value,
            len: r.len,
            size: r.size,
            pattern: r.pattern,
            wait: r.wait,
        }
    }

    /// The type with any bounds taken off: what a bounded type is a type *of*.
    ///
    /// Anything that asks what kind of thing a type is — an integer, a list —
    /// asks this first, since the bounds do not change the answer.
    pub fn unbounded(&self) -> &Type {
        match self {
            Type::Bounded { of, .. } => of,
            other => other,
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
            Type::Apply { .. } => "apply",
            Type::Constructor { .. } => "constructor",
            Type::Bounded { .. } => "bounded",
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
                | Type::Constructor { .. }
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
            // A bounded key is still text or a number on the wire.
            Type::Bounded { of, .. } => of.is_valid_map_key(),
            _ => false,
        }
    }

    /// Visits every immediate child type.
    pub fn children(&self) -> Vec<&Type> {
        match self {
            t if t.is_leaf() => Vec::new(),
            Type::Optional { of }
            | Type::List { of }
            | Type::Future { of }
            | Type::Bounded { of, .. } => vec![of.as_ref()],
            Type::Map { key, value } => vec![key.as_ref(), value.as_ref()],
            Type::Tuple { items } | Type::Union { members: items } => items.iter().collect(),
            Type::Record { fields } => fields.iter().map(|f| &f.ty).collect(),
            Type::Function { params, ret } => {
                params.iter().chain(std::iter::once(ret.as_ref())).collect()
            }
            Type::Ref(r) => r.args.iter().collect(),
            Type::Apply { args, .. } => args.iter().collect(),
            _ => Vec::new(),
        }
    }

    /// Rebuilds this node with `children` in place of its current children,
    /// preserving everything else. The slice must have the length that
    /// [`Type::children`] returns.
    pub fn with_children(&self, mut children: Vec<Type>) -> Type {
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
            Type::Bounded {
                value,
                len,
                size,
                pattern,
                wait,
                ..
            } => Type::Bounded {
                of: Box::new(children.remove(0)),
                value: *value,
                len: *len,
                size: *size,
                pattern: pattern.clone(),
                wait: *wait,
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
            Type::Apply { name, .. } => Type::Apply {
                name: name.clone(),
                args: children,
            },
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
    fn bounds_print_the_way_they_are_said() {
        assert_eq!(Bounds::between(0, 255).to_string(), "0..255");
        assert_eq!(Bounds::exactly(4).to_string(), "4");
        assert_eq!(Bounds::at_most(8).to_string(), "..8");
        assert_eq!(Bounds::at_least(1).to_string(), "1..");
    }

    #[test]
    fn bounds_nest_only_when_every_end_fits() {
        let octet = Bounds::between(0, 255);
        assert!(Bounds::between(1, 10).within(&octet));
        assert!(!Bounds::between(-1, 10).within(&octet));
        assert!(
            !Bounds::at_most(10).within(&octet),
            "an open end fits only an open end"
        );
        assert!(Bounds::at_most(10).within(&Bounds::at_most(255)));
        assert!(octet.contains(0) && octet.contains(255) && !octet.contains(256));
        assert!(Bounds::between(5, 1).is_empty());
    }

    #[test]
    fn a_bounded_type_prints_its_bounds_where_they_belong() {
        let octet = Type::I32.valued(Bounds::between(0, 255));
        assert_eq!(octet.to_string(), "i32[0..255]");
        assert_eq!(
            octet.clone().list().counted(Bounds::exactly(4)).to_string(),
            "[i32[0..255]; 4]"
        );
        assert_eq!(
            Type::Bytes.counted(Bounds::exactly(16)).to_string(),
            "bytes[16]"
        );
        assert_eq!(
            Type::String
                .counted(Bounds::between(1, 63))
                .sized(Bounds::at_most(253))
                .to_string(),
            "str[len 1..63, size ..253]"
        );
        assert_eq!(
            Type::I64
                .map_from(Type::String)
                .counted(Bounds::at_most(10))
                .to_string(),
            "map<str, i64>[..10]"
        );
        assert_eq!(octet.optional().to_string(), "i32[0..255]?");
    }

    #[test]
    fn bounds_build_up_one_node_and_serialize_beside_the_type() {
        let t = Type::String
            .counted(Bounds::at_least(1))
            .sized(Bounds::at_most(253));
        assert!(matches!(
            &t,
            Type::Bounded {
                value: None,
                len: Some(_),
                size: Some(_),
                ..
            }
        ));
        let j = serde_json::to_string(&t).unwrap();
        assert_eq!(
            j,
            r#"{"k":"bounded","of":{"k":"string"},"len":{"min":1},"size":{"max":253}}"#
        );
        assert_eq!(serde_json::from_str::<Type>(&j).unwrap(), t);
        assert_eq!(t.unbounded(), &Type::String);
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
    fn a_type_reads_the_way_it_would_be_written() {
        assert_eq!(Type::I64.to_string(), "i64");
        assert_eq!(Type::String.list().to_string(), "[str]");
        assert_eq!(Type::String.optional().to_string(), "str?");
        assert_eq!(Type::String.optional().list().to_string(), "[str?]");
        assert_eq!(Type::String.list().optional().to_string(), "[str]?");
        assert_eq!(Type::Bool.future().to_string(), "future<bool>");
        assert_eq!(
            Type::I64.map_from(Type::String).to_string(),
            "map<str, i64>"
        );
        assert_eq!(Type::lit_str("circle").to_string(), "\"circle\"");
        assert_eq!(
            Type::Ref(TypeRef::qualified(
                Ident::new("demo").unwrap(),
                Ident::new("Circle").unwrap()
            ))
            .to_string(),
            "demo::Circle"
        );
    }

    #[test]
    fn something_loose_is_bracketed_where_it_has_to_be() {
        let either = Type::Union {
            members: vec![Type::I64, Type::String],
        };
        assert_eq!(either.to_string(), "i64 | str");
        assert_eq!(
            either.optional().to_string(),
            "(i64 | str)?",
            "without the brackets this would read as a union with an optional"
        );
    }

    #[test]
    fn a_function_reads_as_an_arrow() {
        let f = Type::Function {
            params: vec![Type::I64.list()],
            ret: Box::new(Type::Bool),
        };
        assert_eq!(f.to_string(), "([i64]) -> bool");
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
