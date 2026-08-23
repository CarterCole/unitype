//! Applying propositions: whether a call is well-typed, and what it yields.
//!
//! Generic inference here is deliberately shallow. A parameter's binding is
//! read off the shape of the argument, and everything is then checked by
//! subtyping — there is no unification, no constraint solving, no backtracking.
//! When that is not enough, the caller says what the type arguments are. A type
//! system that data has to travel through should be predictable rather than
//! clever; an argument that fails should fail for a reason you can see.
//!
//! Futures are not awaited implicitly. A signature returning `Future<T>`
//! returns `Future<T>`, and threading that through a pipeline is a decision the
//! runtime makes rather than one the type system makes silently.

// Call errors name every type involved, which makes them large. They are built
// once, at the point of failure, and read by a person; the size is not a cost
// worth trading a clear message for.
#![allow(clippy::result_large_err)]

use crate::check::CheckedWorkspace;
use crate::check::error::describe;
use crate::check::subst::{Subst, bind, substitute};
use crate::ident::QualifiedName;
use crate::ir::{FunctionSig, Type};

/// Why a call or a pipeline does not type-check.
#[derive(thiserror::Error, Debug, Clone, PartialEq)]
pub enum CallError {
    /// The wrong number of arguments.
    #[error("`{sig}` takes {expected} argument(s), but {got} were given")]
    Arity {
        /// The signature being called.
        sig: QualifiedName,
        /// How many it takes.
        expected: usize,
        /// How many were given.
        got: usize,
    },
    /// The wrong number of type arguments.
    #[error("`{sig}` takes {expected} type argument(s), but {got} were given")]
    TypeArity {
        /// The signature being called.
        sig: QualifiedName,
        /// How many it takes.
        expected: usize,
        /// How many were given.
        got: usize,
    },
    /// An argument was not of a usable type.
    #[error("argument {index} of `{sig}`: `{found}` cannot be used where `{required}` is expected")]
    Argument {
        /// The signature being called.
        sig: QualifiedName,
        /// Which argument, counting from zero.
        index: usize,
        /// The type supplied.
        found: String,
        /// The type required, after type arguments were applied.
        required: String,
    },
    /// A generic parameter could not be worked out from the arguments.
    #[error(
        "cannot tell what `{param}` is in `{sig}` from the arguments; give the type arguments explicitly"
    )]
    Ambiguous {
        /// The signature being called.
        sig: QualifiedName,
        /// The parameter that stayed unknown.
        param: String,
    },
    /// A pipeline stage could not take what the one before it produced.
    #[error("`{from}` produces `{produced}`, which `{to}` cannot accept as `{accepted}`")]
    PipelineMismatch {
        /// The earlier stage.
        from: QualifiedName,
        /// The later stage.
        to: QualifiedName,
        /// What the earlier stage produces.
        produced: String,
        /// What the later stage accepts.
        accepted: String,
    },
    /// A stage after the first took more than one argument.
    #[error(
        "`{sig}` takes {got} arguments, so it cannot be a pipeline stage: only the first stage may take more than one"
    )]
    PipelineArity {
        /// The offending stage.
        sig: QualifiedName,
        /// How many arguments it takes.
        got: usize,
    },
    /// A pipeline with no stages.
    #[error("a pipeline needs at least one stage")]
    EmptyPipeline,
}

impl CheckedWorkspace {
    /// Checks a call and returns what it yields.
    ///
    /// Pass `explicit` to state the type arguments rather than have them worked
    /// out from the arguments.
    pub fn check_call(
        &self,
        name: &QualifiedName,
        args: &[Type],
        explicit: Option<&[Type]>,
    ) -> Result<Type, CallError> {
        let sig = self.sig(name).ok_or_else(|| CallError::Arity {
            sig: name.clone(),
            expected: 0,
            got: args.len(),
        })?;

        if sig.params.len() != args.len() {
            return Err(CallError::Arity {
                sig: name.clone(),
                expected: sig.params.len(),
                got: args.len(),
            });
        }

        let subst = match explicit {
            Some(supplied) => {
                if supplied.len() != sig.params_decl.len() {
                    return Err(CallError::TypeArity {
                        sig: name.clone(),
                        expected: sig.params_decl.len(),
                        got: supplied.len(),
                    });
                }
                bind(&sig.params_decl, supplied)
            }
            None => self.infer_type_arguments(name, sig, args)?,
        };

        for (index, (param, arg)) in sig.params.iter().zip(args).enumerate() {
            let required = substitute(&param.ty, &subst);
            if !self.is_subtype(arg, &required) {
                return Err(CallError::Argument {
                    sig: name.clone(),
                    index,
                    found: describe(arg),
                    required: describe(&required),
                });
            }
        }

        Ok(substitute(&sig.ret, &subst))
    }

