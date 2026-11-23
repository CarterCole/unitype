//! Complexity bounds: how much time and space a proof of a proposition may
//! claim.
//!
//! A bound is written as a named class, so that two bounds can be compared. A
//! refinement follows the bounds of what it refines the way it follows its
//! type: it may promise less cost, never more, and it must promise a bound at
//! all when what it refines does. What a proof costs is its own claim, declared
//! where it is registered; dispatch refuses a claim that is over the bound.

use crate::check::error::{CheckErrorKind, Diagnostics, PathSeg, TypePath};
use crate::check::{CheckedWorkspace, SigIx};

/// Checks every bound is a named class, and that each refinement promises no
/// more cost than what it refines.
pub(crate) fn verify(ws: &CheckedWorkspace, diags: &mut Diagnostics) {
    for ix in (0..ws.sigs.len()).map(SigIx) {
        let sig = ws.sig_at(ix);
        let name = ws.sig_name(ix).clone();
        let path = TypePath::root()
            .then(PathSeg::Universe(name.universe.clone()))
            .then(PathSeg::Sig(name.name.clone()));

        for (axis, bound) in [("time", &sig.time), ("space", &sig.space)] {
            if let Some(class) = bound {
                if class.rank().is_none() {
                    diags.error(
                        CheckErrorKind::UnnamedCost {
                            sig: name.clone(),
                            axis,
                            class: class.token().to_string(),
                        },
                        path.clone(),
                    );
                }
            }
        }

        let Some(base_name) = sig.refines.clone() else {
            continue;
        };
        // An unknown refinement is reported where refinements are checked.
        let Some(base_ix) = ws.resolve_sig(&base_name) else {
            continue;
        };
        let base = ws.sig_at(base_ix);

        for (axis, want, have) in [
            ("time", &base.time, &sig.time),
            ("space", &base.space, &sig.space),
        ] {
            let Some(want) = want else {
                continue;
            };
            // A bound the refined signature names wrongly is already reported.
            let Some(want_rank) = want.rank() else {
                continue;
            };
            match have {
                None => diags.error(
                    CheckErrorKind::CostNotPromised {
                        sig: name.clone(),
                        base: base_name.clone(),
                        axis,
                        bound: want.to_string(),
                    },
                    path.clone(),
                ),
                Some(have) => {
                    if have.rank().is_some_and(|rank| rank > want_rank) {
                        diags.error(
                            CheckErrorKind::CostTooHigh {
                                sig: name.clone(),
                                base: base_name.clone(),
                                axis,
                                have: have.to_string(),
                                bound: want.to_string(),
                            },
                            path.clone(),
                        );
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::Workspace;
    use crate::canon::sig_hash;
    use crate::check::check_workspace;

    fn errors(json: &str) -> Vec<String> {
        check_workspace(Workspace::from_json(json).expect("reads"))
            .err()
            .map(|d| d.errors.iter().map(ToString::to_string).collect())
            .unwrap_or_default()
    }

    /// A workspace with `sort` bounded by `base_time` and `fast_sort` refining
    /// it with `refiner_time`. Either may be absent.
    fn sorting(base_time: Option<&str>, refiner_time: Option<&str>) -> String {
        let bound = |t: Option<&str>| t.map(|t| format!(r#","time":"{t}""#)).unwrap_or_default();
        format!(
            r#"{{"universes":[{{"name":"demo","functions":[
                {{"name":"sort","params":[{{"name":"xs","ty":"[i64]"}}],"ret":"[i64]"{}}},
                {{"name":"fast_sort","refines":"demo::sort",
                  "params":[{{"name":"xs","ty":"[i64]"}}],"ret":"[i64]"{}}}]}}]}}"#,
            bound(base_time),
            bound(refiner_time)
        )
    }

    #[test]
    fn a_bound_is_a_named_class_or_it_is_refused() {
        let said = errors(
            r#"{"universes":[{"name":"demo","functions":[
                {"name":"sort","params":[{"name":"xs","ty":"[i64]"}],"ret":"[i64]",
                 "time":"about_quadratic"}]}]}"#,
        );
        assert!(
            said.iter().any(|e| e.contains("not a complexity class")),
            "{said:?}"
        );
    }

    #[test]
    fn a_refinement_may_promise_less_cost_and_no_more() {
        assert!(errors(&sorting(Some("o_n2"), Some("o_n_log_n"))).is_empty());
        let said = errors(&sorting(Some("o_n_log_n"), Some("o_n2")));
        assert!(said.iter().any(|e| e.contains("is more than")), "{said:?}");
    }

    #[test]
    fn a_refinement_must_bound_its_cost_when_what_it_refines_does() {
        let said = errors(&sorting(Some("o_n_log_n"), None));
        assert!(
            said.iter().any(|e| e.contains("must bound it too")),
            "{said:?}"
        );
        assert!(
            errors(&sorting(None, Some("o1"))).is_empty(),
            "a bound on a refinement of something unbounded is a stronger claim"
        );
    }

    #[test]
    fn a_bound_is_part_of_what_the_proposition_is() {
        let hash = |bound: &str| {
            let json = format!(
                r#"{{"universes":[{{"name":"demo","functions":[
                    {{"name":"sort","params":[{{"name":"xs","ty":"[i64]"}}],"ret":"[i64]"{bound}}}]}}]}}"#
            );
            let ws = check_workspace(Workspace::from_json(&json).expect("reads")).expect("checks");
            sig_hash(ws.sig(&"demo::sort".parse().unwrap()).expect("present"))
        };
        let bare = hash("");
        let bounded = hash(r#","time":"o_n_log_n""#);
        assert_ne!(bare, bounded, "a different cost is a different proposition");
    }
}
