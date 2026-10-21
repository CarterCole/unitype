//! Working out how one universe's type becomes another's.
//!
//! Two teams describe the same thing and give it different names, different
//! field spellings, and slightly different types. Derivation lines the two up:
//! it pairs fields, works out what conversion each pair needs, and either
//! produces a mapping the system can carry out or says precisely which field it
//! could not account for.
//!
//! Field pairing tries three things in order — a correspondence someone
//! declared, an exact name match, then a match on the folded name, so that
//! `userId` and `user_id` find each other across a language boundary. Anything
//! ambiguous is refused rather than guessed at.
//!
//! # Equivalences
//!
//! When the two hold the same types as a bag — the same data under some
//! renaming and reordering — derivation looks for that renaming before
//! settling for a field map, since an equivalence is the stronger claim and
//! holds both ways. Here the types lead: a field whose type only one field on
//! the other side shares has nowhere else to go, so `request(url: str, cb: fn)`
//! and `request(callback: fn, getUrl: str)` pair up without either name being
//! consulted. Names settle what types cannot, then folded names, and when two
//! fields of one type are still left, their declared order does — which is the
//! positional cast `{x: f64, y: f64}` to `{lat: f64, long: f64}`. Whatever even
//! position leaves open is refused.
//!
//! Two generic signatures pair their generic parameters too, since `first<T>`
//! and `head<E>` hold the same data only once `T` is `E`. Each way of pairing
//! them is tried, the parameters are paired under it, and of the ways that
//! work, the one kept is the one under which the most parameters pair by
//! name: a signature that is the same whichever way round its generic
//! parameters go leaves only names to say which is meant.

use std::collections::BTreeSet;

use crate::canon::TypeId;
use crate::check::{CheckedWorkspace, numberings};
use crate::ident::{Ident, QualifiedName};
use crate::ir::{Field, FunctionSig, Functor, Type, TypeParamDecl, TypeRef};
use crate::link::{Coercion, FieldPair, GenericPair, LinkKind};

/// Which part of one side of an equivalence is which on the other.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Pairing {
    /// Each field or parameter with its partner, in the target's order.
    pub pairs: Vec<FieldPair>,
    /// Each generic parameter with its partner, in the target's order. Empty
    /// unless both ends are generic signatures.
    pub generics: Vec<GenericPair>,
}

/// A field the derivation could not account for.
#[derive(thiserror::Error, Debug, Clone, PartialEq, Eq)]
pub enum BridgeMismatch {
    /// An endpoint is not something with fields.
    #[error("`{0}` has no fields, so there is nothing to map")]
    NotAStruct(QualifiedName),
    /// The endpoints are in the same universe.
    #[error("`{from}` and `{to}` are in the same universe; a link crosses between universes")]
    SameUniverse {
        /// The source.
        from: QualifiedName,
        /// The target.
        to: QualifiedName,
    },
    /// An endpoint takes generic arguments.
    #[error("`{0}` is generic; linking generic declarations is not supported yet")]
    Generic(QualifiedName),
    /// A target field had nothing to fill it.
    #[error("nothing supplies `{field}`, and it cannot be left out")]
    Unfilled {
        /// The field with no source.
        field: Ident,
    },
    /// A pair of fields had no usable conversion.
    #[error(
        "`{from}` is `{found}` but `{to}` needs `{required}`, and nothing converts between them"
    )]
    NoConversion {
        /// The source field.
        from: Ident,
        /// The target field.
        to: Ident,
        /// The source type.
        found: String,
        /// The target type.
        required: String,
    },
    /// A name matched more than one field.
    #[error("`{field}` could pair with more than one field ({}), so it is ambiguous", .candidates.join(", "))]
    Ambiguous {
        /// The field being paired.
        field: Ident,
        /// What it could have paired with.
        candidates: Vec<String>,
    },
    /// A declared pairing named a field neither side has.
    #[error("no such field `{field}` on either end")]
    NoSuchField {
        /// The name that matched nothing.
        field: Ident,
    },
    /// A declared pairing used a field twice.
    #[error("`{field}` is paired more than once")]
    PairedTwice {
        /// The field claimed twice.
        field: Ident,
    },
    /// A source field went nowhere, which an equivalence cannot allow.
    #[error("`{field}` has nowhere to go, and an equivalence drops nothing")]
    Unused {
        /// The field left over.
        field: Ident,
    },
    /// One side is a signature and the other is not.
    #[error("`{from}` and `{to}` are not both types or both signatures")]
    MixedEndpoints {
        /// The source.
        from: QualifiedName,
        /// The target.
        to: QualifiedName,
    },
    /// Two signatures are generic over different numbers of types.
    #[error("`{from}` is generic over {from_count} type(s) and `{to}` over {to_count}")]
    GenericArity {
        /// The source signature.
        from: QualifiedName,
        /// The target signature.
        to: QualifiedName,
        /// How many generic parameters the source has.
        from_count: usize,
        /// How many the target has.
        to_count: usize,
    },
    /// A declared pairing named a generic parameter neither side has.
    #[error("no such generic parameter `{name}` on either end")]
    NoSuchGeneric {
        /// The name that matched nothing.
        name: Ident,
    },
    /// Two generic parameters paired with each other are bounded differently.
    #[error("`{from}` and `{to}` are not bounded by the same things")]
    BoundsDiffer {
        /// The source signature's generic parameter.
        from: Ident,
        /// The target signature's.
        to: Ident,
    },
    /// No pairing of two signatures' generic parameters makes them one
    /// proposition.
    #[error(
        "no way of pairing the generic parameters of `{from}` with those of `{to}` makes them one proposition"
    )]
    NoGenericPairing {
        /// The source signature.
        from: QualifiedName,
        /// The target signature.
        to: QualifiedName,
    },
}

