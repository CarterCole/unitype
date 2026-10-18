//! Free theorems: what a generic proposition promises before any proof runs.
//!
//! A proof of `reverse<T> : [T] -> [T]` cannot look at a `T`. It was given no
//! way to — no comparison, no field, no method, nothing but the shape around
//! it — so whatever it does, it does the same way whatever `T` turns out to be.
//! That is a theorem about *every* proof of the proposition, and it follows
//! from the signature alone (Reynolds's abstraction theorem; Wadler's
//! "Theorems for free!"). Two consequences can be checked by running a proof:
//!
//! - **Conservation.** Every `T` in the result was one of the `T`s in the
//!   arguments. A proof cannot make one up, because it does not know what one
//!   looks like.
//! - **Naturality.** Relabelling the `T`s commutes with the proof: for any
//!   `g`, `f(map g xs) = map g (f xs)`. What the proof does is a function of
//!   shape, not of content.
//!
//! Neither needs anyone to say what the right answer is, and neither needs a
//! vector file. The harness fills each `T` position with a distinct token,
//! runs the proof, and reads off which token landed where. That checks
//! conservation outright, and it predicts what the proof *must* say at any
//! other type — so it runs again with the tokens relabelled: once as distinct
//! strings, once all onto a single boolean, and once onto two integers, and
//! checks that each answer is the first one relabelled the same way. A proof
//! that sorts when handed integers, or compares two elements and branches, or
//! only works at the one type its author had in mind, is caught by one of
//! those.
//!
//! One type is enough to test a parametric proof at, which is the practical
//! face of the theorem: there is no second type at which it could behave
//! differently, so there is no second type to try.
//!
//! # What this does not cover
//!
//! A bounded parameter — `T: Identified` — *can* be looked at, through the
//! bound, so a proof may lawfully treat one `T` differently from another, and
//! the theorem weakens to naturality over relabellings that respect the bound.
//! That is a later addition; here a bounded proposition is reported as not
//! checkable rather than held to a law it need not obey. The same goes for a
//! `T` under an untagged union, where a value does not say whether it is the
//! `T`, and for parameters whose types cannot be filled from nothing, such as
//! functions and interfaces.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use unitype_core::check::{CheckedWorkspace, bind, substitute};
use unitype_core::ir::{Field, FunctionSig, LiteralValue, Type, TypeDef, TypeRef, Variant};
use unitype_core::value::Value;
use unitype_core::{Ident, QualifiedName};

use crate::conform::{alike, unavailable};
use crate::dispatch::{Dispatcher, Policy};
use crate::implementation::ImplId;

/// How many shapes each proposition is tried at.
const TRIALS: usize = 5;
/// How long a list or map is in each trial.
const SIZES: [usize; TRIALS] = [0, 1, 2, 3, 5];
/// Tokens for one parameter are numbered from `param * TOKEN_STRIDE`, so a
/// token says which parameter it stands for as well as which occurrence.
const TOKEN_STRIDE: i64 = 1000;

/// One of the laws a parametric proof is held to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Law {
    /// Every `T` in the result came from the arguments.
    Conservation,
    /// Relabelling the `T`s commutes with the proof.
    Naturality,
}

impl fmt::Display for Law {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Law::Conservation => "conservation",
            Law::Naturality => "naturality",
        })
    }
}

/// What became of one proof on one trial.
#[derive(Clone, Debug, PartialEq)]
pub enum Verdict {
    /// It obeyed both laws.
    Held,
    /// It broke one.
    Broken {
        /// Which.
        law: Law,
        /// How, concretely.
        detail: String,
    },
    /// It ran and failed before anything could be checked.
    Failed(String),
    /// It could not be run here.
    Skipped(String),
}

impl Verdict {
    /// Whether this counts against the proof.
    pub fn is_against(&self) -> bool {
        matches!(self, Verdict::Broken { .. } | Verdict::Failed(_))
    }

    /// A short description, for a report.
    pub fn summary(&self) -> String {
        match self {
            Verdict::Held => "held".into(),
            Verdict::Broken { law, detail } => format!("{law} broken: {detail}"),
            Verdict::Failed(why) => format!("failed: {why}"),
            Verdict::Skipped(why) => format!("skipped: {why}"),
        }
    }
}

/// One shape the proposition was tried at.
#[derive(Clone, Debug)]
pub struct Trial {
    /// The arguments, written out, so a failure can be talked about.
    pub name: String,
    /// The arguments as given, with tokens where the `T`s are.
    pub args: Vec<Value>,
    /// What each proof did with them.
    pub verdicts: BTreeMap<ImplId, Verdict>,
}

impl Trial {
    /// Whether no proof broke a law here.
    pub fn held(&self) -> bool {
        self.verdicts.values().all(|v| !v.is_against())
    }
}

/// How a proposition's free theorem fared across every proof and trial.
#[derive(Clone, Debug)]
pub struct Theorems {
    /// The proposition.
    pub signature: QualifiedName,
    /// Its type parameters, in order.
    pub params: Vec<Ident>,
    /// Every proof that took part.
    pub proofs: Vec<ImplId>,
    /// Each shape tried.
    pub trials: Vec<Trial>,
}

