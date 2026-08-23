//! Checking that the proofs of one proposition agree.
//!
//! A proposition with several proofs is only useful if they all prove the *same*
//! thing. The type system gets part of the way — every proof is held to the
//! signature at the boundary — but a signature says what shape comes back, not
//! what is in it. `sort` and `reverse` have the same type.
//!
//! So the rest is done by running them against the same inputs and comparing.
//! Where an expected answer is given, every proof is held to it; where none is,
//! the proofs are held to each other, which catches a disagreement without
//! anyone having to know which side is right.
//!
//! # What is being compared
//!
//! Values, not text. Two encoders that write the same number differently, or
//! the same object with its keys in a different order, agree — and would look
//! like they disagreed if this compared what they printed.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use unitype_core::QualifiedName;
use unitype_core::ir::Type;
use unitype_core::value::Value;

use crate::dispatch::{Dispatcher, Policy};
use crate::implementation::ImplId;

/// The vector-file format this build reads.
pub const SUITE_VERSION: &str = "1.0";

/// One case: what to pass, and what should come back.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Vector {
    /// What this case is called, so a failure can be talked about.
    pub name: String,
    /// The arguments, in declaration order.
    pub args: Vec<serde_json::Value>,
    /// What should come back.
    ///
    /// Left out, the proofs are compared against each other instead. That is
    /// worth doing on its own: it finds a disagreement without anyone having to
    /// work out the right answer first.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expect: Option<serde_json::Value>,
}

/// A set of cases for one proposition.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Suite {
    /// The format version.
    #[serde(default = "default_version")]
    pub version: String,
    /// The proposition being checked.
    pub signature: QualifiedName,
    /// How close two floats have to be to count as equal.
    ///
    /// Zero means exactly. Anything else is an admission that two languages
    /// will not agree to the last bit, which for floating point is usually
    /// true and always worth stating rather than assuming.
    #[serde(default)]
    pub float_tolerance: f64,
    /// The cases.
    pub vectors: Vec<Vector>,
}

fn default_version() -> String {
    SUITE_VERSION.to_string()
}

/// What became of one proof on one case.
#[derive(Clone, Debug, PartialEq)]
pub enum Answer {
    /// It ran and produced this.
    Value(Value),
    /// It ran and failed.
    Failed(String),
    /// It could not be run here.
    Skipped(String),
}

impl Answer {
    /// The value, if there is one.
    pub fn value(&self) -> Option<&Value> {
        match self {
            Answer::Value(v) => Some(v),
            _ => None,
        }
    }

    /// A short description, for a report.
    pub fn summary(&self) -> String {
        match self {
            Answer::Value(v) => v.to_json_string().unwrap_or_else(|_| "<unwritable>".into()),
            Answer::Failed(why) => format!("failed: {why}"),
            Answer::Skipped(why) => format!("skipped: {why}"),
        }
    }
}

/// How one case went.
#[derive(Clone, Debug)]
pub struct Case {
    /// The case's name.
    pub name: String,
    /// What each proof said.
    pub answers: BTreeMap<ImplId, Answer>,
    /// Proofs grouped by what they said, so a disagreement shows its sides.
    pub agreement: Vec<Vec<ImplId>>,
    /// Whether every proof that ran said the same thing.
    pub agreed: bool,
    /// Proofs that disagreed with the expected answer, where one was given.
    pub wrong: Vec<ImplId>,
}

/// How a whole suite went.
#[derive(Clone, Debug)]
pub struct Report {
    /// The proposition checked.
    pub signature: QualifiedName,
    /// Every proof that took part.
    pub proofs: Vec<ImplId>,
    /// Each case.
    pub cases: Vec<Case>,
}

impl Report {
    /// Whether every case came out with everyone agreeing.
    pub fn agreed(&self) -> bool {
        self.cases.iter().all(|c| c.agreed && c.wrong.is_empty())
    }