impl CheckedWorkspace {
    /// Works out how values of `from` become values of `to`.
    ///
    /// `renames` states correspondences that name-matching would not find, and
    /// takes precedence over it.
    pub fn derive_link(
        &self,
        from: &QualifiedName,
        to: &QualifiedName,
        renames: &[(Ident, Ident)],
    ) -> Result<LinkKind, Vec<BridgeMismatch>> {
        if from.universe == to.universe {
            return Err(vec![BridgeMismatch::SameUniverse {
                from: from.clone(),
                to: to.clone(),
            }]);
        }

        let mut problems = Vec::new();
        for name in [from, to] {
            if self.def(name).is_some_and(|d| !d.params().is_empty()) {
                problems.push(BridgeMismatch::Generic(name.clone()));
            }
        }
        if !problems.is_empty() {
            return Err(problems);
        }

        let (Some(source), Some(target)) = (self.fields_by_name(from), self.fields_by_name(to))
        else {
            return Err([from, to]
                .into_iter()
                .filter(|n| self.fields_by_name(n).is_none())
                .map(|n| BridgeMismatch::NotAStruct(n.clone()))
                .collect());
        };

        // The same structure needs no mapping at all.
        if self.shape_hash(from) == self.shape_hash(to) && self.shape_hash(from).is_some() {
            return Ok(LinkKind::Identical);
        }

        // The same data under other names and in another order is a renaming,
        // which undoes, and is worth more than a mapping that only goes one way.
        let bag = |name| self.address(name, crate::check::View::Bag);
        if bag(from).is_some() && bag(from) == bag(to) {
            if let Ok(pairs) = self.pair_up(&source, &target, renames, &|have, want| {
                self.exact_coercion(have, want)
            }) {
                return Ok(LinkKind::Equivalent {
                    pairs,
                    generics: Vec::new(),
                });
            }
        }

        let mut pairs = Vec::new();
        let mut used: Vec<Ident> = Vec::new();

        for want in &target {
            let Some(have) = self.pair_for(want, &source, renames, &mut problems) else {
                // A target field nothing supplies is only acceptable if the
                // target can do without it.
                if !self.may_be_absent(&want.ty) {
                    problems.push(BridgeMismatch::Unfilled {
                        field: want.name.clone(),
                    });
                }
                continue;
            };

            match self.coercion_between(&have.ty, &want.ty) {
                Some(coercion) => {
                    used.push(have.name.clone());
                    pairs.push(crate::link::FieldPair {
                        from: have.name.clone(),
                        to: want.name.clone(),
                        coercion,
                    });
                }
                None => problems.push(BridgeMismatch::NoConversion {
                    from: have.name.clone(),
                    to: want.name.clone(),
                    found: crate::canon::canonical_string(&have.ty),
                    required: crate::canon::canonical_string(&want.ty),
                }),
            }
        }

        if !problems.is_empty() {
            return Err(problems);
        }

        // Whatever the source carries that the target has no room for is lost.
        // Not an error — but the link says so, so nobody is surprised later.
        let dropped: Vec<Ident> = source
            .iter()
            .filter(|f| !used.contains(&f.name))
            .map(|f| f.name.clone())
            .collect();

        Ok(LinkKind::FieldMap { pairs, dropped })
    }

