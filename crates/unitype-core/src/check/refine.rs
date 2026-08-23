//! Refinement: when a proof of one proposition is also a proof of another.
//!
//! `mergesort refines sort` says that anything proving `mergesort` proves
//! `sort` too. That is only true if `mergesort`'s type is usable everywhere
//! `sort`'s is — it must accept at least as much and promise at least as much —
//! so the claim is checked rather than taken on faith.
//!
//! The payoff is at dispatch time: asking for `sort` considers every
//! implementation of every signature that refines it, so a specific, faster
//! proof is found without the caller having to know it exists.

use std::collections::HashMap;

use crate::check::error::{CheckErrorKind, Diagnostics, PathSeg, TypePath, describe};
use crate::check::{CheckedWorkspace, SigIx};
use crate::ident::QualifiedName;

/// Checks refinement claims and records which signatures strengthen which.
pub(crate) fn verify(ws: &mut CheckedWorkspace, diags: &mut Diagnostics) {
    let mut edges: Vec<(SigIx, QualifiedName)> = Vec::new();

    for ix in (0..ws.sigs.len()).map(SigIx) {
        let sig = ws.sig_at(ix);
        let Some(base_name) = sig.refines.clone() else {
            continue;
        };
        let name = ws.sig_name(ix).clone();
        let path = TypePath::root()
            .then(PathSeg::Universe(name.universe.clone()))
            .then(PathSeg::Sig(name.name.clone()));

        let Some(base_ix) = ws.resolve_sig(&base_name) else {
            diags.error(CheckErrorKind::UnknownRefinement(base_name), path);
            continue;
        };
        let base = ws.sig_at(base_ix);

        if sig.params.len() != base.params.len() {
            diags.error(
                CheckErrorKind::RefinementArity {
                    sig: name.clone(),
                    base: base_name.clone(),
                    expected: base.params.len(),
                    got: sig.params.len(),
                },
                path,
            );
            continue;
        }

        let (sig_ty, base_ty) = (sig.function_type(), base.function_type());
        if !ws.is_subtype(&sig_ty, &base_ty) {
            diags.error(
                CheckErrorKind::RefinementNotSubtype {
                    sig: name.clone(),
                    base: base_name.clone(),
                    sig_ty: describe(&sig_ty),
                    base_ty: describe(&base_ty),
                },
                path,
            );
            continue;
        }

        edges.push((ix, base_name));
    }

    detect_cycles(ws, &edges, diags);

    let mut refiners: HashMap<QualifiedName, Vec<QualifiedName>> = HashMap::new();
    for (ix, base) in edges {
        refiners
            .entry(base)
            .or_default()
            .push(ws.sig_name(ix).clone());
    }
    for names in refiners.values_mut() {
        names.sort();
    }
    ws.refiners = refiners;
}

