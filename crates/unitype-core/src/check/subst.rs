//! Substituting generic arguments for parameters.

use std::collections::HashMap;

use crate::ident::Ident;
use crate::ir::{FunctionSig, NamedParam, Type, TypeParamDecl, TypeRef};

/// A binding of generic parameters to arguments.
pub type Subst = HashMap<Ident, Type>;

/// Pairs a declaration's parameters with the arguments supplied for them.
///
/// Extra arguments are ignored and missing ones are left unbound; arity is
/// checked separately so that one mistake is reported once.
pub fn bind(params: &[TypeParamDecl], args: &[Type]) -> Subst {
    params
        .iter()
        .zip(args)
        .map(|(p, a)| (p.name.clone(), a.clone()))
        .collect()
}

/// Whether `arg` may be supplied for a parameter of `arity`.
///
/// An ordinary parameter takes a type; a higher-order one takes a constructor.
/// The two are never interchangeable, so a mismatch is a kind error, reported
/// where the argument is written.
pub fn fits_arity(arity: u32, arg: &Type) -> bool {
    let is_constructor = matches!(arg, Type::Constructor { .. });
    match arity {
        0 => !is_constructor,
        1 => is_constructor,
        _ => false,
    }
}

/// Replaces every bound generic parameter in `t`.
pub fn substitute(t: &Type, subst: &Subst) -> Type {
    if subst.is_empty() {
        return t.clone();
    }
    let kids: Vec<Type> = t.children().iter().map(|c| substitute(c, subst)).collect();
    let node = t.with_children(kids);
    match node {
        Type::Param { name } => subst.get(&name).cloned().unwrap_or(Type::Param { name }),
        // A constructor bound to a higher-order parameter applies to the argument
        // it was given: `M<A>` with `M` bound to `list` is `[A]`. Anything else
        // bound there is a kind error, which the caller reports before this runs,
        // so the application is left as written.
        Type::Apply { name, args } => match (subst.get(&name), args.as_slice()) {
            (Some(Type::Constructor { functor }), [arg]) => functor.apply(arg.clone()),
            // A higher-order parameter of the caller, passed on, is still
            // applied: its name is what changes.
            (Some(Type::Param { name: forwarded }), _) => Type::Apply {
                name: forwarded.clone(),
                args,
            },
            _ => Type::Apply { name, args },
        },
        other => other,
    }
}

/// Replaces every bound generic parameter in a reference's arguments.
pub fn substitute_ref(r: &TypeRef, subst: &Subst) -> TypeRef {
    TypeRef {
        universe: r.universe.clone(),
        name: r.name.clone(),
        args: r.args.iter().map(|a| substitute(a, subst)).collect(),
    }
}

impl FunctionSig {
    /// This proposition at these type arguments.
    ///
    /// `reverse<T>` is a claim about every `T` at once, which is not something
    /// a value can be checked against: a value has one type. Applying it to a
    /// type gives the particular claim `reverse<i64>`, with no parameters left
    /// to bind. Arity is the caller's business, as with [`bind`].
    pub fn instantiate(&self, args: &[Type]) -> FunctionSig {
        let subst = bind(&self.params_decl, args);
        FunctionSig {
            params_decl: Vec::new(),
            params: self
                .params
                .iter()
                .map(|p| NamedParam {
                    name: p.name.clone(),
                    ty: substitute(&p.ty, &subst),
                    doc: p.doc.clone(),
                })
                .collect(),
            ret: substitute(&self.ret, &subst),
            ..self.clone()
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
    fn applying_a_proposition_to_a_type_leaves_nothing_generic() {
        let generic = FunctionSig {
            name: id("first"),
            doc: None,
            params_decl: vec![TypeParamDecl::new(id("T"))],
            params: vec![NamedParam::new(
                id("xs"),
                Type::Param { name: id("T") }.list(),
            )],
            ret: Type::Param { name: id("T") }.optional(),
            refines: None,
            time: None,
            space: None,
            body: None,
            meta: Default::default(),
        };
        let at_strings = generic.instantiate(&[Type::String]);
        assert!(at_strings.params_decl.is_empty());
        assert_eq!(at_strings.params[0].ty, Type::String.list());
        assert_eq!(at_strings.ret, Type::String.optional());
    }

    #[test]
    fn arguments_replace_parameters_wherever_they_occur() {
        let subst = bind(&[TypeParamDecl::new(id("T"))], &[Type::I64]);
        let before = Type::Param { name: id("T") }.list().optional();
        assert_eq!(substitute(&before, &subst), Type::I64.list().optional());
    }

    #[test]
    fn an_unbound_parameter_is_left_as_it_was() {
        let subst = bind(&[TypeParamDecl::new(id("T"))], &[Type::I64]);
        let other = Type::Param { name: id("U") };
        assert_eq!(substitute(&other, &subst), other);
    }

    #[test]
    fn substitution_reaches_into_generic_arguments() {
        let subst = bind(&[TypeParamDecl::new(id("T"))], &[Type::String]);
        let r = TypeRef {
            universe: Some(id("demo")),
            name: id("Box"),
            args: vec![Type::Param { name: id("T") }],
        };
        assert_eq!(substitute_ref(&r, &subst).args, vec![Type::String]);
    }
}
