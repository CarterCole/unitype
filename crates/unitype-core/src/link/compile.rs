//! Turning a link into something that runs.
//!
//! A derived link says *what* corresponds to what. Compiling turns that into an
//! expression body — the same total language a person writes a proof in — so a
//! bridge is not a special case anywhere downstream. The interpreter runs it
//! like any other body, every backend transpiles it like any other body, and
//! the checker type-checks it like any other body.
//!
//! That is the whole reason the expression language exists: one mechanism,
//! whether the mapping was written by a person or worked out by the system.

use crate::check::CheckedWorkspace;
use crate::ident::{Ident, QualifiedName};
use crate::ir::{Expr, FieldInit, FunctionSig, NamedParam, Type, TypeRef};
use crate::link::{Coercion, Link, LinkKind};
use crate::meta::Metadata;

/// How far a chain of links may be followed before giving up.
///
/// Links that reference each other in a loop are refused by the checker, so
/// reaching this means something pathological.
const MAX_LINK_DEPTH: usize = 32;

/// The name bound to the value a bridge is converting.
const SUBJECT: &str = "value";

/// Why a link could not be turned into a body.
#[derive(thiserror::Error, Debug, Clone, PartialEq, Eq)]
pub enum CompileError {
    /// An endpoint does not exist.
    #[error("`{0}` is not a declaration in this workspace")]
    UnknownEndpoint(QualifiedName),
    /// An endpoint has no fields to map.
    #[error("`{0}` has no fields, so there is nothing to build")]
    NotAStruct(QualifiedName),
    /// A nested conversion referred to a link that is not there.
    #[error("this conversion goes through link `{0}`, which is not declared")]
    MissingLink(Ident),
    /// Links referred to each other too deeply.
    #[error("following this conversion did not terminate")]
    TooDeep,
}

impl CheckedWorkspace {
    /// The signature a link stands for: a function from its source to its
    /// target, carrying the body that performs the conversion.
    ///
    /// A `Witness` link already names a signature someone wrote, so there is
    /// nothing to compile and this returns `None`.
    pub fn compile_link(&self, link: &Link) -> Option<Result<FunctionSig, CompileError>> {
        if matches!(link.kind, LinkKind::Witness { .. }) {
            return None;
        }
        Some(self.build_bridge(link))
    }

    fn build_bridge(&self, link: &Link) -> Result<FunctionSig, CompileError> {
        let subject = Ident::new_unchecked(SUBJECT);
        let body = self.apply_link(
            link,
            Expr::Var {
                name: subject.clone(),
            },
            0,
        )?;

        Ok(FunctionSig {
            name: link.name.clone(),
            doc: Some(format!(
                "Converts a `{}` into a `{}`. Derived from link `{}`.",
                link.from, link.to, link.name
            )),
            params_decl: Vec::new(),
            params: vec![NamedParam::new(subject, named(&link.from))],
            ret: named(&link.to),
            refines: None,
            body: Some(body),
            meta: Metadata::default(),
        })
    }

    /// The expression that converts `subject` according to `link`.
    pub(crate) fn apply_link(
        &self,
        link: &Link,
        subject: Expr,
        depth: usize,
    ) -> Result<Expr, CompileError> {
        if depth > MAX_LINK_DEPTH {
            return Err(CompileError::TooDeep);
        }

        // A witnessed conversion is somebody's function; call it.
        if let LinkKind::Witness { sig } = &link.kind {
            return Ok(Expr::Call {
                sig: sig.clone(),
                args: vec![subject],
            });
        }

        let target = self
            .fields_by_name(&link.to)
            .ok_or_else(|| CompileError::NotAStruct(link.to.clone()))?;
        if self.def(&link.from).is_none() {
            return Err(CompileError::UnknownEndpoint(link.from.clone()));
        }

        // Identical structures still have to be *built* as the target type:
        // nothing crosses a universe boundary by reinterpretation. Treating it
        // as a mapping of each field to itself keeps one code path.
        let pairs: Vec<(Ident, Ident, Coercion)> = match &link.kind {
            LinkKind::Identical => target
                .iter()
                .map(|f| (f.name.clone(), f.name.clone(), Coercion::Identity))
                .collect(),
            LinkKind::FieldMap { pairs, .. } => pairs
                .iter()
                .map(|p| (p.from.clone(), p.to.clone(), p.coercion.clone()))
                .collect(),
            LinkKind::Witness { .. } => unreachable!("handled above"),
        };

        let mut fields = Vec::with_capacity(pairs.len());
        for (from, to, coercion) in pairs {
            let read = Expr::Field {
                of: Box::new(subject.clone()),
                name: from,
            };
            fields.push(FieldInit::new(to, self.apply(&coercion, read, depth)?));
        }

        Ok(Expr::MakeRecord {
            ty: ref_to(&link.to),
            fields,
        })
    }

