//! Subtyping: when a value of one type can be used where another is expected.
//!
//! Two rules shape everything else here.
//!
//! **Within a universe, named types are nominal.** Two structs with identical
//! fields are unrelated unless one declares that it extends the other. This is
//! what lets a universe say that two things which happen to look alike are
//! different, which is the whole reason to have universes.
//!
//! **Across universes, nothing is a subtype of anything.** Not even identical
//! declarations. Data crosses a universe boundary only through a link, which
//! states how and is witnessed. Silent structural compatibility across
//! boundaries would defeat the purpose of drawing them.
//!
//! Anonymous types are structural throughout, and a named struct can always be
//! *read* as the record of its fields — but not the reverse, because
//! constructing a named type is a nominal act.
//!
//! # Variance
//!
//! Lists, optionals, futures, and both halves of a map are covariant.
//! Functions are contravariant in parameters and covariant in returns. A
//! declaration's own generic arguments vary the way its definition uses them:
//! a `Box<T>` that holds a `T` is covariant, a `Sink<T>` that takes one is
//! contravariant, a parameter used both ways is invariant, and one used nowhere
//! accepts any argument at all. Nothing declares this. It is inferred from
//! where each parameter occurs, which is a fact about the declaration rather
//! than a guess about it.
//!
//! Covariant map keys, covariant field overrides, and covariant generic
//! arguments are sound here because values are immutable wire data. A system
//! that mutated through these types would need stricter rules.

use std::collections::HashSet;

use crate::check::error::describe;
use crate::check::subst::{bind, substitute, substitute_ref};
use crate::check::{CheckedWorkspace, Variance};
use crate::ident::QualifiedName;
use crate::ir::{Field, LiteralValue, Type, TypeDef, TypeRef, Variant, fits_f64_exactly};

/// How deep an alias may expand before the checker gives up.
///
/// Alias cycles are reported during resolution, so reaching this limit means
/// something pathological rather than merely circular.
const MAX_ALIAS_DEPTH: usize = 64;

/// Why one type could not be used where another was expected.
///
/// Descriptive rather than diagnostic in v1: it names the two types involved,
/// which is what error messages need.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SubtypeFailure {
    /// The type that was offered.
    pub found: String,
    /// The type that was expected.
    pub required: String,
}

impl CheckedWorkspace {
    /// Whether a value of `a` can be used where `b` is expected.
    pub fn is_subtype(&self, a: &Type, b: &Type) -> bool {
        self.sub(a, b, &mut HashSet::new())
    }

    /// [`CheckedWorkspace::is_subtype`], but describing the mismatch when there
    /// is one.
    pub fn why_not_subtype(&self, a: &Type, b: &Type) -> Option<SubtypeFailure> {
        (!self.is_subtype(a, b)).then(|| SubtypeFailure {
            found: describe(a),
            required: describe(b),
        })
    }

    /// Replaces an alias with what it stands for.
    ///
    /// An alias is only a name, so nothing that reasons about types should ever
    /// see one.
    pub fn expand_alias(&self, t: &Type) -> Type {
        let mut current = t.clone();
        for _ in 0..MAX_ALIAS_DEPTH {
            let Type::Ref(r) = &current else {
                return current;
            };
            let Some(ix) = self.ref_target(r).and_then(|q| self.resolve(&q)) else {
                return current;
            };
            let TypeDef::Alias(alias) = self.def_at(ix) else {
                return current;
            };
            let subst = bind(&alias.params, &r.args);
            current = crate::canon::normalize_type(&substitute(&alias.target, &subst));
        }
        current
    }

    /// Every field a reference's target has, with its generic arguments applied.
    ///
    /// `None` for anything without fields.
    pub fn fields_of(&self, r: &TypeRef) -> Option<Vec<Field>> {
        let ix = self.ref_target(r).and_then(|q| self.resolve(&q))?;
        let def = self.def_at(ix);
        if def.as_struct().is_none() && def.as_interface().is_none() {
            return None;
        }
        let subst = bind(def.params(), &r.args);
        Some(
            self.defs[ix.0]
                .flat_fields
                .iter()
                .map(|f| Field {
                    name: f.name.clone(),
                    ty: substitute(&f.ty, &subst),
                    doc: f.doc.clone(),
                })
                .collect(),
        )
    }

