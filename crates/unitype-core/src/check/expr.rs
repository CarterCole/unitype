//! Type-checking constructive proofs.
//!
//! A signature carrying a body claims to prove itself. This pass checks the
//! claim: the body has to be well-typed, and it has to produce something usable
//! where the signature's return type is expected.
//!
//! It also checks that the bodies as a whole terminate. Each body may call
//! other signatures, and the ones that call each other in a loop would never
//! finish; a topological sort of the call graph among body-carrying signatures
//! rules that out. Together with the expression language having no recursion
//! and no unbounded loops of its own, that makes every body total — which is
//! what lets a body be trusted as a proof, and lets it be transpiled into a
//! language that offers no such guarantee.

use std::collections::HashMap;

use crate::check::error::{CheckErrorKind, Diagnostics, PathSeg, TypePath, describe};
use crate::check::{CheckedWorkspace, SigIx};
use crate::ident::{Ident, QualifiedName};
use crate::ir::{Expr, Functor, Type, TypeDef};
use crate::value::{infer, unify};

/// Names bound where an expression appears.
type Env = HashMap<Ident, Type>;

/// Checks every body in the workspace, and that the bodies terminate.
pub(crate) fn verify(ws: &CheckedWorkspace, diags: &mut Diagnostics) {
    for ix in (0..ws.sigs.len()).map(SigIx) {
        let sig = ws.sig_at(ix);
        let Some(body) = &sig.body else {
            continue;
        };
        let name = ws.sig_name(ix);
        let path = TypePath::root()
            .then(PathSeg::Universe(name.universe.clone()))
            .then(PathSeg::Sig(name.name.clone()))
            .then(PathSeg::Body);

        let env: Env = sig
            .params
            .iter()
            .map(|p| (p.name.clone(), p.ty.clone()))
            .collect();

        match ws.type_of(body, &env, &path, diags) {
            Some(produced) if !ws.is_subtype(&produced, &sig.ret) => diags.error(
                CheckErrorKind::BodyTypeMismatch {
                    found: describe(&produced),
                    required: describe(&sig.ret),
                },
                path,
            ),
            _ => {}
        }
    }

    detect_cycles(ws, diags);
}