    /// The source field that fills `want`, by declaration, by name, or by
    /// folded name.
    fn pair_for<'a>(
        &self,
        want: &Field,
        source: &'a [Field],
        renames: &[(Ident, Ident)],
        problems: &mut Vec<BridgeMismatch>,
    ) -> Option<&'a Field> {
        if let Some((from, _)) = renames.iter().find(|(_, to)| to == &want.name) {
            return source.iter().find(|f| &f.name == from);
        }
        // A field claimed by a declared rename belongs to that pairing, not to
        // whatever else happens to share its name.
        let spoken_for = |name: &Ident| renames.iter().any(|(from, _)| from == name);

        if let Some(exact) = source
            .iter()
            .find(|f| f.name == want.name && !spoken_for(&f.name))
        {
            return Some(exact);
        }

        let folded = want.name.fold();
        let candidates: Vec<&Field> = source
            .iter()
            .filter(|f| f.name.fold() == folded && !spoken_for(&f.name))
            .collect();
        match candidates.as_slice() {
            [only] => Some(only),
            [] => None,
            many => {
                problems.push(BridgeMismatch::Ambiguous {
                    field: want.name.clone(),
                    candidates: many.iter().map(|f| f.name.to_string()).collect(),
                });
                None
            }
        }
    }

    /// How a value of `have` becomes one of `want`, if it can.
    ///
    /// Worked out from the structure rather than from subtyping. Subtyping
    /// would say `[i32]` is already usable as `[i64]`, which is true of the
    /// data but no help to a backend that has to write the conversion out in a
    /// language where those are different types.
    fn coercion_between(&self, have: &Type, want: &Type) -> Option<Coercion> {
        let have = self.expand_alias(have);
        let want = self.expand_alias(want);

        if have == want {
            return Some(Coercion::Identity);
        }

        match (&have, &want) {
            (Type::I32, Type::I64 | Type::F64) => Some(Coercion::WidenInt),

            // Same constructor on both sides: convert what is inside it.
            (_, _) if Functor::of(&have).is_some() && Functor::of(&have) == Functor::of(&want) => {
                let (functor, a) = Functor::split(&have).expect("just checked");
                let (_, b) = Functor::split(&want).expect("just checked");
                Some(Coercion::through(functor, self.coercion_between(a, b)?))
            }
            // Absence on the way in is absence on the way out.
            (Type::Unit | Type::Never, Type::Optional { .. }) => Some(Coercion::Identity),
            // A present value can fill a slot that allows absence, once the
            // value itself converts.
            (_, Type::Optional { of }) => Some(Coercion::WrapOptional {
                of: Box::new(self.coercion_between(&have, of)?),
            }),

            (Type::Ref(a), Type::Ref(b)) => {
                let (from, to) = (self.ref_target(a)?, self.ref_target(b)?);
                // Inside one universe, inheritance already relates these.
                if from.universe == to.universe {
                    return self.is_subtype(&have, &want).then_some(Coercion::Identity);
                }
                // Across universes, only a link someone declared will do.
                self.link_between(&from, &to, false)
            }

            _ => self.is_subtype(&have, &want).then_some(Coercion::Identity),
        }
    }

    /// A declared link that converts `from` into `to`: one that runs that way,
    /// or an equivalence that runs the other way and so runs this way too.
    ///
    /// With `reversible_only`, only an equivalence will do, whichever way round
    /// it was declared — which is what a step inside another equivalence
    /// needs, since that step has to undo as well.
    fn link_between(
        &self,
        from: &QualifiedName,
        to: &QualifiedName,
        reversible_only: bool,
    ) -> Option<Coercion> {
        let links = &self.workspace().links;
        let forward = links
            .iter()
            .filter(|l| !reversible_only || l.is_reversible())
            .find(|l| &l.from == from && &l.to == to);
        if let Some(link) = forward {
            return Some(Coercion::ViaLink {
                link: link.name.clone(),
                reversed: false,
            });
        }
        links
            .iter()
            .filter(|l| l.is_reversible())
            .find(|l| &l.from == to && &l.to == from)
            .map(|link| Coercion::ViaLink {
                link: link.name.clone(),
                reversed: true,
            })
    }

    /// How a value of `have` becomes one of `want` and back again, if it can.
    ///
    /// Stricter than [`coercion_between`](Self::coercion_between): nothing is
    /// widened and nothing is wrapped, because neither undoes. What is left is
    /// the same type, a container of things that convert exactly, or another
    /// equivalence.
    pub(crate) fn exact_coercion(&self, have: &Type, want: &Type) -> Option<Coercion> {
        let have = self.expand_alias(have);
        let want = self.expand_alias(want);

        if have == want {
            return Some(Coercion::Identity);
        }

        match (&have, &want) {
            (_, _) if Functor::of(&have).is_some() && Functor::of(&have) == Functor::of(&want) => {
                let (functor, a) = Functor::split(&have).expect("just checked");
                let (_, b) = Functor::split(&want).expect("just checked");
                Some(Coercion::through(functor, self.exact_coercion(a, b)?))
            }
            (Type::Ref(a), Type::Ref(b)) if a.args.is_empty() && b.args.is_empty() => {
                let (from, to) = (self.ref_target(a)?, self.ref_target(b)?);
                if from.universe == to.universe {
                    return None;
                }
                self.link_between(&from, &to, true)
            }
            _ => None,
        }
    }

    /// Works out the equivalence between two types or two signatures: which
    /// field or parameter on one side is which on the other, and between two
    /// generic signatures, which generic parameter.
    ///
    /// `renames` pins down pairings that should not be worked out, and
    /// `generic_renames` does the same for generic parameters. Every part on
    /// each side ends up paired with exactly one on the other, or this says why
    /// it cannot.
    ///
    /// For types, each pair has to convert exactly — the same type, or through
    /// another equivalence. For signatures, each pair has to hold the same data,
    /// since a call's arguments are reordered but not converted.
    pub fn derive_equivalence(
        &self,
        from: &QualifiedName,
        to: &QualifiedName,
        renames: &[(Ident, Ident)],
        generic_renames: &[(Ident, Ident)],
    ) -> Result<Pairing, Vec<BridgeMismatch>> {
        if from.universe == to.universe {
            return Err(vec![BridgeMismatch::SameUniverse {
                from: from.clone(),
                to: to.clone(),
            }]);
        }

        let is_sig = |n: &QualifiedName| self.def(n).is_none() && self.sig(n).is_some();
        match (is_sig(from), is_sig(to)) {
            (true, true) => {
                let (a, b) = (
                    self.sig(from).expect("is one"),
                    self.sig(to).expect("is one"),
                );
                self.pair_signatures(from, to, a, b, renames, generic_renames)
            }
            (false, false) => {
                let generic: Vec<BridgeMismatch> = [from, to]
                    .into_iter()
                    .filter(|n| self.def(n).is_some_and(|d| !d.params().is_empty()))
                    .map(|n| BridgeMismatch::Generic(n.clone()))
                    .collect();
                if !generic.is_empty() {
                    return Err(generic);
                }
                // Neither end has a generic parameter for a declared pair to
                // name.
                if !generic_renames.is_empty() {
                    return Err(generic_renames
                        .iter()
                        .map(|(name, _)| BridgeMismatch::NoSuchGeneric { name: name.clone() })
                        .collect());
                }
                let (Some(source), Some(target)) =
                    (self.fields_by_name(from), self.fields_by_name(to))
                else {
                    return Err([from, to]
                        .into_iter()
                        .filter(|n| self.fields_by_name(n).is_none())
                        .map(|n| BridgeMismatch::NotAStruct(n.clone()))
                        .collect());
                };
                let pairs = self.pair_up(&source, &target, renames, &|have, want| {
                    self.exact_coercion(have, want)
                })?;
                Ok(Pairing {
                    pairs,
                    generics: Vec::new(),
                })
            }
            _ => Err(vec![BridgeMismatch::MixedEndpoints {
                from: from.clone(),
                to: to.clone(),
            }]),
        }
    }

    /// Pairs two signatures' parameters, and their generic parameters with
    /// them.
    ///
    /// Each pairing of the generic parameters that keeps the declared ones is
    /// tried in declared order, and under each the two have to answer alike,
    /// bound each pair of generic parameters alike, and pair their parameters
    /// up. Of the pairings that work, the one under which the most parameters
    /// pair by name is kept — exactly, then folded — then the one pairing the
    /// most generic parameters by name, then the first.
    fn pair_signatures(
        &self,
        from: &QualifiedName,
        to: &QualifiedName,
        a: &FunctionSig,
        b: &FunctionSig,
        renames: &[(Ident, Ident)],
        generic_renames: &[(Ident, Ident)],
    ) -> Result<Pairing, Vec<BridgeMismatch>> {
        let ours: Vec<Ident> = a.params_decl.iter().map(|d| d.name.clone()).collect();
        let theirs: Vec<Ident> = b.params_decl.iter().map(|d| d.name.clone()).collect();
        if ours.len() != theirs.len() {
            return Err(vec![BridgeMismatch::GenericArity {
                from: from.clone(),
                to: to.clone(),
                from_count: ours.len(),
                to_count: theirs.len(),
            }]);
        }

        // `fixed[i]` is the generic parameter of `to` declared to go with the
        // `i`th of `from`.
        let mut fixed: Vec<Option<usize>> = vec![None; ours.len()];
        let mut claimed = vec![false; theirs.len()];
        let mut problems = Vec::new();
        for (x, y) in generic_renames {
            let i = ours.iter().position(|n| n == x);
            let j = theirs.iter().position(|n| n == y);
            let (Some(i), Some(j)) = (i, j) else {
                problems.push(BridgeMismatch::NoSuchGeneric {
                    name: if i.is_none() { x } else { y }.clone(),
                });
                continue;
            };
            if fixed[i].is_some() || claimed[j] {
                problems.push(BridgeMismatch::PairedTwice {
                    field: if fixed[i].is_some() { x } else { y }.clone(),
                });
                continue;
            }
            fixed[i] = Some(j);
            claimed[j] = true;
        }
        if !problems.is_empty() {
            return Err(problems);
        }

        let candidates: Vec<Vec<usize>> = numberings(ours.len())
            .into_iter()
            .filter(|sigma| {
                fixed
                    .iter()
                    .zip(sigma)
                    .all(|(f, &j)| f.is_none_or(|f| f == j))
            })
            .collect();

        let mut best: Option<((usize, usize, usize), Pairing)> = None;
        let mut first_problems = None;
        for sigma in &candidates {
            match self.pair_signatures_under(a, b, &ours, sigma, renames) {
                Ok(pairs) => {
                    let score = (
                        pairs.iter().filter(|p| p.from == p.to).count(),
                        pairs
                            .iter()
                            .filter(|p| p.from.fold() == p.to.fold())
                            .count(),
                        sigma
                            .iter()
                            .enumerate()
                            .filter(|&(i, &j)| ours[i] == theirs[j])
                            .count(),
                    );
                    if best.as_ref().is_none_or(|(kept, _)| score > *kept) {
                        // In the target's order, like the parameters.
                        let mut generics: Vec<GenericPair> = sigma
                            .iter()
                            .enumerate()
                            .map(|(i, &j)| GenericPair {
                                from: ours[i].clone(),
                                to: theirs[j].clone(),
                            })
                            .collect();
                        generics.sort_by_key(|g| theirs.iter().position(|n| *n == g.to));
                        best = Some((score, Pairing { pairs, generics }));
                    }
                }
                Err(problems) => {
                    first_problems.get_or_insert(problems);
                }
            }
        }

        match best {
            Some((_, pairing)) => Ok(pairing),
            // With only one way the generic parameters could go, what went
            // wrong under it is what went wrong.
            None if candidates.len() == 1 => Err(first_problems.expect("one was tried")),
            None => Err(vec![BridgeMismatch::NoGenericPairing {
                from: from.clone(),
                to: to.clone(),
            }]),
        }
    }

    /// Pairs two signatures' parameters with the `i`th generic parameter of
    /// `a`, called `ours[i]`, taken to be the `sigma[i]`th of `b`.
    fn pair_signatures_under(
        &self,
        a: &FunctionSig,
        b: &FunctionSig,
        ours: &[Ident],
        sigma: &[usize],
        renames: &[(Ident, Ident)],
    ) -> Result<Vec<FieldPair>, Vec<BridgeMismatch>> {
        // `b`'s generic parameters, each where its partner in `a` is, so a
        // type of `b`'s numbers them the way the same type of `a`'s would.
        let aligned: Vec<Ident> = sigma
            .iter()
            .map(|&j| b.params_decl[j].name.clone())
            .collect();
        let same = |have: &Type, want: &Type| {
            self.data_address(have, ours) == self.data_address(want, &aligned)
        };
        let bounds = |d: &TypeParamDecl, generics: &[Ident]| -> BTreeSet<TypeId> {
            d.bounds
                .iter()
                .map(|r| self.data_address(&Type::Ref(r.clone()), generics))
                .collect()
        };

        let mut problems: Vec<BridgeMismatch> = a
            .params_decl
            .iter()
            .zip(sigma)
            .filter_map(|(ours_decl, &j)| {
                let theirs_decl = &b.params_decl[j];
                (bounds(ours_decl, ours) != bounds(theirs_decl, &aligned)).then(|| {
                    BridgeMismatch::BoundsDiffer {
                        from: ours_decl.name.clone(),
                        to: theirs_decl.name.clone(),
                    }
                })
            })
            .collect();
        if !same(&a.ret, &b.ret) {
            problems.push(BridgeMismatch::NoConversion {
                from: Ident::literal("ret"),
                to: Ident::literal("ret"),
                found: crate::canon::canonical_string(&a.ret),
                required: crate::canon::canonical_string(&b.ret),
            });
        }
        if !problems.is_empty() {
            return Err(problems);
        }

        let params = |s: &FunctionSig| -> Vec<Field> {
            s.params
                .iter()
                .map(|p| Field::new(p.name.clone(), p.ty.clone()))
                .collect()
        };
        self.pair_up(&params(a), &params(b), renames, &|have, want| {
            same(have, want).then_some(Coercion::Identity)
        })
    }

    /// Pairs every source part with exactly one target part.
    ///
    /// `convert` says whether, and how, one part's type becomes another's; a
    /// pair it refuses is never made. Declared pairings come first. Then, until
    /// nothing changes: a part with only one partner it could convert with
    /// takes it, an exact name match pairs, a folded name match that is the only
    /// one either way pairs. Whatever is left pairs by declared order among
    /// parts that convert. Anything still unpaired is reported.
    fn pair_up(
        &self,
        source: &[Field],
        target: &[Field],
        renames: &[(Ident, Ident)],
        convert: &dyn Fn(&Type, &Type) -> Option<Coercion>,
    ) -> Result<Vec<FieldPair>, Vec<BridgeMismatch>> {
        let mut problems = Vec::new();
        // `fits[t][s]`: how source `s` becomes target `t`, if it can.
        let fits: Vec<Vec<Option<Coercion>>> = target
            .iter()
            .map(|t| source.iter().map(|s| convert(&s.ty, &t.ty)).collect())
            .collect();
        // `taken[t]`: the source paired with target `t`.
        let mut taken: Vec<Option<usize>> = vec![None; target.len()];
        let mut used = vec![false; source.len()];

        for (from, to) in renames {
            let s = source.iter().position(|f| &f.name == from);
            let t = target.iter().position(|f| &f.name == to);
            let (Some(s), Some(t)) = (s, t) else {
                problems.push(BridgeMismatch::NoSuchField {
                    field: if s.is_none() { from } else { to }.clone(),
                });
                continue;
            };
            if used[s] || taken[t].is_some() {
                problems.push(BridgeMismatch::PairedTwice {
                    field: if used[s] { from } else { to }.clone(),
                });
                continue;
            }
            if fits[t][s].is_none() {
                problems.push(BridgeMismatch::NoConversion {
                    from: from.clone(),
                    to: to.clone(),
                    found: crate::canon::canonical_string(&source[s].ty),
                    required: crate::canon::canonical_string(&target[t].ty),
                });
                continue;
            }
            taken[t] = Some(s);
            used[s] = true;
        }
        if !problems.is_empty() {
            return Err(problems);
        }

        let open = |taken: &[Option<usize>], used: &[bool], t: usize| -> Vec<usize> {
            if taken[t].is_some() {
                return Vec::new();
            }
            (0..source.len())
                .filter(|&s| !used[s] && fits[t][s].is_some())
                .collect()
        };

        loop {
            let mut progress = false;
            for t in 0..target.len() {
                if taken[t].is_some() {
                    continue;
                }
                let candidates = open(&taken, &used, t);
                // Only one thing it could be.
                let forced = match candidates.as_slice() {
                    [only] => Some(*only),
                    _ => None,
                };
                // Or something only it could take.
                let claimed = || {
                    candidates.iter().copied().find(|&s| {
                        (0..target.len())
                            .filter(|&u| open(&taken, &used, u).contains(&s))
                            .count()
                            == 1
                    })
                };
                let by_name = || {
                    candidates
                        .iter()
                        .copied()
                        .find(|&s| source[s].name == target[t].name)
                };
                let by_folded = || {
                    let folded = target[t].name.fold();
                    let mine: Vec<usize> = candidates
                        .iter()
                        .copied()
                        .filter(|&s| source[s].name.fold() == folded)
                        .collect();
                    let theirs = (0..target.len())
                        .filter(|&u| taken[u].is_none() && target[u].name.fold() == folded)
                        .count();
                    match mine.as_slice() {
                        [only] if theirs == 1 => Some(*only),
                        _ => None,
                    }
                };
                if let Some(s) = forced.or_else(claimed).or_else(by_name).or_else(by_folded) {
                    taken[t] = Some(s);
                    used[s] = true;
                    progress = true;
                }
            }
            if !progress {
                break;
            }
        }

        // What names could not settle, declared order does: the first field of
        // a type left on one side goes with the first of that type left on the
        // other, and so on down.
        for t in 0..target.len() {
            if taken[t].is_some() {
                continue;
            }
            if let Some(s) = (0..source.len()).find(|&s| !used[s] && fits[t][s].is_some()) {
                taken[t] = Some(s);
                used[s] = true;
            }
        }

        for t in 0..target.len() {
            if taken[t].is_some() {
                continue;
            }
            let candidates = open(&taken, &used, t);
            problems.push(if candidates.is_empty() {
                BridgeMismatch::Unfilled {
                    field: target[t].name.clone(),
                }
            } else {
                BridgeMismatch::Ambiguous {
                    field: target[t].name.clone(),
                    candidates: candidates
                        .iter()
                        .map(|&s| source[s].name.to_string())
                        .collect(),
                }
            });
        }
        for s in 0..source.len() {
            if !used[s] {
                problems.push(BridgeMismatch::Unused {
                    field: source[s].name.clone(),
                });
            }
        }
        if !problems.is_empty() {
            return Err(problems);
        }

        Ok(target
            .iter()
            .enumerate()
            .map(|(t, want)| {
                let s = taken[t].expect("every target is paired");
                FieldPair {
                    from: source[s].name.clone(),
                    to: want.name.clone(),
                    coercion: fits[t][s].clone().expect("only fitting pairs are made"),
                }
            })
            .collect())
    }

    /// Whether a field can simply not be there.
    fn may_be_absent(&self, ty: &Type) -> bool {
        matches!(
            self.expand_alias(ty),
            Type::Optional { .. } | Type::Unit | Type::Any
        )
    }

    /// Every field of a declaration, by qualified name.
    ///
    /// `None` for anything without fields, which is how a caller distinguishes
    /// "an enum" from "a struct with no fields".
    pub fn fields_by_name(&self, name: &QualifiedName) -> Option<Vec<Field>> {
        self.fields_of(&TypeRef::qualified(
            name.universe.clone(),
            name.name.clone(),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Workspace;
    use crate::check::check_workspace;

    fn ws(json: &str) -> CheckedWorkspace {
        check_workspace(Workspace::from_json(json).expect("reads")).expect("checks")
    }

    fn q(s: &str) -> QualifiedName {
        s.parse().unwrap()
    }

    fn id(s: &str) -> Ident {
        Ident::new(s).unwrap()
    }

    #[test]
    fn the_same_structure_needs_no_mapping() {
        let ws = ws(r#"{"universes":[
            {"name":"geo","types":[{"k":"struct","name":"Point","fields":[
                {"name":"x","ty":"float"},{"name":"y","ty":"float"}]}]},
            {"name":"gfx","types":[{"k":"struct","name":"Vec2","fields":[
                {"name":"x","ty":"float"},{"name":"y","ty":"float"}]}]}]}"#);

        assert_eq!(
            ws.derive_link(&q("geo::Point"), &q("gfx::Vec2"), &[]),
            Ok(LinkKind::Identical)
        );
    }

    #[test]
    fn fields_find_each_other_across_naming_conventions() {
        let ws = ws(r#"{"universes":[
            {"name":"api","types":[{"k":"struct","name":"User","fields":[
                {"name":"userId","ty":"str"},{"name":"displayName","ty":"str"}]}]},
            {"name":"db","types":[{"k":"struct","name":"UserRow","fields":[
                {"name":"user_id","ty":"str"},{"name":"display_name","ty":"str"}]}]}]}"#);

        // The same data, renamed: a renaming undoes, so it is an equivalence.
        let LinkKind::Equivalent { pairs, .. } = ws
            .derive_link(&q("api::User"), &q("db::UserRow"), &[])
            .expect("derives")
        else {
            panic!("expected an equivalence");
        };
        assert_eq!(pairs.len(), 2);
        let by_target: Vec<(&str, &str)> = pairs
            .iter()
            .map(|p| (p.from.as_str(), p.to.as_str()))
            .collect();
        assert!(by_target.contains(&("userId", "user_id")), "{by_target:?}");
    }

    #[test]
    fn a_correspondence_nobody_could_guess_can_be_declared() {
        // `colour` keeps this from being the same data renamed, so nothing
        // pairs fields by type or by position, and only names are left.
        let ws = ws(r#"{"universes":[
            {"name":"a","types":[{"k":"struct","name":"X","fields":[
                {"name":"handle","ty":"str"}]}]},
            {"name":"b","types":[{"k":"struct","name":"Y","fields":[
                {"name":"nickname","ty":"str"},{"name":"colour","ty":"str?"}]}]}]}"#);

        assert!(ws.derive_link(&q("a::X"), &q("b::Y"), &[]).is_err());

        let LinkKind::FieldMap { pairs, .. } = ws
            .derive_link(&q("a::X"), &q("b::Y"), &[(id("handle"), id("nickname"))])
            .expect("derives once told")
        else {
            panic!("expected a field mapping");
        };
        assert_eq!(pairs[0].from.as_str(), "handle");
        assert_eq!(pairs[0].to.as_str(), "nickname");
    }

    /// The pairs of a derived equivalence, as `from -> to`, in target order.
    fn renaming(kind: LinkKind) -> Vec<String> {
        let LinkKind::Equivalent { pairs, .. } = kind else {
            panic!("expected an equivalence, got {kind:?}");
        };
        pairs
            .iter()
            .map(|p| format!("{} -> {}", p.from, p.to))
            .collect()
    }

    #[test]
    fn distinct_types_say_which_field_is_which_without_the_names() {
        let ws = ws(r#"{"universes":[
            {"name":"a","types":[{"k":"struct","name":"Fetch","fields":[
                {"name":"url","ty":"str"},{"name":"retries","ty":"i64"}]}]},
            {"name":"b","types":[{"k":"struct","name":"Get","fields":[
                {"name":"attempts","ty":"i64"},{"name":"getUrl","ty":"str"}]}]}]}"#);

        assert_eq!(
            renaming(
                ws.derive_link(&q("a::Fetch"), &q("b::Get"), &[])
                    .expect("derives")
            ),
            ["retries -> attempts", "url -> getUrl"],
            "only one string and one integer on each side, so no name had to agree"
        );
    }

    #[test]
    fn what_types_and_names_leave_open_goes_by_position() {
        let ws = ws(r#"{"universes":[
            {"name":"geo","types":[{"k":"struct","name":"Point","fields":[
                {"name":"x","ty":"float"},{"name":"y","ty":"float"}]}]},
            {"name":"map","types":[{"k":"struct","name":"LatLong","fields":[
                {"name":"lat","ty":"float"},{"name":"long","ty":"float"}]}]}]}"#);

        assert_eq!(
            renaming(
                ws.derive_link(&q("geo::Point"), &q("map::LatLong"), &[])
                    .expect("derives")
            ),
            ["x -> lat", "y -> long"],
            "two numbers each, no names in common: first with first, second with second"
        );
        assert_eq!(
            renaming(
                ws.derive_link(
                    &q("geo::Point"),
                    &q("map::LatLong"),
                    &[(id("x"), id("long"))]
                )
                .expect("derives")
            ),
            ["y -> lat", "x -> long"],
            "and a swap someone declares is a swap"
        );
    }

    #[test]
    fn names_settle_what_types_cannot_before_position_does() {
        let ws = ws(r#"{"universes":[
            {"name":"a","types":[{"k":"struct","name":"X","fields":[
                {"name":"x","ty":"float"},{"name":"y","ty":"float"},{"name":"id","ty":"str"}]}]},
            {"name":"b","types":[{"k":"struct","name":"Y","fields":[
                {"name":"ident","ty":"str"},{"name":"y","ty":"float"},{"name":"x","ty":"float"}]}]}]}"#);

        assert_eq!(
            renaming(
                ws.derive_link(&q("a::X"), &q("b::Y"), &[])
                    .expect("derives")
            ),
            ["id -> ident", "y -> y", "x -> x"],
            "position would have crossed x and y over; their names say otherwise"
        );
    }

    #[test]
    fn a_nested_equivalence_is_used_whichever_way_it_was_declared() {
        let ws = ws(r#"{"universes":[
            {"name":"a","types":[
                {"k":"struct","name":"Point","fields":[{"name":"x","ty":"float"},{"name":"y","ty":"float"}]},
                {"k":"struct","name":"Pin","fields":[{"name":"at","ty":"Point"},{"name":"label","ty":"str"}]}]},
            {"name":"b","types":[
                {"k":"struct","name":"LatLong","fields":[{"name":"lat","ty":"float"},{"name":"long","ty":"float"}]},
                {"k":"struct","name":"Marker","fields":[{"name":"name","ty":"str"},{"name":"where","ty":"LatLong"}]}]}],
          "links":[{"name":"latlong_is_a_point","from":"b::LatLong","to":"a::Point",
                    "kind":{"k":"equivalent"}}]}"#);

        let LinkKind::Equivalent { pairs, .. } = ws
            .derive_link(&q("a::Pin"), &q("b::Marker"), &[])
            .expect("derives")
        else {
            panic!("expected an equivalence");
        };
        let at = pairs
            .iter()
            .find(|p| p.from.as_str() == "at")
            .expect("paired");
        assert_eq!(at.to.as_str(), "where");
        assert_eq!(
            at.coercion,
            Coercion::ViaLink {
                link: id("latlong_is_a_point"),
                reversed: true
            },
            "declared from b to a, so going from a to b runs it backwards"
        );
    }

    #[test]
    fn signatures_pair_their_parameters_by_type() {
        let ws = ws(r#"{"universes":[
            {"name":"node","functions":[{"name":"request","ret":"unit","params":[
                {"name":"url","ty":"str"},
                {"name":"cb","ty":{"k":"function","params":["str"],"ret":"unit"}}]}]},
            {"name":"browser","functions":[{"name":"request","ret":"unit","params":[
                {"name":"callback","ty":{"k":"function","params":["str"],"ret":"unit"}},
                {"name":"getUrl","ty":"str"}]}]}]}"#);

        let pairing = ws
            .derive_equivalence(&q("node::request"), &q("browser::request"), &[], &[])
            .expect("one string and one callback each: only one way to pair them");
        let pairs: Vec<String> = pairing
            .pairs
            .iter()
            .map(|p| format!("{} -> {}", p.from, p.to))
            .collect();
        assert_eq!(pairs, ["cb -> callback", "url -> getUrl"]);
    }

    #[test]
    fn signatures_that_answer_differently_are_not_one_proposition() {
        let ws = ws(r#"{"universes":[
            {"name":"a","functions":[{"name":"f","params":[{"name":"n","ty":"i64"}],"ret":"str"}]},
            {"name":"b","functions":[{"name":"g","params":[{"name":"m","ty":"i64"}],"ret":"i64"}]}]}"#);

        assert!(matches!(
            ws.derive_equivalence(&q("a::f"), &q("b::g"), &[], &[])
                .unwrap_err()[0],
            BridgeMismatch::NoConversion { .. }
        ));
    }

    /// A signature `name<g0, g1>(p0: t0, p1: t1) -> ret`, where a type that is
    /// a bare capital letter is that generic parameter and `[X]` a list of it.
    fn generic_sig(name: &str, generics: &[&str], params: &[(&str, &str)], ret: &str) -> String {
        let ty = |t: &str| match t.strip_prefix('[').and_then(|t| t.strip_suffix(']')) {
            Some(inner) => format!(r#"{{"k":"list","of":{{"k":"param","name":"{inner}"}}}}"#),
            None if t.len() == 1 && t.chars().all(|c| c.is_ascii_uppercase()) => {
                format!(r#"{{"k":"param","name":"{t}"}}"#)
            }
            None => format!("{t:?}"),
        };
        let generics: Vec<String> = generics
            .iter()
            .map(|g| format!(r#"{{"name":"{g}"}}"#))
            .collect();
        let params: Vec<String> = params
            .iter()
            .map(|(n, t)| format!(r#"{{"name":"{n}","ty":{}}}"#, ty(t)))
            .collect();
        format!(
            r#"{{"name":"{name}","params_decl":[{}],"params":[{}],"ret":{}}}"#,
            generics.join(","),
            params.join(","),
            ty(ret)
        )
    }

    fn two_universes(a: &[String], b: &[String]) -> CheckedWorkspace {
        ws(&format!(
            r#"{{"universes":[{{"name":"a","functions":[{}]}},{{"name":"b","functions":[{}]}}]}}"#,
            a.join(","),
            b.join(",")
        ))
    }

    /// A pairing as `from -> to` strings, parameters then generic parameters.
    fn said(pairing: &Pairing) -> (Vec<String>, Vec<String>) {
        (
            pairing
                .pairs
                .iter()
                .map(|p| format!("{} -> {}", p.from, p.to))
                .collect(),
            pairing
                .generics
                .iter()
                .map(|g| format!("{} -> {}", g.from, g.to))
                .collect(),
        )
    }

    #[test]
    fn generic_signatures_pair_their_generic_parameters_too() {
        let ws = two_universes(
            &[generic_sig(
                "konst",
                &["A", "B"],
                &[("x", "A"), ("y", "B")],
                "A",
            )],
            &[generic_sig(
                "konst",
                &["Y", "X"],
                &[("ignored", "Y"), ("kept", "X")],
                "X",
            )],
        );
        let pairing = ws
            .derive_equivalence(&q("a::konst"), &q("b::konst"), &[], &[])
            .expect("the one answered is the one answered, whatever it is called");
        assert_eq!(
            said(&pairing),
            (
                vec!["y -> ignored".to_string(), "x -> kept".to_string()],
                vec!["B -> Y".to_string(), "A -> X".to_string()]
            )
        );
    }

    #[test]
    fn names_say_which_way_round_a_symmetric_signature_goes() {
        // Swap the generic parameters and the parameters along with them and
        // this is the same signature, so types alone cannot say whether `x`
        // goes with `x` or with `y`.
        let ws = two_universes(
            &[generic_sig(
                "same",
                &["A", "B"],
                &[("x", "A"), ("y", "B")],
                "bool",
            )],
            &[generic_sig(
                "same",
                &["P", "Q"],
                &[("x", "Q"), ("y", "P")],
                "bool",
            )],
        );
        let pairing = ws
            .derive_equivalence(&q("a::same"), &q("b::same"), &[], &[])
            .expect("pairs");
        assert_eq!(
            said(&pairing),
            (
                vec!["x -> x".to_string(), "y -> y".to_string()],
                vec!["B -> P".to_string(), "A -> Q".to_string()]
            ),
            "the pairing that keeps every name beats the one in declared order"
        );

        let declared = ws
            .derive_equivalence(&q("a::same"), &q("b::same"), &[], &[(id("A"), id("P"))])
            .expect("pairs once told");
        assert_eq!(
            said(&declared),
            (
                vec!["y -> x".to_string(), "x -> y".to_string()],
                vec!["A -> P".to_string(), "B -> Q".to_string()]
            ),
            "and a generic pair someone declares is kept, parameters following it"
        );
    }

    #[test]
    fn generic_signatures_over_different_numbers_of_types_are_not_one() {
        let ws = two_universes(
            &[generic_sig(
                "f",
                &["A", "B"],
                &[("x", "A"), ("y", "B")],
                "A",
            )],
            &[generic_sig("f", &["T"], &[("x", "T"), ("y", "T")], "T")],
        );
        assert!(matches!(
            ws.derive_equivalence(&q("a::f"), &q("b::f"), &[], &[])
                .unwrap_err()[..],
            [BridgeMismatch::GenericArity {
                from_count: 2,
                to_count: 1,
                ..
            }]
        ));
    }

    #[test]
    fn no_pairing_of_generic_parameters_makes_different_claims_one() {
        // One answers with the value on its own, the other with an element of
        // the list, and no renaming of the generic parameters changes that.
        let ws = two_universes(
            &[generic_sig(
                "plain",
                &["A", "B"],
                &[("x", "A"), ("ys", "[B]")],
                "A",
            )],
            &[generic_sig(
                "element",
                &["A", "B"],
                &[("x", "A"), ("ys", "[B]")],
                "B",
            )],
        );
        assert_eq!(
            ws.derive_equivalence(&q("a::plain"), &q("b::element"), &[], &[])
                .unwrap_err(),
            [BridgeMismatch::NoGenericPairing {
                from: q("a::plain"),
                to: q("b::element")
            }]
        );
        // Told which is which, it can say what goes wrong under that.
        assert!(
            ws.derive_equivalence(
                &q("a::plain"),
                &q("b::element"),
                &[],
                &[(id("A"), id("A")), (id("B"), id("B"))]
            )
            .unwrap_err()
            .iter()
            .any(
                |p| matches!(p, BridgeMismatch::NoConversion { from, .. } if from.as_str() == "ret")
            )
        );
    }

    #[test]
    fn generic_parameters_paired_together_are_bounded_alike() {
        let ws = ws(r#"{"universes":[
            {"name":"a","types":[{"k":"interface","name":"Named","fields":[{"name":"name","ty":"str"}]}],
             "functions":[{"name":"f","params_decl":[{"name":"T","bounds":["Named"]}],
                "params":[{"name":"x","ty":{"k":"param","name":"T"}}],"ret":"str"}]},
            {"name":"b","functions":[{"name":"f","params_decl":[{"name":"U"}],
                "params":[{"name":"x","ty":{"k":"param","name":"U"}}],"ret":"str"}]}]}"#);
        assert_eq!(
            ws.derive_equivalence(&q("a::f"), &q("b::f"), &[], &[])
                .unwrap_err(),
            [BridgeMismatch::BoundsDiffer {
                from: id("T"),
                to: id("U")
            }]
        );
    }

    #[test]
    fn a_generic_pair_has_to_name_generic_parameters_that_are_there() {
        let ws = two_universes(
            &[generic_sig("id", &["T"], &[("x", "T")], "T")],
            &[generic_sig("id", &["U"], &[("x", "U")], "U")],
        );
        assert_eq!(
            ws.derive_equivalence(&q("a::id"), &q("b::id"), &[], &[(id("T"), id("V"))])
                .unwrap_err(),
            [BridgeMismatch::NoSuchGeneric { name: id("V") }]
        );
    }

    #[test]
    fn an_equivalence_drops_nothing() {
        let ws = ws(r#"{"universes":[
            {"name":"a","types":[{"k":"struct","name":"X","fields":[
                {"name":"id","ty":"str"},{"name":"secret","ty":"str"}]}]},
            {"name":"b","types":[{"k":"struct","name":"Y","fields":[
                {"name":"id","ty":"str"}]}]}]}"#);

        let problems = ws
            .derive_equivalence(&q("a::X"), &q("b::Y"), &[], &[])
            .unwrap_err();
        assert!(
            problems.iter().any(
                |p| matches!(p, BridgeMismatch::Unused { field } if field.as_str() == "secret")
            ),
            "{problems:?}"
        );
    }

    #[test]
    fn a_field_the_target_has_no_room_for_is_reported_as_dropped() {
        let ws = ws(r#"{"universes":[
            {"name":"a","types":[{"k":"struct","name":"X","fields":[
                {"name":"id","ty":"str"},{"name":"secret","ty":"str"}]}]},
            {"name":"b","types":[{"k":"struct","name":"Y","fields":[
                {"name":"id","ty":"str"}]}]}]}"#);

        let kind = ws
            .derive_link(&q("a::X"), &q("b::Y"), &[])
            .expect("derives");
        let LinkKind::FieldMap { dropped, .. } = &kind else {
            panic!("expected a field mapping");
        };
        assert_eq!(dropped, &[id("secret")], "a lossy bridge should say so");
    }

    #[test]
    fn a_target_field_nothing_supplies_is_refused_unless_it_can_be_absent() {
        let required = r#"{"universes":[
            {"name":"a","types":[{"k":"struct","name":"X","fields":[
                {"name":"id","ty":"str"}]}]},
            {"name":"b","types":[{"k":"struct","name":"Y","fields":[
                {"name":"id","ty":"str"},{"name":"colour","ty":"str"}]}]}]}"#;
        let problems = ws(required)
            .derive_link(&q("a::X"), &q("b::Y"), &[])
            .unwrap_err();
        assert!(
            matches!(problems[0], BridgeMismatch::Unfilled { .. }),
            "{problems:?}"
        );

        let optional = required.replace(
            r#"{"name":"colour","ty":"str"}"#,
            r#"{"name":"colour","ty":"str?"}"#,
        );
        assert!(
            ws(&optional)
                .derive_link(&q("a::X"), &q("b::Y"), &[])
                .is_ok(),
            "a field that may be absent needs no source"
        );
    }

    #[test]
    fn a_pair_with_no_conversion_between_them_is_reported() {
        let ws = ws(r#"{"universes":[
            {"name":"a","types":[{"k":"struct","name":"X","fields":[
                {"name":"id","ty":"str"}]}]},
            {"name":"b","types":[{"k":"struct","name":"Y","fields":[
                {"name":"id","ty":"bool"}]}]}]}"#);

        let problems = ws.derive_link(&q("a::X"), &q("b::Y"), &[]).unwrap_err();
        assert!(
            matches!(problems[0], BridgeMismatch::NoConversion { .. }),
            "{problems:?}"
        );
    }

    #[test]
    fn a_value_widens_and_wraps_where_the_target_allows_it() {
        let ws = ws(r#"{"universes":[
            {"name":"a","types":[{"k":"struct","name":"X","fields":[
                {"name":"n","ty":"i32"},{"name":"note","ty":"str"}]}]},
            {"name":"b","types":[{"k":"struct","name":"Y","fields":[
                {"name":"n","ty":"i64"},{"name":"note","ty":"str?"}]}]}]}"#);

        let LinkKind::FieldMap { pairs, .. } = ws
            .derive_link(&q("a::X"), &q("b::Y"), &[])
            .expect("derives")
        else {
            panic!("expected a field mapping");
        };
        let coercion = |name: &str| {
            pairs
                .iter()
                .find(|p| p.to.as_str() == name)
                .map(|p| p.coercion.clone())
                .unwrap()
        };
        assert_eq!(coercion("n"), Coercion::WidenInt);
        assert_eq!(
            coercion("note"),
            Coercion::WrapOptional {
                of: Box::new(Coercion::Identity)
            }
        );
    }

    #[test]
    fn a_list_converts_through_its_elements() {
        let ws = ws(r#"{"universes":[
            {"name":"a","types":[{"k":"struct","name":"X","fields":[
                {"name":"ns","ty":"[i32]"}]}]},
            {"name":"b","types":[{"k":"struct","name":"Y","fields":[
                {"name":"ns","ty":"[i64]"}]}]}]}"#);

        let LinkKind::FieldMap { pairs, .. } = ws
            .derive_link(&q("a::X"), &q("b::Y"), &[])
            .expect("derives")
        else {
            panic!("expected a field mapping");
        };
        assert_eq!(
            pairs[0].coercion,
            Coercion::Through {
                functor: Functor::List,
                of: Box::new(Coercion::WidenInt)
            }
        );
    }

    /// Two universes whose nested types differ, so the pair is not simply
    /// identical and the nested field genuinely needs converting.
    const NESTED: &str = r#"{"universes":[
        {"name":"a","types":[
            {"k":"struct","name":"Point","fields":[{"name":"x","ty":"float"}]},
            {"k":"struct","name":"Line","fields":[{"name":"start","ty":"Point"}]}]},
        {"name":"b","types":[
            {"k":"struct","name":"Vec2","fields":[
                {"name":"x","ty":"float"},{"name":"label","ty":"str?"}]},
            {"k":"struct","name":"Seg","fields":[{"name":"start","ty":"Vec2"}]}]}]}"#;

    #[test]
    fn a_nested_type_converts_through_a_link_that_already_exists() {
        let with_link = NESTED.trim_end_matches('}').to_string()
            + r#","links":[{"name":"point_to_vec2","from":"a::Point","to":"b::Vec2",
                            "kind":{"k":"field_map","pairs":[
                                {"from":"x","to":"x","coercion":{"k":"identity"}}]}}]}"#;
        let ws = ws(&with_link);

        let LinkKind::FieldMap { pairs, .. } = ws
            .derive_link(&q("a::Line"), &q("b::Seg"), &[])
            .expect("derives")
        else {
            panic!("expected a field mapping");
        };
        assert_eq!(
            pairs[0].coercion,
            Coercion::ViaLink {
                link: id("point_to_vec2"),
                reversed: false,
            }
        );
    }

    #[test]
    fn a_nested_type_with_no_link_between_them_is_refused() {
        assert!(
            ws(NESTED)
                .derive_link(&q("a::Line"), &q("b::Seg"), &[])
                .is_err(),
            "the outer types line up, but nobody has said how the inner ones convert"
        );
    }

    #[test]
    fn deriving_a_mapping_is_a_proposal_rather_than_permission() {
        // These two hold the same data all the way down, so derivation says a
        // cast would do. That is an answer to "how would this work", not a
        // decision that it may: nothing crosses until a link is declared.
        let ws = ws(r#"{"universes":[
            {"name":"a","types":[
                {"k":"struct","name":"Point","fields":[{"name":"x","ty":"float"}]},
                {"k":"struct","name":"Line","fields":[{"name":"start","ty":"Point"}]}]},
            {"name":"b","types":[
                {"k":"struct","name":"Vec2","fields":[{"name":"x","ty":"float"}]},
                {"k":"struct","name":"Seg","fields":[{"name":"start","ty":"Vec2"}]}]}]}"#);

        assert_eq!(
            ws.derive_link(&q("a::Line"), &q("b::Seg"), &[]),
            Ok(LinkKind::Identical)
        );
        assert!(
            !ws.is_subtype(
                &Type::Ref(TypeRef::qualified(id("a"), id("Line"))),
                &Type::Ref(TypeRef::qualified(id("b"), id("Seg")))
            ),
            "and the type system still refuses to let one stand in for the other"
        );
    }

    #[test]
    fn linking_within_one_universe_is_refused() {
        let ws = ws(r#"{"universes":[{"name":"a","types":[
            {"k":"struct","name":"X","fields":[{"name":"id","ty":"str"}]},
            {"k":"struct","name":"Y","fields":[{"name":"id","ty":"str"}]}]}]}"#);

        assert!(matches!(
            ws.derive_link(&q("a::X"), &q("a::Y"), &[]).unwrap_err()[0],
            BridgeMismatch::SameUniverse { .. }
        ));
    }

    #[test]
    fn inherited_fields_take_part_like_any_other() {
        let ws = ws(r#"{"universes":[
            {"name":"a","types":[
                {"k":"struct","name":"Base","fields":[{"name":"id","ty":"str"}]},
                {"k":"struct","name":"Circle","extends":"Base",
                 "fields":[{"name":"radius","ty":"float"}]}]},
            {"name":"b","types":[{"k":"struct","name":"Disc","fields":[
                {"name":"id","ty":"str"},{"name":"radius","ty":"float"}]}]}]}"#);

        assert_eq!(
            ws.derive_link(&q("a::Circle"), &q("b::Disc"), &[]),
            Ok(LinkKind::Identical),
            "where a field came from is not part of the data"
        );
    }
}
