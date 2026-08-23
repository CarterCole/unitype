//! Generic bounds: an argument has to be the kind of thing the parameter
//! promised.
//!
//! Bounds are checked where arguments are supplied rather than where parameters
//! are used, because that is where the information is. Subtyping itself has no
//! declaration in scope and so cannot consult a bound.

use crate::check::CheckedWorkspace;
use crate::check::error::{CheckErrorKind, Diagnostics, PathSeg, TypePath, describe};
use crate::check::subst::bind;
use crate::ir::{Type, TypeDef, TypeRef};

/// Checks every generic argument in the workspace against its parameter's
/// bounds.
pub(crate) fn verify(ws: &CheckedWorkspace, diags: &mut Diagnostics) {
    for universe in &ws.workspace().universes {
        let at = TypePath::root().then(PathSeg::Universe(universe.name.clone()));

        for def in &universe.types {
            let path = at.then(PathSeg::Def(def.name().clone()));
            match def {
                TypeDef::Struct(d) => {
                    for r in d.extends.iter().chain(&d.implements) {
                        check_ref(ws, r, &path, diags);
                    }
                    for field in &d.fields {
                        walk(
                            ws,
                            &field.ty,
                            &path.then(PathSeg::Field(field.name.clone())),
                            diags,
                        );
                    }
                }
                TypeDef::Interface(d) => {
                    for r in &d.extends {
                        check_ref(ws, r, &path, diags);
                    }
                    for field in &d.fields {
                        walk(
                            ws,
                            &field.ty,
                            &path.then(PathSeg::Field(field.name.clone())),
                            diags,
                        );
                    }
                }
                TypeDef::Enum(d) => {
                    for variant in &d.variants {
                        walk(
                            ws,
                            &variant.payload,
                            &path.then(PathSeg::Variant(variant.name.clone())),
                            diags,
                        );
                    }
                }
                TypeDef::Alias(d) => walk(ws, &d.target, &path, diags),
            }
        }

        for sig in &universe.functions {
            let path = at.then(PathSeg::Sig(sig.name.clone()));
            for (i, param) in sig.params.iter().enumerate() {
                walk(ws, &param.ty, &path.then(PathSeg::Param(i)), diags);
            }
            walk(ws, &sig.ret, &path.then(PathSeg::Ret), diags);
        }
    }
}

fn walk(ws: &CheckedWorkspace, t: &Type, path: &TypePath, diags: &mut Diagnostics) {
    if let Type::Ref(r) = t {
        check_ref(ws, r, path, diags);
    }
    for child in t.children() {
        walk(ws, child, path, diags);
    }
}

fn check_ref(ws: &CheckedWorkspace, r: &TypeRef, path: &TypePath, diags: &mut Diagnostics) {
    let Some(ix) = ws.ref_target(r).and_then(|q| ws.resolve(&q)) else {
        return;
    };
    let params = ws.def_at(ix).params();
    if params.len() != r.args.len() {
        // Arity was already reported; one mistake, one message.
        return;
    }

    // A bound may itself mention the declaration's parameters, as in
    // `Pair<A, B: Comparable<A>>`, so arguments are substituted in first.
    let subst = bind(params, &r.args);
    for (param, arg) in params.iter().zip(&r.args) {
        for bound in &param.bounds {
            let required = Type::Ref(crate::check::substitute_ref(bound, &subst));
            if !ws.is_subtype(arg, &required) {
                diags.error(
                    CheckErrorKind::BoundNotSatisfied {
                        arg: describe(arg),
                        bound: ws
                            .ref_target(bound)
                            .expect("references are qualified after normalization"),
                    },
                    path.clone(),
                );
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::Workspace;
    use crate::check::check_workspace;

    fn errors(json: &str) -> Vec<String> {
        check_workspace(Workspace::from_json(json).expect("reads"))
            .err()
            .map(|d| d.errors.iter().map(ToString::to_string).collect())
            .unwrap_or_default()
    }

    const BASE: &str = r#"
        {"k":"interface","name":"Identified","fields":[{"name":"id","ty":"str"}]},
        {"k":"struct","name":"User","implements":["Identified"],
         "fields":[{"name":"id","ty":"str"}]},
        {"k":"struct","name":"Anon","fields":[]},
        {"k":"struct","name":"Registry","params":[{"name":"T","bounds":["Identified"]}],
         "fields":[{"name":"items","ty":{"k":"list","of":{"k":"param","name":"T"}}}]}"#;

    #[test]
    fn an_argument_that_meets_the_bound_is_accepted() {
        let json = format!(
            r#"{{"universes":[{{"name":"demo","types":[{BASE},
                {{"k":"struct","name":"Users",
                 "fields":[{{"name":"r","ty":{{"k":"ref","name":"Registry","args":["User"]}}}}]}}]}}]}}"#
        );
        assert!(errors(&json).is_empty(), "{:?}", errors(&json));
    }

    #[test]
    fn an_argument_that_misses_the_bound_is_reported() {
        let json = format!(
            r#"{{"universes":[{{"name":"demo","types":[{BASE},
                {{"k":"struct","name":"Anons",
                 "fields":[{{"name":"r","ty":{{"k":"ref","name":"Registry","args":["Anon"]}}}}]}}]}}]}}"#
        );
        let msgs = errors(&json);
        assert!(
            msgs.iter()
                .any(|m| m.contains("does not satisfy the bound")),
            "{msgs:?}"
        );
    }
}