impl Theorems {
    /// Whether every proof obeyed both laws on every trial.
    pub fn held(&self) -> bool {
        self.trials.iter().all(Trial::held)
    }

    /// The trials where some proof broke a law.
    pub fn broken(&self) -> Vec<&Trial> {
        self.trials.iter().filter(|t| !t.held()).collect()
    }

    /// The proposition with its parameters, as it would be said: `reverse<T>`.
    pub fn heading(&self) -> String {
        let params: Vec<String> = self.params.iter().map(ToString::to_string).collect();
        format!("{}<{}>", self.signature, params.join(", "))
    }
}

/// Why a proposition's free theorem could not be checked.
#[derive(thiserror::Error, Debug)]
pub enum ParametricError {
    /// The proposition is not in the workspace.
    #[error("`{0}` is not a proposition in this workspace")]
    UnknownSignature(QualifiedName),
    /// It has no type parameters, so there is nothing to be parametric in.
    #[error("`{0}` has no type parameters, so it has no free theorem")]
    NotGeneric(QualifiedName),
    /// A parameter is bounded, so a proof may lawfully look at one.
    #[error(
        "`{sig}` bounds `{param}` by `{bound}`, so a proof may look at one; \
         the free theorem for a bounded parameter is a later addition"
    )]
    Bounded {
        /// The proposition.
        sig: QualifiedName,
        /// The bounded parameter.
        param: Ident,
        /// What bounds it.
        bound: String,
    },
    /// Some part of the signature cannot be filled or read without help.
    #[error("`{sig}` cannot be checked this way: {what} {why}")]
    Unfillable {
        /// The proposition.
        sig: QualifiedName,
        /// The type in the way.
        what: String,
        /// Why it is in the way.
        why: String,
    },
    /// Nothing proves it, so there is nothing to hold to the theorem.
    #[error("nothing proves `{0}`, so there is nothing to check")]
    NoProofs(QualifiedName),
}

/// Whether a proposition has a free theorem this harness can check, and if
/// not, why not.
///
/// Answered without running anything, so a caller can decide what to try.
pub fn applies(ws: &CheckedWorkspace, name: &QualifiedName) -> Result<(), ParametricError> {
    let sig = ws
        .sig(name)
        .ok_or_else(|| ParametricError::UnknownSignature(name.clone()))?;
    plan(ws, name, sig).map(|_| ())
}

/// Holds every proof of a generic proposition to its free theorem.
pub async fn check(
    dispatcher: &Dispatcher,
    name: &QualifiedName,
) -> Result<Theorems, ParametricError> {
    let ws = dispatcher.workspace();
    let sig = ws
        .sig(name)
        .ok_or_else(|| ParametricError::UnknownSignature(name.clone()))?;
    let params = plan(ws, name, sig)?;

    let proofs: Vec<ImplId> = dispatcher
        .candidates(name, &Policy::Default)
        .iter()
        .map(|i| i.id.clone())
        .collect();
    if proofs.is_empty() {
        return Err(ParametricError::NoProofs(name.clone()));
    }

    let shape = Shape {
        ws,
        params: &params,
    };
    let ret = eventual(&sig.ret);
    let at_tokens = vec![Type::I64; params.len()];

    let mut trials = Vec::with_capacity(TRIALS);
    for trial in 0..TRIALS {
        let mut filler = shape.filler(trial);
        let mut args = Vec::with_capacity(sig.params.len());
        for param in &sig.params {
            args.push(filler.fill(&param.ty).map_err(|u| u.at(name))?);
        }
        let given = shape.tokens_given(&args, sig);

        let mut verdicts = BTreeMap::new();
        for id in &proofs {
            let verdict = judge(
                dispatcher, name, sig, &shape, &ret, &at_tokens, &args, &given, id,
            )
            .await;
            verdicts.insert(id.clone(), verdict);
        }

        trials.push(Trial {
            name: describe(sig, &args),
            args,
            verdicts,
        });
    }

    Ok(Theorems {
        signature: name.clone(),
        params,
        proofs,
        trials,
    })
}

/// The parameters a proposition is parametric in, or why it cannot be held to
/// its theorem.
fn plan(
    ws: &CheckedWorkspace,
    name: &QualifiedName,
    sig: &FunctionSig,
) -> Result<Vec<Ident>, ParametricError> {
    if sig.params_decl.is_empty() {
        return Err(ParametricError::NotGeneric(name.clone()));
    }
    for decl in &sig.params_decl {
        if let Some(bound) = decl.bounds.first() {
            return Err(ParametricError::Bounded {
                sig: name.clone(),
                param: decl.name.clone(),
                bound: bound.to_string(),
            });
        }
    }
    let params: Vec<Ident> = sig.params_decl.iter().map(|d| d.name.clone()).collect();
    let shape = Shape {
        ws,
        params: &params,
    };

    // Filling is deterministic in the type, so if the first trial can be
    // filled they all can.
    let mut filler = shape.filler(0);
    for param in &sig.params {
        filler.fill(&param.ty).map_err(|u| u.at(name))?;
    }
    shape
        .readable(&eventual(&sig.ret))
        .map_err(|u| u.at(name))?;
    Ok(params)
}