impl CheckedWorkspace {
    /// The type an expression produces, or `None` if it is not well-typed.
    ///
    /// Problems are reported into `diags` as they are found; returning `None`
    /// means "already reported, and there is nothing further to say about the
    /// enclosing expression".
    fn type_of(
        &self,
        expr: &Expr,
        env: &Env,
        path: &TypePath,
        diags: &mut Diagnostics,
    ) -> Option<Type> {
        match expr {
            Expr::Lit { value } => Some(infer(value)),

            Expr::Var { name } => match env.get(name) {
                Some(ty) => Some(ty.clone()),
                None => {
                    diags.error(CheckErrorKind::UnboundVar(name.clone()), path.clone());
                    None
                }
            },

            Expr::Field { of, name } => {
                let subject = self.type_of(of, env, path, diags)?;
                let fields = self.readable_fields(&subject)?;
                match fields.iter().find(|f| &f.name == name) {
                    Some(field) => Some(field.ty.clone()),
                    None => {
                        diags.error(
                            CheckErrorKind::NoSuchField {
                                ty: describe(&subject),
                                field: name.clone(),
                            },
                            path.clone(),
                        );
                        None
                    }
                }
            }

            Expr::MakeRecord { ty, fields } => {
                let target = self.ref_target(ty)?;
                let Some(def) = self.def(&target).and_then(TypeDef::as_struct) else {
                    diags.error(
                        CheckErrorKind::NotConstructible {
                            name: target,
                            what: "a struct",
                        },
                        path.clone(),
                    );
                    return None;
                };
                let _ = def;

                let declared = self.fields_of(ty).unwrap_or_default();
                for init in fields {
                    let at = path.then(PathSeg::Field(init.name.clone()));
                    let Some(field) = declared.iter().find(|f| f.name == init.name) else {
                        diags.error(
                            CheckErrorKind::NoSuchField {
                                ty: target.to_string(),
                                field: init.name.clone(),
                            },
                            at,
                        );
                        continue;
                    };
                    if let Some(found) = self.type_of(&init.value, env, &at, diags) {
                        if !self.is_subtype(&found, &field.ty) {
                            diags.error(
                                CheckErrorKind::FieldInitMismatch {
                                    field: init.name.clone(),
                                    found: describe(&found),
                                    required: describe(&field.ty),
                                },
                                at,
                            );
                        }
                    }
                }

                for field in &declared {
                    let supplied = fields.iter().any(|i| i.name == field.name);
                    // A field that may be absent need not be given a value.
                    let optional = matches!(
                        self.expand_alias(&field.ty),
                        Type::Optional { .. } | Type::Unit | Type::Any
                    );
                    if !supplied && !optional {
                        diags.error(
                            CheckErrorKind::MissingFieldInit {
                                name: target.clone(),
                                field: field.name.clone(),
                            },
                            path.clone(),
                        );
                    }
                }

                Some(Type::Ref(ty.clone()))
            }

            Expr::MakeList { items } => {
                let types: Vec<Type> = items
                    .iter()
                    .filter_map(|i| self.type_of(i, env, path, diags))
                    .collect();
                if types.len() != items.len() {
                    return None;
                }
                Some(
                    types
                        .iter()
                        .fold(Type::Never, |acc, t| unify(&acc, t))
                        .list(),
                )
            }

            Expr::MakeTuple { items } => {
                let types: Vec<Type> = items
                    .iter()
                    .filter_map(|i| self.type_of(i, env, path, diags))
                    .collect();
                (types.len() == items.len()).then_some(Type::Tuple { items: types })
            }

            Expr::MakeVariant { ty, tag, value } => {
                let target = self.ref_target(ty)?;
                let Some(TypeDef::Enum(def)) = self.def(&target) else {
                    diags.error(
                        CheckErrorKind::NotConstructible {
                            name: target,
                            what: "an enum",
                        },
                        path.clone(),
                    );
                    return None;
                };
                let Some(variant) = def.variants.iter().find(|v| &v.name == tag) else {
                    diags.error(
                        CheckErrorKind::NoSuchVariant {
                            name: target,
                            tag: tag.clone(),
                        },
                        path.clone(),
                    );
                    return None;
                };

                let payload = variant.payload.clone();
                let found = match value {
                    Some(v) => self.type_of(v, env, path, diags)?,
                    None => Type::Unit,
                };
                if !self.is_subtype(&found, &payload) {
                    diags.error(
                        CheckErrorKind::FieldInitMismatch {
                            field: tag.clone(),
                            found: describe(&found),
                            required: describe(&payload),
                        },
                        path.clone(),
                    );
                }
                Some(Type::Ref(ty.clone()))
            }

            Expr::Call { sig, args } => {
                let types: Vec<Type> = args
                    .iter()
                    .filter_map(|a| self.type_of(a, env, path, diags))
                    .collect();
                if types.len() != args.len() {
                    return None;
                }
                match self.check_call(sig, &types, None) {
                    Ok(ret) => Some(ret),
                    Err(e) => {
                        diags.error(CheckErrorKind::BadCall(e.to_string()), path.clone());
                        None
                    }
                }
            }

            Expr::Let { name, value, body } => {
                let bound = self.type_of(value, env, path, diags)?;
                let mut inner = env.clone();
                inner.insert(name.clone(), bound);
                self.type_of(body, &inner, path, diags)
            }

            Expr::If {
                cond,
                then,
                otherwise,
            } => {
                if let Some(found) = self.type_of(cond, env, path, diags) {
                    if !self.is_subtype(&found, &Type::Bool) {
                        diags.error(
                            CheckErrorKind::ConditionNotBool {
                                found: describe(&found),
                            },
                            path.clone(),
                        );
                    }
                }
                // Either arm may run, so the result is whatever admits both.
                let a = self.type_of(then, env, path, diags)?;
                let b = self.type_of(otherwise, env, path, diags)?;
                Some(unify(&a, &b))
            }

            Expr::MapThrough {
                functor,
                value,
                var,
                body,
            } => {
                let subject = self.type_of(value, env, path, diags)?;
                let expanded = self.expand_alias(&subject);
                let Some((found, inside)) = Functor::split(&expanded) else {
                    diags.error(
                        CheckErrorKind::NotMappable {
                            found: describe(&subject),
                            wanted: functor.describe(),
                        },
                        path.clone(),
                    );
                    return None;
                };
                if found != *functor {
                    diags.error(
                        CheckErrorKind::NotMappable {
                            found: describe(&subject),
                            wanted: functor.describe(),
                        },
                        path.clone(),
                    );
                    return None;
                }
                let mut inner = env.clone();
                inner.insert(var.clone(), inside.clone());
                // Whatever the body produces comes back inside the same
                // constructor it went in through.
                Some(functor.apply(self.type_of(body, &inner, path, diags)?))
            }
        }
    }