    /// The expression that applies one conversion to a value.
    fn apply(&self, coercion: &Coercion, value: Expr, depth: usize) -> Result<Expr, CompileError> {
        Ok(match coercion {
            // Both are no-ops on the data itself. A target language where the
            // two types differ emits a cast; the value is the same value.
            Coercion::Identity | Coercion::WidenInt => value,

            Coercion::WrapOptional { of } => self.apply(of, value, depth)?,

            Coercion::Through { functor, of } => {
                let it = Ident::new_unchecked("it");
                Expr::MapThrough {
                    functor: *functor,
                    value: Box::new(value),
                    var: it.clone(),
                    body: Box::new(self.apply(of, Expr::Var { name: it }, depth)?),
                }
            }

            Coercion::ViaLink { link } => {
                let nested = self
                    .workspace()
                    .link(link)
                    .ok_or_else(|| CompileError::MissingLink(link.clone()))?;
                self.apply_link(nested, value, depth + 1)?
            }
        })
    }
}

fn named(name: &QualifiedName) -> Type {
    Type::Ref(ref_to(name))
}

fn ref_to(name: &QualifiedName) -> TypeRef {
    TypeRef::qualified(name.universe.clone(), name.name.clone())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Workspace;
    use crate::check::check_workspace;
    use crate::ir::Functor;
    use crate::value::Value;

    fn ws(json: &str) -> CheckedWorkspace {
        check_workspace(Workspace::from_json(json).expect("reads")).expect("checks")
    }

    fn compiled(ws: &CheckedWorkspace, name: &str) -> FunctionSig {
        let link = ws
            .workspace()
            .link(&Ident::new(name).unwrap())
            .expect("declared");
        ws.compile_link(link).expect("compiles").expect("succeeds")
    }

    /// Runs the compiled body far enough to see the shape it builds, which is
    /// what a backend and the interpreter both walk.
    fn built_fields(sig: &FunctionSig) -> Vec<String> {
        let Some(Expr::MakeRecord { fields, .. }) = &sig.body else {
            panic!("expected a record to be built, got {:?}", sig.body);
        };
        fields.iter().map(|f| f.name.to_string()).collect()
    }

    const RENAMED: &str = r#"{"universes":[
        {"name":"api","types":[{"k":"struct","name":"User","fields":[
            {"name":"userId","ty":"str"},{"name":"age","ty":"i32"}]}]},
        {"name":"db","types":[{"k":"struct","name":"UserRow","fields":[
            {"name":"user_id","ty":"str"},{"name":"age","ty":"i64"}]}]}]}"#;

    fn with_derived_link(json: &str, from: &str, to: &str, name: &str) -> CheckedWorkspace {
        let base = ws(json);
        let kind = base
            .derive_link(&from.parse().unwrap(), &to.parse().unwrap(), &[])
            .expect("derives");
        let mut workspace = base.workspace().clone();
        workspace.links.push(Link {
            name: Ident::new(name).unwrap(),
            doc: None,
            from: from.parse().unwrap(),
            to: to.parse().unwrap(),
            kind,
            derived: true,
            meta: Metadata::default(),
        });
        check_workspace(workspace).expect("checks with the link")
    }

    #[test]
    fn a_mapping_becomes_a_signature_from_one_universe_to_the_other() {
        let ws = with_derived_link(RENAMED, "api::User", "db::UserRow", "user_row");
        let sig = compiled(&ws, "user_row");

        assert_eq!(sig.params.len(), 1);
        assert_eq!(
            crate::canon::canonical_string(&sig.params[0].ty),
            crate::canon::canonical_string(&named(&"api::User".parse().unwrap()))
        );
        assert_eq!(built_fields(&sig), ["user_id", "age"]);
    }

    #[test]
    fn the_compiled_body_type_checks_like_any_other() {
        let ws = with_derived_link(RENAMED, "api::User", "db::UserRow", "user_row");
        let sig = compiled(&ws, "user_row");

        // Put the derived signature back into the workspace and check it: a
        // bridge is not a special case, so it has to survive the same pass.
        let mut workspace = ws.workspace().clone();
        workspace.universes[0].functions.push(sig);
        assert!(
            check_workspace(workspace).is_ok(),
            "a derived bridge should be an ordinary well-typed body"
        );
    }

    #[test]
    fn identical_structures_are_still_built_rather_than_reinterpreted() {
        let ws = with_derived_link(
            r#"{"universes":[
                {"name":"geo","types":[{"k":"struct","name":"Point","fields":[
                    {"name":"x","ty":"float"},{"name":"y","ty":"float"}]}]},
                {"name":"gfx","types":[{"k":"struct","name":"Vec2","fields":[
                    {"name":"x","ty":"float"},{"name":"y","ty":"float"}]}]}]}"#,
            "geo::Point",
            "gfx::Vec2",
            "point_to_vec2",
        );
        let sig = compiled(&ws, "point_to_vec2");
        assert_eq!(
            built_fields(&sig),
            ["x", "y"],
            "nothing crosses a boundary by reinterpretation, even when the bytes match"
        );
    }

    #[test]
    fn a_list_conversion_becomes_a_mapping_over_the_list() {
        let ws = with_derived_link(
            r#"{"universes":[
                {"name":"a","types":[
                    {"k":"struct","name":"Point","fields":[{"name":"x","ty":"i32"}]},
                    {"k":"struct","name":"Path","fields":[{"name":"points","ty":"[Point]"}]}]},
                {"name":"b","types":[
                    {"k":"struct","name":"Vec2","fields":[
                        {"name":"x","ty":"i32"},{"name":"label","ty":"str?"}]},
                    {"k":"struct","name":"Line","fields":[{"name":"points","ty":"[Vec2]"}]}]}],
              "links":[{"name":"pt","from":"a::Point","to":"b::Vec2",
                        "kind":{"k":"field_map","pairs":[
                            {"from":"x","to":"x","coercion":{"k":"identity"}}]}}]}"#,
            "a::Path",
            "b::Line",
            "path_to_line",
        );
        let sig = compiled(&ws, "path_to_line");
        let Some(Expr::MakeRecord { fields, .. }) = &sig.body else {
            panic!("expected a record");
        };
        assert!(
            matches!(
                fields[0].value,
                Expr::MapThrough {
                    functor: Functor::List,
                    ..
                }
            ),
            "each point has to be converted, so the list is mapped: {:?}",
            fields[0].value
        );
    }

    #[test]
    fn a_conversion_inside_optionality_maps_through_it() {
        let ws = with_derived_link(
            r#"{"universes":[
                {"name":"a","types":[
                    {"k":"struct","name":"Point","fields":[{"name":"x","ty":"i32"}]},
                    {"k":"struct","name":"Pin","fields":[{"name":"at","ty":"Point?"}]}]},
                {"name":"b","types":[
                    {"k":"struct","name":"Vec2","fields":[
                        {"name":"x","ty":"i32"},{"name":"label","ty":"str?"}]},
                    {"k":"struct","name":"Marker","fields":[{"name":"at","ty":"Vec2?"}]}]}],
              "links":[{"name":"pt","from":"a::Point","to":"b::Vec2",
                        "kind":{"k":"field_map","pairs":[
                            {"from":"x","to":"x","coercion":{"k":"identity"}}]}}]}"#,
            "a::Pin",
            "b::Marker",
            "pin_to_marker",
        );
        let sig = compiled(&ws, "pin_to_marker");
        let Some(Expr::MakeRecord { fields, .. }) = &sig.body else {
            panic!("expected a record");
        };
        assert!(
            matches!(
                fields[0].value,
                Expr::MapThrough {
                    functor: Functor::Optional,
                    ..
                }
            ),
            "absent should stay absent: {:?}",
            fields[0].value
        );
    }

    #[test]
    fn a_witnessed_conversion_has_nothing_to_compile() {
        let ws = ws(r#"{"universes":[
            {"name":"a","types":[{"k":"struct","name":"X","fields":[{"name":"id","ty":"str"}]}],
             "functions":[{"name":"to_y","params":[{"name":"x","ty":"X"}],"ret":"b::Y"}]},
            {"name":"b","types":[{"k":"struct","name":"Y","fields":[{"name":"id","ty":"str"}]}]}],
          "links":[{"name":"x_to_y","from":"a::X","to":"b::Y",
                    "kind":{"k":"witness","sig":"a::to_y"}}]}"#);

        let link = ws.workspace().link(&Ident::new("x_to_y").unwrap()).unwrap();
        assert!(
            ws.compile_link(link).is_none(),
            "somebody already wrote this one"
        );
    }

    #[test]
    fn a_conversion_through_a_link_that_is_not_there_is_reported() {
        let ws = ws(r#"{"universes":[
            {"name":"a","types":[{"k":"struct","name":"X","fields":[{"name":"id","ty":"str"}]}]},
            {"name":"b","types":[{"k":"struct","name":"Y","fields":[{"name":"id","ty":"str"}]}]}]}"#);

        let link = Link {
            name: Ident::new("broken").unwrap(),
            doc: None,
            from: "a::X".parse().unwrap(),
            to: "b::Y".parse().unwrap(),
            kind: LinkKind::FieldMap {
                pairs: vec![crate::link::FieldPair {
                    from: Ident::new("id").unwrap(),
                    to: Ident::new("id").unwrap(),
                    coercion: Coercion::ViaLink {
                        link: Ident::new("nowhere").unwrap(),
                    },
                }],
                dropped: vec![],
            },
            derived: false,
            meta: Metadata::default(),
        };
        assert_eq!(
            ws.compile_link(&link),
            Some(Err(CompileError::MissingLink(
                Ident::new("nowhere").unwrap()
            )))
        );
    }

    #[test]
    fn the_body_reads_from_the_value_it_was_given() {
        let ws = with_derived_link(RENAMED, "api::User", "db::UserRow", "user_row");
        let sig = compiled(&ws, "user_row");
        let Some(Expr::MakeRecord { fields, .. }) = &sig.body else {
            panic!("expected a record");
        };
        let Expr::Field { of, name } = &fields[0].value else {
            panic!("expected a field read, got {:?}", fields[0].value);
        };
        assert_eq!(name.as_str(), "userId");
        assert_eq!(
            **of,
            Expr::Var {
                name: Ident::new(SUBJECT).unwrap()
            }
        );
        let _ = Value::Null;
    }
}
