//! What can go wrong while running a proof.

use unitype_core::QualifiedName;
use unitype_core::value::ValueError;

/// Why an implementation did not produce an answer.
///
/// The distinction that matters here is between a proof being *wrong* and a
/// proof being *unreachable*. An implementation that returns the wrong shape
/// has failed to prove what it claimed, and trying a different one would be
/// papering over that. An implementation that could not be started because the
/// interpreter is not installed has not failed at all, and the dispatcher is
/// free to look elsewhere.
#[derive(thiserror::Error, Debug)]
pub enum ExecError {
    /// The environment could not be reached at all.
    #[error("could not reach the `{environment}` environment: {detail}")]
    Unreachable {
        /// Where the implementation was to run.
        environment: String,
        /// What went wrong.
        detail: String,
    },
    /// The implementation did not answer in time.
    #[error("`{id}` did not answer within {millis}ms")]
    Timeout {
        /// The implementation that hung.
        id: String,
        /// How long it was given.
        millis: u64,
    },
    /// The implementation used up the work it was allowed.
    ///
    /// The sandbox working rather than the proof being wrong: a guest with no
    /// process to kill is stopped this way instead.
    #[error("`{id}` used up its budget of {budget} before answering")]
    Exhausted {
        /// The implementation that would not stop.
        id: String,
        /// How much work it was allowed.
        budget: u64,
    },
    /// The implementation reported a failure of its own.
    #[error("`{id}` failed: {message}")]
    Failed {
        /// The implementation that failed.
        id: String,
        /// What it said.
        message: String,
    },
    /// The two sides did not agree on how to talk.
    #[error("`{id}` broke the protocol: {detail}")]
    Protocol {
        /// The implementation involved.
        id: String,
        /// What was wrong with what it sent.
        detail: String,
    },
    /// An implementation was registered without a body to run.
    #[error("`{0}` has no body registered in this process")]
    NotRegistered(String),

    /// A body used a name nothing bound.
    #[error("`{0}` has nothing bound to it")]
    Unbound(String),
    /// A body read a field off something that has none.
    #[error("`{value}` has no field `{field}`")]
    NoSuchField {
        /// What was being read.
        value: String,
        /// The field asked for.
        field: String,
    },
    /// A body did something its types said it would not.
    ///
    /// The checker rules these out ahead of time, so reaching one means a body
    /// ran without being checked.
    #[error("{0}")]
    Untyped(String),
    /// A body called a signature that is not in the workspace.
    #[error("`{0}` is not a signature in this workspace")]
    UnknownSignature(QualifiedName),
    /// Following calls between bodies went too deep.
    #[error("evaluation did not terminate")]
    TooDeep,
}

impl ExecError {
    /// Whether this says something about the environment rather than about the
    /// proof.
    ///
    /// Only these are worth retrying somewhere else. A proof that returned the
    /// wrong thing will return the wrong thing again.
    pub fn is_environmental(&self) -> bool {
        matches!(
            self,
            ExecError::Unreachable { .. } | ExecError::Timeout { .. } | ExecError::Exhausted { .. }
        )
    }
}

/// Why a call did not reach an implementation at all.
#[derive(thiserror::Error, Debug)]
pub enum DispatchError {
    /// The workspace has no such signature.
    #[error("`{0}` is not a signature in this workspace")]
    UnknownSignature(QualifiedName),
    /// Nothing has been registered that proves it.
    #[error("nothing proves `{0}`")]
    NoImplementation(QualifiedName),
    /// Everything that proves it is out of reach here.
    #[error("nothing that proves `{sig}` can run here: {}", .skipped.join("; "))]
    NoneAvailable {
        /// The signature being called.
        sig: QualifiedName,
        /// Each candidate and why it was passed over.
        skipped: Vec<String>,
    },
    /// A specific implementation was asked for and cannot be used.
    #[error("`{id}` cannot be used: {reason}")]
    Unusable {
        /// The implementation that was named.
        id: String,
        /// Why not.
        reason: String,
    },
    /// The arguments were not what the signature takes.
    #[error("argument {index} of `{sig}`: {}", .errors.iter().map(ToString::to_string).collect::<Vec<_>>().join("; "))]
    BadArgument {
        /// The signature being called.
        sig: QualifiedName,
        /// Which argument.
        index: usize,
        /// What was wrong with it.
        errors: Vec<ValueError>,
    },
    /// The wrong number of type arguments.
    ///
    /// A generic proposition is a claim about every type at once, and a value
    /// has one type, so the call has to say which type it is at before anything
    /// can be checked against it.
    #[error("`{sig}` takes {expected} type argument(s), but {got} were given")]
    TypeArity {
        /// The signature being called.
        sig: QualifiedName,
        /// How many it takes.
        expected: usize,
        /// How many were given.
        got: usize,
    },
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
    /// An implementation returned something that is not what it promised.
    ///
    /// This is the one failure that never falls back. The point of checking
    /// both sides of the boundary is to catch a proof that does not prove what
    /// it claims, and quietly asking someone else would defeat it.
    #[error("`{id}` claims to prove `{sig}` but returned something else: {}", .errors.iter().map(ToString::to_string).collect::<Vec<_>>().join("; "))]
    BadResult {
        /// The implementation at fault.
        id: String,
        /// What it claimed to prove.
        sig: QualifiedName,
        /// How the result fell short.
        errors: Vec<ValueError>,
    },
    /// Running it failed.
    #[error("`{id}`: {source}")]
    Exec {
        /// The implementation that was run.
        id: String,
        /// What went wrong.
        #[source]
        source: ExecError,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_failure_of_the_environment_is_worth_trying_elsewhere() {
        assert!(
            ExecError::Unreachable {
                environment: "python3".into(),
                detail: "not installed".into(),
            }
            .is_environmental()
        );
        assert!(
            ExecError::Timeout {
                id: "slow".into(),
                millis: 100
            }
            .is_environmental()
        );
    }

    #[test]
    fn a_failure_of_the_proof_is_not() {
        assert!(
            !ExecError::Failed {
                id: "wrong".into(),
                message: "divided by zero".into(),
            }
            .is_environmental(),
            "asking someone else will not make this proof correct"
        );
        assert!(
            !ExecError::Protocol {
                id: "confused".into(),
                detail: "sent nothing".into(),
            }
            .is_environmental()
        );
    }
}