/// Runs one proof against one trial and says what it did.
#[allow(clippy::too_many_arguments)]
async fn judge(
    dispatcher: &Dispatcher,
    name: &QualifiedName,
    sig: &FunctionSig,
    shape: &Shape<'_>,
    ret: &Type,
    at_tokens: &[Type],
    args: &[Value],
    given: &[BTreeSet<i64>],
    id: &ImplId,
) -> Verdict {
    let pinned = Policy::Pinned(id.clone());

    let result = match dispatcher
        .call_at(name, at_tokens, args.to_vec(), &pinned)
        .await
    {
        Ok(invocation) => invocation.value,
        Err(error) if unavailable(&error) => return Verdict::Skipped(error.to_string()),
        Err(error) => return Verdict::Failed(error.to_string()),
    };

    // Conservation: every token that came back is one that went in, standing
    // for the same parameter.
    let mut stray = None;
    let read = shape.map_params(&result, ret, &mut |param, value| {
        let token = Token::decode(value);
        if !token.is_some_and(|t| t.param == param && given[param].contains(&t.ordinal))
            && stray.is_none()
        {
            stray = Some(format!(
                "a `{}` came back that was never passed in: {}",
                shape.params[param],
                json(value)
            ));
        }
        value.clone()
    });
    if let Err(why) = read {
        return Verdict::Failed(format!("could not read the result: {why}"));
    }
    if let Some(detail) = stray {
        return Verdict::Broken {
            law: Law::Conservation,
            detail,
        };
    }

    // Naturality: relabel the tokens, run again, and the proof must have
    // relabelled its own answer the same way.
    for relabel in [Relabel::Strings, Relabel::Collapse, Relabel::Parity] {
        let mut g = |param: usize, value: &Value| match Token::decode(value) {
            Some(token) => relabel.apply(&shape.params[param], token),
            None => value.clone(),
        };
        let mut relabelled = Vec::with_capacity(args.len());
        for (arg, param) in args.iter().zip(&sig.params) {
            match shape.map_params(arg, &param.ty, &mut g) {
                Ok(value) => relabelled.push(value),
                Err(why) => {
                    return Verdict::Failed(format!("could not relabel an argument: {why}"));
                }
            }
        }
        let expected = match shape.map_params(&result, ret, &mut g) {
            Ok(value) => value,
            Err(why) => return Verdict::Failed(format!("could not relabel the result: {why}")),
        };

        let at = vec![relabel.at(); shape.params.len()];
        match dispatcher.call_at(name, &at, relabelled, &pinned).await {
            Ok(invocation) if alike(&invocation.value, &expected, 0.0) => {}
            Ok(invocation) => {
                return Verdict::Broken {
                    law: Law::Naturality,
                    detail: format!(
                        "{}, it gave {}, but relabelling what it gave at {} requires {}",
                        relabel.describe(shape.params),
                        json(&invocation.value),
                        Relabel::tokens(shape.params),
                        json(&expected)
                    ),
                };
            }
            Err(error) if unavailable(&error) => return Verdict::Skipped(error.to_string()),
            Err(error) => {
                return Verdict::Broken {
                    law: Law::Naturality,
                    detail: format!("{}, {error}", relabel.describe(shape.params)),
                };
            }
        }
    }

    Verdict::Held
}

/// A proof's eventual answer: what a future resolves to, or the type itself.
fn eventual(ret: &Type) -> Type {
    match ret {
        Type::Future { of } => (**of).clone(),
        other => other.clone(),
    }
}

/// A trial, named by what was passed: `xs = [0, 1, 2]`.
fn describe(sig: &FunctionSig, args: &[Value]) -> String {
    if args.is_empty() {
        return "no arguments".into();
    }
    sig.params
        .iter()
        .zip(args)
        .map(|(p, a)| format!("{} = {}", p.name, json(a)))
        .collect::<Vec<_>>()
        .join(", ")
}

fn json(value: &Value) -> String {
    value
        .to_json_string()
        .unwrap_or_else(|_| "<unwritable>".into())
}

/// A stand-in for one `T`: distinct from every other, and saying which
/// parameter it stands for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Token {
    param: usize,
    ordinal: i64,
}

impl Token {
    fn value(self) -> Value {
        Value::Int(self.param as i64 * TOKEN_STRIDE + self.ordinal)
    }

    fn decode(value: &Value) -> Option<Token> {
        let n = value.as_int()?;
        (n >= 0).then_some(Token {
            param: (n / TOKEN_STRIDE) as usize,
            ordinal: n % TOKEN_STRIDE,
        })
    }
}

/// A renaming of every token, into a type that is not the tokens' own.
///
/// Naturality holds for *every* renaming, so three are sampled that a proof
/// could tell apart: one that keeps the tokens distinct, one that makes them
/// all the same, and one in between.
#[derive(Clone, Copy, Debug)]
enum Relabel {
    /// Each token to a distinct string: the same shape, at another type.
    Strings,
    /// Every token onto the one boolean `true`: a proof that compared two of
    /// them for equality and branched now takes the other branch.
    Collapse,
    /// Tokens onto `0` and `1` by parity: some pairs equal, some not, for a
    /// proof whose branch the other two happen to agree with.
    Parity,
}

impl Relabel {
    fn at(self) -> Type {
        match self {
            Relabel::Strings => Type::String,
            Relabel::Collapse => Type::Bool,
            Relabel::Parity => Type::I64,
        }
    }

