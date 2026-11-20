//! Resolution: every name means something, and means it here.

use std::collections::HashSet;

use crate::check::error::{CheckErrorKind, Diagnostics, PathSeg, TypePath, describe};
use crate::check::{CheckedWorkspace, DefIx};
use crate::ident::Ident;
use crate::ir::{Type, TypeDef, TypeParamDecl, TypeRef};

/// Checks that every reference resolves, every generic use has the right number
/// of arguments, every parameter is bound, and no alias expands forever.
pub(crate) fn run(ws: &CheckedWorkspace, diags: &mut Diagnostics) {
    for (ui, universe) in ws.workspace().universes.iter().enumerate() {
        let at = TypePath::root().then(PathSeg::Universe(universe.name.clone()));

        for def in &universe.types {
            let path = at.then(PathSeg::Def(def.name().clone()));
            let scope: HashSet<Ident> = def.params().iter().map(|p| p.name.clone()).collect();
            check_bounds(ws, def.params(), &path, diags);

            match def {
                TypeDef::Struct(d) => {
                    check_unique_fields(d.fields.iter().map(|f| &f.name), &path, diags);
                    if let Some(parent) = &d.extends {
                        check_ref(ws, parent, &path.then(PathSeg::Extends), diags);
                    }
                    for (i, iface) in d.implements.iter().enumerate() {
                        check_ref(ws, iface, &path.then(PathSeg::Implements(i)), diags);
                    }
                    for field in &d.fields {
                        let at = path.then(PathSeg::Field(field.name.clone()));
                        check_type(ws, &field.ty, &scope, &at, diags);
                    }
                }
                TypeDef::Interface(d) => {
                    check_unique_fields(d.fields.iter().map(|f| &f.name), &path, diags);
                    for (i, parent) in d.extends.iter().enumerate() {
                        check_ref(ws, parent, &path.then(PathSeg::Implements(i)), diags);
                    }
                    for field in &d.fields {
                        let at = path.then(PathSeg::Field(field.name.clone()));
                        check_type(ws, &field.ty, &scope, &at, diags);
                    }
                }
                TypeDef::Enum(d) => {
                    for variant in &d.variants {
                        let at = path.then(PathSeg::Variant(variant.name.clone()));
                        check_type(ws, &variant.payload, &scope, &at, diags);
                    }
                }
                TypeDef::Alias(d) => check_type(ws, &d.target, &scope, &path, diags),
            }
        }

        for sig in &universe.functions {
            let path = at.then(PathSeg::Sig(sig.name.clone()));
            let scope: HashSet<Ident> = sig.params_decl.iter().map(|p| p.name.clone()).collect();
            check_bounds(ws, &sig.params_decl, &path, diags);
            for (i, param) in sig.params.iter().enumerate() {
                check_type(ws, &param.ty, &scope, &path.then(PathSeg::Param(i)), diags);
            }
            check_type(ws, &sig.ret, &scope, &path.then(PathSeg::Ret), diags);
        }

        let _ = ui;
    }

    check_alias_cycles(ws, diags);
}

/// A generic parameter may only be bounded by interfaces.
fn check_bounds(
    ws: &CheckedWorkspace,
    params: &[TypeParamDecl],
    path: &TypePath,
    diags: &mut Diagnostics,
) {
    for (i, param) in params.iter().enumerate() {
        for bound in &param.bounds {
            let at = path.then(PathSeg::Bound(i));
            let Some(ix) = check_ref(ws, bound, &at, diags) else {
                continue;
            };
            let def = ws.def_at(ix);
            if def.as_interface().is_none() {
                diags.error(
                    CheckErrorKind::NotAnInterface {
                        name: ws.def_name(ix).clone(),
                        article: CheckErrorKind::article_for(def.kind_name()),
                        kind: def.kind_name(),
                    },
                    at,
                );
            }
        }
    }
}

fn check_unique_fields<'a>(
    names: impl Iterator<Item = &'a Ident>,
    path: &TypePath,
    diags: &mut Diagnostics,
) {
    let mut seen = HashSet::new();
    for name in names {
        if !seen.insert(name) {
            diags.error(
                CheckErrorKind::DuplicateField(name.clone()),
                path.then(PathSeg::Field(name.clone())),
            );
        }
    }
}