    /// Reads each generic parameter's binding off the shape of the arguments.
    fn infer_type_arguments(
        &self,
        name: &QualifiedName,
        sig: &FunctionSig,
        args: &[Type],
    ) -> Result<Subst, CallError> {
        let mut subst = Subst::new();
        for (param, arg) in sig.params.iter().zip(args) {
            self.match_shape(&param.ty, arg, &mut subst);
        }
        for decl in &sig.params_decl {
            if !subst.contains_key(&decl.name) {
                return Err(CallError::Ambiguous {
                    sig: name.clone(),
                    param: decl.name.to_string(),
                });
            }
        }
        Ok(subst)
    }

    /// Walks a parameter's type alongside the argument's, binding parameters as
    /// they line up.
    ///
    /// Where the shapes diverge, matching simply stops: the subtyping check
    /// that follows will report the mismatch with the types the caller actually
    /// wrote, which reads better than anything this could say.
    fn match_shape(&self, pattern: &Type, actual: &Type, subst: &mut Subst) {
        let pattern = self.expand_alias(pattern);
        let actual = self.expand_alias(actual);

        if let Type::Param { name } = &pattern {
            // First binding wins. A second, different one is not reconciled
            // here; the subtyping check reports it, and the caller can always
            // say what they meant with explicit type arguments.
            subst.entry(name.clone()).or_insert(actual);
            return;
        }

        // An argument may be more specific than the pattern — a list of circles
        // against a list of shapes — so only matching constructors line up.
        if std::mem::discriminant(&pattern) != std::mem::discriminant(&actual) {
            return;
        }
        let (want, have) = (pattern.children(), actual.children());
        if want.len() != have.len() {
            return;
        }
        for (w, h) in want.into_iter().zip(have) {
            self.match_shape(w, h, subst);
        }
    }

