//! Links: declared correspondences that let data cross between universes.
//!
//! Subtyping stops at a universe boundary, so nothing flows between two
//! universes by accident. A link is the explicit permission — and it always
//! says *how* the conversion happens, so that the crossing can be carried out
//! rather than merely allowed.

mod classes;
mod compile;
mod derive;
mod discover;
mod model;

pub use classes::Equivalence;
pub use compile::CompileError;
pub use derive::{BridgeMismatch, Pairing};
pub use discover::LinkSuggestion;
pub use model::{Coercion, FieldPair, GenericPair, Link, LinkKind};
