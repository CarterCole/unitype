//! Links: the declared correspondences that let data cross between universes.
//!
//! Subtyping deliberately stops at a universe boundary, so nothing flows
//! between two universes by accident. A link is the explicit permission, and it
//! always says how the conversion is witnessed — by structural identity, by a
//! field mapping, or by a function that someone had to write.

use serde::{Deserialize, Serialize};

use crate::ident::{Ident, QualifiedName};
use crate::ir::Functor;
use crate::meta::Metadata;

/// How one field's value becomes another's.
///
/// Conversions nest, because the types do: turning a `[i32]` into an
/// `[i64]?` is widening, applied to each element, of a value that then becomes
/// optional. Each case that wraps a type carries the conversion for what it
/// wraps, so a backend can emit the whole thing by walking this structure.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "k", rename_all = "snake_case")]
pub enum Coercion {
    /// Nothing to do; the value passes through.
    Identity,
    /// A 32-bit integer widens, losing nothing.
    WidenInt,
    /// A present value becomes an optional one.
    WrapOptional {
        /// How the value itself converts first.
        of: Box<Coercion>,
    },
    /// Whatever is inside a constructor is converted.
    ///
    /// One case for every container: elements of a list, the value of an
    /// optional if it is there, the result of a future when it arrives.
    Through {
        /// The constructor being mapped through.
        functor: Functor,
        /// The conversion applied to what is inside.
        of: Box<Coercion>,
    },
    /// The field's own type is bridged by another link.
    ViaLink {
        /// The link that converts it.
        link: Ident,
        /// True to run that link backwards, which only an equivalence allows.
        #[serde(default, skip_serializing_if = "is_false")]
        reversed: bool,
    },
}

/// Nothing to do, which is what a pairing in an equivalence starts out as
/// until the conversion is worked out.
impl Default for Coercion {
    fn default() -> Self {
        Coercion::Identity
    }
}

impl Coercion {
    /// The conversion that undoes this one, if there is one.
    ///
    /// Widening and wrapping in an optional have none: an `i64` does not fit
    /// back into an `i32`, and an absent value has nothing to unwrap. Every
    /// other step undoes, as long as whatever it contains does.
    pub fn inverse(&self) -> Option<Coercion> {
        match self {
            Coercion::Identity => Some(Coercion::Identity),
            Coercion::WidenInt | Coercion::WrapOptional { .. } => None,
            Coercion::Through { functor, of } => Some(Coercion::Through {
                functor: *functor,
                of: Box::new(of.inverse()?),
            }),
            Coercion::ViaLink { link, reversed } => Some(Coercion::ViaLink {
                link: link.clone(),
                reversed: !reversed,
            }),
        }
    }

    /// Wraps `inner` in this constructor, collapsing the case where there was
    /// nothing to do in the first place.
    fn nested(inner: Coercion, wrap: impl FnOnce(Box<Coercion>) -> Coercion) -> Coercion {
        match inner {
            Coercion::Identity => Coercion::Identity,
            other => wrap(Box::new(other)),
        }
    }

    /// A conversion applied inside a constructor.
    pub(crate) fn through(functor: Functor, inner: Coercion) -> Coercion {
        Self::nested(inner, |of| Coercion::Through { functor, of })
    }

    /// True when the value passes through untouched.
    pub fn is_identity(&self) -> bool {
        matches!(self, Coercion::Identity)
    }
}

/// One field correspondence within a [`LinkKind::FieldMap`] or a
/// [`LinkKind::Equivalent`].
///
/// In an equivalence between two signatures, the "fields" are parameters.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct FieldPair {
    /// The source field.
    pub from: Ident,
    /// The target field it feeds.
    pub to: Ident,
    /// How the value is converted on the way.
    ///
    /// May be left out on input, where it means nothing to do. An
    /// equivalence works its conversions out regardless, so there a pair only
    /// has to say which name goes with which.
    #[serde(default)]
    pub coercion: Coercion,
}

/// One generic parameter of a signature paired with one of another's, in a
/// [`LinkKind::Equivalent`] between two generic signatures.
///
/// `first<T>` and `head<E>` are one proposition only once `T` is `E`. Nothing
/// is converted, so there is only which goes with which to say.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct GenericPair {
    /// The source signature's generic parameter.
    pub from: Ident,
    /// The target signature's generic parameter it stands for.
    pub to: Ident,
}

/// What justifies a link.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "k", rename_all = "snake_case")]
pub enum LinkKind {
    /// The two types have the same structure, so the conversion is a cast.
    Identical,
    /// The conversion is a mechanical rearrangement of fields, which every
    /// backend can generate rather than anyone having to write it.
    FieldMap {
        /// The field correspondences.
        pairs: Vec<FieldPair>,
        /// Source fields with no target, dropped by the conversion. A
        /// non-empty list means the bridge loses information.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        dropped: Vec<Ident>,
    },
    /// The conversion is arbitrary, and this signature is the obligation
    /// someone discharged by implementing it.
    Witness {
        /// A signature of type `fn(From) -> To` or `fn(From) -> Future<To>`.
        sig: QualifiedName,
    },
    /// The two are the same thing up to what each part is called and where it
    /// sits: every field on one side pairs with exactly one on the other, and
    /// each pair holds the same data or converts through another equivalence.
    ///
    /// Nothing is dropped, defaulted, or widened, so the conversion undoes
    /// exactly, and one link holds in both directions.
    ///
    /// Between two signatures it says they are one proposition with their
    /// parameters renamed and reordered, and a proof of either answers a call
    /// to the other.
    Equivalent {
        /// The correspondences. Any left out are worked out when the link is
        /// checked — by type where only one pairing fits, then by name, then by
        /// position — and the checked link carries the whole pairing.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        pairs: Vec<FieldPair>,
        /// Between two generic signatures, which generic parameter is which.
        /// Any left out are worked out along with the parameters they appear
        /// in, and the checked link carries them all.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        generics: Vec<GenericPair>,
    },
}

