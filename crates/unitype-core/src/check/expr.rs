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
//!
//! A constant is typed as itself where it can be: `"circle"` is the literal
//! type `"circle"`, which is a `str` wherever a `str` is wanted, and which is
//! what lets a body compare a discriminator against the one value it may hold.

use std::collections::{HashMap, HashSet};

use crate::canon::normalize_type;
use crate::check::error::{CheckErrorKind, Diagnostics, PathSeg, TypePath, describe};
use crate::check::{CheckedWorkspace, SigIx};
use crate::ident::{Ident, QualifiedName};
use crate::ir::{Expr, Functor, LiteralValue, PrimOp, Type, TypeDef};
use crate::value::{Value, infer, unify};

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
            Expr::Lit { value } => Some(match value {
                Value::Bool(b) => Type::Literal {
                    value: LiteralValue::Bool(*b),
                },
                Value::Int(i) => Type::Literal {
                    value: LiteralValue::Int(*i),
                },
                Value::Str(s) => Type::Literal {
                    value: LiteralValue::Str(s.clone()),
                },
                other => infer(other),
            }),

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

            Expr::Prim { op, args } => self.type_of_prim(*op, args, env, path, diags),

            Expr::Fold {
                list,
                init,
                ty,
                acc,
                item,
                body,
            } => {
                let subject = self.type_of(list, env, path, diags)?;
                let Type::List { of: element } = self.expand_alias(&subject) else {
                    diags.error(
                        CheckErrorKind::CannotEliminate {
                            found: describe(&subject),
                            wanted: "a list".into(),
                        },
                        path.clone(),
                    );
                    return None;
                };
                let start = self.type_of(init, env, path, diags)?;
                let carried = match ty {
                    Some(declared) => {
                        if !self.is_subtype(&start, declared) {
                            diags.error(
                                CheckErrorKind::AccumulatorMismatch {
                                    what: "the starting value",
                                    found: describe(&start),
                                    required: describe(declared),
                                },
                                path.clone(),
                            );
                        }
                        declared.clone()
                    }
                    // A count that starts at `0` is counting integers, not
                    // holding the one value `0`.
                    None => widen(&start),
                };

                let mut inner = env.clone();
                inner.insert(acc.clone(), carried.clone());
                inner.insert(item.clone(), *element);
                let step = self.type_of(body, &inner, path, diags)?;
                if !self.is_subtype(&step, &carried) {
                    diags.error(
                        CheckErrorKind::AccumulatorMismatch {
                            what: "each step",
                            found: describe(&step),
                            required: describe(&carried),
                        },
                        path.clone(),
                    );
                }
                Some(carried)
            }

            Expr::Case {
                value,
                var,
                some,
                none,
            } => {
                let subject = self.type_of(value, env, path, diags)?;
                let inside = match self.expand_alias(&subject) {
                    Type::Optional { of } => *of,
                    // Always absent, so the present arm binds something that
                    // can never be there.
                    Type::Unit => Type::Never,
                    _ => {
                        diags.error(
                            CheckErrorKind::CannotEliminate {
                                found: describe(&subject),
                                wanted: "something that may be absent".into(),
                            },
                            path.clone(),
                        );
                        return None;
                    }
                };
                let mut inner = env.clone();
                inner.insert(var.clone(), inside);
                // Both arms are checked before either result is used, so a
                // mistake in the second is not hidden by one in the first.
                let present = self.type_of(some, &inner, path, diags);
                let absent = self.type_of(none, env, path, diags);
                Some(unify(&present?, &absent?))
            }

            Expr::Match { ty, value, arms } => {
                let target = self.ref_target(ty)?;
                if self.def(&target).is_none() {
                    diags.error(CheckErrorKind::UnknownType(target), path.clone());
                    return None;
                }
                let Some(variants) = self.variants_of(ty) else {
                    diags.error(
                        CheckErrorKind::CannotEliminate {
                            found: target.to_string(),
                            wanted: "an enum".into(),
                        },
                        path.clone(),
                    );
                    return None;
                };
                let subject = self.type_of(value, env, path, diags)?;
                if !self.is_subtype(&subject, &Type::Ref(ty.clone())) {
                    diags.error(
                        CheckErrorKind::CannotEliminate {
                            found: describe(&subject),
                            wanted: format!("a `{target}`"),
                        },
                        path.clone(),
                    );
                    return None;
                }

                let mut covered: HashSet<&Ident> = HashSet::new();
                let mut result = Some(Type::Never);
                for arm in arms {
                    let Some(variant) = variants.iter().find(|v| v.name == arm.tag) else {
                        diags.error(
                            CheckErrorKind::NoSuchVariant {
                                name: target.clone(),
                                tag: arm.tag.clone(),
                            },
                            path.clone(),
                        );
                        result = None;
                        continue;
                    };
                    if !covered.insert(&arm.tag) {
                        diags.error(CheckErrorKind::DuplicateArm(arm.tag.clone()), path.clone());
                        result = None;
                        continue;
                    }
                    let mut inner = env.clone();
                    if let Some(var) = &arm.var {
                        if matches!(variant.payload, Type::Unit) {
                            diags.error(
                                CheckErrorKind::NothingToBind(arm.tag.clone()),
                                path.clone(),
                            );
                            result = None;
                            continue;
                        }
                        inner.insert(var.clone(), variant.payload.clone());
                    }
                    // Any arm may be the one that runs, so the result is
                    // whatever admits them all.
                    match self.type_of(&arm.body, &inner, path, diags) {
                        Some(produced) => result = result.map(|r| unify(&r, &produced)),
                        None => result = None,
                    }
                }

                let missing: Vec<Ident> = variants
                    .iter()
                    .filter(|v| !covered.contains(&v.name))
                    .map(|v| v.name.clone())
                    .collect();
                if !missing.is_empty() {
                    diags.error(
                        CheckErrorKind::NonExhaustiveMatch {
                            name: target,
                            missing,
                        },
                        path.clone(),
                    );
                    return None;
                }
                result
            }
        }
    }

    /// The type an operation produces, or `None` if its operands do not suit
    /// it.
    fn type_of_prim(
        &self,
        op: PrimOp,
        args: &[Expr],
        env: &Env,
        path: &TypePath,
        diags: &mut Diagnostics,
    ) -> Option<Type> {
        if args.len() != op.arity() {
            diags.error(
                CheckErrorKind::PrimArity {
                    op: op.token(),
                    expected: op.arity(),
                    got: args.len(),
                },
                path.clone(),
            );
            return None;
        }
        let types: Vec<Type> = args
            .iter()
            .filter_map(|a| self.type_of(a, env, path, diags))
            .collect();
        if types.len() != args.len() {
            return None;
        }

        let mut refuse = |wanted: &'static str| {
            diags.error(
                CheckErrorKind::BadOperands {
                    op: op.token(),
                    found: types
                        .iter()
                        .map(describe)
                        .collect::<Vec<_>>()
                        .join("` and `"),
                    wanted,
                },
                path.clone(),
            );
            None
        };
        let kinds: Vec<Option<Operand>> = types.iter().map(|t| self.operand(t)).collect();

        match op {
            PrimOp::Eq | PrimOp::Ne => {
                let (a, b) = (&types[0], &types[1]);
                // Comparing two things that can never be equal is a mistake
                // worth saying so about, not a question with the answer no.
                if self.is_subtype(a, b) || self.is_subtype(b, a) {
                    Some(Type::Bool)
                } else {
                    refuse("two things of the same type")
                }
            }
            _ if op.is_ordering() => match (kinds[0], kinds[1]) {
                (Some(x), Some(y)) if x == y => Some(Type::Bool),
                _ => refuse("two numbers of the same kind, or two strings"),
            },
            PrimOp::Add | PrimOp::Sub | PrimOp::Mul => match (kinds[0], kinds[1]) {
                (Some(Operand::Int), Some(Operand::Int)) => Some(Type::I64),
                (Some(Operand::Float), Some(Operand::Float)) => Some(Type::F64),
                _ => refuse("two integers or two floats"),
            },
            PrimOp::Neg => match kinds[0] {
                Some(Operand::Int) => Some(Type::I64),
                Some(Operand::Float) => Some(Type::F64),
                _ => refuse("a number"),
            },
            PrimOp::And | PrimOp::Or => {
                if types.iter().all(|t| self.is_subtype(t, &Type::Bool)) {
                    Some(Type::Bool)
                } else {
                    refuse("two booleans")
                }
            }
            PrimOp::Not => {
                if self.is_subtype(&types[0], &Type::Bool) {
                    Some(Type::Bool)
                } else {
                    refuse("a boolean")
                }
            }
            PrimOp::Len => match self.expand_alias(&types[0]) {
                Type::List { .. } => Some(Type::I64),
                _ => refuse("a list"),
            },
            // Every ordering was handled above.
            PrimOp::Lt | PrimOp::Le | PrimOp::Gt | PrimOp::Ge => unreachable!(),
        }
    }

    /// What kind of thing an operand is, for the operations that care.
    ///
    /// Integers and floats are kept apart: `x + 1` on a float is a mistake in
    /// the body, not something to widen silently, because the backends would
    /// each widen it differently.
    fn operand(&self, ty: &Type) -> Option<Operand> {
        if self.is_subtype(ty, &Type::I64) {
            Some(Operand::Int)
        } else if self.is_subtype(ty, &Type::F64) {
            Some(Operand::Float)
        } else if self.is_subtype(ty, &Type::String) {
            Some(Operand::Str)
        } else {
            None
        }
    }

    /// The fields that can be read off a type, whether it is a named
    /// declaration or an anonymous record.
    fn readable_fields(&self, ty: &Type) -> Option<Vec<crate::ir::Field>> {
        match self.expand_alias(ty) {
            Type::Record { fields } => Some(fields),
            Type::Ref(r) => self.fields_of(&r),
            // Something that might be absent has to be taken apart with a
            // `case` first, which says what to do when it is.
            _ => None,
        }
    }
}