    /// The fields that can be read off a type, whether it is a named
    /// declaration or an anonymous record.
    fn readable_fields(&self, ty: &Type) -> Option<Vec<crate::ir::Field>> {
        match self.expand_alias(ty) {
            Type::Record { fields } => Some(fields),
            Type::Ref(r) => self.fields_of(&r),
            // Reading a field off something that might be absent would need a
            // check first, which this language has no way to express.
            _ => None,
        }
    }
}

/// Bodies that call each other in a loop would never finish.
fn detect_cycles(ws: &CheckedWorkspace, diags: &mut Diagnostics) {
    // Only signatures that carry a body take part. A call to a signature proved
    // externally leaves the graph: whether *that* terminates is a question
    // about an implementation, not about these bodies.
    let with_bodies: HashMap<QualifiedName, SigIx> = (0..ws.sigs.len())
        .map(SigIx)
        .filter(|ix| ws.sig_at(*ix).body.is_some())
        .map(|ix| (ws.sig_name(ix).clone(), ix))
        .collect();

    let calls = |ix: SigIx| -> Vec<SigIx> {
        let Some(body) = &ws.sig_at(ix).body else {
            return Vec::new();
        };
        let mut out: Vec<SigIx> = body
            .called_signatures()
            .into_iter()
            .filter_map(|q| with_bodies.get(q).copied())
            .collect();
        out.sort();
        out.dedup();
        out
    };

    #[derive(Clone, Copy, PartialEq)]
    enum Mark {
        Open,
        Done,
    }
    let mut marks: HashMap<SigIx, Mark> = HashMap::new();

    fn walk(
        ws: &CheckedWorkspace,
        ix: SigIx,
        calls: &impl Fn(SigIx) -> Vec<SigIx>,
        marks: &mut HashMap<SigIx, Mark>,
        stack: &mut Vec<SigIx>,
        diags: &mut Diagnostics,
    ) {
        match marks.get(&ix) {
            Some(Mark::Done) => return,
            Some(Mark::Open) => {
                let start = stack.iter().position(|s| *s == ix).unwrap_or(0);
                let mut trail: Vec<String> = stack[start..]
                    .iter()
                    .map(|s| ws.sig_name(*s).to_string())
                    .collect();
                trail.push(ws.sig_name(ix).to_string());
                let name = ws.sig_name(ix);
                diags.error(
                    CheckErrorKind::BodyCycle(trail),
                    TypePath::root()
                        .then(PathSeg::Universe(name.universe.clone()))
                        .then(PathSeg::Sig(name.name.clone()))
                        .then(PathSeg::Body),
                );
                return;
            }
            None => {}
        }
        marks.insert(ix, Mark::Open);
        stack.push(ix);
        for next in calls(ix) {
            walk(ws, next, calls, marks, stack, diags);
        }
        stack.pop();
        marks.insert(ix, Mark::Done);
    }

    let mut starts: Vec<SigIx> = with_bodies.values().copied().collect();
    starts.sort();
    for ix in starts {
        walk(ws, ix, &calls, &mut marks, &mut Vec::new(), diags);
    }
}

#[cfg(test)]
mod tests {
    use crate::Workspace;
    use crate::check::{CheckedWorkspace, Diagnostics, check_workspace};

    fn check(json: &str) -> Result<CheckedWorkspace, Diagnostics> {
        check_workspace(Workspace::from_json(json).expect("reads"))
    }

    fn errors(json: &str) -> Vec<String> {
        check(json)
            .err()
            .map(|d| d.errors.iter().map(ToString::to_string).collect())
            .unwrap_or_default()
    }

    /// A universe with the types the body tests build from.
    const TYPES: &str = r#"
        {"k":"struct","name":"User","fields":[
            {"name":"first","ty":"str"},{"name":"last","ty":"str"},
            {"name":"age","ty":"i64"}]},
        {"k":"struct","name":"Badge","fields":[
            {"name":"label","ty":"str"},{"name":"adult","ty":"bool"},
            {"name":"note","ty":"str?"}]},
        {"k":"enum","name":"Color","variants":[{"name":"Red"},{"name":"Custom","payload":"str"}]}"#;