    fn apply(self, param: &Ident, token: Token) -> Value {
        match self {
            Relabel::Strings => Value::Str(format!("{param}{}", token.ordinal)),
            Relabel::Collapse => Value::Bool(true),
            Relabel::Parity => Value::Int(token.ordinal % 2),
        }
    }

    /// `at T = str`, or `with every T made equal`.
    fn describe(self, params: &[Ident]) -> String {
        let names: Vec<String> = params.iter().map(ToString::to_string).collect();
        match self {
            Relabel::Strings => format!("at {} = str", names.join(" = ")),
            Relabel::Collapse => format!("with every {} made equal", names.join(" and ")),
            Relabel::Parity => format!("with {} collapsed onto two values", names.join(", ")),
        }
    }

    /// Where the tokens were: `T = i64`.
    fn tokens(params: &[Ident]) -> String {
        let names: Vec<String> = params.iter().map(ToString::to_string).collect();
        format!("{} = i64", names.join(" = "))
    }
}

/// A type that cannot be filled from nothing, or read back, and why.
#[derive(Debug)]
struct Unfillable {
    what: String,
    why: &'static str,
}

impl Unfillable {
    fn of(ty: &Type, why: &'static str) -> Self {
        Self {
            what: format!("`{ty}`"),
            why,
        }
    }

    fn at(self, sig: &QualifiedName) -> ParametricError {
        ParametricError::Unfillable {
            sig: sig.clone(),
            what: self.what,
            why: self.why.to_string(),
        }
    }
}

/// A declaration, seen through a reference to it with its arguments applied.
enum Resolved {
    Fields(Vec<Field>),
    Interface,
    Variants(Vec<Variant>),
    Alias(Type),
}

/// The signature's parameters, and the means to walk a type looking for them.
struct Shape<'a> {
    ws: &'a CheckedWorkspace,
    params: &'a [Ident],
}

impl<'a> Shape<'a> {
    fn filler(&self, trial: usize) -> Filler<'a, '_> {
        Filler {
            shape: self,
            trial,
            next: vec![0; self.params.len()],
        }
    }

    fn param(&self, name: &Ident) -> Option<usize> {
        self.params.iter().position(|p| p == name)
    }

    fn mentions_param(&self, ty: &Type) -> bool {
        match ty {
            Type::Param { name } => self.param(name).is_some(),
            other => other.children().into_iter().any(|c| self.mentions_param(c)),
        }
    }

    fn resolve(&self, r: &TypeRef) -> Result<Resolved, String> {
        let name = self
            .ws
            .ref_target_of(r)
            .ok_or_else(|| format!("`{r}` is not a qualified reference"))?;
        let def = self
            .ws
            .def(&name)
            .ok_or_else(|| format!("`{name}` is not declared"))?;
        let subst = bind(def.params(), &r.args);
        let fields = || -> Vec<Field> {
            self.ws
                .flat_fields(&name)
                .unwrap_or_default()
                .iter()
                .map(|f| Field {
                    name: f.name.clone(),
                    ty: substitute(&f.ty, &subst),
                    doc: None,
                })
                .collect()
        };
        Ok(match def {
            TypeDef::Alias(a) => Resolved::Alias(substitute(&a.target, &subst)),
            TypeDef::Struct(_) => Resolved::Fields(fields()),
            TypeDef::Interface(_) => Resolved::Interface,
            TypeDef::Enum(e) => Resolved::Variants(
                e.variants
                    .iter()
                    .map(|v| Variant {
                        name: v.name.clone(),
                        payload: substitute(&v.payload, &subst),
                        doc: None,
                    })
                    .collect(),
            ),
        })
    }

    /// Whether every `T` position in a result can be found from its type.
    fn readable(&self, ty: &Type) -> Result<(), Unfillable> {
        match ty {
            Type::Union { .. } if self.mentions_param(ty) => Err(Unfillable::of(
                ty,
                "puts a parameter under an untagged union, where a value does not \
                 say whether it is the parameter",
            )),
            Type::Function { .. } if self.mentions_param(ty) => Err(Unfillable::of(
                ty,
                "is a runtime handle, so what it does with a parameter cannot be seen",
            )),
            Type::Ref(r) if self.mentions_param(ty) => match self.resolve(r) {
                Ok(Resolved::Alias(target)) => self.readable(&target),
                Ok(Resolved::Fields(fields)) => {
                    fields.iter().try_for_each(|f| self.readable(&f.ty))
                }
                Ok(Resolved::Variants(variants)) => {
                    variants.iter().try_for_each(|v| self.readable(&v.payload))
                }
                Ok(Resolved::Interface) => Err(Unfillable::of(
                    ty,
                    "is an interface, whose fields do not say where a parameter is",
                )),
                Err(_) => Err(Unfillable::of(ty, "does not resolve")),
            },
            other => other
                .children()
                .into_iter()
                .try_for_each(|c| self.readable(c)),
        }
    }

    /// The tokens standing in each argument, per parameter.
    fn tokens_given(&self, args: &[Value], sig: &FunctionSig) -> Vec<BTreeSet<i64>> {
        let mut given = vec![BTreeSet::new(); self.params.len()];
        for (arg, param) in args.iter().zip(&sig.params) {
            // The arguments were built here, so they read back.
            let _ = self.map_params(arg, &param.ty, &mut |p, value| {
                if let Some(token) = Token::decode(value) {
                    given[p].insert(token.ordinal);
                }
                value.clone()
            });
        }
        given
    }

