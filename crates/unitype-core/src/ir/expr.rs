//! The expression IR: constructive proofs, written once and built anywhere.
//!
//! A signature carrying an [`Expr`] body says not just *what* it computes but
//! *how*, in a form no language owns. The same body runs directly through the
//! reference interpreter and transpiles into every target backend, so a mapping
//! written once is a Rust function, a TypeScript function, and a Python
//! function without being written three times.
//!
//! The language is deliberately **total**: no recursion, no lambdas, no
//! unbounded loops. Every body terminates, which is what lets bodies be trusted
//! as proofs and lets the call graph among them be checked by topological sort.
//! Computations that need real recursion — sorting, for instance — stay
//! propositions with external implementations, which is exactly the division of
//! labor this system is built around.

use serde::{Deserialize, Serialize};

use crate::ident::{Ident, QualifiedName};
use crate::ir::functor::Functor;
use crate::ir::ty::TypeRef;
use crate::value::Value;

/// One field of a record being constructed.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct FieldInit {
    /// The field being set.
    pub name: Ident,
    /// The expression producing its value.
    pub value: Expr,
}

impl FieldInit {
    /// Binds `name` to `value`.
    pub fn new(name: Ident, value: Expr) -> Self {
        Self { name, value }
    }
}

/// An expression.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "k", rename_all = "snake_case")]
pub enum Expr {
    /// A constant.
    Lit {
        /// The constant value.
        value: Value,
    },
    /// A parameter or a name bound by [`Expr::Let`] or [`Expr::MapThrough`].
    Var {
        /// The bound name.
        name: Ident,
    },
    /// Projection of one field.
    Field {
        /// The record being projected.
        of: Box<Expr>,
        /// The field to read.
        name: Ident,
    },
    /// Construction of a named struct.
    MakeRecord {
        /// The struct being built.
        ty: TypeRef,
        /// The field values.
        fields: Vec<FieldInit>,
    },
    /// Construction of a list.
    MakeList {
        /// The elements, in order.
        items: Vec<Expr>,
    },
    /// Construction of a tuple.
    MakeTuple {
        /// The elements, in order.
        items: Vec<Expr>,
    },
    /// Construction of one alternative of an enum.
    MakeVariant {
        /// The enum being built.
        ty: TypeRef,
        /// The alternative's tag.
        tag: Ident,
        /// The payload, absent for a bare tag.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        value: Option<Box<Expr>>,
    },
    /// Invocation of another signature.
    ///
    /// Which proof answers the call is the dispatcher's decision, so a body can
    /// call a proposition without committing to any one implementation of it.
    Call {
        /// The signature being invoked.
        sig: QualifiedName,
        /// The arguments, in declaration order.
        args: Vec<Expr>,
    },
    /// A local binding.
    Let {
        /// The name being bound.
        name: Ident,
        /// The bound expression.
        value: Box<Expr>,
        /// The expression the binding is visible in.
        body: Box<Expr>,
    },
    /// A conditional. Both arms are typed, and the result is their join.
    If {
        /// The boolean condition.
        cond: Box<Expr>,
        /// The value when the condition holds.
        then: Box<Expr>,
        /// The value otherwise.
        #[serde(rename = "else")]
        otherwise: Box<Expr>,
    },
    /// Transformation of whatever is inside a constructor.
    ///
    /// One form covers every container: a list has each element transformed, a
    /// value that may be absent stays absent or is transformed, a value that
    /// arrives later is transformed when it does. Which of those happens is the
    /// functor's business, not this expression's.
    ///
    /// This is the only iteration the language has, and it is bounded by
    /// construction: it visits what is already there and cannot produce more.
    /// Notice also what it does *not* offer — there is no way to ask whether an
    /// optional value is present, so a body cannot branch on absence and do
    /// something else entirely. It can only say what to do with a value if
    /// there is one.
    MapThrough {
        /// The constructor being mapped through.
        functor: Functor,
        /// The value the constructor was applied to.
        value: Box<Expr>,
        /// The name bound to what is inside.
        var: Ident,
        /// The transformation applied to it.
        body: Box<Expr>,
    },
}

