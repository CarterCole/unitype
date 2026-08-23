//! Inheritance: what a declaration inherits, and whether it was allowed to.
//!
//! Two things happen here. First the claims are validated — a parent must be a
//! struct in the same universe, an implemented name must be an interface, and
//! nothing may inherit from itself. Then the results are flattened: each
//! declaration gets the complete list of fields it actually has, and the
//! complete list of types it can stand in for.
//!
//! Flattening is what lets everything downstream stop caring about inheritance.
//! A backend emitting a struct, a bridge matching fields across universes, and
//! a validator reading a value all work from the flat list.

use std::collections::{HashMap, HashSet};

use crate::check::error::{CheckErrorKind, Diagnostics, PathSeg, TypePath, describe};
use crate::check::subst::{bind, substitute, substitute_ref};
use crate::check::{CheckedWorkspace, DefIx};
use crate::ident::{Ident, QualifiedName};
use crate::ir::{Field, TypeDef, TypeRef};

/// Validates inheritance claims, then computes flattened fields and supertypes.
pub(crate) fn build(ws: &mut CheckedWorkspace, diags: &mut Diagnostics) {
    check_kinds(ws, diags);
    if !diags.is_ok() {
        return;
    }

    let Some(order) = topological_order(ws, diags) else {
        return;
    };

    let mut ancestors: Vec<Vec<TypeRef>> = vec![Vec::new(); ws.defs.len()];
    let mut fields: Vec<Vec<Field>> = vec![Vec::new(); ws.defs.len()];

    for ix in order {
        let def = ws.def_at(ix);
        let path = def_path(ws, ix);
        match def {
            TypeDef::Struct(d) => {
                let mut own_ancestors = Vec::new();
                let mut flat = Vec::new();

                if let Some(parent) = &d.extends {
                    let (pa, pf) = inherited_from(ws, parent, &ancestors, &fields);
                    own_ancestors.push(parent.clone());
                    own_ancestors.extend(pa);
                    flat = pf;
                }
                for iface in &d.implements {
                    let (ia, _) = inherited_from(ws, iface, &ancestors, &fields);
                    own_ancestors.push(iface.clone());
                    own_ancestors.extend(ia);
                }

                for field in &d.fields {
                    merge_field(&mut flat, field.clone());
                }
                ancestors[ix.0] = dedupe(own_ancestors);
                fields[ix.0] = flat;
            }
            TypeDef::Interface(d) => {
                let mut own_ancestors = Vec::new();
                let mut flat: Vec<Field> = Vec::new();
                // An interface may refine several others, so the same
                // requirement can arrive by two routes. Identical requirements
                // are one requirement; conflicting ones cannot both be met.
                let mut source: HashMap<Ident, QualifiedName> = HashMap::new();

                for parent in &d.extends {
                    let (pa, pf) = inherited_from(ws, parent, &ancestors, &fields);
                    own_ancestors.push(parent.clone());
                    own_ancestors.extend(pa);
                    let from = ws
                        .ref_target(parent)
                        .expect("references are qualified after normalization");
                    for field in pf {
                        match flat.iter().find(|f| f.name == field.name) {
                            Some(existing) if existing.ty != field.ty => diags.error(
                                CheckErrorKind::InterfaceFieldConflict {
                                    field: field.name.clone(),
                                    a: source
                                        .get(&field.name)
                                        .cloned()
                                        .unwrap_or_else(|| from.clone()),
                                    b: from.clone(),
                                },
                                path.then(PathSeg::Field(field.name.clone())),
                            ),
                            Some(_) => {}
                            None => {
                                source.insert(field.name.clone(), from.clone());
                                flat.push(field);
                            }
                        }
                    }
                }
                for field in &d.fields {
                    merge_field(&mut flat, field.clone());
                }
                ancestors[ix.0] = dedupe(own_ancestors);
                fields[ix.0] = flat;
            }
            TypeDef::Enum(_) | TypeDef::Alias(_) => {}
        }
    }

    for (entry, (a, f)) in ws.defs.iter_mut().zip(ancestors.into_iter().zip(fields)) {
        entry.ancestors = a;
        entry.flat_fields = f;
    }
}