    /// Every variant a reference's target has, with its generic arguments
    /// applied.
    ///
    /// `None` for anything that is not an enum. The counterpart of
    /// [`CheckedWorkspace::fields_of`]: a `Maybe<i64>` carries an `i64`, not a
    /// `T`, and anything checking a payload has to see it that way.
    pub fn variants_of(&self, r: &TypeRef) -> Option<Vec<Variant>> {
        let ix = self.ref_target(r).and_then(|q| self.resolve(&q))?;
        let def = self.def_at(ix);
        let TypeDef::Enum(d) = def else {
            return None;
        };
        let subst = bind(def.params(), &r.args);
        Some(
            d.variants
                .iter()
                .map(|v| Variant {
                    name: v.name.clone(),
                    payload: substitute(&v.payload, &subst),
                    doc: v.doc.clone(),
                })
                .collect(),
        )
    }

    fn sub(&self, a: &Type, b: &Type, seen: &mut HashSet<(TypeRef, TypeRef)>) -> bool {
        let a = &self.expand_alias(a);
        let b = &self.expand_alias(b);

        if a == b {
            return true;
        }
        if matches!(a, Type::Never) || matches!(b, Type::Any) {
            return true;
        }

        // Everything in the union has to fit, so this must come before the
        // rules that inspect `b`.
        if let Type::Union { members } = a {
            return members.iter().all(|m| self.sub(m, b, seen));
        }

        // An optional target accepts absence, the same type without the
        // optionality, or a narrower optional. Checked before unions on the
        // right so that `Optional<A | B>` is reached through this rule.
        if let Type::Optional { of: want } = b {
            return match a {
                Type::Optional { of: have } => self.sub(have, want, seen),
                Type::Unit => true,
                have => self.sub(have, want, seen),
            };
        }
        // Something that might be absent cannot go where absence is not allowed.
        if matches!(a, Type::Optional { .. } | Type::Unit) {
            return false;
        }

        if let Type::Union { members } = b {
            return members.iter().any(|m| self.sub(a, m, seen));
        }

        if let Type::Literal { value } = a {
            if self.sub(&value.base(), b, seen) {
                return true;
            }
            // A specific integer also fits any narrower type it happens to fit
            // in, which the general rule below cannot say about `i64`.
            return match (value, b) {
                (LiteralValue::Int(i), Type::I32) => i32::try_from(*i).is_ok(),
                (LiteralValue::Int(i), Type::F64) => fits_f64_exactly(*i),
                _ => false,
            };
        }

        // The widenings between primitives, and only the ones that lose
        // nothing. Every 32-bit integer is exactly a 64-bit integer and exactly
        // a float. A 64-bit integer is neither guaranteed: past 2^53 it does
        // not survive being a float, and losing precision silently is the worst
        // way for that to happen.
        if matches!((a, b), (Type::I32, Type::I64) | (Type::I32, Type::F64)) {
            return true;
        }

        match (a, b) {
            (Type::List { of: x }, Type::List { of: y })
            | (Type::Future { of: x }, Type::Future { of: y }) => self.sub(x, y, seen),

            (Type::Map { key: kx, value: vx }, Type::Map { key: ky, value: vy }) => {
                self.sub(kx, ky, seen) && self.sub(vx, vy, seen)
            }

            (Type::Tuple { items: x }, Type::Tuple { items: y }) => {
                x.len() == y.len() && x.iter().zip(y).all(|(p, q)| self.sub(p, q, seen))
            }

            (
                Type::Function {
                    params: px,
                    ret: rx,
                },
                Type::Function {
                    params: py,
                    ret: ry,
                },
            ) => {
                // A function usable in place of another must accept everything
                // the other accepts, and return something the caller can use.
                px.len() == py.len()
                    && py
                        .iter()
                        .zip(px)
                        .all(|(want, have)| self.sub(want, have, seen))
                    && self.sub(rx, ry, seen)
            }

            (Type::Record { fields: x }, Type::Record { fields: y }) => self.record_sub(x, y, seen),

            (Type::Ref(x), Type::Ref(y)) => self.ref_sub(x, y, seen),

            // A named struct can be read as the record of its fields. The
            // reverse does not hold: building a named type is a nominal act.
            (Type::Ref(x), Type::Record { fields: y }) => match self.fields_of(x) {
                Some(have) => self.record_sub(&have, y, seen),
                None => false,
            },

            // A generic parameter stands only for itself. Its bounds are
            // checked where arguments are supplied, not here, because this
            // relation has no declaration in scope to read them from.
            _ => false,
        }
    }