impl Expr {
    /// A constant expression.
    pub fn lit(value: Value) -> Expr {
        Expr::Lit { value }
    }

    /// A reference to a bound name.
    pub fn var(name: &str) -> Expr {
        Expr::Var {
            name: Ident::new_unchecked(name),
        }
    }

    /// Projection of `name` from this expression.
    pub fn field(self, name: &str) -> Expr {
        Expr::Field {
            of: Box::new(self),
            name: Ident::new_unchecked(name),
        }
    }

    /// Every signature this expression invokes, directly or within a subterm.
    ///
    /// Used to build the call graph between bodies, whose acyclicity is what
    /// guarantees termination.
    pub fn called_signatures(&self) -> Vec<&QualifiedName> {
        let mut out = Vec::new();
        self.walk(&mut |e| {
            if let Expr::Call { sig, .. } = e {
                out.push(sig);
            }
        });
        out
    }

    /// Applies `f` to this expression and every subexpression, outermost first.
    pub fn walk<'a>(&'a self, f: &mut impl FnMut(&'a Expr)) {
        f(self);
        for child in self.children() {
            child.walk(f);
        }
    }

    /// The immediate subexpressions.
    pub fn children(&self) -> Vec<&Expr> {
        match self {
            Expr::Lit { .. } | Expr::Var { .. } => Vec::new(),
            Expr::Field { of, .. } => vec![of.as_ref()],
            Expr::MakeRecord { fields, .. } => fields.iter().map(|f| &f.value).collect(),
            Expr::MakeList { items } | Expr::MakeTuple { items } => items.iter().collect(),
            Expr::MakeVariant { value, .. } => value.iter().map(|v| v.as_ref()).collect(),
            Expr::Call { args, .. } => args.iter().collect(),
            Expr::Let { value, body, .. } => vec![value.as_ref(), body.as_ref()],
            Expr::If {
                cond,
                then,
                otherwise,
            } => vec![cond.as_ref(), then.as_ref(), otherwise.as_ref()],
            Expr::MapThrough { value, body, .. } => vec![value.as_ref(), body.as_ref()],
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id(s: &str) -> Ident {
        Ident::new(s).unwrap()
    }

    #[test]
    fn expressions_round_trip_through_json() {
        let e = Expr::MakeRecord {
            ty: TypeRef::local(id("Badge")),
            fields: vec![
                FieldInit::new(id("label"), Expr::var("u").field("name")),
                FieldInit::new(id("count"), Expr::lit(Value::Int(0))),
            ],
        };
        let j = serde_json::to_string(&e).unwrap();
        assert_eq!(serde_json::from_str::<Expr>(&j).unwrap(), e);
    }

    #[test]
    fn the_conditional_uses_then_and_else_on_the_wire() {
        let e = Expr::If {
            cond: Expr::lit(Value::Bool(true)).into(),
            then: Expr::lit(Value::Int(1)).into(),
            otherwise: Expr::lit(Value::Int(2)).into(),
        };
        let j = serde_json::to_string(&e).unwrap();
        assert!(j.contains(r#""then""#) && j.contains(r#""else""#), "{j}");
        assert_eq!(serde_json::from_str::<Expr>(&j).unwrap(), e);
    }

    #[test]
    fn calls_are_collected_from_anywhere_in_a_body() {
        let e = Expr::MapThrough {
            functor: Functor::List,
            value: Expr::var("xs").into(),
            var: id("x"),
            body: Box::new(Expr::Call {
                sig: "demo::area".parse().unwrap(),
                args: vec![Expr::var("x")],
            }),
        };
        let calls = e.called_signatures();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].to_string(), "demo::area");
    }
}
