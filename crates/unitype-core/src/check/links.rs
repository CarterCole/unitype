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
//!
//! An equivalence is settled before anything else looks at it: whatever
//! pairing it leaves out is worked out, and the checked workspace carries the
//! whole of it. An equivalence that cannot be settled — a field with nowhere to
//! go, a pair that does not convert exactly — is an error, because the claim is
//! that nothing is lost either way.

use std::collections::HashMap;

use crate::check::error::{CheckErrorKind, Diagnostics, PathSeg, TypePath, describe};
use crate::check::{CheckedWorkspace, Reordering};
use crate::ident::{Ident, QualifiedName};
use crate::ir::Type;
use crate::link::{Coercion, Link, LinkKind};

/// Works out every equivalence's pairing, and records which signatures are
/// one proposition and how a call to one reorders into a call to another.
pub(crate) fn settle(ws: &mut CheckedWorkspace, diags: &mut Diagnostics) {
    let mut settled = Vec::new();
    for (i, link) in ws.workspace().links.iter().enumerate() {
        let LinkKind::Equivalent { pairs, generics } = &link.kind else {
            continue;
        };
        // Endpoints that are not there, or a link that never leaves its
        // universe, are reported once, by `verify`.
        let present = |n: &QualifiedName| ws.def(n).is_some() || ws.sig(n).is_some();
        if !present(&link.from) || !present(&link.to) || link.from.universe == link.to.universe {
            continue;
        }
        let renames: Vec<(Ident, Ident)> = pairs
            .iter()
            .map(|p| (p.from.clone(), p.to.clone()))
            .collect();
        let generic_renames: Vec<(Ident, Ident)> = generics
            .iter()
            .map(|g| (g.from.clone(), g.to.clone()))
            .collect();
        match ws.derive_equivalence(&link.from, &link.to, &renames, &generic_renames) {
            Ok(pairing) => settled.push((i, pairing)),
            Err(problems) => diags.error(
                CheckErrorKind::NotEquivalent {
                    from: link.from.clone(),
                    to: link.to.clone(),
                    problems: problems.iter().map(ToString::to_string).collect(),
                },
                TypePath::root().then(PathSeg::Link(link.name.clone())),
            ),
        }
    }
    for (i, pairing) in settled {
        ws.ws.links[i].kind = LinkKind::Equivalent {
            pairs: pairing.pairs,
            generics: pairing.generics,
        };
    }

    let mut equivalents: HashMap<QualifiedName, Vec<Reordering>> = HashMap::new();
    for link in &ws.ws.links {
        let LinkKind::Equivalent { pairs, .. } = &link.kind else {
            continue;
        };
        if !ws.joins_signatures(link) || ws.sig(&link.to).is_none() {
            continue;
        }
        let (from, to) = (
            ws.sig(&link.from).expect("joins"),
            ws.sig(&link.to).expect("joins"),
        );
        let at = |params: &[crate::ir::NamedParam], name: &Ident| {
            params.iter().position(|p| &p.name == name)
        };
        // A call to `from` reorders into a call to `to` by looking up, for each
        // of `to`'s parameters, which of `from`'s feeds it — and the other way
        // round by the same pairs read backwards.
        let forward: Option<Vec<usize>> = to
            .params
            .iter()
            .map(|p| {
                let pair = pairs.iter().find(|pair| pair.to == p.name)?;
                at(&from.params, &pair.from)
            })
            .collect();
        let backward: Option<Vec<usize>> = from
            .params
            .iter()
            .map(|p| {
                let pair = pairs.iter().find(|pair| pair.from == p.name)?;
                at(&to.params, &pair.to)
            })
            .collect();
        if let (Some(forward), Some(backward)) = (forward, backward) {
            equivalents
                .entry(link.from.clone())
                .or_default()
                .push(Reordering {
                    sig: link.to.clone(),
                    order: forward,
                });
            equivalents
                .entry(link.to.clone())
                .or_default()
                .push(Reordering {
                    sig: link.from.clone(),
                    order: backward,
                });
        }
    }
    for reorderings in equivalents.values_mut() {
        reorderings.sort_by(|a, b| a.sig.cmp(&b.sig));
        reorderings.dedup();
    }
    ws.equivalents = equivalents;
}

