//! Variance: how far a declaration's generic arguments may differ while the
//! whole still fits.
//!
//! Nothing declares variance. It is read off each declaration from where its
//! parameters occur, which is a fact about the declaration rather than a guess
//! about it. A parameter that only appears where a value is held can narrow
//! with the whole, because anything reading it still gets something it can
//! handle. One that only appears where a value is taken can widen as the whole
//! narrows, for the same reason the other way round. One that appears both
//! ways has to match. One that appears nowhere constrains nothing, so a
//! parameter meant to tell two otherwise identical types apart has to be
//! carried by a field: a brand that no value holds is not a difference in the
//! data.
//!
//! An occurrence inside another generic declaration composes with that
//! declaration's variance, so declarations that mention each other are solved
//! together. Every parameter starts unused and only ever moves up a lattice
//! three steps tall, so repeating the pass until nothing moves always ends.
//! Starting at the bottom gives the least answer: a parameter that only
//! reaches itself through a cycle stays unused, because no value ever holds
//! one.
//!
//! A declaration also varies no more freely than what it claims to be.
//! Otherwise two steps could reach where no single step can: `Named<A>` as a
//! `Named<B>`, and that as a `Has<B>`, when `Has` itself would not let an `A`
//! stand for a `B`. So the arguments a declaration passes to its supertypes
//! count as occurrences too.

use std::collections::HashMap;

use crate::check::{CheckedWorkspace, DefIx};
use crate::ident::Ident;
use crate::ir::{Type, TypeDef, TypeRef};

/// How a generic argument may vary while the whole still fits.
///
/// A lattice: unused at the bottom, invariant at the top, and the two
/// directions between them and unrelated to each other.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Variance {
    /// The parameter occurs nowhere, so the argument makes no difference.
    Unused,
    /// The argument may narrow with the whole: a `Box<Circle>` is a
    /// `Box<Shape>`.
    Covariant,
    /// The argument may widen as the whole narrows: a `Sink<Shape>` is a
    /// `Sink<Circle>`, because it takes everything a `Sink<Circle>` would.
    Contravariant,
    /// The arguments must each fit the other.
    Invariant,
}

impl Variance {
    /// The least variance that allows nothing either one forbids.
    pub(crate) fn join(self, other: Variance) -> Variance {
        use Variance::*;
        match (self, other) {
            (Unused, v) | (v, Unused) => v,
            (a, b) if a == b => a,
            _ => Invariant,
        }
    }

    /// The same occurrence, seen from the other side of a function arrow.
    pub(crate) fn flip(self) -> Variance {
        use Variance::*;
        match self {
            Covariant => Contravariant,
            Contravariant => Covariant,
            other => other,
        }
    }

    /// An occurrence that is `inner` within a position that is itself `self`.
    pub(crate) fn then(self, inner: Variance) -> Variance {
        use Variance::*;
        match (self, inner) {
            (Unused, _) | (_, Unused) => Unused,
            (outer, Covariant) => outer,
            (outer, Contravariant) => outer.flip(),
            (_, Invariant) => Invariant,
        }
    }
}

/// Works out every declaration's variance, solving declarations that mention
/// each other together.
///
/// Runs after inheritance is flattened, because inherited fields and claimed
/// supertypes are occurrences, and before anything asks whether one type fits
/// another.
pub(crate) fn compute(ws: &mut CheckedWorkspace) {
    let mut known: Vec<Vec<Variance>> = (0..ws.defs.len())
        .map(|i| vec![Variance::Unused; ws.def_at(DefIx(i)).params().len()])
        .collect();

    loop {
        let mut moved = false;
        for ix in (0..ws.defs.len()).map(DefIx) {
            let found = occurrences(ws, ix, &known);
            for (now, seen) in known[ix.0].iter_mut().zip(found) {
                let next = now.join(seen);
                if next != *now {
                    *now = next;
                    moved = true;
                }
            }
        }
        if !moved {
            break;
        }
    }

    for (entry, variance) in ws.defs.iter_mut().zip(known) {
        entry.variance = variance;
    }
}

