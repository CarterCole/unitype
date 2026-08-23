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

use crate::check::CheckedWorkspace;
use crate::ident::{Ident, QualifiedName};
use crate::ir::{Field, Functor, Type, TypeRef};
use crate::link::{Coercion, LinkKind};

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
                let link = self
                    .workspace()
                    .links
                    .iter()
                    .find(|l| l.from == from && l.to == to)?;
                Some(Coercion::ViaLink {
                    link: link.name.clone(),
                })
            }

            _ => self.is_subtype(&have, &want).then_some(Coercion::Identity),
        }
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

        let LinkKind::FieldMap { pairs, dropped } = ws
            .derive_link(&q("api::User"), &q("db::UserRow"), &[])
            .expect("derives")
        else {
            panic!("expected a field mapping");
        };
        assert_eq!(pairs.len(), 2);
        assert!(dropped.is_empty());
        let by_target: Vec<(&str, &str)> = pairs
            .iter()
            .map(|p| (p.from.as_str(), p.to.as_str()))
            .collect();
        assert!(by_target.contains(&("userId", "user_id")), "{by_target:?}");
    }

    #[test]
    fn a_correspondence_nobody_could_guess_can_be_declared() {
        let ws = ws(r#"{"universes":[
            {"name":"a","types":[{"k":"struct","name":"X","fields":[
                {"name":"handle","ty":"str"}]}]},
            {"name":"b","types":[{"k":"struct","name":"Y","fields":[
                {"name":"nickname","ty":"str"}]}]}]}"#);

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
                link: id("point_to_vec2")
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