/// Resolves one reference, reporting whatever is wrong with it.
fn check_ref(
    ws: &CheckedWorkspace,
    r: &TypeRef,
    path: &TypePath,
    diags: &mut Diagnostics,
) -> Option<DefIx> {
    let Some(target) = ws.ref_target(r) else {
        // Normalization qualifies every reference, so this cannot happen for a
        // workspace that came through the front door.
        return None;
    };

    if ws.workspace().universe(&target.universe).is_none() {
        diags.error(
            CheckErrorKind::UnknownUniverse(target.universe.clone()),
            path.clone(),
        );
        return None;
    }

    let Some(ix) = ws.resolve(&target) else {
        diags.error(CheckErrorKind::UnknownType(target), path.clone());
        return None;
    };

    let expected = ws.def_at(ix).params().len();
    if r.args.len() != expected {
        diags.error(
            CheckErrorKind::ArityMismatch {
                name: target,
                expected,
                got: r.args.len(),
            },
            path.clone(),
        );
    }
    Some(ix)
}

/// Walks a type, checking every reference, parameter use, and map key in it.
fn check_type(
    ws: &CheckedWorkspace,
    t: &Type,
    scope: &HashSet<Ident>,
    path: &TypePath,
    diags: &mut Diagnostics,
) {
    match t {
        Type::Ref(r) => {
            check_ref(ws, r, path, diags);
            for (i, arg) in r.args.iter().enumerate() {
                check_type(ws, arg, scope, &path.then(PathSeg::Arg(i)), diags);
            }
        }
        Type::Param { name } => {
            if !scope.contains(name) {
                diags.error(CheckErrorKind::UnboundParam(name.clone()), path.clone());
            }
        }
        Type::Map { key, value } => {
            if !key.is_valid_map_key() {
                diags.error(
                    CheckErrorKind::BadMapKey(describe(key)),
                    path.then(PathSeg::Key),
                );
            }
            check_type(ws, key, scope, &path.then(PathSeg::Key), diags);
            check_type(ws, value, scope, &path.then(PathSeg::Elem), diags);
        }
        Type::Optional { of } | Type::List { of } | Type::Future { of } => {
            check_type(ws, of, scope, &path.then(PathSeg::Elem), diags);
        }
        Type::Record { fields } => {
            check_unique_fields(fields.iter().map(|f| &f.name), path, diags);
            for field in fields {
                let at = path.then(PathSeg::Field(field.name.clone()));
                check_type(ws, &field.ty, scope, &at, diags);
            }
        }
        Type::Tuple { items } | Type::Union { members: items } => {
            for (i, item) in items.iter().enumerate() {
                check_type(ws, item, scope, &path.then(PathSeg::Member(i)), diags);
            }
        }
        Type::Function { params, ret } => {
            for (i, param) in params.iter().enumerate() {
                check_type(ws, param, scope, &path.then(PathSeg::Param(i)), diags);
            }
            check_type(ws, ret, scope, &path.then(PathSeg::Ret), diags);
        }
        Type::Bounded { of, .. } => {
            check_bounded(t, path, diags);
            check_type(ws, of, scope, &path.then(PathSeg::Elem), diags);
        }
        leaf => debug_assert!(leaf.is_leaf(), "unhandled type node {}", leaf.kind_name()),
    }
}