    fn universe(functions: &str) -> String {
        format!(
            r#"{{"universes":[{{"name":"demo","types":[{TYPES}],"functions":[{functions}]}}]}}"#
        )
    }

    #[test]
    fn a_body_that_builds_what_it_promised_checks() {
        let json = universe(
            r#"{"name":"badge_of","params":[{"name":"u","ty":"User"}],"ret":"Badge",
                "body":{"k":"make_record","ty":"Badge","fields":[
                    {"name":"label","value":{"k":"field","of":{"k":"var","name":"u"},"name":"first"}},
                    {"name":"adult","value":{"k":"lit","value":true}}]}}"#,
        );
        assert!(errors(&json).is_empty(), "{:?}", errors(&json));
    }

    #[test]
    fn a_body_that_builds_the_wrong_thing_is_reported() {
        let json = universe(
            r#"{"name":"badge_of","params":[{"name":"u","ty":"User"}],"ret":"Badge",
                "body":{"k":"var","name":"u"}}"#,
        );
        let msgs = errors(&json);
        assert!(
            msgs.iter().any(|m| m.contains("does not produce")),
            "{msgs:?}"
        );
    }

    #[test]
    fn a_name_nothing_bound_is_reported() {
        let json =
            universe(r#"{"name":"f","params":[],"ret":"str","body":{"k":"var","name":"nope"}}"#);
        assert!(
            errors(&json)[0].contains("nothing bound"),
            "{:?}",
            errors(&json)
        );
    }

    #[test]
    fn reading_a_field_a_type_does_not_have_is_reported() {
        let json = universe(
            r#"{"name":"f","params":[{"name":"u","ty":"User"}],"ret":"str",
                "body":{"k":"field","of":{"k":"var","name":"u"},"name":"middle"}}"#,
        );
        assert!(
            errors(&json)[0].contains("no field `middle`"),
            "{:?}",
            errors(&json)
        );
    }

    #[test]
    fn a_field_left_unset_is_reported_unless_it_may_be_absent() {
        let json = universe(
            r#"{"name":"f","params":[],"ret":"Badge",
                "body":{"k":"make_record","ty":"Badge","fields":[
                    {"name":"label","value":{"k":"lit","value":"x"}}]}}"#,
        );
        let msgs = errors(&json);
        assert!(
            msgs.iter().any(|m| m.contains("`adult`")),
            "adult is required: {msgs:?}"
        );
        assert!(
            !msgs.iter().any(|m| m.contains("`note`")),
            "note may be absent: {msgs:?}"
        );
    }

    #[test]
    fn a_field_set_to_the_wrong_type_is_reported() {
        let json = universe(
            r#"{"name":"f","params":[],"ret":"Badge",
                "body":{"k":"make_record","ty":"Badge","fields":[
                    {"name":"label","value":{"k":"lit","value":1}},
                    {"name":"adult","value":{"k":"lit","value":true}}]}}"#,
        );
        assert!(
            errors(&json).iter().any(|m| m.contains("cannot be used")),
            "{:?}",
            errors(&json)
        );
    }

    #[test]
    fn a_binding_is_visible_in_what_follows_it() {
        let json = universe(
            r#"{"name":"f","params":[{"name":"u","ty":"User"}],"ret":"str",
                "body":{"k":"let","name":"n",
                        "value":{"k":"field","of":{"k":"var","name":"u"},"name":"first"},
                        "body":{"k":"var","name":"n"}}}"#,
        );
        assert!(errors(&json).is_empty(), "{:?}", errors(&json));
    }

    #[test]
    fn mapping_through_a_list_binds_its_elements() {
        let json = universe(
            r#"{"name":"names","params":[{"name":"us","ty":"[User]"}],"ret":"[str]",
                "body":{"k":"map_through","functor":"list","value":{"k":"var","name":"us"},
                        "var":"u",
                        "body":{"k":"field","of":{"k":"var","name":"u"},"name":"first"}}}"#,
        );
        assert!(errors(&json).is_empty(), "{:?}", errors(&json));
    }

    #[test]
    fn mapping_through_a_constructor_the_value_lacks_is_reported() {
        let json = universe(
            r#"{"name":"f","params":[{"name":"u","ty":"User"}],"ret":"[str]",
                "body":{"k":"map_through","functor":"list","value":{"k":"var","name":"u"},
                        "var":"x","body":{"k":"var","name":"x"}}}"#,
        );
        assert!(
            errors(&json)[0].contains("nothing to map through"),
            "{:?}",
            errors(&json)
        );
    }

    #[test]
    fn both_arms_of_a_conditional_contribute_to_the_result() {
        let json = universe(
            r#"{"name":"f","params":[{"name":"u","ty":"User"}],"ret":"str",
                "body":{"k":"if","cond":{"k":"lit","value":true},
                        "then":{"k":"field","of":{"k":"var","name":"u"},"name":"first"},
                        "else":{"k":"lit","value":"anonymous"}}}"#,
        );
        assert!(errors(&json).is_empty(), "{:?}", errors(&json));
    }

    #[test]
    fn a_condition_that_is_not_a_boolean_is_reported() {
        let json = universe(
            r#"{"name":"f","params":[],"ret":"str",
                "body":{"k":"if","cond":{"k":"lit","value":1},
                        "then":{"k":"lit","value":"a"},"else":{"k":"lit","value":"b"}}}"#,
        );
        assert!(
            errors(&json)[0].contains("condition"),
            "{:?}",
            errors(&json)
        );
    }

    #[test]
    fn a_body_may_call_another_signature() {
        let json = universe(
            r#"{"name":"label_of","params":[{"name":"u","ty":"User"}],"ret":"str",
                "body":{"k":"field","of":{"k":"var","name":"u"},"name":"first"}},
               {"name":"badge_of","params":[{"name":"u","ty":"User"}],"ret":"Badge",
                "body":{"k":"make_record","ty":"Badge","fields":[
                    {"name":"label","value":{"k":"call","sig":"demo::label_of",
                                             "args":[{"k":"var","name":"u"}]}},
                    {"name":"adult","value":{"k":"lit","value":true}}]}}"#,
        );
        assert!(errors(&json).is_empty(), "{:?}", errors(&json));
    }

    #[test]
    fn bodies_that_call_each_other_in_a_loop_are_reported() {
        let json = universe(
            r#"{"name":"a","params":[],"ret":"str",
                "body":{"k":"call","sig":"demo::b","args":[]}},
               {"name":"b","params":[],"ret":"str",
                "body":{"k":"call","sig":"demo::a","args":[]}}"#,
        );
        assert!(
            errors(&json)
                .iter()
                .any(|m| m.contains("would never finish")),
            "{:?}",
            errors(&json)
        );
    }

    #[test]
    fn a_body_that_calls_itself_is_reported() {
        let json = universe(
            r#"{"name":"a","params":[],"ret":"str",
                "body":{"k":"call","sig":"demo::a","args":[]}}"#,
        );
        assert!(
            errors(&json)
                .iter()
                .any(|m| m.contains("would never finish")),
            "{:?}",
            errors(&json)
        );
    }

    #[test]
    fn calling_a_signature_proved_elsewhere_is_not_a_loop() {
        // `sort` has no body: whether it terminates is a question about its
        // implementations, not about this body.
        let json = universe(
            r#"{"name":"sort","params":[{"name":"xs","ty":"[i64]"}],"ret":"[i64]"},
               {"name":"sort_twice","params":[{"name":"xs","ty":"[i64]"}],"ret":"[i64]",
                "body":{"k":"call","sig":"demo::sort","args":[
                    {"k":"call","sig":"demo::sort","args":[{"k":"var","name":"xs"}]}]}}"#,
        );
        assert!(errors(&json).is_empty(), "{:?}", errors(&json));
    }

    #[test]
    fn building_a_variant_of_an_enum_checks_its_payload() {
        let good = universe(
            r#"{"name":"f","params":[],"ret":"Color",
                "body":{"k":"make_variant","ty":"Color","tag":"Custom",
                        "value":{"k":"lit","value":"teal"}}}"#,
        );
        assert!(errors(&good).is_empty(), "{:?}", errors(&good));

        let bad = universe(
            r#"{"name":"f","params":[],"ret":"Color",
                "body":{"k":"make_variant","ty":"Color","tag":"Mauve"}}"#,
        );
        assert!(errors(&bad)[0].contains("Mauve"), "{:?}", errors(&bad));
    }
}
