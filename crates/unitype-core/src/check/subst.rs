//! Substituting generic arguments for parameters.

use std::collections::HashMap;

use crate::ident::Ident;
use crate::ir::{Type, TypeParamDecl, TypeRef};

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

/// Replaces every bound generic parameter in `t`.
pub fn substitute(t: &Type, subst: &Subst) -> Type {
    if subst.is_empty() {
        return t.clone();
    }
    let kids: Vec<Type> = t.children().iter().map(|c| substitute(c, subst)).collect();
    let node = t.with_children(kids);
    match node {
        Type::Param { name } => subst.get(&name).cloned().unwrap_or(Type::Param { name }),
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

#[cfg(test)]
mod tests {
    use super::*;

    fn id(s: &str) -> Ident {
        Ident::new(s).unwrap()
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