/// Bounds go on the kinds of type that have something to bound, and only the
/// restrictions that kind has: an integer's value; the length of a list, a map,
/// or bytes; a string's length in scalar values, its size in bytes, and the
/// pattern its text must match; a future's window. A count or a window cannot
/// go below zero, a range whose ends are the wrong way round admits nothing,
/// and a pattern must be a regular expression.
fn check_bounded(node: &Type, path: &TypePath, diags: &mut Diagnostics) {
    let Type::Bounded {
        of,
        value,
        len,
        size,
        pattern,
        wait,
    } = node
    else {
        return;
    };
    let mut bad = |what: String| diags.error(CheckErrorKind::BadBounds(what), path.clone());
    let (takes_value, takes_len, takes_size, takes_pattern, takes_wait) = match of.as_ref() {
        Type::I32 | Type::I64 => (true, false, false, false, false),
        Type::List { .. } | Type::Map { .. } | Type::Bytes => (false, true, false, false, false),
        Type::String => (false, true, true, true, false),
        Type::Future { .. } => (false, false, false, false, true),
        Type::Bounded { .. } => {
            bad("bounds are given once".into());
            return;
        }
        other => {
            bad(format!(
                "`{}` takes no bounds; only an integer, a string, bytes, a list, a map, or a future does",
                describe(other)
            ));
            return;
        }
    };
    for (slot, bounds, allowed, counts) in [
        ("value", value, takes_value, false),
        ("len", len, takes_len, true),
        ("size", size, takes_size, true),
        ("wait", wait, takes_wait, true),
    ] {
        let Some(bounds) = bounds else {
            continue;
        };
        if !allowed {
            bad(format!("`{}` has no `{slot}` to bound", describe(of)));
            continue;
        }
        let ends = || [bounds.min, bounds.max].into_iter().flatten();
        if bounds.is_empty() {
            bad(format!("`{slot}` bounds of {bounds} admit nothing"));
        }
        if counts && ends().any(|n| n < 0) {
            bad(format!(
                "`{slot}` bounds of {bounds} go below zero, which a count or a window cannot"
            ));
        }
        if matches!(of.as_ref(), Type::I32) && ends().any(|n| i32::try_from(n).is_err()) {
            bad(format!(
                "`value` bounds of {bounds} reach outside a 32-bit integer"
            ));
        }
    }
    if let Some(pattern) = pattern {
        if !takes_pattern {
            bad(format!("`{}` has no `match` to bound", describe(of)));
        } else if let Err(e) = crate::ir::restrict::full_match(pattern, "") {
            bad(format!(
                "`match` pattern {pattern:?} is not a regular expression: {e}"
            ));
        }
    }
}

