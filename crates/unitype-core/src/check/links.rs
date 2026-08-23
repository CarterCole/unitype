//! Checking that declared links say something true.
//!
//! A link is permission for data to cross a boundary the type system otherwise
//! closes, so the claim it makes gets checked rather than taken on trust. A
//! link claiming two types are identical has to be right about that. A field
//! mapping has to name fields that exist and account for the ones that must be
//! filled. A witnessed link has to name a signature that actually converts
//! between the two.
//!
//! A lossy bridge is reported as a warning rather than an error. Discarding
//! fields is a legitimate thing to do — it is just something nobody should
//! discover afterwards.

use std::collections::HashMap;

use crate::check::CheckedWorkspace;
use crate::check::error::{CheckErrorKind, Diagnostics, PathSeg, TypePath, describe};
use crate::ident::Ident;
use crate::ir::Type;
use crate::link::{Coercion, Link, LinkKind};

/// Checks every declared link.
pub(crate) fn verify(ws: &CheckedWorkspace, diags: &mut Diagnostics) {
    let mut seen: HashMap<&Ident, ()> = HashMap::new();

    for link in &ws.workspace().links {
        let at = TypePath::root().then(PathSeg::Link(link.name.clone()));

        if seen.insert(&link.name, ()).is_some() {
            diags.error(CheckErrorKind::DuplicateLink(link.name.clone()), at.clone());
        }

        let mut endpoints_exist = true;
        for endpoint in [&link.from, &link.to] {
            if ws.def(endpoint).is_none() {
                diags.error(CheckErrorKind::UnknownType(endpoint.clone()), at.clone());
                endpoints_exist = false;
            }
        }
        if !endpoints_exist {
            continue;
        }

        if link.from.universe == link.to.universe {
            diags.error(
                CheckErrorKind::LinkWithinUniverse {
                    from: link.from.clone(),
                    to: link.to.clone(),
                },
                at.clone(),
            );
            continue;
        }

        match &link.kind {
            LinkKind::Identical => check_identical(ws, link, &at, diags),
            LinkKind::FieldMap { pairs, dropped } => {
                check_field_map(ws, link, pairs, dropped, &at, diags)
            }
            LinkKind::Witness { sig } => check_witness(ws, link, sig, &at, diags),
        }
    }
}

/// A link claiming the two are the same has to be right about it.
fn check_identical(ws: &CheckedWorkspace, link: &Link, at: &TypePath, diags: &mut Diagnostics) {
    let (from, to) = (ws.shape_hash(&link.from), ws.shape_hash(&link.to));
    if from.is_none() || from != to {
        diags.error(
            CheckErrorKind::NotIdentical {
                from: link.from.clone(),
                to: link.to.clone(),
            },
            at.clone(),
        );
    }
}

fn check_field_map(
    ws: &CheckedWorkspace,
    link: &Link,
    pairs: &[crate::link::FieldPair],
    dropped: &[Ident],
    at: &TypePath,
    diags: &mut Diagnostics,
) {
    let (Some(source), Some(target)) = (ws.fields_by_name(&link.from), ws.fields_by_name(&link.to))
    else {
        diags.error(
            CheckErrorKind::LinkEndpointHasNoFields {
                name: if ws.fields_by_name(&link.from).is_none() {
                    link.from.clone()
                } else {
                    link.to.clone()
                },
            },
            at.clone(),
        );
        return;
    };

    for pair in pairs {
        let at = at.then(PathSeg::Field(pair.to.clone()));
        let (Some(_), Some(_)) = (
            source.iter().find(|f| f.name == pair.from),
            target.iter().find(|f| f.name == pair.to),
        ) else {
            diags.error(
                CheckErrorKind::LinkNamesNoSuchField {
                    field: if source.iter().all(|f| f.name != pair.from) {
                        pair.from.clone()
                    } else {
                        pair.to.clone()
                    },
                },
                at,
            );
            continue;
        };
        check_coercion(ws, &pair.coercion, &at, diags);
    }

    // Anything the target insists on has to come from somewhere.
    for field in &target {
        let filled = pairs.iter().any(|p| p.to == field.name);
        let optional = matches!(
            ws.expand_alias(&field.ty),
            Type::Optional { .. } | Type::Unit | Type::Any
        );
        if !filled && !optional {
            diags.error(
                CheckErrorKind::LinkLeavesFieldUnfilled {
                    field: field.name.clone(),
                    to: link.to.clone(),
                },
                at.clone(),
            );
        }
    }

    for name in dropped {
        if !source.iter().any(|f| &f.name == name) {
            diags.error(
                CheckErrorKind::LinkNamesNoSuchField {
                    field: name.clone(),
                },
                at.clone(),
            );
        }
    }

    if !dropped.is_empty() {
        diags.warn(
            CheckErrorKind::LossyLink {
                from: link.from.clone(),
                to: link.to.clone(),
                fields: dropped.iter().map(ToString::to_string).collect(),
            },
            at.clone(),
        );
    }
}