    /// Rebuilds `value` with every `T` position passed through `f`, following
    /// `ty` to know where those are.
    fn map_params(
        &self,
        value: &Value,
        ty: &Type,
        f: &mut dyn FnMut(usize, &Value) -> Value,
    ) -> Result<Value, String> {
        let mismatch = || format!("a {} is not a `{ty}`", value.kind_name());
        Ok(match ty {
            Type::Param { name } => match self.param(name) {
                Some(ix) => f(ix, value),
                None => value.clone(),
            },
            _ if !self.mentions_param(ty) => value.clone(),

            Type::Optional { of } => match value {
                Value::Null => Value::Null,
                present => self.map_params(present, of, f)?,
            },
            Type::Future { of } => self.map_params(value, of, f)?,
            Type::List { of } => {
                let Value::List(items) = value else {
                    return Err(mismatch());
                };
                Value::List(
                    items
                        .iter()
                        .map(|item| self.map_params(item, of, f))
                        .collect::<Result<_, _>>()?,
                )
            }
            Type::Map { value: of, .. } => {
                let Value::Map(entries) = value else {
                    return Err(mismatch());
                };
                Value::Map(
                    entries
                        .iter()
                        .map(|(k, v)| Ok((k.clone(), self.map_params(v, of, f)?)))
                        .collect::<Result<_, String>>()?,
                )
            }
            Type::Tuple { items: types } => {
                let Value::Tuple(items) = value else {
                    return Err(mismatch());
                };
                if items.len() != types.len() {
                    return Err(mismatch());
                }
                Value::Tuple(
                    items
                        .iter()
                        .zip(types)
                        .map(|(item, ty)| self.map_params(item, ty, f))
                        .collect::<Result<_, _>>()?,
                )
            }
            Type::Record { fields } => self.map_fields(value, fields, f)?,
            Type::Ref(r) => match self.resolve(r)? {
                Resolved::Alias(target) => self.map_params(value, &target, f)?,
                Resolved::Fields(fields) => self.map_fields(value, &fields, f)?,
                Resolved::Interface => {
                    return Err(format!(
                        "`{ty}` is an interface, which cannot be read through"
                    ));
                }
                Resolved::Variants(variants) => {
                    let Value::Variant { tag, value: inner } = value else {
                        return Err(mismatch());
                    };
                    let Some(variant) = variants.iter().find(|v| v.name.as_str() == tag) else {
                        return Err(format!("`{ty}` has no variant `{tag}`"));
                    };
                    Value::Variant {
                        tag: tag.clone(),
                        value: Box::new(self.map_params(inner, &variant.payload, f)?),
                    }
                }
            },
            Type::Union { .. } => {
                return Err(format!("`{ty}` puts a parameter under an untagged union"));
            }
            _ => return Err(mismatch()),
        })
    }

    fn map_fields(
        &self,
        value: &Value,
        fields: &[Field],
        f: &mut dyn FnMut(usize, &Value) -> Value,
    ) -> Result<Value, String> {
        let Value::Record(record) = value else {
            return Err(format!("a {} is not a record", value.kind_name()));
        };
        let mut out = record.clone();
        for (key, v) in record {
            if let Some(field) = fields.iter().find(|f| f.name.as_str() == key) {
                out.insert(key.clone(), self.map_params(v, &field.ty, f)?);
            }
        }
        Ok(Value::Record(out))
    }
}

/// Builds arguments for one trial: a fresh token at every `T`, and something
/// small and deterministic everywhere else.
struct Filler<'a, 's> {
    shape: &'s Shape<'a>,
    trial: usize,
    next: Vec<i64>,
}

impl Filler<'_, '_> {
    fn token(&mut self, param: usize) -> Value {
        let ordinal = self.next[param];
        self.next[param] += 1;
        Token { param, ordinal }.value()
    }