    /// The cases where something disagreed.
    pub fn disagreements(&self) -> Vec<&Case> {
        self.cases
            .iter()
            .filter(|c| !c.agreed || !c.wrong.is_empty())
            .collect()
    }
}

/// Why a suite could not be run.
#[derive(thiserror::Error, Debug)]
pub enum ConformError {
    /// The file was not a suite.
    #[error("invalid vector file: {0}")]
    Malformed(#[from] serde_json::Error),
    /// The suite is from a version this build does not read.
    #[error("this vector file is version {found}; this build reads {expected}")]
    Version {
        /// What it said.
        found: String,
        /// What this build knows.
        expected: &'static str,
    },
    /// The proposition is not in the workspace.
    #[error("`{0}` is not a proposition in this workspace")]
    UnknownSignature(QualifiedName),
    /// Nothing proves it, so there is nothing to compare.
    #[error("nothing proves `{0}`, so there is nothing to compare")]
    NoProofs(QualifiedName),
    /// A case's arguments were not what the signature takes.
    #[error("case `{name}`: {detail}")]
    BadVector {
        /// The case.
        name: String,
        /// What was wrong.
        detail: String,
    },
}

impl Suite {
    /// Reads a suite.
    pub fn from_json(text: &str) -> Result<Self, ConformError> {
        let suite: Suite = serde_json::from_str(text)?;
        if suite.version != SUITE_VERSION {
            return Err(ConformError::Version {
                found: suite.version,
                expected: SUITE_VERSION,
            });
        }
        Ok(suite)
    }
}

/// Runs every proof of a proposition against every case.
pub async fn run(dispatcher: &Dispatcher, suite: &Suite) -> Result<Report, ConformError> {
    let ws = dispatcher.workspace();
    let Some(sig) = ws.sig(&suite.signature) else {
        return Err(ConformError::UnknownSignature(suite.signature.clone()));
    };

    // Every proof, not the one a policy would pick: the whole point is to try
    // them all.
    let proofs: Vec<ImplId> = dispatcher
        .candidates(&suite.signature, &Policy::Default)
        .iter()
        .map(|i| i.id.clone())
        .collect();
    if proofs.is_empty() {
        return Err(ConformError::NoProofs(suite.signature.clone()));
    }

    let expected_type = match &sig.ret {
        Type::Future { of } => (**of).clone(),
        other => other.clone(),
    };

    let mut cases = Vec::with_capacity(suite.vectors.len());
    for vector in &suite.vectors {
        // Arguments are read against the parameters, so a case is written the
        // way the data is written rather than the way a decoder guesses.
        if vector.args.len() != sig.params.len() {
            return Err(ConformError::BadVector {
                name: vector.name.clone(),
                detail: format!(
                    "`{}` takes {} argument(s), but this gives {}",
                    suite.signature,
                    sig.params.len(),
                    vector.args.len()
                ),
            });
        }
        let mut args = Vec::with_capacity(vector.args.len());
        for (param, given) in sig.params.iter().zip(&vector.args) {
            args.push(
                ws.read(given, &param.ty)
                    .map_err(|e| ConformError::BadVector {
                        name: vector.name.clone(),
                        detail: format!("argument `{}`: {e}", param.name),
                    })?,
            );
        }

        let expected = match &vector.expect {
            Some(json) => {
                Some(
                    ws.read(json, &expected_type)
                        .map_err(|e| ConformError::BadVector {
                            name: vector.name.clone(),
                            detail: format!("the expected answer is not a `{expected_type}`: {e}"),
                        })?,
                )
            }
            None => None,
        };

        let mut answers: BTreeMap<ImplId, Answer> = BTreeMap::new();
        for id in &proofs {
            let answer = match dispatcher
                .call(&suite.signature, args.clone(), &Policy::Pinned(id.clone()))
                .await
            {
                Ok(invocation) => Answer::Value(invocation.value),
                Err(error) => {
                    // A machine without Python is not a disagreement between
                    // proofs, so it is set aside rather than counted as one.
                    if unavailable(&error) {
                        Answer::Skipped(error.to_string())
                    } else {
                        Answer::Failed(error.to_string())
                    }
                }
            };
            answers.insert(id.clone(), answer);
        }

        let agreement = group(&answers, suite.float_tolerance);
        let wrong: Vec<ImplId> = match &expected {
            Some(want) => answers
                .iter()
                .filter(|(_, answer)| match answer {
                    Answer::Value(got) => !alike(got, want, suite.float_tolerance),
                    Answer::Skipped(_) => false,
                    Answer::Failed(_) => true,
                })
                .map(|(id, _)| id.clone())
                .collect(),
            None => Vec::new(),
        };

        cases.push(Case {
            name: vector.name.clone(),
            agreed: agreement.len() <= 1,
            agreement,
            wrong,
            answers,
        });
    }

    Ok(Report {
        signature: suite.signature.clone(),
        proofs,
        cases,
    })
}

/// Whether a failure was the environment's rather than the proof's.
fn unavailable(error: &crate::error::DispatchError) -> bool {
    match error {
        crate::error::DispatchError::Exec { source, .. } => source.is_environmental(),
        crate::error::DispatchError::Unusable { .. }
        | crate::error::DispatchError::NoneAvailable { .. } => true,
        _ => false,
    }
}

/// Proofs grouped by what they said. More than one group is a disagreement.
fn group(answers: &BTreeMap<ImplId, Answer>, tolerance: f64) -> Vec<Vec<ImplId>> {
    let mut groups: Vec<(Value, Vec<ImplId>)> = Vec::new();
    for (id, answer) in answers {
        // Something that did not run has no opinion, so it is not a side.
        let Answer::Value(value) = answer else {
            continue;
        };
        match groups
            .iter_mut()
            .find(|(seen, _)| alike(seen, value, tolerance))
        {
            Some((_, members)) => members.push(id.clone()),
            None => groups.push((value.clone(), vec![id.clone()])),
        }
    }
    groups.into_iter().map(|(_, members)| members).collect()
}

/// Whether two values say the same thing.
///
/// Compared as values rather than as text, so that two encoders which write a
/// number differently, or an object with its keys in another order, still
/// agree. Floats are compared within whatever slack the suite allows, because
/// two languages computing the same thing will not always land on the same bit.
pub fn alike(a: &Value, b: &Value, tolerance: f64) -> bool {
    match (a, b) {
        (Value::Float(x), Value::Float(y)) => {
            x == y || (tolerance > 0.0 && (x - y).abs() <= tolerance)
        }
        // A whole number and the same number written as a float are the same
        // number, which is the sort of thing languages differ about.
        (Value::Int(x), Value::Float(y)) | (Value::Float(y), Value::Int(x)) => {
            let x = *x as f64;
            x == *y || (tolerance > 0.0 && (x - y).abs() <= tolerance)
        }
        (Value::List(x), Value::List(y)) | (Value::Tuple(x), Value::Tuple(y)) => {
            x.len() == y.len() && x.iter().zip(y).all(|(p, q)| alike(p, q, tolerance))
        }
        (Value::Record(x), Value::Record(y)) => {
            // Key order is not information.
            x.len() == y.len()
                && x.iter()
                    .all(|(k, v)| y.get(k).is_some_and(|other| alike(v, other, tolerance)))
        }
        (Value::Map(x), Value::Map(y)) => {
            x.len() == y.len()
                && x.iter().all(|(k, v)| {
                    y.iter()
                        .any(|(k2, v2)| alike(k, k2, tolerance) && alike(v, v2, tolerance))
                })
        }
        (Value::Variant { tag: a, value: x }, Value::Variant { tag: b, value: y }) => {
            a == b && alike(x, y, tolerance)
        }
        _ => a == b,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use unitype_core::check::check_workspace;
    use unitype_core::{BigO, Ident, ImplProfile, Workspace};

    use crate::implementation::Implementation;
    use crate::registry::Registry;

    const WS: &str = r#"{"universes":[{"name":"demo","functions":[
        {"name":"sort","params":[{"name":"xs","ty":"[i64]"}],"ret":"[i64]"}]}]}"#;

    fn ints(value: &Value) -> Vec<i64> {
        let Value::List(items) = value else {
            return Vec::new();
        };
        items.iter().filter_map(Value::as_int).collect()
    }

    fn list(items: &[i64]) -> serde_json::Value {
        serde_json::json!(items)
    }

    /// A dispatcher with the proofs named, each behaving as its name suggests.
    fn dispatcher(proofs: &[&str]) -> Dispatcher {
        let ws =
            Arc::new(check_workspace(Workspace::from_json(WS).expect("reads")).expect("checks"));
        let mut registry = Registry::new();

        registry.register_native_sync("sorts", |args| {
            let mut items = ints(&args[0]);
            items.sort_unstable();
            Ok(Value::List(items.into_iter().map(Value::Int).collect()))
        });
        registry.register_native_sync("also_sorts", |args| {
            // The same answer by a different route.
            let mut items = ints(&args[0]);
            for i in 0..items.len() {
                for j in 0..items.len().saturating_sub(i + 1) {
                    if items[j] > items[j + 1] {
                        items.swap(j, j + 1);
                    }
                }
            }
            Ok(Value::List(items.into_iter().map(Value::Int).collect()))
        });
        registry.register_native_sync("reverses", |args| {
            // The right *type*, the wrong answer — which is exactly what a
            // signature cannot catch.
            let mut items = ints(&args[0]);
            items.reverse();
            Ok(Value::List(items.into_iter().map(Value::Int).collect()))
        });
        registry.register_native_sync("gives_up", |_| {
            Err(crate::ExecError::Failed {
                id: "gives_up".into(),
                message: "not today".into(),
            })
        });

        for entry in proofs {
            registry
                .register(Implementation::new(
                    *entry,
                    "demo::sort".parse().unwrap(),
                    Ident::new("here").unwrap(),
                    *entry,
                    ImplProfile::new(BigO::ON, BigO::ON),
                ))
                .expect("registers");
        }
        Dispatcher::new(ws, Arc::new(registry))
    }

    fn suite(vectors: Vec<Vector>) -> Suite {
        Suite {
            version: SUITE_VERSION.into(),
            signature: "demo::sort".parse().unwrap(),
            float_tolerance: 0.0,
            vectors,
        }
    }

    fn case(name: &str, input: &[i64], expect: Option<&[i64]>) -> Vector {
        Vector {
            name: name.into(),
            args: vec![list(input)],
            expect: expect.map(list),
        }
    }

    #[tokio::test]
    async fn proofs_that_agree_are_reported_as_agreeing() {
        let report = run(
            &dispatcher(&["sorts", "also_sorts"]),
            &suite(vec![
                case("already in order", &[1, 2, 3], None),
                case("backwards", &[3, 2, 1], None),
            ]),
        )
        .await
        .expect("runs");

        assert!(report.agreed(), "{:?}", report.disagreements());
        assert_eq!(report.proofs.len(), 2);
    }

    #[tokio::test]
    async fn a_proof_of_the_wrong_thing_is_caught_where_the_type_could_not_catch_it() {
        // `reverse` has exactly the type `sort` has. Nothing about the
        // signature separates them; only running both does.
        let report = run(
            &dispatcher(&["sorts", "reverses"]),
            &suite(vec![case("backwards", &[3, 1, 2], None)]),
        )
        .await
        .expect("runs");

        assert!(!report.agreed());
        let disagreement = report.disagreements()[0];
        assert_eq!(
            disagreement.agreement.len(),
            2,
            "there should be two sides: {:?}",
            disagreement.agreement
        );
    }

    #[tokio::test]
    async fn an_input_they_happen_to_agree_on_does_not_hide_a_disagreement_elsewhere() {
        // Sorting and reversing agree on a single-element list. A suite with
        // only that case would pass, which is why the report is per case.
        let report = run(
            &dispatcher(&["sorts", "reverses"]),
            &suite(vec![
                case("one thing", &[7], None),
                case("several", &[3, 1, 2], None),
            ]),
        )
        .await
        .expect("runs");

        assert!(report.cases[0].agreed, "they do agree about this one");
        assert!(!report.cases[1].agreed, "and disagree about this one");
        assert!(!report.agreed());
    }

    #[tokio::test]
    async fn an_expected_answer_holds_every_proof_to_it() {
        let report = run(
            &dispatcher(&["sorts", "reverses"]),
            &suite(vec![case("backwards", &[3, 1, 2], Some(&[1, 2, 3]))]),
        )
        .await
        .expect("runs");

        let wrong: Vec<String> = report.cases[0]
            .wrong
            .iter()
            .map(ToString::to_string)
            .collect();
        assert_eq!(wrong, ["reverses"], "the right one should not be blamed");
    }

    #[tokio::test]
    async fn a_proof_that_fails_is_a_disagreement_with_an_expected_answer() {
        let report = run(
            &dispatcher(&["sorts", "gives_up"]),
            &suite(vec![case("anything", &[2, 1], Some(&[1, 2]))]),
        )
        .await
        .expect("runs");

        assert_eq!(report.cases[0].wrong.len(), 1);
        assert!(matches!(
            report.cases[0].answers[&ImplId::new("gives_up")],
            Answer::Failed(_)
        ));
    }

    #[tokio::test]
    async fn a_proposition_nothing_proves_says_so_rather_than_passing_vacuously() {
        let ws =
            Arc::new(check_workspace(Workspace::from_json(WS).expect("reads")).expect("checks"));
        let empty = Dispatcher::new(ws, Arc::new(Registry::new()));
        assert!(matches!(
            run(&empty, &suite(vec![case("anything", &[1], None)])).await,
            Err(ConformError::NoProofs(_))
        ));
    }

    #[tokio::test]
    async fn a_case_with_the_wrong_arguments_is_reported_before_anything_runs() {
        let bad = Suite {
            vectors: vec![Vector {
                name: "wrong shape".into(),
                args: vec![serde_json::json!("not a list")],
                expect: None,
            }],
            ..suite(vec![])
        };
        assert!(matches!(
            run(&dispatcher(&["sorts"]), &bad).await,
            Err(ConformError::BadVector { .. })
        ));
    }

    #[test]
    fn a_suite_reads_the_way_someone_would_write_one() {
        let suite = Suite::from_json(
            r#"{"signature":"demo::sort",
                "vectors":[
                    {"name":"backwards","args":[[3,1,2]],"expect":[1,2,3]},
                    {"name":"empty","args":[[]]}]}"#,
        )
        .expect("reads");

        assert_eq!(suite.signature.to_string(), "demo::sort");
        assert_eq!(suite.vectors.len(), 2);
        assert!(
            suite.vectors[1].expect.is_none(),
            "a case may leave the answer to the proofs"
        );
    }

    #[test]
    fn a_suite_from_a_later_version_is_refused_rather_than_guessed_at() {
        assert!(matches!(
            Suite::from_json(r#"{"version":"9.9","signature":"demo::sort","vectors":[]}"#),
            Err(ConformError::Version { .. })
        ));
    }

    #[test]
    fn two_encoders_writing_the_same_thing_differently_still_agree() {
        assert!(alike(&Value::Int(2), &Value::Float(2.0), 0.0));
        assert!(alike(
            &Value::record([("a", Value::Int(1)), ("b", Value::Int(2))]),
            &Value::record([("b", Value::Int(2)), ("a", Value::Int(1))]),
            0.0,
        ));
    }

    #[test]
    fn floats_agree_within_whatever_slack_was_allowed() {
        let a = Value::Float(0.1 + 0.2);
        let b = Value::Float(0.3);
        assert!(!alike(&a, &b, 0.0), "exactly means exactly");
        assert!(alike(&a, &b, 1e-9), "and this is the honest amount");
    }
}