/// A nested conversion has to name a link that exists.
///
/// Whether the conversion is the *right* one for the types is derivation's
/// question; this pass only catches a reference to a link that is not there,
/// which would leave the bridge with no way to carry out that step.
fn check_coercion(
    ws: &CheckedWorkspace,
    coercion: &Coercion,
    at: &TypePath,
    diags: &mut Diagnostics,
) {
    match coercion {
        Coercion::ViaLink { link } if ws.workspace().link(link).is_none() => {
            diags.error(CheckErrorKind::UnknownLink(link.clone()), at.clone());
        }
        Coercion::Through { of, .. } | Coercion::WrapOptional { of } => {
            check_coercion(ws, of, at, diags);
        }
        _ => {}
    }
}

/// A witnessed link has to name a function that really does the conversion.
fn check_witness(
    ws: &CheckedWorkspace,
    link: &Link,
    sig_name: &crate::ident::QualifiedName,
    at: &TypePath,
    diags: &mut Diagnostics,
) {
    let Some(sig) = ws.sig(sig_name) else {
        diags.error(
            CheckErrorKind::UnknownFunction(sig_name.clone()),
            at.clone(),
        );
        return;
    };

    let from = Type::Ref(crate::ir::TypeRef::qualified(
        link.from.universe.clone(),
        link.from.name.clone(),
    ));
    let to = Type::Ref(crate::ir::TypeRef::qualified(
        link.to.universe.clone(),
        link.to.name.clone(),
    ));

    let takes_it = matches!(sig.params.as_slice(), [only] if ws.is_subtype(&from, &only.ty));
    // A conversion that has to go and fetch something is still a conversion, so
    // a witness may promise its result later.
    let gives_it = ws.is_subtype(&sig.ret, &to) || ws.is_subtype(&sig.ret, &to.clone().future());

    if !takes_it || !gives_it {
        diags.error(
            CheckErrorKind::WitnessDoesNotConvert {
                sig: sig_name.clone(),
                from: link.from.clone(),
                to: link.to.clone(),
                found: describe(&sig.function_type()),
            },
            at.clone(),
        );
    }
}

#[cfg(test)]
mod tests {
    use crate::Workspace;
    use crate::check::check_workspace;

    fn report(json: &str) -> (Vec<String>, Vec<String>) {
        match check_workspace(Workspace::from_json(json).expect("reads")) {
            Ok(ws) => (
                Vec::new(),
                ws.warnings().iter().map(ToString::to_string).collect(),
            ),
            Err(d) => (
                d.errors.iter().map(ToString::to_string).collect(),
                d.warnings.iter().map(ToString::to_string).collect(),
            ),
        }
    }

    fn errors(json: &str) -> Vec<String> {
        report(json).0
    }