/// An alias that expands to itself has no meaning, and would hang anything that
/// tried to expand it.
fn check_alias_cycles(ws: &CheckedWorkspace, diags: &mut Diagnostics) {
    #[derive(Clone, Copy, PartialEq)]
    enum Mark {
        Open,
        Done,
    }

    let mut marks: Vec<Option<Mark>> = vec![None; ws.defs.len()];
    for start in 0..ws.defs.len() {
        if marks[start].is_some() {
            continue;
        }
        let mut stack = vec![DefIx(start)];
        walk_alias(ws, DefIx(start), &mut marks, &mut stack, diags);
    }

    /// Depth-first through alias targets, reporting the first loop found.
    fn walk_alias(
        ws: &CheckedWorkspace,
        ix: DefIx,
        marks: &mut Vec<Option<Mark>>,
        stack: &mut Vec<DefIx>,
        diags: &mut Diagnostics,
    ) {
        marks[ix.0] = Some(Mark::Open);
        let TypeDef::Alias(alias) = ws.def_at(ix) else {
            marks[ix.0] = Some(Mark::Done);
            return;
        };

        for next in alias_targets(ws, &alias.target) {
            match marks[next.0] {
                Some(Mark::Open) => {
                    let start = stack.iter().position(|s| *s == next).unwrap_or(0);
                    let mut trail: Vec<String> = stack[start..]
                        .iter()
                        .map(|s| ws.def_name(*s).to_string())
                        .collect();
                    trail.push(ws.def_name(next).to_string());
                    diags.error(
                        CheckErrorKind::AliasCycle(trail),
                        TypePath::root()
                            .then(PathSeg::Universe(ws.def_name(ix).universe.clone()))
                            .then(PathSeg::Def(ws.def_name(ix).name.clone())),
                    );
                }
                Some(Mark::Done) => {}
                None => {
                    stack.push(next);
                    walk_alias(ws, next, marks, stack, diags);
                    stack.pop();
                }
            }
        }
        marks[ix.0] = Some(Mark::Done);
    }

    /// Every declaration this type mentions, at any depth.
    ///
    /// An alias is only a name, so it has nothing for recursion to anchor on:
    /// there is no finite way to say what `alias A = [A]` stands for. Recursive
    /// types are declared as structs, which do have a name to refer back to.
    fn alias_targets(ws: &CheckedWorkspace, t: &Type) -> Vec<DefIx> {
        let mut out = Vec::new();
        if let Type::Ref(r) = t {
            out.extend(ws.ref_target(r).and_then(|q| ws.resolve(&q)));
        }
        for child in t.children() {
            out.extend(alias_targets(ws, child));
        }
        out
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

    #[test]
    fn a_pattern_and_a_window_are_checked_where_they_belong() {
        let msgs = errors(
            r#"{"universes":[{"name":"demo","types":[
                {"k":"struct","name":"S","fields":[
                    {"name":"a","ty":{"k":"bounded","of":"str","match":"("}},
                    {"name":"b","ty":{"k":"bounded","of":"i32","match":"[a-z]+"}},
                    {"name":"c","ty":{"k":"bounded","of":"i32","wait":{"max":5}}},
                    {"name":"d","ty":{"k":"bounded","of":{"k":"future","of":"str"},
                        "wait":{"min":5000,"max":100}}},
                    {"name":"e","ty":{"k":"bounded","of":{"k":"future","of":"str"},
                        "wait":{"min":-1}}}]}]}]}"#,
        );
        let said = msgs.join("\n");
        for why in [
            "not a regular expression",
            "has no `match` to bound",
            "has no `wait` to bound",
            "admit nothing",
            "below zero",
        ] {
            assert!(said.contains(why), "{why}:\n{said}");
        }

        let fine = errors(
            r#"{"universes":[{"name":"demo","types":[
                {"k":"struct","name":"S","fields":[
                    {"name":"word","ty":{"k":"bounded","of":"str","match":"[a-z]+"}},
                    {"name":"soon","ty":{"k":"bounded","of":{"k":"future","of":"str"},
                        "wait":{"min":100,"max":5000}}}]}]}]}"#,
        );
        assert!(fine.is_empty(), "{fine:?}");
    }

    #[test]
    fn bounds_go_only_where_something_can_be_bounded() {
        let msgs = errors(
            r#"{"universes":[{"name":"demo","types":[
                {"k":"struct","name":"S","fields":[
                    {"name":"a","ty":{"k":"bounded","of":"i32","len":{"max":4}}},
                    {"name":"b","ty":"[i64; -1]"},
                    {"name":"c","ty":"i32[5..1]"},
                    {"name":"d","ty":"i32[0..5000000000]"},
                    {"name":"e","ty":{"k":"bounded","of":"S","value":{"min":0}}},
                    {"name":"f","ty":{"k":"bounded","value":{"max":3},
                        "of":{"k":"bounded","of":"i32","value":{"min":0}}}}]}]}]}"#,
        );
        let said = msgs.join("\n");
        for why in [
            "has no `len` to bound",
            "below zero",
            "admit nothing",
            "outside a 32-bit integer",
            "takes no bounds",
            "given once",
        ] {
            assert!(said.contains(why), "{why}:\n{said}");
        }

        let fine = errors(
            r#"{"universes":[{"name":"demo","types":[
                {"k":"struct","name":"Addr","fields":[
                    {"name":"octets","ty":"[i32[0..255]; 4]"},
                    {"name":"host","ty":"str[len 1..253, size ..253]"},
                    {"name":"tags","ty":"map<str, i64>[..10]"}]}]}]}"#,
        );
        assert!(fine.is_empty(), "{fine:?}");
    }

    #[test]
    fn a_reference_to_something_that_does_not_exist_is_reported() {
        let msgs = errors(
            r#"{"universes":[{"name":"demo","types":[
                {"k":"struct","name":"S","fields":[{"name":"f","ty":"Missing"}]}]}]}"#,
        );
        assert_eq!(msgs.len(), 1);
        assert!(msgs[0].contains("unknown type `demo::Missing`"), "{msgs:?}");
        assert!(msgs[0].starts_with("demo::S.f:"), "{msgs:?}");
    }

    #[test]
    fn a_reference_into_a_universe_that_is_not_loaded_is_reported() {
        let msgs = errors(
            r#"{"universes":[{"name":"demo","types":[
                {"k":"struct","name":"S","fields":[{"name":"f","ty":"other::T"}]}]}]}"#,
        );
        assert!(msgs[0].contains("unknown universe `other`"), "{msgs:?}");
    }

    #[test]
    fn a_reference_across_universes_is_fine_when_it_is_spelled_out() {
        let ws = check_workspace(
            Workspace::from_json(
                r#"{"universes":[
                    {"name":"geo","types":[{"k":"struct","name":"Point","fields":[]}]},
                    {"name":"app","types":[{"k":"struct","name":"Pin",
                        "fields":[{"name":"at","ty":"geo::Point"}]}]}]}"#,
            )
            .expect("reads"),
        );
        assert!(ws.is_ok(), "{:?}", ws.err());
    }

    #[test]
    fn the_wrong_number_of_type_arguments_is_reported() {
        let msgs = errors(
            r#"{"universes":[{"name":"demo","types":[
                {"k":"struct","name":"Box","params":[{"name":"T"}],
                 "fields":[{"name":"item","ty":{"k":"param","name":"T"}}]},
                {"k":"struct","name":"S","fields":[{"name":"b","ty":"Box"}]}]}]}"#,
        );
        assert!(
            msgs[0].contains("takes 1 type argument(s), but 0 were given"),
            "{msgs:?}"
        );
    }

    #[test]
    fn a_parameter_nothing_bound_is_reported() {
        let msgs = errors(
            r#"{"universes":[{"name":"demo","types":[
                {"k":"struct","name":"S","fields":[{"name":"f","ty":{"k":"param","name":"T"}}]}]}]}"#,
        );
        assert!(msgs[0].contains("not a generic parameter"), "{msgs:?}");
    }

    #[test]
    fn a_map_key_that_no_language_could_represent_is_reported() {
        let msgs = errors(
            r#"{"universes":[{"name":"demo","types":[
                {"k":"struct","name":"S","fields":[{"name":"m",
                 "ty":{"k":"map","key":{"k":"f64"},"value":"str"}}]}]}]}"#,
        );
        assert!(msgs[0].contains("cannot be a map key"), "{msgs:?}");
        assert!(msgs[0].contains("demo::S.m.key"), "{msgs:?}");
    }

    #[test]
    fn an_alias_that_expands_to_itself_is_reported_rather_than_hanging() {
        let msgs = errors(
            r#"{"universes":[{"name":"demo","types":[
                {"k":"alias","name":"A","target":"B"},
                {"k":"alias","name":"B","target":"A"}]}]}"#,
        );
        assert!(msgs.iter().any(|m| m.contains("alias cycle")), "{msgs:?}");
    }

    #[test]
    fn an_alias_cannot_be_recursive_even_through_a_constructor() {
        // There is no finite way to say what this stands for, and an alias has
        // no name of its own to refer back to. Recursive types are structs.
        let msgs = errors(
            r#"{"universes":[{"name":"demo","types":[
                {"k":"alias","name":"Tree","target":"[Tree]"}]}]}"#,
        );
        assert!(msgs.iter().any(|m| m.contains("alias cycle")), "{msgs:?}");
    }

    #[test]
    fn a_struct_may_be_recursive_because_it_has_a_name_to_refer_back_to() {
        let ws = check_workspace(
            Workspace::from_json(
                r#"{"universes":[{"name":"demo","types":[
                    {"k":"struct","name":"Tree","fields":[
                        {"name":"value","ty":"i64"},{"name":"kids","ty":"[Tree]"}]}]}]}"#,
            )
            .expect("reads"),
        );
        assert!(ws.is_ok(), "{:?}", ws.err());
    }

    #[test]
    fn a_field_declared_twice_is_reported() {
        let msgs = errors(
            r#"{"universes":[{"name":"demo","types":[
                {"k":"struct","name":"S","fields":[
                    {"name":"f","ty":"i64"},{"name":"f","ty":"str"}]}]}]}"#,
        );
        assert!(msgs[0].contains("declared more than once"), "{msgs:?}");
    }

    #[test]
    fn a_generic_bounded_by_something_that_is_not_an_interface_is_reported() {
        let msgs = errors(
            r#"{"universes":[{"name":"demo","types":[
                {"k":"struct","name":"Thing","fields":[]},
                {"k":"struct","name":"Box","params":[{"name":"T","bounds":["Thing"]}],
                 "fields":[{"name":"item","ty":{"k":"param","name":"T"}}]}]}]}"#,
        );
        assert!(msgs[0].contains("not an interface"), "{msgs:?}");
    }
}