    fn fill(&mut self, ty: &Type) -> Result<Value, Unfillable> {
        let k = self.trial;
        Ok(match ty {
            Type::Param { name } => match self.shape.param(name) {
                Some(ix) => self.token(ix),
                None => return Err(Unfillable::of(ty, "is bound by nothing in scope")),
            },
            Type::Never => return Err(Unfillable::of(ty, "has no values")),
            Type::Any => Value::Int(k as i64),
            Type::Unit => Value::Null,
            Type::Bool => Value::Bool(k % 2 == 0),
            Type::I32 | Type::I64 => Value::Int(k as i64),
            Type::F64 => Value::Float(k as f64 + 0.5),
            Type::String => Value::Str(format!("s{k}")),
            Type::Bytes => Value::Bytes(vec![k as u8; k]),
            Type::Literal { value } => match value {
                LiteralValue::Bool(b) => Value::Bool(*b),
                LiteralValue::Int(i) => Value::Int(*i),
                LiteralValue::Str(s) => Value::Str(s.clone()),
            },
            Type::Optional { of } => {
                if k % 2 == 0 {
                    Value::Null
                } else {
                    self.fill(of)?
                }
            }
            Type::List { of } => Value::List(
                (0..SIZES[k])
                    .map(|_| self.fill(of))
                    .collect::<Result<_, _>>()?,
            ),
            Type::Map { key, value } => {
                // Keys have to be distinct, which only strings and integers
                // can manage in quantity.
                let entries = match key.as_ref() {
                    Type::String | Type::I32 | Type::I64 => SIZES[k],
                    _ => SIZES[k].min(1),
                };
                let mut out = Vec::with_capacity(entries);
                for j in 0..entries {
                    let k = match key.as_ref() {
                        Type::String => Value::Str(format!("k{j}")),
                        Type::I32 | Type::I64 => Value::Int(j as i64),
                        other => self.fill(other)?,
                    };
                    out.push((k, self.fill(value)?));
                }
                Value::Map(out)
            }
            Type::Tuple { items } => Value::Tuple(
                items
                    .iter()
                    .map(|t| self.fill(t))
                    .collect::<Result<_, _>>()?,
            ),
            Type::Record { fields } => self.fill_fields(fields)?,
            Type::Union { members } => {
                if self.shape.mentions_param(ty) {
                    return Err(Unfillable::of(
                        ty,
                        "puts a parameter under an untagged union, where a value does not \
                         say whether it is the parameter",
                    ));
                }
                match members.first() {
                    Some(first) => self.fill(first)?,
                    None => return Err(Unfillable::of(ty, "has no alternatives")),
                }
            }
            Type::Function { .. } | Type::Future { .. } => {
                return Err(Unfillable::of(ty, "is a runtime handle, not data"));
            }
            Type::Ref(r) => match self.shape.resolve(r) {
                Ok(Resolved::Alias(target)) => self.fill(&target)?,
                Ok(Resolved::Fields(fields)) => self.fill_fields(&fields)?,
                Ok(Resolved::Interface) => {
                    return Err(Unfillable::of(
                        ty,
                        "is an interface, which can be satisfied but not constructed",
                    ));
                }
                Ok(Resolved::Variants(variants)) => {
                    if variants.is_empty() {
                        return Err(Unfillable::of(ty, "has no variants"));
                    }
                    let variant = &variants[k % variants.len()];
                    Value::Variant {
                        tag: variant.name.to_string(),
                        value: Box::new(self.fill(&variant.payload)?),
                    }
                }
                Err(_) => return Err(Unfillable::of(ty, "does not resolve")),
            },
        })
    }