    /// Width and depth: the target's fields must all be present and usable.
    fn record_sub(
        &self,
        have: &[Field],
        want: &[Field],
        seen: &mut HashSet<(TypeRef, TypeRef)>,
    ) -> bool {
        want.iter().all(|field| {
            match have.iter().find(|f| f.name == field.name) {
                Some(present) => self.sub(&present.ty, &field.ty, seen),
                // A missing field and a present null are the same state, so a
                // record without an optional field still satisfies it.
                None => matches!(
                    self.expand_alias(&field.ty),
                    Type::Optional { .. } | Type::Unit | Type::Any
                ),
            }
        })
    }

    fn ref_sub(&self, a: &TypeRef, b: &TypeRef, seen: &mut HashSet<(TypeRef, TypeRef)>) -> bool {
        let (Some(from), Some(to)) = (self.ref_target(a), self.ref_target(b)) else {
            return false;
        };

        // Recursive types would otherwise chase themselves forever. Assuming
        // the pair holds while deciding whether it holds is the standard
        // treatment, and gives the right answer for types like `Tree`.
        if !seen.insert((a.clone(), b.clone())) {
            return true;
        }
        let result = self.ref_sub_inner(a, b, &from, &to, seen);
        seen.remove(&(a.clone(), b.clone()));
        result
    }

    fn ref_sub_inner(
        &self,
        a: &TypeRef,
        b: &TypeRef,
        from: &QualifiedName,
        to: &QualifiedName,
        seen: &mut HashSet<(TypeRef, TypeRef)>,
    ) -> bool {
        if from == to {
            return self.args_related(to, &a.args, &b.args, seen);
        }
        // Inheritance is a claim within one world.
        if from.universe != to.universe {
            return false;
        }
        let Some(ix) = self.resolve(from) else {
            return false;
        };

        let subst = bind(self.def_at(ix).params(), &a.args);
        // Supertypes are already transitive, so one pass finds any ancestor.
        self.defs[ix.0].ancestors.iter().any(|ancestor| {
            let ancestor = substitute_ref(ancestor, &subst);
            self.ref_target(&ancestor).as_ref() == Some(to)
                && self.args_related(to, &ancestor.args, &b.args, seen)
        })
    }