/// Checks every declared link.
pub(crate) fn verify(ws: &CheckedWorkspace, diags: &mut Diagnostics) {
    let mut seen: HashMap<&Ident, ()> = HashMap::new();

    for link in &ws.workspace().links {
        let at = TypePath::root().then(PathSeg::Link(link.name.clone()));

        if seen.insert(&link.name, ()).is_some() {
            diags.error(CheckErrorKind::DuplicateLink(link.name.clone()), at.clone());
        }

        // Only an equivalence may join two signatures; every other link, and
        // an equivalence that starts at a type, joins two types.
        let over_signatures = ws.joins_signatures(link);
        let equivalence = matches!(link.kind, LinkKind::Equivalent { .. });
        let mut endpoints_exist = true;
        for endpoint in [&link.from, &link.to] {
            // A type at one end of an equivalence and a signature at the other
            // was reported when it was settled.
            if equivalence && (ws.def(endpoint).is_some() || ws.sig(endpoint).is_some()) {
                continue;
            }
            if over_signatures && ws.sig(endpoint).is_none() {
                diags.error(
                    CheckErrorKind::UnknownFunction(endpoint.clone()),
                    at.clone(),
                );
                endpoints_exist = false;
            } else if !over_signatures && ws.def(endpoint).is_none() {
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
            // Settled already, and reported then if it could not be.
            LinkKind::Equivalent { pairs, .. } => {
                for pair in pairs {
                    check_coercion(ws, &pair.coercion, &at, diags);
                }
            }
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
        Coercion::ViaLink { link, reversed } => match ws.workspace().link(link) {
            None => diags.error(CheckErrorKind::UnknownLink(link.clone()), at.clone()),
            Some(found) if *reversed && !found.is_reversible() => {
                diags.error(CheckErrorKind::LinkNotReversible(link.clone()), at.clone());
            }
            Some(_) => {}
        },
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

    /// Two structs holding the same numbers under other names, and two
    /// signatures taking the same arguments in the other order.
    const SAME_DATA: &str = r#"{"universes":[
        {"name":"geo","types":[{"k":"struct","name":"Point","fields":[
            {"name":"x","ty":"float"},{"name":"y","ty":"float"}]}],
         "functions":[{"name":"fetch","ret":"str","params":[
            {"name":"url","ty":"str"},{"name":"retries","ty":"i64"}]}]},
        {"name":"map","types":[{"k":"struct","name":"LatLong","fields":[
            {"name":"lat","ty":"float"},{"name":"long","ty":"float"}]}],
         "functions":[{"name":"get","ret":"str","params":[
            {"name":"attempts","ty":"i64"},{"name":"getUrl","ty":"str"}]}]}]"#;

    fn with_links(links: &str) -> String {
        format!(r#"{SAME_DATA},"links":[{links}]}}"#)
    }

    fn checked(json: &str) -> crate::check::CheckedWorkspace {
        check_workspace(Workspace::from_json(json).expect("reads")).expect("checks")
    }

    #[test]
    fn an_equivalence_left_to_be_worked_out_is_worked_out() {
        let ws = checked(&with_links(
            r#"{"name":"point_latlong","from":"geo::Point","to":"map::LatLong",
                "kind":{"k":"equivalent"}}"#,
        ));
        let link = ws.workspace().links.first().expect("declared");
        let crate::link::LinkKind::Equivalent { pairs, .. } = &link.kind else {
            panic!("still an equivalence");
        };
        let pairs: Vec<String> = pairs
            .iter()
            .map(|p| format!("{} -> {}", p.from, p.to))
            .collect();
        assert_eq!(
            pairs,
            ["x -> lat", "y -> long"],
            "the checked link carries the pairing"
        );
    }

    #[test]
    fn a_pairing_that_is_stated_is_kept() {
        let ws = checked(&with_links(
            r#"{"name":"point_latlong","from":"geo::Point","to":"map::LatLong",
                "kind":{"k":"equivalent","pairs":[{"from":"x","to":"long"}]}}"#,
        ));
        let crate::link::LinkKind::Equivalent { pairs, .. } = &ws.workspace().links[0].kind else {
            panic!("still an equivalence");
        };
        assert!(
            pairs
                .iter()
                .any(|p| p.from.as_str() == "x" && p.to.as_str() == "long")
        );
        assert!(
            pairs
                .iter()
                .any(|p| p.from.as_str() == "y" && p.to.as_str() == "lat")
        );
    }

    #[test]
    fn an_equivalence_that_would_lose_something_is_refused() {
        let msgs = errors(&with_link(r#"{"k":"equivalent"}"#));
        assert!(
            msgs.iter()
                .any(|m| m.contains("not the same thing renamed") && m.contains("nowhere to go")),
            "{msgs:?}"
        );
    }

    #[test]
    fn running_a_one_way_link_backwards_is_refused() {
        let json = r#"{"universes":[
            {"name":"a","types":[
                {"k":"struct","name":"P","fields":[{"name":"x","ty":"float"}]},
                {"k":"struct","name":"Q","fields":[{"name":"p","ty":"P"}]}]},
            {"name":"b","types":[
                {"k":"struct","name":"V","fields":[{"name":"x","ty":"float"}]},
                {"k":"struct","name":"W","fields":[{"name":"v","ty":"V"}]}]}],
          "links":[
            {"name":"v_to_p","from":"b::V","to":"a::P","kind":{"k":"identical"}},
            {"name":"q_to_w","from":"a::Q","to":"b::W","kind":{"k":"field_map","pairs":[
                {"from":"p","to":"v","coercion":{"k":"via_link","link":"v_to_p","reversed":true}}]}}]}"#;
        assert!(
            errors(json).iter().any(|m| m.contains("only runs one way")),
            "{:?}",
            errors(json)
        );
    }

    #[test]
    fn equivalent_signatures_know_how_to_reorder_a_call() {
        let ws = checked(&with_links(
            r#"{"name":"fetch_is_get","from":"geo::fetch","to":"map::get",
                "kind":{"k":"equivalent"}}"#,
        ));
        let q = |s: &str| s.parse().unwrap();

        let from_fetch = ws.equivalents_of(&q("geo::fetch"));
        assert_eq!(from_fetch.len(), 1);
        assert_eq!(from_fetch[0].sig, q("map::get"));
        assert_eq!(
            from_fetch[0].apply(&["https://x", "3"]),
            ["3", "https://x"],
            "get takes the attempts first"
        );

        let from_get = ws.equivalents_of(&q("map::get"));
        assert_eq!(from_get[0].sig, q("geo::fetch"));
        assert_eq!(from_get[0].apply(&["3", "https://x"]), ["https://x", "3"]);
    }

    #[test]
    fn generic_signatures_settle_which_generic_parameter_is_which() {
        let ws = checked(
            r#"{"universes":[
                {"name":"py","functions":[{"name":"konst","params_decl":[{"name":"A"},{"name":"B"}],
                    "params":[{"name":"x","ty":{"k":"param","name":"A"}},{"name":"y","ty":{"k":"param","name":"B"}}],
                    "ret":{"k":"param","name":"A"}}]},
                {"name":"js","functions":[{"name":"always","params_decl":[{"name":"Y"},{"name":"X"}],
                    "params":[{"name":"ignored","ty":{"k":"param","name":"Y"}},{"name":"kept","ty":{"k":"param","name":"X"}}],
                    "ret":{"k":"param","name":"X"}}]}],
              "links":[{"name":"konst_is_always","from":"py::konst","to":"js::always",
                        "kind":{"k":"equivalent"}}]}"#,
        );
        let crate::link::LinkKind::Equivalent { generics, .. } = &ws.workspace().links[0].kind
        else {
            panic!("still an equivalence");
        };
        let generics: Vec<String> = generics
            .iter()
            .map(|g| format!("{} -> {}", g.from, g.to))
            .collect();
        assert_eq!(
            generics,
            ["B -> Y", "A -> X"],
            "the checked link carries every generic pair"
        );

        let q = |s: &str| s.parse().unwrap();
        assert_eq!(
            ws.equivalents_of(&q("py::konst"))[0].apply(&["kept", "dropped"]),
            ["dropped", "kept"],
            "a call reorders the same way as for any other signature"
        );
    }

    #[test]
    fn a_generic_pair_on_an_equivalence_between_types_names_nothing() {
        let msgs = errors(&with_links(
            r#"{"name":"point_latlong","from":"geo::Point","to":"map::LatLong",
                "kind":{"k":"equivalent","generics":[{"from":"T","to":"U"}]}}"#,
        ));
        assert!(
            msgs.iter()
                .any(|m| m.contains("no such generic parameter `T`")),
            "{msgs:?}"
        );
    }

    #[test]
    fn a_type_and_a_signature_are_never_one_thing() {
        let msgs = errors(&with_links(
            r#"{"name":"odd","from":"geo::fetch","to":"map::LatLong","kind":{"k":"equivalent"}}"#,
        ));
        assert!(
            msgs.iter()
                .any(|m| m.contains("not both types or both signatures")),
            "{msgs:?}"
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