/// A declared correspondence between a type in one universe and a type in
/// another — or, for an equivalence, between two signatures.
///
/// Links are directed. A round trip is two links, which keeps a lossy
/// conversion from masquerading as a reversible one. The exception is an
/// equivalence, which cannot lose anything and so is one link both ways.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Link {
    /// A workspace-unique name for this link.
    pub name: Ident,
    /// Prose documentation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub doc: Option<String>,
    /// The source type.
    pub from: QualifiedName,
    /// The target type.
    pub to: QualifiedName,
    /// What justifies the conversion.
    pub kind: LinkKind,
    /// True when this link was derived by the system rather than declared by a
    /// person. Derived links are re-derived and re-checked on every load.
    #[serde(default, skip_serializing_if = "is_false")]
    pub derived: bool,
    /// Non-identifying annotations.
    #[serde(default, skip_serializing_if = "Metadata::is_empty")]
    pub meta: Metadata,
}

fn is_false(b: &bool) -> bool {
    !*b
}

impl Link {
    /// True when this link discards source fields.
    pub fn is_lossy(&self) -> bool {
        matches!(&self.kind, LinkKind::FieldMap { dropped, .. } if !dropped.is_empty())
    }

    /// True when this link may be followed backwards as well as forwards.
    pub fn is_reversible(&self) -> bool {
        matches!(self.kind, LinkKind::Equivalent { .. })
    }

    /// The same link, followed backwards, if it may be.
    ///
    /// Its name stays the same, since it is the same claim. Each pair is turned
    /// around, and so is each conversion in it.
    pub fn reversed(&self) -> Option<Link> {
        let LinkKind::Equivalent { pairs, generics } = &self.kind else {
            return None;
        };
        let pairs = pairs
            .iter()
            .map(|p| {
                Some(FieldPair {
                    from: p.to.clone(),
                    to: p.from.clone(),
                    coercion: p.coercion.inverse()?,
                })
            })
            .collect::<Option<Vec<_>>>()?;
        Some(Link {
            name: self.name.clone(),
            doc: self.doc.clone(),
            from: self.to.clone(),
            to: self.from.clone(),
            kind: LinkKind::Equivalent {
                pairs,
                generics: generics
                    .iter()
                    .map(|g| GenericPair {
                        from: g.to.clone(),
                        to: g.from.clone(),
                    })
                    .collect(),
            },
            derived: self.derived,
            meta: self.meta.clone(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn links_round_trip_through_json() {
        let l = Link {
            name: Ident::new("point_to_vec2").unwrap(),
            doc: None,
            from: "geo::Point".parse().unwrap(),
            to: "gfx::Vec2".parse().unwrap(),
            kind: LinkKind::Identical,
            derived: true,
            meta: Metadata::default(),
        };
        let j = serde_json::to_string(&l).unwrap();
        assert_eq!(serde_json::from_str::<Link>(&j).unwrap(), l);
    }

    #[test]
    fn a_bridge_that_drops_fields_reports_itself_as_lossy() {
        let dropping = LinkKind::FieldMap {
            pairs: vec![],
            dropped: vec![Ident::new("secret").unwrap()],
        };
        let total = LinkKind::FieldMap {
            pairs: vec![],
            dropped: vec![],
        };
        let mk = |kind| Link {
            name: Ident::new("l").unwrap(),
            doc: None,
            from: "a::X".parse().unwrap(),
            to: "b::Y".parse().unwrap(),
            kind,
            derived: false,
            meta: Metadata::default(),
        };
        assert!(mk(dropping).is_lossy());
        assert!(!mk(total).is_lossy());
    }

    #[test]
    fn an_equivalence_followed_backwards_turns_its_generic_pairs_around_too() {
        let id = |s: &str| Ident::new(s).unwrap();
        let link = Link {
            name: id("first_is_head"),
            doc: None,
            from: "a::first".parse().unwrap(),
            to: "b::head".parse().unwrap(),
            kind: LinkKind::Equivalent {
                pairs: vec![FieldPair {
                    from: id("xs"),
                    to: id("items"),
                    coercion: Coercion::Identity,
                }],
                generics: vec![GenericPair {
                    from: id("T"),
                    to: id("E"),
                }],
            },
            derived: false,
            meta: Metadata::default(),
        };
        let back = link.reversed().expect("an equivalence goes both ways");
        let LinkKind::Equivalent { generics, .. } = &back.kind else {
            panic!("still an equivalence");
        };
        assert_eq!(
            generics,
            &[GenericPair {
                from: id("E"),
                to: id("T")
            }]
        );
        assert_eq!(back.reversed().as_ref(), Some(&link));
    }
}