/// A loop of refinement claims would make each proposition stronger than
/// itself, and would send the dispatcher round forever looking for proofs.
fn detect_cycles(ws: &CheckedWorkspace, edges: &[(SigIx, QualifiedName)], diags: &mut Diagnostics) {
    let next: HashMap<SigIx, SigIx> = edges
        .iter()
        .filter_map(|(ix, base)| Some((*ix, ws.resolve_sig(base)?)))
        .collect();

    let mut settled: Vec<bool> = vec![false; ws.sigs.len()];
    for start in next.keys().copied() {
        if settled[start.0] {
            continue;
        }
        let mut trail = vec![start];
        let mut at = start;
        while let Some(&up) = next.get(&at) {
            if let Some(loop_start) = trail.iter().position(|s| *s == up) {
                let mut names: Vec<String> = trail[loop_start..]
                    .iter()
                    .map(|s| ws.sig_name(*s).to_string())
                    .collect();
                names.push(ws.sig_name(up).to_string());
                let name = ws.sig_name(start);
                diags.error(
                    CheckErrorKind::RefinementCycle(names),
                    TypePath::root()
                        .then(PathSeg::Universe(name.universe.clone()))
                        .then(PathSeg::Sig(name.name.clone())),
                );
                break;
            }
            if settled[up.0] {
                break;
            }
            trail.push(up);
            at = up;
        }
        for s in trail {
            settled[s.0] = true;
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::Workspace;
    use crate::check::{CheckedWorkspace, check_workspace};

    fn check(json: &str) -> Result<CheckedWorkspace, crate::check::Diagnostics> {
        check_workspace(Workspace::from_json(json).expect("reads"))
    }

    fn errors(json: &str) -> Vec<String> {
        check(json)
            .err()
            .map(|d| d.errors.iter().map(ToString::to_string).collect())
            .unwrap_or_default()
    }

    #[test]
    fn a_refinement_of_the_same_type_is_accepted_and_recorded() {
        let ws = check(
            r#"{"universes":[{"name":"demo","functions":[
                {"name":"sort","params":[{"name":"xs","ty":"[int]"}],"ret":"[int]"},
                {"name":"mergesort","refines":"demo::sort",
                 "params":[{"name":"xs","ty":"[int]"}],"ret":"[int]"}]}]}"#,
        )
        .expect("checks");
        let refiners = ws.refiners_of(&"demo::sort".parse().unwrap());
        assert_eq!(refiners.len(), 1);
        assert_eq!(refiners[0].to_string(), "demo::mergesort");
    }

    #[test]
    fn a_refinement_may_accept_more_than_what_it_refines() {
        let ws = check(
            r#"{"universes":[{"name":"demo","types":[
                {"k":"struct","name":"Shape","fields":[]},
                {"k":"struct","name":"Circle","extends":"Shape","fields":[]}],
              "functions":[
                {"name":"area","params":[{"name":"s","ty":"Circle"}],"ret":"float"},
                {"name":"any_area","refines":"demo::area",
                 "params":[{"name":"s","ty":"Shape"}],"ret":"float"}]}]}"#,
        );
        assert!(ws.is_ok(), "{:?}", ws.err());
    }

    #[test]
    fn a_refinement_that_accepts_less_is_refused() {
        let msgs = errors(
            r#"{"universes":[{"name":"demo","types":[
                {"k":"struct","name":"Shape","fields":[]},
                {"k":"struct","name":"Circle","extends":"Shape","fields":[]}],
              "functions":[
                {"name":"area","params":[{"name":"s","ty":"Shape"}],"ret":"float"},
                {"name":"circle_area","refines":"demo::area",
                 "params":[{"name":"s","ty":"Circle"}],"ret":"float"}]}]}"#,
        );
        assert!(
            msgs.iter().any(|m| m.contains("cannot refine")),
            "a proof that only handles circles does not prove the general claim: {msgs:?}"
        );
    }

    #[test]
    fn refining_something_that_is_not_a_signature_is_reported() {
        let msgs = errors(
            r#"{"universes":[{"name":"demo","functions":[
                {"name":"f","refines":"demo::nope","params":[],"ret":"i64"}]}]}"#,
        );
        assert!(msgs[0].contains("nothing can refine it"), "{msgs:?}");
    }

    #[test]
    fn a_loop_of_refinements_is_reported_rather_than_hanging() {
        let msgs = errors(
            r#"{"universes":[{"name":"demo","functions":[
                {"name":"a","refines":"demo::b","params":[],"ret":"i64"},
                {"name":"b","refines":"demo::a","params":[],"ret":"i64"}]}]}"#,
        );
        assert!(
            msgs.iter().any(|m| m.contains("refinement cycle")),
            "{msgs:?}"
        );
    }

    #[test]
    fn a_refinement_with_a_different_arity_is_reported() {
        let msgs = errors(
            r#"{"universes":[{"name":"demo","functions":[
                {"name":"f","params":[{"name":"x","ty":"i64"}],"ret":"i64"},
                {"name":"g","refines":"demo::f","params":[],"ret":"i64"}]}]}"#,
        );
        assert!(msgs[0].contains("but refines"), "{msgs:?}");
    }
}