/// The kinds of operand the operations distinguish.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Operand {
    Int,
    Float,
    Str,
}

/// A type with every literal in it replaced by the type it is a value of.
fn widen(ty: &Type) -> Type {
    let kids: Vec<Type> = ty.children().into_iter().map(widen).collect();
    normalize_type(&match ty.with_children(kids) {
        Type::Literal { value } => value.base(),
        other => other,
    })
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

    /// A body over one parameter of each kind the new forms take apart.
    fn over(ret: &str, body: &str) -> String {
        universe(&format!(
            r#"{{"name":"f","params":[{{"name":"xs","ty":"[i64]"}},{{"name":"u","ty":"User"}},
                                      {{"name":"b","ty":"Badge"}},{{"name":"c","ty":"Color"}}],
                "ret":{ret},"body":{body}}}"#
        ))
    }

    fn var(name: &str) -> String {
        format!(r#"{{"k":"var","name":"{name}"}}"#)
    }

    fn prim(op: &str, args: &[&str]) -> String {
        format!(r#"{{"k":"prim","op":"{op}","args":[{}]}}"#, args.join(","))
    }

    #[test]
    fn a_fold_carries_a_value_from_one_element_to_the_next() {
        let sum = over(
            r#""i64""#,
            &format!(
                r#"{{"k":"fold","list":{},"init":{{"k":"lit","value":0}},"acc":"total","item":"x",
                    "body":{}}}"#,
                var("xs"),
                prim("add", &[&var("total"), &var("x")])
            ),
        );
        assert!(errors(&sum).is_empty(), "{:?}", errors(&sum));
    }

    #[test]
    fn a_fold_that_changes_what_it_carries_is_reported() {
        let json = over(
            r#""i64""#,
            &format!(
                r#"{{"k":"fold","list":{},"init":{{"k":"lit","value":0}},"acc":"total","item":"x",
                    "body":{{"k":"lit","value":"oops"}}}}"#,
                var("xs")
            ),
        );
        let msgs = errors(&json);
        assert!(
            msgs.iter()
                .any(|m| m.contains("each step") && m.contains("i64")),
            "{msgs:?}"
        );
    }

    #[test]
    fn a_fold_may_say_what_it_carries_when_its_start_does_not() {
        // Starting from nothing, there is no type to read off the start.
        let json = over(
            r#""i64?""#,
            &format!(
                r#"{{"k":"fold","list":{},"init":{{"k":"lit","value":null}},"ty":"i64?",
                    "acc":"last","item":"x","body":{}}}"#,
                var("xs"),
                var("x")
            ),
        );
        assert!(errors(&json).is_empty(), "{:?}", errors(&json));
    }

    #[test]
    fn folding_something_that_is_not_a_list_is_reported() {
        let json = over(
            r#""i64""#,
            &format!(
                r#"{{"k":"fold","list":{},"init":{{"k":"lit","value":0}},"acc":"a","item":"x",
                    "body":{}}}"#,
                var("u"),
                var("a")
            ),
        );
        assert!(
            errors(&json)[0].contains("is not a list"),
            "{:?}",
            errors(&json)
        );
    }

    #[test]
    fn a_constant_is_its_own_type_so_it_can_be_compared_with_what_it_belongs_to() {
        let good = over(
            r#""bool""#,
            &prim(
                "eq",
                &[
                    r#"{"k":"field","of":{"k":"var","name":"u"},"name":"first"}"#,
                    r#"{"k":"lit","value":"Ada"}"#,
                ],
            ),
        );
        assert!(errors(&good).is_empty(), "{:?}", errors(&good));

        let bad = over(
            r#""bool""#,
            &prim(
                "eq",
                &[
                    r#"{"k":"field","of":{"k":"var","name":"u"},"name":"age"}"#,
                    r#"{"k":"lit","value":"Ada"}"#,
                ],
            ),
        );
        assert!(
            errors(&bad)[0].contains("two things of the same type"),
            "an integer is never a string: {:?}",
            errors(&bad)
        );
    }

    #[test]
    fn arithmetic_does_not_mix_integers_and_floats() {
        let json = over(
            r#""f64""#,
            &prim(
                "add",
                &[
                    r#"{"k":"field","of":{"k":"var","name":"u"},"name":"age"}"#,
                    r#"{"k":"lit","value":1.5}"#,
                ],
            ),
        );
        assert!(
            errors(&json)[0].contains("two integers or two floats"),
            "{:?}",
            errors(&json)
        );
    }

    #[test]
    fn an_operation_given_the_wrong_number_of_operands_is_reported() {
        let json = over(r#""bool""#, &prim("not", &[&var("xs"), &var("xs")]));
        assert!(
            errors(&json)[0].contains("takes 1 operand"),
            "{:?}",
            errors(&json)
        );
    }

    #[test]
    fn the_length_of_a_list_is_an_integer() {
        let json = over(
            r#""bool""#,
            &prim(
                "lt",
                &[&prim("len", &[&var("xs")]), r#"{"k":"lit","value":3}"#],
            ),
        );
        assert!(errors(&json).is_empty(), "{:?}", errors(&json));
    }

    #[test]
    fn a_case_says_what_to_do_whether_or_not_something_is_there() {
        let json = over(
            r#""str""#,
            r#"{"k":"case","value":{"k":"field","of":{"k":"var","name":"b"},"name":"note"},
                "var":"n","some":{"k":"var","name":"n"},"none":{"k":"lit","value":"no note"}}"#,
        );
        assert!(errors(&json).is_empty(), "{:?}", errors(&json));
    }

    #[test]
    fn a_case_on_something_that_is_always_there_is_reported() {
        let json = over(
            r#""str""#,
            r#"{"k":"case","value":{"k":"field","of":{"k":"var","name":"b"},"name":"label"},
                "var":"n","some":{"k":"var","name":"n"},"none":{"k":"lit","value":"-"}}"#,
        );
        assert!(
            errors(&json)[0].contains("may be absent"),
            "{:?}",
            errors(&json)
        );
    }

    fn matching(arms: &str) -> String {
        over(
            r#""str""#,
            &format!(
                r#"{{"k":"match","ty":"Color","value":{},"arms":[{arms}]}}"#,
                var("c")
            ),
        )
    }

    #[test]
    fn a_match_binds_the_payload_of_the_variant_it_found() {
        let json = matching(
            r#"{"tag":"Red","body":{"k":"lit","value":"red"}},
               {"tag":"Custom","var":"name","body":{"k":"var","name":"name"}}"#,
        );
        assert!(errors(&json).is_empty(), "{:?}", errors(&json));
    }

    #[test]
    fn a_match_that_leaves_a_variant_out_is_reported() {
        let json = matching(r#"{"tag":"Red","body":{"k":"lit","value":"red"}}"#);
        let msgs = errors(&json);
        assert!(
            msgs.iter().any(|m| m.contains("no arm for `Custom`")),
            "{msgs:?}"
        );
    }

    #[test]
    fn a_match_arm_for_something_the_enum_does_not_have_is_reported() {
        let json = matching(
            r#"{"tag":"Red","body":{"k":"lit","value":"red"}},
               {"tag":"Custom","var":"n","body":{"k":"var","name":"n"}},
               {"tag":"Mauve","body":{"k":"lit","value":"?"}}"#,
        );
        assert!(
            errors(&json).iter().any(|m| m.contains("Mauve")),
            "{:?}",
            errors(&json)
        );
    }

    #[test]
    fn a_match_arm_given_twice_is_reported() {
        let json = matching(
            r#"{"tag":"Red","body":{"k":"lit","value":"red"}},
               {"tag":"Red","body":{"k":"lit","value":"again"}},
               {"tag":"Custom","var":"n","body":{"k":"var","name":"n"}}"#,
        );
        assert!(
            errors(&json)
                .iter()
                .any(|m| m.contains("more than one arm")),
            "{:?}",
            errors(&json)
        );
    }

    #[test]
    fn binding_the_payload_of_a_variant_that_carries_nothing_is_reported() {
        let json = matching(
            r#"{"tag":"Red","var":"r","body":{"k":"lit","value":"red"}},
               {"tag":"Custom","var":"n","body":{"k":"var","name":"n"}}"#,
        );
        assert!(
            errors(&json).iter().any(|m| m.contains("carries nothing")),
            "{:?}",
            errors(&json)
        );
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