/// Where each of a declaration's parameters occurs, given what is known so far
/// about the declarations it mentions.
fn occurrences(ws: &CheckedWorkspace, ix: DefIx, known: &[Vec<Variance>]) -> Vec<Variance> {
    let def = ws.def_at(ix);
    let mut walk = Walk {
        ws,
        known,
        params: def
            .params()
            .iter()
            .enumerate()
            .map(|(i, p)| (&p.name, i))
            .collect(),
        found: vec![Variance::Unused; def.params().len()],
    };

    let entry = &ws.defs[ix.0];
    match def {
        TypeDef::Struct(_) | TypeDef::Interface(_) => {
            for field in &entry.flat_fields {
                walk.ty(&field.ty, Variance::Covariant);
            }
            for ancestor in &entry.ancestors {
                walk.reference(ancestor, Variance::Covariant);
            }
        }
        TypeDef::Enum(d) => {
            for variant in &d.variants {
                walk.ty(&variant.payload, Variance::Covariant);
            }
        }
        TypeDef::Alias(d) => walk.ty(&d.target, Variance::Covariant),
    }
    walk.found
}

/// One pass over one declaration.
struct Walk<'a> {
    ws: &'a CheckedWorkspace,
    known: &'a [Vec<Variance>],
    params: HashMap<&'a Ident, usize>,
    found: Vec<Variance>,
}