    /// Two universes describing the same thing, with the fields spelled
    /// differently so the pair is not simply identical.
    const PAIR: &str = r#"{"universes":[
        {"name":"a","types":[{"k":"struct","name":"X","fields":[
            {"name":"id","ty":"str"},{"name":"secret","ty":"str"}]}],
         "functions":[{"name":"to_y","params":[{"name":"x","ty":"X"}],"ret":"b::Y"},
                      {"name":"wrong","params":[{"name":"n","ty":"i64"}],"ret":"i64"}]},
        {"name":"b","types":[{"k":"struct","name":"Y","fields":[
            {"name":"ident","ty":"str"}]}]}]"#;

    fn with_link(kind: &str) -> String {
        format!(r#"{PAIR},"links":[{{"name":"x_to_y","from":"a::X","to":"b::Y","kind":{kind}}}]}}"#)
    }

    #[test]
    fn a_link_claiming_two_types_are_the_same_is_checked() {
        let msgs = errors(&with_link(r#"{"k":"identical"}"#));
        assert!(
            msgs.iter().any(|m| m.contains("do not hold the same data")),
            "{msgs:?}"
        );
    }

    #[test]
    fn a_link_that_really_is_identical_passes() {
        let json = r#"{"universes":[
            {"name":"a","types":[{"k":"struct","name":"X","fields":[{"name":"id","ty":"str"}]}]},
            {"name":"b","types":[{"k":"struct","name":"Y","fields":[{"name":"id","ty":"str"}]}]}],
          "links":[{"name":"x_to_y","from":"a::X","to":"b::Y","kind":{"k":"identical"}}]}"#;
        assert!(errors(json).is_empty(), "{:?}", errors(json));
    }

    #[test]
    fn a_mapping_that_names_a_field_nothing_has_is_reported() {
        let msgs = errors(&with_link(
            r#"{"k":"field_map","pairs":[
                {"from":"nope","to":"ident","coercion":{"k":"identity"}}]}"#,
        ));
        assert!(msgs.iter().any(|m| m.contains("no such field")), "{msgs:?}");
    }

    #[test]
    fn a_mapping_that_leaves_a_required_field_empty_is_reported() {
        let msgs = errors(&with_link(r#"{"k":"field_map","pairs":[]}"#));
        assert!(
            msgs.iter().any(|m| m.contains("nothing to fill")),
            "{msgs:?}"
        );
    }

    #[test]
    fn a_bridge_that_drops_fields_is_allowed_but_says_so() {
        let json = with_link(
            r#"{"k":"field_map","dropped":["secret"],"pairs":[
                {"from":"id","to":"ident","coercion":{"k":"identity"}}]}"#,
        );
        let (errors, warnings) = report(&json);
        assert!(errors.is_empty(), "dropping a field is allowed: {errors:?}");
        assert!(
            warnings.iter().any(|w| w.contains("secret")),
            "but it should be said out loud: {warnings:?}"
        );
    }

    #[test]
    fn a_witness_that_does_not_convert_between_the_two_is_reported() {
        let msgs = errors(&with_link(r#"{"k":"witness","sig":"a::wrong"}"#));
        assert!(
            msgs.iter().any(|m| m.contains("does not convert")),
            "{msgs:?}"
        );
    }

    #[test]
    fn a_witness_that_does_convert_passes() {
        let json = with_link(r#"{"k":"witness","sig":"a::to_y"}"#);
        assert!(errors(&json).is_empty(), "{:?}", errors(&json));
    }

    #[test]
    fn a_witness_may_promise_its_result_later() {
        let json = r#"{"universes":[
            {"name":"a","types":[{"k":"struct","name":"X","fields":[{"name":"id","ty":"str"}]}],
             "functions":[{"name":"fetch_y","params":[{"name":"x","ty":"X"}],
                           "ret":{"k":"future","of":"b::Y"}}]},
            {"name":"b","types":[{"k":"struct","name":"Y","fields":[{"name":"other","ty":"str"}]}]}],
          "links":[{"name":"x_to_y","from":"a::X","to":"b::Y",
                    "kind":{"k":"witness","sig":"a::fetch_y"}}]}"#;
        assert!(errors(json).is_empty(), "{:?}", errors(json));
    }

    #[test]
    fn a_link_naming_something_that_does_not_exist_is_reported() {
        let json = with_link(r#"{"k":"witness","sig":"a::missing"}"#);
        assert!(
            errors(&json)
                .iter()
                .any(|m| m.contains("unknown signature")),
            "{:?}",
            errors(&json)
        );
    }

    #[test]
    fn a_link_within_one_universe_is_reported() {
        let json = r#"{"universes":[{"name":"a","types":[
            {"k":"struct","name":"X","fields":[{"name":"id","ty":"str"}]},
            {"k":"struct","name":"Y","fields":[{"name":"id","ty":"str"}]}]}],
          "links":[{"name":"x_to_y","from":"a::X","to":"a::Y","kind":{"k":"identical"}}]}"#;
        assert!(
            errors(json).iter().any(|m| m.contains("same universe")),
            "{:?}",
            errors(json)
        );
    }

    #[test]
    fn two_links_with_one_name_are_reported() {
        let json = r#"{"universes":[
            {"name":"a","types":[{"k":"struct","name":"X","fields":[{"name":"id","ty":"str"}]}]},
            {"name":"b","types":[{"k":"struct","name":"Y","fields":[{"name":"id","ty":"str"}]}]}],
          "links":[
            {"name":"same","from":"a::X","to":"b::Y","kind":{"k":"identical"}},
            {"name":"same","from":"b::Y","to":"a::X","kind":{"k":"identical"}}]}"#;
        assert!(
            errors(json).iter().any(|m| m.contains("more than once")),
            "{:?}",
            errors(json)
        );
    }
}