/// A supertype's own supertypes and fields, with its generic arguments applied.
fn inherited_from(
    ws: &CheckedWorkspace,
    parent: &TypeRef,
    ancestors: &[Vec<TypeRef>],
    fields: &[Vec<Field>],
) -> (Vec<TypeRef>, Vec<Field>) {
    let Some(ix) = ws.ref_target(parent).and_then(|q| ws.resolve(&q)) else {
        return (Vec::new(), Vec::new());
    };
    let subst = bind(ws.def_at(ix).params(), &parent.args);
    (
        ancestors[ix.0]
            .iter()
            .map(|a| substitute_ref(a, &subst))
            .collect(),
        fields[ix.0]
            .iter()
            .map(|f| Field {
                name: f.name.clone(),
                ty: substitute(&f.ty, &subst),
                doc: f.doc.clone(),
            })
            .collect(),
    )
}

/// Adds a field, or replaces the inherited one it overrides in place.
///
/// Overriding in place rather than appending keeps the parent's field order,
/// which generated code depends on.
fn merge_field(flat: &mut Vec<Field>, field: Field) {
    match flat.iter_mut().find(|f| f.name == field.name) {
        Some(existing) => *existing = field,
        None => flat.push(field),
    }
}

fn dedupe(refs: Vec<TypeRef>) -> Vec<TypeRef> {
    let mut seen = HashSet::new();
    refs.into_iter()
        .filter(|r| seen.insert(r.clone()))
        .collect()
}

fn def_path(ws: &CheckedWorkspace, ix: DefIx) -> TypePath {
    let name = ws.def_name(ix);
    TypePath::root()
        .then(PathSeg::Universe(name.universe.clone()))
        .then(PathSeg::Def(name.name.clone()))
}

/// Checks that what a declaration inherits from is the kind of thing it can
/// inherit from.
fn check_kinds(ws: &CheckedWorkspace, diags: &mut Diagnostics) {
    for ix in (0..ws.defs.len()).map(DefIx) {
        let path = def_path(ws, ix);
        let home = &ws.def_name(ix).universe;

        match ws.def_at(ix) {
            TypeDef::Struct(d) => {
                if let Some(parent) = &d.extends {
                    let at = path.then(PathSeg::Extends);
                    if let Some(target) = ws.ref_target(parent) {
                        if &target.universe != home {
                            diags.error(
                                CheckErrorKind::ParentInOtherUniverse { parent: target },
                                at.clone(),
                            );
                        } else if let Some(pix) = ws.resolve(&target) {
                            let def = ws.def_at(pix);
                            if def.as_struct().is_none() {
                                diags.error(
                                    CheckErrorKind::ParentNotStruct {
                                        name: target,
                                        article: CheckErrorKind::article_for(def.kind_name()),
                                        kind: def.kind_name(),
                                    },
                                    at,
                                );
                            }
                        }
                    }
                }
                for (i, iface) in d.implements.iter().enumerate() {
                    require_interface(ws, iface, &path.then(PathSeg::Implements(i)), diags);
                }
            }
            TypeDef::Interface(d) => {
                for (i, parent) in d.extends.iter().enumerate() {
                    require_interface(ws, parent, &path.then(PathSeg::Implements(i)), diags);
                }
            }
            TypeDef::Enum(_) | TypeDef::Alias(_) => {}
        }
    }
}

fn require_interface(ws: &CheckedWorkspace, r: &TypeRef, path: &TypePath, diags: &mut Diagnostics) {
    let Some(ix) = ws.ref_target(r).and_then(|q| ws.resolve(&q)) else {
        return;
    };
    let def = ws.def_at(ix);
    if def.as_interface().is_none() {
        diags.error(
            CheckErrorKind::NotAnInterface {
                name: ws.def_name(ix).clone(),
                article: CheckErrorKind::article_for(def.kind_name()),
                kind: def.kind_name(),
            },
            path.clone(),
        );
    }
}

/// How far the traversal has got with a declaration.
#[derive(Clone, Copy, PartialEq)]
enum Mark {
    /// On the current path, so reaching it again is a cycle.
    Open,
    /// Finished, along with everything it inherits from.
    Done,
}