    /// Checks that each stage can take what the one before it produced, and
    /// returns the function type of the whole pipeline.
    pub fn check_pipeline(&self, stages: &[QualifiedName]) -> Result<Type, CallError> {
        let [first, rest @ ..] = stages else {
            return Err(CallError::EmptyPipeline);
        };

        let missing = |name: &QualifiedName| CallError::Arity {
            sig: name.clone(),
            expected: 0,
            got: 0,
        };
        let head = self.sig(first).ok_or_else(|| missing(first))?;
        let params: Vec<Type> = head.params.iter().map(|p| p.ty.clone()).collect();
        let mut carried = head.ret.clone();
        let mut previous = first;

        for name in rest {
            let stage = self.sig(name).ok_or_else(|| missing(name))?;
            let [only] = stage.params.as_slice() else {
                return Err(CallError::PipelineArity {
                    sig: name.clone(),
                    got: stage.params.len(),
                });
            };
            if !self.is_subtype(&carried, &only.ty) {
                return Err(CallError::PipelineMismatch {
                    from: previous.clone(),
                    to: name.clone(),
                    produced: describe(&carried),
                    accepted: describe(&only.ty),
                });
            }
            carried = stage.ret.clone();
            previous = name;
        }

        Ok(Type::Function {
            params,
            ret: Box::new(carried),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Workspace;
    use crate::check::check_workspace;

    const WS: &str = r#"{"universes":[{"name":"demo","types":[
        {"k":"struct","name":"Shape","fields":[{"name":"id","ty":"str"}]},
        {"k":"struct","name":"Circle","extends":"Shape","fields":[{"name":"r","ty":"float"}]}],
      "functions":[
        {"name":"sort","params":[{"name":"xs","ty":"[int]"}],"ret":"[int]"},
        {"name":"count","params":[{"name":"xs","ty":"[int]"}],"ret":"int"},
        {"name":"label","params":[{"name":"n","ty":"int"}],"ret":"str"},
        {"name":"area","params":[{"name":"s","ty":"Shape"}],"ret":"float"},
        {"name":"first","params_decl":[{"name":"T"}],
         "params":[{"name":"xs","ty":{"k":"list","of":{"k":"param","name":"T"}}}],
         "ret":{"k":"optional","of":{"k":"param","name":"T"}}},
        {"name":"empty","params_decl":[{"name":"T"}],"params":[],
         "ret":{"k":"list","of":{"k":"param","name":"T"}}},
        {"name":"load","params":[{"name":"id","ty":"str"}],
         "ret":{"k":"future","of":"Shape"}},
        {"name":"pair","params":[{"name":"a","ty":"int"},{"name":"b","ty":"str"}],
         "ret":"bool"}]}]}"#;

    fn ws() -> crate::check::CheckedWorkspace {
        check_workspace(Workspace::from_json(WS).expect("reads")).expect("checks")
    }

    fn q(s: &str) -> QualifiedName {
        s.parse().unwrap()
    }

    fn circle(ws: &crate::check::CheckedWorkspace) -> Type {
        ws.def(&q("demo::Circle")).expect("declared");
        Type::Ref(crate::ir::TypeRef::qualified(
            crate::Ident::new("demo").unwrap(),
            crate::Ident::new("Circle").unwrap(),
        ))
    }

    #[test]
    fn a_well_typed_call_yields_the_return_type() {
        let ws = ws();
        let got = ws
            .check_call(&q("demo::sort"), &[Type::I64.list()], None)
            .expect("checks");
        assert_eq!(got, Type::I64.list());
    }

    #[test]
    fn an_argument_may_be_more_specific_than_the_parameter() {
        let ws = ws();
        assert!(
            ws.check_call(&q("demo::area"), &[circle(&ws)], None)
                .is_ok(),
            "a circle is a shape"
        );
    }

    #[test]
    fn the_wrong_number_of_arguments_is_reported() {
        let ws = ws();
        let err = ws
            .check_call(&q("demo::pair"), &[Type::I64], None)
            .unwrap_err();
        assert!(
            matches!(
                err,
                CallError::Arity {
                    expected: 2,
                    got: 1,
                    ..
                }
            ),
            "{err}"
        );
    }

    #[test]
    fn an_argument_of_the_wrong_type_names_both_types() {
        let ws = ws();
        let err = ws
            .check_call(&q("demo::sort"), &[Type::String.list()], None)
            .unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("argument 0"), "{msg}");
        assert!(msg.contains("string") && msg.contains("i64"), "{msg}");
    }

    #[test]
    fn a_generic_parameter_is_read_off_the_argument() {
        let ws = ws();
        let got = ws
            .check_call(&q("demo::first"), &[Type::String.list()], None)
            .expect("checks");
        assert_eq!(got, Type::String.optional());
    }

    #[test]
    fn a_generic_nothing_pins_down_asks_for_type_arguments() {
        let ws = ws();
        let err = ws.check_call(&q("demo::empty"), &[], None).unwrap_err();
        assert!(
            err.to_string()
                .contains("give the type arguments explicitly"),
            "{err}"
        );
    }

    #[test]
    fn stating_the_type_arguments_resolves_what_inference_cannot() {
        let ws = ws();
        let got = ws
            .check_call(&q("demo::empty"), &[], Some(&[Type::Bool]))
            .expect("checks");
        assert_eq!(got, Type::Bool.list());
    }

    #[test]
    fn the_wrong_number_of_type_arguments_is_reported() {
        let ws = ws();
        let err = ws
            .check_call(&q("demo::empty"), &[], Some(&[Type::Bool, Type::I64]))
            .unwrap_err();
        assert!(matches!(err, CallError::TypeArity { .. }), "{err}");
    }

    #[test]
    fn a_pipeline_that_lines_up_gives_the_whole_function_type() {
        let ws = ws();
        let got = ws
            .check_pipeline(&[q("demo::sort"), q("demo::count"), q("demo::label")])
            .expect("checks");
        assert_eq!(
            got,
            Type::Function {
                params: vec![Type::I64.list()],
                ret: Box::new(Type::String),
            }
        );
    }

    #[test]
    fn a_pipeline_that_does_not_line_up_says_where() {
        let ws = ws();
        let err = ws
            .check_pipeline(&[q("demo::sort"), q("demo::label")])
            .unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("demo::sort") && msg.contains("demo::label"),
            "{msg}"
        );
    }

    #[test]
    fn only_the_first_stage_may_take_several_arguments() {
        let ws = ws();
        let err = ws
            .check_pipeline(&[q("demo::sort"), q("demo::pair")])
            .unwrap_err();
        assert!(matches!(err, CallError::PipelineArity { .. }), "{err}");
    }

    #[test]
    fn a_promise_is_not_quietly_awaited() {
        let ws = ws();
        let err = ws
            .check_pipeline(&[q("demo::load"), q("demo::area")])
            .unwrap_err();
        assert!(
            err.to_string().contains("future"),
            "awaiting is the runtime's decision, not a silent coercion: {err}"
        );
    }

    #[test]
    fn an_empty_pipeline_is_refused() {
        assert!(matches!(
            ws().check_pipeline(&[]),
            Err(CallError::EmptyPipeline)
        ));
    }
}