    /// Generic arguments to one declaration, each compared the way that
    /// declaration uses it.
    ///
    /// An argument whose variance is not known yet has to fit both ways, which
    /// is never wrong, only strict.
    fn args_related(
        &self,
        of: &QualifiedName,
        a: &[Type],
        b: &[Type],
        seen: &mut HashSet<(TypeRef, TypeRef)>,
    ) -> bool {
        let variance = self.variance_of(of).unwrap_or_default();
        a.len() == b.len()
            && a.iter().zip(b).enumerate().all(|(i, (x, y))| {
                match variance.get(i).copied().unwrap_or(Variance::Invariant) {
                    Variance::Unused => true,
                    Variance::Covariant => self.sub(x, y, seen),
                    Variance::Contravariant => self.sub(y, x, seen),
                    Variance::Invariant => self.sub(x, y, seen) && self.sub(y, x, seen),
                }
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Workspace;
    use crate::check::check_workspace;
    use crate::ir::Type;

    const WS: &str = r#"{"universes":[
        {"name":"demo","types":[
            {"k":"interface","name":"Identified","fields":[{"name":"id","ty":"str"}]},
            {"k":"struct","name":"Shape","implements":["Identified"],
             "fields":[{"name":"id","ty":"str"}]},
            {"k":"struct","name":"Circle","extends":"Shape",
             "fields":[{"name":"radius","ty":"float"}]},
            {"k":"struct","name":"Rect","extends":"Shape",
             "fields":[{"name":"w","ty":"float"}]},
            {"k":"struct","name":"Tree","fields":[
                {"name":"value","ty":"i64"},{"name":"kids","ty":"[Tree]"}]},
            {"k":"struct","name":"Box","params":[{"name":"T"}],
             "fields":[{"name":"item","ty":{"k":"param","name":"T"}}]},
            {"k":"struct","name":"Labelled","params":[{"name":"T"}],
             "extends":{"name":"Box","args":[{"k":"param","name":"T"}]},
             "fields":[{"name":"label","ty":"str"}]},
            {"k":"struct","name":"Sink","params":[{"name":"T"}],"fields":[
                {"name":"take","ty":{"k":"function",
                    "params":[{"k":"param","name":"T"}],"ret":"unit"}}]},
            {"k":"struct","name":"Cell","params":[{"name":"T"}],"fields":[
                {"name":"get","ty":{"k":"function","params":[],"ret":{"k":"param","name":"T"}}},
                {"name":"set","ty":{"k":"function",
                    "params":[{"k":"param","name":"T"}],"ret":"unit"}}]},
            {"k":"struct","name":"Tag","params":[{"name":"T"}],
             "fields":[{"name":"label","ty":"str"}]},
            {"k":"struct","name":"Branch","params":[{"name":"T"}],"fields":[
                {"name":"value","ty":{"k":"param","name":"T"}},
                {"name":"kids","ty":{"k":"list","of":
                    {"k":"ref","name":"Branch","args":[{"k":"param","name":"T"}]}}}]},
            {"k":"struct","name":"Ping","params":[{"name":"T"}],"fields":[
                {"name":"reply","ty":{"k":"function","ret":"unit","params":[
                    {"k":"ref","name":"Pong","args":[{"k":"param","name":"T"}]}]}}]},
            {"k":"struct","name":"Pong","params":[{"name":"T"}],"fields":[
                {"name":"value","ty":{"k":"param","name":"T"}},
                {"name":"back","ty":{"k":"function","ret":"unit","params":[
                    {"k":"ref","name":"Ping","args":[{"k":"param","name":"T"}]}]}}]},
            {"k":"alias","name":"Id","target":"str"},
            {"k":"enum","name":"Color","variants":[{"name":"Red"}]}]},
        {"name":"other","types":[
            {"k":"struct","name":"Shape","implements":[],
             "fields":[{"name":"id","ty":"str"}]}]}]}"#;

    fn ws() -> CheckedWorkspace {
        check_workspace(Workspace::from_json(WS).expect("reads")).expect("checks")
    }

    fn ty(s: &str) -> Type {
        let json = crate::ir::sugar::parse_type_sugar(s).expect("shorthand");
        let t: Type = serde_json::from_value(json).expect("type");
        // References written here are in `demo` unless they say otherwise.
        crate::canon::normalize_type(&qualify(t))
    }

    fn qualify(t: Type) -> Type {
        let kids: Vec<Type> = t.children().into_iter().cloned().map(qualify).collect();
        let mut node = t.with_children(kids);
        if let Type::Ref(r) = &mut node {
            if r.universe.is_none() {
                r.universe = Some(crate::Ident::new("demo").unwrap());
            }
        }
        node
    }

    #[test]
    fn everything_sits_between_never_and_any() {
        let ws = ws();
        for t in [Type::I64, Type::String.list(), ty("Circle")] {
            assert!(ws.is_subtype(&Type::Never, &t));
            assert!(ws.is_subtype(&t, &Type::Any));
            assert!(!ws.is_subtype(&Type::Any, &t));
        }
    }

    #[test]
    fn a_child_can_be_used_where_its_parent_is_expected() {
        let ws = ws();
        assert!(ws.is_subtype(&ty("Circle"), &ty("Shape")));
        assert!(!ws.is_subtype(&ty("Shape"), &ty("Circle")));
        assert!(
            !ws.is_subtype(&ty("Circle"), &ty("Rect")),
            "siblings are unrelated"
        );
    }

    #[test]
    fn a_struct_satisfies_an_interface_it_reaches_through_a_parent() {
        let ws = ws();
        assert!(ws.is_subtype(&ty("Circle"), &ty("Identified")));
    }

    #[test]
    fn identical_declarations_in_different_universes_stay_unrelated() {
        let ws = ws();
        let here = ty("Shape");
        let there = ty("other::Shape");
        assert!(
            !ws.is_subtype(&here, &there) && !ws.is_subtype(&there, &here),
            "structural accident must not cross a universe boundary"
        );
    }

    #[test]
    fn a_struct_can_be_read_as_a_record_but_not_built_from_one() {
        let ws = ws();
        let record: Type =
            serde_json::from_str(r#"{"k":"record","fields":[{"name":"id","ty":{"k":"string"}}]}"#)
                .unwrap();
        assert!(ws.is_subtype(&ty("Circle"), &record));
        assert!(!ws.is_subtype(&record, &ty("Circle")));
    }

    #[test]
    fn a_record_may_omit_a_field_that_is_allowed_to_be_absent() {
        let ws = ws();
        let empty: Type = serde_json::from_str(r#"{"k":"record","fields":[]}"#).unwrap();
        let wants_optional: Type = serde_json::from_str(
            r#"{"k":"record","fields":[{"name":"note","ty":{"k":"optional","of":{"k":"string"}}}]}"#,
        )
        .unwrap();
        let wants_required: Type = serde_json::from_str(
            r#"{"k":"record","fields":[{"name":"note","ty":{"k":"string"}}]}"#,
        )
        .unwrap();
        assert!(ws.is_subtype(&empty, &wants_optional));
        assert!(!ws.is_subtype(&empty, &wants_required));
    }

    #[test]
    fn a_literal_fits_wherever_its_base_type_does() {
        let ws = ws();
        assert!(ws.is_subtype(&Type::lit_str("circle"), &Type::String));
        assert!(!ws.is_subtype(&Type::String, &Type::lit_str("circle")));
        let three = Type::Literal {
            value: LiteralValue::Int(3),
        };
        assert!(ws.is_subtype(&three, &Type::I64));
        assert!(ws.is_subtype(&three, &Type::I32));
        let huge = Type::Literal {
            value: LiteralValue::Int(i64::MAX),
        };
        assert!(ws.is_subtype(&huge, &Type::I64));
        assert!(!ws.is_subtype(&huge, &Type::I32), "it does not fit");
    }

    #[test]
    fn the_only_numeric_widenings_are_the_ones_that_lose_nothing() {
        let ws = ws();
        assert!(ws.is_subtype(&Type::I32, &Type::I64));
        assert!(
            ws.is_subtype(&Type::I32, &Type::F64),
            "every 32-bit integer is exactly a float"
        );
        assert!(!ws.is_subtype(&Type::I64, &Type::I32));
        assert!(
            !ws.is_subtype(&Type::I64, &Type::F64),
            "past 2^53 an integer does not survive being a float"
        );
    }

    #[test]
    fn a_specific_integer_fits_wherever_that_integer_actually_fits() {
        let ws = ws();
        let small = Type::Literal {
            value: LiteralValue::Int(3),
        };
        let huge = Type::Literal {
            value: LiteralValue::Int(i64::MAX),
        };
        assert!(ws.is_subtype(&small, &Type::F64));
        assert!(
            !ws.is_subtype(&huge, &Type::F64),
            "this particular integer does not survive being a float"
        );
    }

    #[test]
    fn absence_is_accepted_only_where_it_is_allowed() {
        let ws = ws();
        assert!(ws.is_subtype(&Type::I64, &Type::I64.optional()));
        assert!(ws.is_subtype(&Type::Unit, &Type::I64.optional()));
        assert!(!ws.is_subtype(&Type::I64.optional(), &Type::I64));
        assert!(ws.is_subtype(&Type::I32.optional(), &Type::I64.optional()));
    }

    #[test]
    fn containers_follow_their_contents() {
        let ws = ws();
        assert!(ws.is_subtype(&ty("Circle").list(), &ty("Shape").list()));
        assert!(!ws.is_subtype(&ty("Shape").list(), &ty("Circle").list()));
        assert!(ws.is_subtype(&ty("Circle").future(), &ty("Shape").future()));
        assert!(
            !ws.is_subtype(&ty("Circle"), &ty("Circle").future()),
            "a value is not a promise of itself"
        );
    }

    #[test]
    fn a_function_may_accept_more_and_return_less() {
        let ws = ws();
        let takes_shape = Type::Function {
            params: vec![ty("Shape")],
            ret: Box::new(ty("Circle")),
        };
        let takes_circle = Type::Function {
            params: vec![ty("Circle")],
            ret: Box::new(ty("Shape")),
        };
        assert!(ws.is_subtype(&takes_shape, &takes_circle));
        assert!(!ws.is_subtype(&takes_circle, &takes_shape));
    }

    #[test]
    fn a_union_fits_only_where_all_of_it_fits() {
        let ws = ws();
        let shapes = crate::canon::normalize_type(&Type::Union {
            members: vec![ty("Circle"), ty("Rect")],
        });
        assert!(ws.is_subtype(&shapes, &ty("Shape")));
        assert!(ws.is_subtype(&ty("Circle"), &shapes));
        assert!(!ws.is_subtype(&ty("Shape"), &shapes));
    }

    #[test]
    fn an_alias_is_only_a_name() {
        let ws = ws();
        assert!(ws.is_subtype(&ty("Id"), &Type::String));
        assert!(ws.is_subtype(&Type::String, &ty("Id")));
    }

    #[test]
    fn a_recursive_type_settles_instead_of_chasing_itself() {
        let ws = ws();
        assert!(ws.is_subtype(&ty("Tree"), &ty("Tree")));
        assert!(!ws.is_subtype(&ty("Tree"), &ty("Shape")));
    }

    /// A declaration applied to two arguments.
    fn both(name: &str, narrow: Type, wide: Type) -> (Type, Type) {
        (
            ty(name).with_args(vec![narrow]),
            ty(name).with_args(vec![wide]),
        )
    }

    #[test]
    fn a_generic_that_holds_its_argument_narrows_with_it() {
        let ws = ws();
        let (ints, longs) = both("Box", Type::I32, Type::I64);
        assert!(ws.is_subtype(&ints, &longs));
        assert!(!ws.is_subtype(&longs, &ints));
        let (circles, shapes) = both("Box", ty("Circle"), ty("Shape"));
        assert!(ws.is_subtype(&circles, &shapes));
        assert!(!ws.is_subtype(&shapes, &circles));
    }

    #[test]
    fn a_generic_that_takes_its_argument_widens_against_it() {
        let ws = ws();
        let (for_circles, for_shapes) = both("Sink", ty("Circle"), ty("Shape"));
        assert!(
            ws.is_subtype(&for_shapes, &for_circles),
            "something that takes any shape takes a circle"
        );
        assert!(!ws.is_subtype(&for_circles, &for_shapes));
    }

    #[test]
    fn a_generic_that_uses_its_argument_both_ways_does_not_vary() {
        let ws = ws();
        let (circles, shapes) = both("Cell", ty("Circle"), ty("Shape"));
        assert!(ws.is_subtype(&circles, &circles));
        assert!(!ws.is_subtype(&circles, &shapes));
        assert!(!ws.is_subtype(&shapes, &circles));
    }

    #[test]
    fn an_argument_used_nowhere_accepts_anything() {
        let ws = ws();
        let (numbers, strings) = both("Tag", Type::I64, Type::String);
        assert!(ws.is_subtype(&numbers, &strings));
        assert!(ws.is_subtype(&strings, &numbers));
    }

    #[test]
    fn a_recursive_generic_narrows_through_itself() {
        let ws = ws();
        let (circles, shapes) = both("Branch", ty("Circle"), ty("Shape"));
        assert!(ws.is_subtype(&circles, &shapes));
        assert!(!ws.is_subtype(&shapes, &circles));
    }

    #[test]
    fn a_mutually_recursive_pair_varies_once_it_settles() {
        let ws = ws();
        let (pong_circles, pong_shapes) = both("Pong", ty("Circle"), ty("Shape"));
        assert!(ws.is_subtype(&pong_circles, &pong_shapes));
        let (ping_circles, ping_shapes) = both("Ping", ty("Circle"), ty("Shape"));
        assert!(ws.is_subtype(&ping_shapes, &ping_circles));
        assert!(!ws.is_subtype(&ping_circles, &ping_shapes));
    }

    #[test]
    fn a_generic_child_fits_a_parent_at_a_wider_argument() {
        let ws = ws();
        let circles = ty("Labelled").with_args(vec![ty("Circle")]);
        let (_, shapes) = both("Box", ty("Circle"), ty("Shape"));
        assert!(ws.is_subtype(&circles, &shapes));
        let (_, numbers) = both("Box", Type::I32, Type::I64);
        assert!(!ws.is_subtype(&circles, &numbers));
    }

    #[test]
    fn an_enum_relates_only_to_itself() {
        let ws = ws();
        assert!(ws.is_subtype(&ty("Color"), &ty("Color")));
        assert!(!ws.is_subtype(&ty("Color"), &Type::String));
    }

    #[test]
    fn subtyping_is_reflexive_and_transitive_over_the_hierarchy() {
        let ws = ws();
        let chain = [ty("Circle"), ty("Shape"), ty("Identified")];
        for t in &chain {
            assert!(ws.is_subtype(t, t));
        }
        assert!(ws.is_subtype(&chain[0], &chain[1]));
        assert!(ws.is_subtype(&chain[1], &chain[2]));
        assert!(ws.is_subtype(&chain[0], &chain[2]));
    }

    /// Helper for building an applied generic in tests.
    trait WithArgs {
        fn with_args(self, args: Vec<Type>) -> Type;
    }
    impl WithArgs for Type {
        fn with_args(self, args: Vec<Type>) -> Type {
            match self {
                Type::Ref(r) => Type::Ref(TypeRef { args, ..r }),
                other => other,
            }
        }
    }
}