    fn fill_fields(&mut self, fields: &[Field]) -> Result<Value, Unfillable> {
        let mut out = Vec::with_capacity(fields.len());
        for field in fields {
            out.push((field.name.to_string(), self.fill(&field.ty)?));
        }
        Ok(Value::record(out))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use unitype_core::check::check_workspace;
    use unitype_core::{BigO, ImplProfile, Workspace};

    use crate::error::ExecError;
    use crate::implementation::Implementation;
    use crate::registry::Registry;

    const WS: &str = r#"{"universes":[{"name":"demo",
      "types":[
        {"k":"struct","name":"Box","params":[{"name":"T"}],
         "fields":[{"name":"label","ty":"str"},{"name":"inside","ty":{"k":"param","name":"T"}}]},
        {"k":"interface","name":"Identified","fields":[{"name":"id","ty":"str"}]},
        {"k":"enum","name":"Maybe","params":[{"name":"T"}],
         "variants":[{"name":"nothing"},{"name":"just","payload":{"k":"param","name":"T"}}]}],
      "functions":[
        {"name":"reverse","params_decl":[{"name":"T"}],
         "params":[{"name":"xs","ty":{"k":"list","of":{"k":"param","name":"T"}}}],
         "ret":{"k":"list","of":{"k":"param","name":"T"}}},
        {"name":"first","params_decl":[{"name":"T"}],
         "params":[{"name":"xs","ty":{"k":"list","of":{"k":"param","name":"T"}}}],
         "ret":{"k":"optional","of":{"k":"param","name":"T"}}},
        {"name":"swap","params_decl":[{"name":"A"},{"name":"B"}],
         "params":[{"name":"a","ty":{"k":"param","name":"A"}},{"name":"b","ty":{"k":"param","name":"B"}}],
         "ret":{"k":"tuple","items":[{"k":"param","name":"B"},{"k":"param","name":"A"}]}},
        {"name":"unbox","params_decl":[{"name":"T"}],
         "params":[{"name":"b","ty":{"k":"ref","name":"Box","args":[{"k":"param","name":"T"}]}}],
         "ret":{"k":"ref","name":"Maybe","args":[{"k":"param","name":"T"}]}},
        {"name":"sort","params":[{"name":"xs","ty":"[i64]"}],"ret":"[i64]"},
        {"name":"ids","params_decl":[{"name":"T","bounds":["Identified"]}],
         "params":[{"name":"xs","ty":{"k":"list","of":{"k":"param","name":"T"}}}],
         "ret":"[str]"},
        {"name":"either","params_decl":[{"name":"T"}],
         "params":[{"name":"x","ty":{"k":"union","members":[{"k":"param","name":"T"},"str"]}}],
         "ret":{"k":"param","name":"T"}}]}]}"#;

    fn q(s: &str) -> QualifiedName {
        s.parse().unwrap()
    }

    fn list(value: &Value) -> Vec<Value> {
        match value {
            Value::List(items) => items.clone(),
            _ => Vec::new(),
        }
    }

    /// Every proof this file talks about, under the name it is talked about by.
    fn registry() -> Registry {
        let mut registry = Registry::new();

        registry.register_native_sync("reverses", |args| {
            let mut items = list(&args[0]);
            items.reverse();
            Ok(Value::List(items))
        });
        // Sorts when it can tell the elements are integers, and reverses
        // otherwise: right at every type the author tried, and not parametric.
        registry.register_native_sync("sorts_integers", |args| {
            let mut items = list(&args[0]);
            if items.iter().all(|v| v.as_int().is_some()) {
                items.sort_by_key(|v| v.as_int());
            } else {
                items.reverse();
            }
            Ok(Value::List(items))
        });
        // Reverses, unless the first two are equal, in which case it forgets
        // the rest. Only a relabelling that makes two tokens equal finds this.
        registry.register_native_sync("compares", |args| {
            let mut items = list(&args[0]);
            if items.len() >= 2 && items[0] == items[1] {
                items.truncate(1);
            } else {
                items.reverse();
            }
            Ok(Value::List(items))
        });
        // Pads an empty list with something it made up.
        registry.register_native_sync("pads", |args| {
            let mut items = list(&args[0]);
            if items.is_empty() {
                items.push(Value::Int(0));
            }
            items.reverse();
            Ok(Value::List(items))
        });
        // Only works at one type.
        registry.register_native_sync("integers_only", |args| {
            let items = list(&args[0]);
            if items.iter().any(|v| v.as_int().is_none()) {
                return Err(ExecError::Failed {
                    id: "integers_only".into(),
                    message: "expected integers".into(),
                });
            }
            Ok(Value::List(items.into_iter().rev().collect()))
        });
        registry.register_native_sync("heads", |args| {
            Ok(list(&args[0]).into_iter().next().unwrap_or(Value::Null))
        });
        registry.register_native_sync("swaps", |mut args| {
            let b = args.remove(1);
            let a = args.remove(0);
            Ok(Value::Tuple(vec![b, a]))
        });
        // Returns the pair the way it came: an `A` where a `B` was promised.
        registry.register_native_sync("keeps", |args| Ok(Value::Tuple(args)));
        registry.register_native_sync("unboxes", |mut args| {
            let Value::Record(mut fields) = args.remove(0) else {
                return Err(ExecError::Untyped("expected a record".into()));
            };
            let inside = fields.swap_remove("inside").unwrap_or(Value::Null);
            Ok(Value::Variant {
                tag: "just".into(),
                value: Box::new(inside),
            })
        });
        registry
    }

    fn dispatcher(proves: &[(&str, &str)]) -> Dispatcher {
        let ws =
            Arc::new(check_workspace(Workspace::from_json(WS).expect("reads")).expect("checks"));
        let mut registry = registry();
        for (entry, sig) in proves {
            registry
                .register(Implementation::new(
                    *entry,
                    q(sig),
                    Ident::new("here").unwrap(),
                    *entry,
                    ImplProfile::new(BigO::ON, BigO::ON),
                ))
                .expect("registers");
        }
        Dispatcher::new(ws, Arc::new(registry))
    }

    /// The one verdict a proof got that was not `Held`, if any.
    fn against(theorems: &Theorems, id: &str) -> Option<Verdict> {
        theorems
            .trials
            .iter()
            .filter_map(|t| t.verdicts.get(&ImplId::new(id)))
            .find(|v| v.is_against())
            .cloned()
    }

    #[tokio::test]
    async fn a_parametric_proof_holds_without_anyone_saying_what_it_should_return() {
        let theorems = check(
            &dispatcher(&[("reverses", "demo::reverse")]),
            &q("demo::reverse"),
        )
        .await
        .expect("checks");

        assert!(theorems.held(), "{:?}", theorems.broken());
        assert_eq!(theorems.trials.len(), TRIALS);
        assert_eq!(theorems.heading(), "demo::reverse<T>");
    }

    #[tokio::test]
    async fn a_proof_that_looks_at_the_elements_breaks_naturality() {
        // At `i64` it sorts, which is a permutation, so conservation holds.
        // At `str` it does something else, which is the whole complaint.
        let theorems = check(
            &dispatcher(&[("sorts_integers", "demo::reverse")]),
            &q("demo::reverse"),
        )
        .await
        .expect("checks");

        let verdict = against(&theorems, "sorts_integers").expect("caught");
        assert!(
            matches!(
                &verdict,
                Verdict::Broken {
                    law: Law::Naturality,
                    ..
                }
            ),
            "{verdict:?}"
        );
        assert!(
            verdict.summary().contains("T = str"),
            "{}",
            verdict.summary()
        );
    }

    #[tokio::test]
    async fn a_proof_that_compares_two_elements_is_found_by_collapsing_them() {
        // Distinct tokens never compare equal, and neither do distinct
        // strings, so only a relabelling that makes two equal can show this.
        let theorems = check(
            &dispatcher(&[("compares", "demo::reverse")]),
            &q("demo::reverse"),
        )
        .await
        .expect("checks");

        let verdict = against(&theorems, "compares").expect("caught");
        assert!(
            verdict.summary().contains("made equal"),
            "{}",
            verdict.summary()
        );
    }

    #[tokio::test]
    async fn a_proof_that_makes_up_an_element_breaks_conservation() {
        let theorems = check(
            &dispatcher(&[("pads", "demo::reverse")]),
            &q("demo::reverse"),
        )
        .await
        .expect("checks");

        let verdict = against(&theorems, "pads").expect("caught");
        assert!(
            matches!(
                verdict,
                Verdict::Broken {
                    law: Law::Conservation,
                    ..
                }
            ),
            "{verdict:?}"
        );
        assert!(
            !theorems.trials[0].held(),
            "the empty list is where it pads"
        );
        assert!(theorems.trials[1].held(), "and nowhere else");
    }

    #[tokio::test]
    async fn a_proof_that_only_works_at_one_type_has_not_proved_the_generic_claim() {
        let theorems = check(
            &dispatcher(&[("integers_only", "demo::reverse")]),
            &q("demo::reverse"),
        )
        .await
        .expect("checks");

        let verdict = against(&theorems, "integers_only").expect("caught");
        assert!(
            matches!(
                verdict,
                Verdict::Broken {
                    law: Law::Naturality,
                    ..
                }
            ),
            "failing at another type breaks the law rather than excusing the proof: {verdict:?}"
        );
        assert!(
            theorems.trials[0].held(),
            "on the empty list there was nothing to be the wrong type"
        );
    }

    #[tokio::test]
    async fn the_right_proof_and_the_wrong_one_are_told_apart_side_by_side() {
        let theorems = check(
            &dispatcher(&[
                ("reverses", "demo::reverse"),
                ("sorts_integers", "demo::reverse"),
            ]),
            &q("demo::reverse"),
        )
        .await
        .expect("checks");

        assert!(against(&theorems, "reverses").is_none());
        assert!(against(&theorems, "sorts_integers").is_some());
        assert!(!theorems.held());
    }

    #[tokio::test]
    async fn absence_in_the_result_is_a_position_with_nothing_in_it() {
        let theorems = check(&dispatcher(&[("heads", "demo::first")]), &q("demo::first"))
            .await
            .expect("checks");
        assert!(theorems.held(), "{:?}", theorems.broken());
    }

    #[tokio::test]
    async fn two_parameters_are_told_apart() {
        let honest = check(&dispatcher(&[("swaps", "demo::swap")]), &q("demo::swap"))
            .await
            .expect("checks");
        assert!(honest.held(), "{:?}", honest.broken());
        assert_eq!(honest.heading(), "demo::swap<A, B>");

        // Handing back an `A` where a `B` was promised is making up a `B`.
        let dishonest = check(&dispatcher(&[("keeps", "demo::swap")]), &q("demo::swap"))
            .await
            .expect("checks");
        let verdict = against(&dishonest, "keeps").expect("caught");
        assert!(
            matches!(
                verdict,
                Verdict::Broken {
                    law: Law::Conservation,
                    ..
                }
            ),
            "{verdict:?}"
        );
    }

    #[tokio::test]
    async fn parameters_are_found_inside_declared_types() {
        // A `T` inside a struct field going in, and inside an enum payload
        // coming out: both reached through the declarations.
        let theorems = check(
            &dispatcher(&[("unboxes", "demo::unbox")]),
            &q("demo::unbox"),
        )
        .await
        .expect("checks");
        assert!(theorems.held(), "{:?}", theorems.broken());
        assert!(
            theorems.trials[0].name.contains("\"inside\":0"),
            "the token should have been put in the field: {}",
            theorems.trials[0].name
        );
    }

    #[tokio::test]
    async fn a_proposition_with_nothing_generic_about_it_has_no_theorem() {
        let d = dispatcher(&[]);
        assert!(matches!(
            applies(d.workspace(), &q("demo::sort")),
            Err(ParametricError::NotGeneric(_))
        ));
    }

    #[tokio::test]
    async fn a_bounded_parameter_is_declined_rather_than_held_to_a_law_it_need_not_obey() {
        let d = dispatcher(&[]);
        let err = applies(d.workspace(), &q("demo::ids")).unwrap_err();
        assert!(matches!(err, ParametricError::Bounded { .. }), "{err}");
        assert!(err.to_string().contains("Identified"), "{err}");
    }

    #[tokio::test]
    async fn a_parameter_under_an_untagged_union_cannot_be_found() {
        let d = dispatcher(&[]);
        let err = applies(d.workspace(), &q("demo::either")).unwrap_err();
        assert!(matches!(err, ParametricError::Unfillable { .. }), "{err}");
        assert!(err.to_string().contains("untagged union"), "{err}");
    }

    #[tokio::test]
    async fn nothing_to_check_is_said_rather_than_passed() {
        assert!(matches!(
            check(&dispatcher(&[]), &q("demo::reverse")).await,
            Err(ParametricError::NoProofs(_))
        ));
    }

    #[test]
    fn tokens_say_which_parameter_they_stand_for() {
        let token = Token {
            param: 1,
            ordinal: 7,
        };
        assert_eq!(Token::decode(&token.value()), Some(token));
        assert_eq!(Token::decode(&Value::Str("T7".into())), None);
    }
}
