//! Type constructors you can map through.
//!
//! `[T]`, `T?`, and `Future<T>` are all the same shape of idea: a constructor
//! applied to a type, holding values of that type somewhere inside. Given a way
//! to turn an `A` into a `B`, each of them gives a way to turn its application
//! at `A` into its application at `B` — a list by visiting every element, an
//! optional by leaving absence alone, a future by waiting.
//!
//! Naming that pattern once means the rest of the system says it once. There is
//! one conversion case for "convert what is inside" rather than one per
//! container, one expression form for mapping rather than one per container, and
//! one thing each backend has to know how to emit. A constructor added later
//! costs a line here instead of a case everywhere.
//!
//! Only single-parameter constructors are functors in this sense. `Map<K, V>`
//! is one in `V` once `K` is fixed, which needs the functor to carry that fixed
//! part; that is a later addition, not a different idea.

use serde::{Deserialize, Serialize};

use crate::ir::ty::Type;

/// A constructor that a conversion can be lifted through.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Functor {
    /// A sequence: mapping visits every element.
    List,
    /// A value that may be absent: mapping leaves absence alone.
    Optional,
    /// A value that arrives later: mapping applies when it does.
    Future,
}

impl Functor {
    /// This constructor applied to `inner`.
    pub fn apply(self, inner: Type) -> Type {
        match self {
            Functor::List => inner.list(),
            Functor::Optional => inner.optional(),
            Functor::Future => inner.future(),
        }
    }

    /// Splits a type into the constructor it was built with and what is inside,
    /// if it was built with one of these.
    pub fn split(ty: &Type) -> Option<(Functor, &Type)> {
        match ty {
            Type::List { of } => Some((Functor::List, of)),
            Type::Optional { of } => Some((Functor::Optional, of)),
            Type::Future { of } => Some((Functor::Future, of)),
            _ => None,
        }
    }

    /// The constructor `ty` was built with, if any.
    pub fn of(ty: &Type) -> Option<Functor> {
        Functor::split(ty).map(|(f, _)| f)
    }

    /// How this constructor reads in a message.
    pub fn describe(self) -> &'static str {
        match self {
            Functor::List => "a list",
            Functor::Optional => "a value that may be absent",
            Functor::Future => "a value that arrives later",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn applying_a_constructor_and_splitting_it_again_gets_back_what_went_in() {
        for functor in [Functor::List, Functor::Optional, Functor::Future] {
            let applied = functor.apply(Type::I64);
            let (found, inner) = Functor::split(&applied).expect("splits");
            assert_eq!(found, functor);
            assert_eq!(inner, &Type::I64);
        }
    }

    #[test]
    fn a_type_built_with_no_constructor_does_not_split() {
        for t in [Type::I64, Type::String, Type::local_ref("User")] {
            assert!(Functor::split(&t).is_none());
        }
    }

    #[test]
    fn constructors_round_trip_through_json() {
        for functor in [Functor::List, Functor::Optional, Functor::Future] {
            let j = serde_json::to_string(&functor).unwrap();
            assert_eq!(serde_json::from_str::<Functor>(&j).unwrap(), functor);
        }
        assert_eq!(
            serde_json::to_string(&Functor::Optional).unwrap(),
            r#""optional""#
        );
    }
}
