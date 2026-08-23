//! Running proofs.
//!
//! A signature is a proposition and an implementation is a proof of it. This
//! crate is where proofs actually run: it holds the registry of what proves
//! what, the environments those proofs run in, and the dispatcher that decides
//! which proof answers a given call.
//!
//! Everything here checks both sides of the boundary. Arguments are checked
//! against the signature before an implementation sees them, and the result is
//! checked against the signature before the caller does. An implementation that
//! returns the wrong shape has failed to prove what it claimed, and that fails
//! loudly rather than propagating.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod conform;
pub mod dispatch;
pub mod environment;
pub mod error;
pub mod implementation;
pub mod interpret;
pub mod registry;
pub mod subprocess;

pub use conform::{Answer, ConformError, Report, Suite, Vector};
pub use dispatch::{Considered, Dispatcher, Invocation, Options, Outcome, Policy};
pub use environment::{Environment, Reach};
pub use error::{DispatchError, ExecError};
pub use implementation::{ImplId, Implementation, Manifest};
pub use interpret::{BoxFuture, Calls, NoCalls, Scope, eval, eval_signature};
pub use registry::{Registry, RegistryError, RegistryWarning};
pub use subprocess::PROTOCOL_VERSION;