impl Walk<'_> {
    fn ty(&mut self, t: &Type, at: Variance) {
        match t {
            Type::Param { name } => {
                if let Some(&i) = self.params.get(name) {
                    self.found[i] = self.found[i].join(at);
                }
            }
            // A function takes its parameters, so they face the other way.
            Type::Function { params, ret } => {
                for p in params {
                    self.ty(p, at.flip());
                }
                self.ty(ret, at);
            }
            Type::Ref(r) => self.reference(r, at),
            // Everything else holds its parts the way it is held, map keys
            // included: values are immutable wire data, so a narrower key is
            // as safe to read as a narrower value.
            other => {
                for child in other.children() {
                    self.ty(child, at);
                }
            }
        }
    }

    fn reference(&mut self, r: &TypeRef, at: Variance) {
        let target = self.ws.ref_target(r).and_then(|q| self.ws.resolve(&q));
        for (i, arg) in r.args.iter().enumerate() {
            // An argument with nowhere to go has been reported already; holding
            // it to the strictest rule keeps that from passing quietly.
            let inner = target
                .and_then(|t| self.known[t.0].get(i).copied())
                .unwrap_or(Variance::Invariant);
            let here = at.then(inner);
            // An argument the declaration ignores cannot constrain anything.
            if here != Variance::Unused {
                self.ty(arg, here);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::Variance::{self, *};
    use crate::Workspace;
    use crate::check::{CheckedWorkspace, check_workspace};

    const T: &str = r#"{"k":"param","name":"T"}"#;

    fn takes(t: &str) -> String {
        format!(r#"{{"k":"function","params":[{t}],"ret":"unit"}}"#)
    }

    fn gives(t: &str) -> String {
        format!(r#"{{"k":"function","params":[],"ret":{t}}}"#)
    }

    fn applied(name: &str, arg: &str) -> String {
        format!(r#"{{"k":"ref","name":"{name}","args":[{arg}]}}"#)
    }

    /// The same reference, where a supertype goes rather than a type.
    fn parent(name: &str, arg: &str) -> String {
        format!(r#"{{"name":"{name}","args":[{arg}]}}"#)
    }

    fn generic(name: &str, fields: &[(&str, String)]) -> String {
        let fields: Vec<String> = fields
            .iter()
            .map(|(n, ty)| format!(r#"{{"name":"{n}","ty":{ty}}}"#))
            .collect();
        format!(
            r#"{{"k":"struct","name":"{name}","params":[{{"name":"T"}}],"fields":[{}]}}"#,
            fields.join(",")
        )
    }

    fn check(types: &[String]) -> CheckedWorkspace {
        let json = format!(
            r#"{{"universes":[{{"name":"demo","types":[{}]}}]}}"#,
            types.join(",")
        );
        check_workspace(Workspace::from_json(&json).expect("reads")).expect("checks")
    }

    fn of(ws: &CheckedWorkspace, name: &str) -> Vec<Variance> {
        ws.variance_of(&format!("demo::{name}").parse().unwrap())
            .expect("declared")
            .to_vec()
    }

    fn sink() -> String {
        generic("Sink", &[("take", takes(T))])
    }

    #[test]
    fn a_parameter_that_is_only_held_is_covariant() {
        let ws = check(&[generic("Box", &[("inside", T.into())])]);
        assert_eq!(of(&ws, "Box"), [Covariant]);
    }

    #[test]
    fn a_parameter_that_is_only_taken_is_contravariant() {
        let ws = check(&[sink()]);
        assert_eq!(of(&ws, "Sink"), [Contravariant]);
    }

    #[test]
    fn a_parameter_used_both_ways_is_invariant() {
        let ws = check(&[generic("Cell", &[("get", gives(T)), ("set", takes(T))])]);
        assert_eq!(of(&ws, "Cell"), [Invariant]);
    }

    #[test]
    fn a_parameter_used_nowhere_is_unused() {
        let ws = check(&[generic("Tag", &[("label", r#""str""#.into())])]);
        assert_eq!(of(&ws, "Tag"), [Unused]);
    }

    #[test]
    fn every_container_holds_what_it_contains() {
        let ws = check(&[generic(
            "Holds",
            &[
                ("list", format!(r#"{{"k":"list","of":{T}}}"#)),
                ("maybe", format!(r#"{{"k":"optional","of":{T}}}"#)),
                ("later", format!(r#"{{"k":"future","of":{T}}}"#)),
                ("index", format!(r#"{{"k":"map","key":"str","value":{T}}}"#)),
                ("pair", format!(r#"{{"k":"tuple","items":[{T},"i64"]}}"#)),
                (
                    "row",
                    format!(r#"{{"k":"record","fields":[{{"name":"a","ty":{T}}}]}}"#),
                ),
                (
                    "either",
                    format!(r#"{{"k":"union","members":[{T},"str"]}}"#),
                ),
            ],
        )]);
        assert_eq!(of(&ws, "Holds"), [Covariant]);
    }

    #[test]
    fn each_parameter_is_inferred_on_its_own() {
        let ws = check(&[
            r#"{"k":"struct","name":"Pipe","params":[{"name":"In"},{"name":"Out"}],
            "fields":[{"name":"run","ty":{"k":"function",
                "params":[{"k":"param","name":"In"}],"ret":{"k":"param","name":"Out"}}}]}"#
                .to_string(),
        ]);
        assert_eq!(of(&ws, "Pipe"), [Contravariant, Covariant]);
    }

    #[test]
    fn a_self_recursive_type_is_covariant_in_what_it_holds() {
        let ws = check(&[generic(
            "Tree",
            &[
                ("value", T.into()),
                (
                    "kids",
                    format!(r#"{{"k":"list","of":{}}}"#, applied("Tree", T)),
                ),
            ],
        )]);
        assert_eq!(of(&ws, "Tree"), [Covariant]);
    }

    #[test]
    fn a_mutually_recursive_pair_settles() {
        // A `Pong` holds a `T`; a `Ping` takes a `Pong`, so it takes a `T`; a
        // `Pong` takes a `Ping`, which only confirms what it already was.
        let ws = check(&[
            generic("Ping", &[("reply", takes(&applied("Pong", T)))]),
            generic(
                "Pong",
                &[("value", T.into()), ("back", takes(&applied("Ping", T)))],
            ),
        ]);
        assert_eq!(of(&ws, "Ping"), [Contravariant]);
        assert_eq!(of(&ws, "Pong"), [Covariant]);
    }

    #[test]
    fn a_parameter_that_only_reaches_itself_is_unused() {
        let ws = check(&[generic(
            "Chain",
            &[(
                "next",
                format!(r#"{{"k":"optional","of":{}}}"#, applied("Chain", T)),
            )],
        )]);
        assert_eq!(
            of(&ws, "Chain"),
            [Unused],
            "no value of a Chain ever holds a T"
        );
    }

    #[test]
    fn variance_composes_through_another_generic() {
        let ws = check(&[
            sink(),
            generic("Feeds", &[("into", applied("Sink", T))]),
            generic("Drains", &[("from", takes(&applied("Sink", T)))]),
            generic("Ignores", &[("tag", applied("Tag", T))]),
            generic("Tag", &[("label", r#""str""#.into())]),
        ]);
        assert_eq!(of(&ws, "Feeds"), [Contravariant]);
        assert_eq!(
            of(&ws, "Drains"),
            [Covariant],
            "taking something that takes a T is a way of giving one"
        );
        assert_eq!(of(&ws, "Ignores"), [Unused]);
    }

    #[test]
    fn an_alias_varies_the_way_its_target_does() {
        let ws = check(&[
            format!(
                r#"{{"k":"alias","name":"Callback","params":[{{"name":"T"}}],"target":{}}}"#,
                takes(T)
            ),
            generic("Button", &[("on_click", applied("Callback", T))]),
        ]);
        assert_eq!(of(&ws, "Callback"), [Contravariant]);
        assert_eq!(of(&ws, "Button"), [Contravariant]);
    }

    #[test]
    fn an_enum_varies_the_way_its_payloads_do() {
        let ws = check(&[format!(
            r#"{{"k":"enum","name":"Maybe","params":[{{"name":"T"}}],
                "variants":[{{"name":"Some","payload":{T}}},{{"name":"None"}}]}}"#
        )]);
        assert_eq!(of(&ws, "Maybe"), [Covariant]);
    }

    #[test]
    fn an_inherited_field_counts_where_it_lands() {
        let ws = check(&[
            sink(),
            format!(
                r#"{{"k":"struct","name":"Logger","params":[{{"name":"T"}}],
                    "extends":{},"fields":[{{"name":"name","ty":"str"}}]}}"#,
                parent("Sink", T)
            ),
        ]);
        assert_eq!(of(&ws, "Logger"), [Contravariant]);
    }

    #[test]
    fn a_claimed_supertype_counts_as_an_occurrence() {
        // `x: unit` satisfies `x: T?` without mentioning `T`, but a `Named<A>`
        // is still a `Has<A>`, and has to vary no more freely than one.
        let ws = check(&[
            format!(
                r#"{{"k":"interface","name":"Has","params":[{{"name":"T"}}],
                    "fields":[{{"name":"x","ty":{{"k":"optional","of":{T}}}}}]}}"#
            ),
            format!(
                r#"{{"k":"struct","name":"Named","params":[{{"name":"T"}}],
                    "implements":[{}],"fields":[{{"name":"x","ty":"unit"}}]}}"#,
                parent("Has", T)
            ),
        ]);
        assert_eq!(of(&ws, "Named"), [Covariant]);
    }

    #[test]
    fn a_declaration_without_parameters_has_no_variance_to_report() {
        let ws = check(&[r#"{"k":"struct","name":"Plain","fields":[]}"#.to_string()]);
        assert_eq!(of(&ws, "Plain"), []);
        assert!(ws.variance_of(&"demo::Missing".parse().unwrap()).is_none());
    }

    #[test]
    fn joining_only_ever_moves_up() {
        let all = [Unused, Covariant, Contravariant, Invariant];
        for a in all {
            assert_eq!(a.join(Unused), a);
            assert_eq!(a.join(Invariant), Invariant);
            assert_eq!(a.join(a), a);
            for b in all {
                assert_eq!(a.join(b), b.join(a));
            }
        }
        assert_eq!(Covariant.join(Contravariant), Invariant);
    }

    #[test]
    fn composing_follows_the_arrows() {
        assert_eq!(Contravariant.then(Contravariant), Covariant);
        assert_eq!(Covariant.then(Contravariant), Contravariant);
        assert_eq!(Contravariant.then(Covariant), Contravariant);
        assert_eq!(Covariant.then(Invariant), Invariant);
        assert_eq!(Invariant.then(Unused), Unused);
        for v in [Unused, Covariant, Contravariant, Invariant] {
            assert_eq!(v.flip().flip(), v);
            assert_eq!(v.then(Covariant), v);
        }
    }
}