/// Orders declarations so that everything comes after what it inherits from,
/// reporting any loop that makes such an order impossible.
fn topological_order(ws: &CheckedWorkspace, diags: &mut Diagnostics) -> Option<Vec<DefIx>> {
    let mut marks: Vec<Option<Mark>> = vec![None; ws.defs.len()];
    let mut order = Vec::with_capacity(ws.defs.len());
    let mut ok = true;

    for start in (0..ws.defs.len()).map(DefIx) {
        visit(
            ws,
            start,
            &mut marks,
            &mut Vec::new(),
            &mut order,
            &mut ok,
            diags,
        );
    }

    ok.then_some(order)
}

#[allow(clippy::too_many_arguments)]
fn visit(
    ws: &CheckedWorkspace,
    ix: DefIx,
    marks: &mut Vec<Option<Mark>>,
    stack: &mut Vec<DefIx>,
    order: &mut Vec<DefIx>,
    ok: &mut bool,
    diags: &mut Diagnostics,
) {
    use Mark::*;
    match marks[ix.0] {
        Some(Done) => return,
        Some(Open) => {
            let start = stack.iter().position(|s| *s == ix).unwrap_or(0);
            let mut trail: Vec<String> = stack[start..]
                .iter()
                .map(|s| ws.def_name(*s).to_string())
                .collect();
            trail.push(ws.def_name(ix).to_string());
            diags.error(CheckErrorKind::ExtendsCycle(trail), def_path(ws, ix));
            *ok = false;
            return;
        }
        None => {}
    }

    marks[ix.0] = Some(Open);
    stack.push(ix);
    for parent in supertypes(ws, ix) {
        visit(ws, parent, marks, stack, order, ok, diags);
    }
    stack.pop();
    marks[ix.0] = Some(Done);
    order.push(ix);
}

/// Everything a declaration directly inherits from.
fn supertypes(ws: &CheckedWorkspace, ix: DefIx) -> Vec<DefIx> {
    let resolve = |r: &TypeRef| ws.ref_target(r).and_then(|q| ws.resolve(&q));
    match ws.def_at(ix) {
        TypeDef::Struct(d) => d
            .extends
            .iter()
            .chain(&d.implements)
            .filter_map(resolve)
            .collect(),
        TypeDef::Interface(d) => d.extends.iter().filter_map(resolve).collect(),
        TypeDef::Enum(_) | TypeDef::Alias(_) => Vec::new(),
    }
}

/// Checks the claims that need subtyping: that overrides narrow rather than
/// widen, and that a struct really satisfies the interfaces it claims.
pub(crate) fn verify(ws: &CheckedWorkspace, diags: &mut Diagnostics) {
    for ix in (0..ws.defs.len()).map(DefIx) {
        let path = def_path(ws, ix);
        let TypeDef::Struct(d) = ws.def_at(ix) else {
            continue;
        };

        if let Some(parent) = &d.extends {
            let (_, inherited) = inherited_fields(ws, parent);
            for field in &d.fields {
                let Some(base) = inherited.iter().find(|f| f.name == field.name) else {
                    continue;
                };
                // Values here are read-only wire data, so a narrower field type
                // in a child is safe: anything reading the parent's view still
                // gets something it can handle.
                if !ws.is_subtype(&field.ty, &base.ty) {
                    diags.error(
                        CheckErrorKind::BadOverride {
                            field: field.name.clone(),
                            parent: ws
                                .ref_target(parent)
                                .expect("references are qualified after normalization"),
                            required: describe(&base.ty),
                            found: describe(&field.ty),
                        },
                        path.then(PathSeg::Field(field.name.clone())),
                    );
                }
            }
        }

        let flat = &ws.defs[ix.0].flat_fields;
        for (i, iface) in d.implements.iter().enumerate() {
            let Some(name) = ws.ref_target(iface) else {
                continue;
            };
            let (_, required) = inherited_fields(ws, iface);
            for want in required {
                let at = path.then(PathSeg::Implements(i));
                match flat.iter().find(|f| f.name == want.name) {
                    None => diags.error(
                        CheckErrorKind::MissingInterfaceField {
                            interface: name.clone(),
                            field: want.name.clone(),
                        },
                        at,
                    ),
                    Some(have) if !ws.is_subtype(&have.ty, &want.ty) => diags.error(
                        CheckErrorKind::InterfaceFieldMismatch {
                            interface: name.clone(),
                            field: want.name.clone(),
                            required: describe(&want.ty),
                            found: describe(&have.ty),
                        },
                        at,
                    ),
                    Some(_) => {}
                }
            }
        }
    }
}

/// A supertype's flattened fields, with its generic arguments applied.
fn inherited_fields(ws: &CheckedWorkspace, parent: &TypeRef) -> (Vec<TypeRef>, Vec<Field>) {
    let Some(ix) = ws.ref_target(parent).and_then(|q| ws.resolve(&q)) else {
        return (Vec::new(), Vec::new());
    };
    let subst = bind(ws.def_at(ix).params(), &parent.args);
    (
        ws.defs[ix.0]
            .ancestors
            .iter()
            .map(|a| substitute_ref(a, &subst))
            .collect(),
        ws.defs[ix.0]
            .flat_fields
            .iter()
            .map(|f| Field {
                name: f.name.clone(),
                ty: substitute(&f.ty, &subst),
                doc: f.doc.clone(),
            })
            .collect(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Workspace;
    use crate::check::{CheckedWorkspace, check_workspace};

    fn check(json: &str) -> Result<CheckedWorkspace, Diagnostics> {
        check_workspace(Workspace::from_json(json).expect("reads"))
    }

    fn errors(json: &str) -> Vec<String> {
        check(json)
            .err()
            .map(|d| d.errors.iter().map(ToString::to_string).collect())
            .unwrap_or_default()
    }

    const SHAPES: &str = r#"{"universes":[{"name":"demo","types":[
        {"k":"interface","name":"Identified","fields":[{"name":"id","ty":"str"}]},
        {"k":"struct","name":"Shape","implements":["Identified"],
         "fields":[{"name":"id","ty":"str"},{"name":"tag","ty":"str"}]},
        {"k":"struct","name":"Circle","extends":"Shape",
         "fields":[{"name":"radius","ty":"float"}]}]}]}"#;

    #[test]
    fn a_child_has_its_parents_fields_and_then_its_own() {
        let ws = check(SHAPES).expect("checks");
        let fields = ws.flat_fields(&"demo::Circle".parse().unwrap()).unwrap();
        let names: Vec<&str> = fields.iter().map(|f| f.name.as_str()).collect();
        assert_eq!(names, ["id", "tag", "radius"]);
    }

    #[test]
    fn a_child_can_stand_in_for_everything_it_inherits_from() {
        let ws = check(SHAPES).expect("checks");
        let circle = ws.resolve(&"demo::Circle".parse().unwrap()).unwrap();
        let ancestors: Vec<String> = ws.defs[circle.0]
            .ancestors
            .iter()
            .map(|a| format!("{}::{}", a.universe.as_ref().unwrap(), a.name))
            .collect();
        assert!(ancestors.contains(&"demo::Shape".to_string()));
        assert!(
            ancestors.contains(&"demo::Identified".to_string()),
            "an interface reached through a parent is still a supertype: {ancestors:?}"
        );
    }

    #[test]
    fn inheriting_from_yourself_is_reported_rather_than_hanging() {
        let msgs = errors(
            r#"{"universes":[{"name":"demo","types":[
                {"k":"struct","name":"A","extends":"B","fields":[]},
                {"k":"struct","name":"B","extends":"A","fields":[]}]}]}"#,
        );
        assert!(
            msgs.iter().any(|m| m.contains("inheritance cycle")),
            "{msgs:?}"
        );
    }

    #[test]
    fn extending_something_that_is_not_a_struct_is_reported() {
        let msgs = errors(
            r#"{"universes":[{"name":"demo","types":[
                {"k":"interface","name":"I","fields":[]},
                {"k":"struct","name":"S","extends":"I","fields":[]}]}]}"#,
        );
        assert!(
            msgs[0].contains("it is an interface, not a struct"),
            "{msgs:?}"
        );
    }

    #[test]
    fn inheriting_across_universes_is_refused_and_suggests_a_link() {
        let msgs = errors(
            r#"{"universes":[
                {"name":"a","types":[{"k":"struct","name":"Base","fields":[]}]},
                {"name":"b","types":[{"k":"struct","name":"Derived","extends":"a::Base","fields":[]}]}]}"#,
        );
        assert!(msgs[0].contains("link the two types instead"), "{msgs:?}");
    }

    #[test]
    fn an_override_may_narrow_a_field() {
        let ws = check(
            r#"{"universes":[{"name":"demo","types":[
                {"k":"struct","name":"Base","fields":[{"name":"kind","ty":"str"}]},
                {"k":"struct","name":"Circle","extends":"Base",
                 "fields":[{"name":"kind","ty":{"k":"literal","value":{"str":"circle"}}}]}]}]}"#,
        );
        assert!(ws.is_ok(), "{:?}", ws.err());
    }

    #[test]
    fn an_override_may_not_widen_a_field() {
        let msgs = errors(
            r#"{"universes":[{"name":"demo","types":[
                {"k":"struct","name":"Base","fields":[{"name":"n","ty":"i32"}]},
                {"k":"struct","name":"Wide","extends":"Base","fields":[{"name":"n","ty":"str"}]}]}]}"#,
        );
        assert!(
            msgs[0].contains("is not a subtype of the inherited"),
            "{msgs:?}"
        );
    }

    #[test]
    fn an_override_replaces_the_field_where_it_already_was() {
        let ws = check(
            r#"{"universes":[{"name":"demo","types":[
                {"k":"struct","name":"Base","fields":[
                    {"name":"kind","ty":"str"},{"name":"z","ty":"i64"}]},
                {"k":"struct","name":"Circle","extends":"Base","fields":[
                    {"name":"kind","ty":{"k":"literal","value":{"str":"circle"}}}]}]}]}"#,
        )
        .expect("checks");
        let fields = ws.flat_fields(&"demo::Circle".parse().unwrap()).unwrap();
        let names: Vec<&str> = fields.iter().map(|f| f.name.as_str()).collect();
        assert_eq!(names, ["kind", "z"], "the parent's field order should hold");
    }

    #[test]
    fn claiming_an_interface_without_its_fields_is_reported() {
        let msgs = errors(
            r#"{"universes":[{"name":"demo","types":[
                {"k":"interface","name":"Identified","fields":[{"name":"id","ty":"str"}]},
                {"k":"struct","name":"S","implements":["Identified"],"fields":[]}]}]}"#,
        );
        assert!(
            msgs[0].contains("requires field `id`, which is missing"),
            "{msgs:?}"
        );
    }

    #[test]
    fn an_interface_field_may_be_satisfied_by_an_inherited_one() {
        let ws = check(
            r#"{"universes":[{"name":"demo","types":[
                {"k":"interface","name":"Identified","fields":[{"name":"id","ty":"str"}]},
                {"k":"struct","name":"Base","fields":[{"name":"id","ty":"str"}]},
                {"k":"struct","name":"S","extends":"Base","implements":["Identified"],"fields":[]}]}]}"#,
        );
        assert!(ws.is_ok(), "{:?}", ws.err());
    }

    #[test]
    fn claiming_an_interface_with_an_unusable_field_type_is_reported() {
        let msgs = errors(
            r#"{"universes":[{"name":"demo","types":[
                {"k":"interface","name":"Identified","fields":[{"name":"id","ty":"str"}]},
                {"k":"struct","name":"S","implements":["Identified"],
                 "fields":[{"name":"id","ty":"i64"}]}]}]}"#,
        );
        assert!(msgs[0].contains("requires `id:"), "{msgs:?}");
    }

    #[test]
    fn a_generic_parent_is_inherited_with_its_arguments_applied() {
        let ws = check(
            r#"{"universes":[{"name":"demo","types":[
                {"k":"struct","name":"Box","params":[{"name":"T"}],
                 "fields":[{"name":"item","ty":{"k":"param","name":"T"}}]},
                {"k":"struct","name":"IntBox",
                 "extends":{"name":"Box","args":["i64"]},"fields":[]}]}]}"#,
        )
        .expect("checks");
        let fields = ws.flat_fields(&"demo::IntBox".parse().unwrap()).unwrap();
        assert_eq!(
            fields[0].ty,
            crate::ir::Type::I64,
            "T should have become i64"
        );
    }
}
