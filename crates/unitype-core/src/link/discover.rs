//! Finding correspondences nobody wrote down.
//!
//! Once several universes are loaded together, the same data has usually been
//! described more than once — by different teams, under different names, at
//! different times. Structural addressing makes the exact matches free to find:
//! two declarations with the same address hold the same data, full stop.
//!
//! For the near misses, field names do the work. If most of one declaration's
//! folded field names appear in another's, the two are worth looking at, and
//! derivation is run to see whether a mapping actually closes.
//!
//! Nothing here creates a link. Suggestions are for a person to look at and
//! accept, because deciding that two universes are talking about the same thing
//! is a judgement about the world rather than about the types.

use std::collections::HashSet;

use crate::check::CheckedWorkspace;
use crate::ident::QualifiedName;
use crate::link::LinkKind;

/// A correspondence that looks likely.
#[derive(Clone, Debug, PartialEq)]
pub struct LinkSuggestion {
    /// The source type.
    pub from: QualifiedName,
    /// The target type.
    pub to: QualifiedName,
    /// How the conversion would work.
    pub kind: LinkKind,
    /// How confident this is: 1.0 for the same structure, lower for a match
    /// found by name.
    pub score: f32,
    /// True when the conversion would discard fields.
    pub lossy: bool,
}

impl CheckedWorkspace {
    /// Correspondences between universes that look likely enough to consider.
    ///
    /// `min_score` is the share of field names that must line up for a near
    /// miss to be reported; 0.5 is a reasonable starting point. Declarations
    /// already linked are left out, since the answer is known.
    pub fn suggest_links(&self, min_score: f32) -> Vec<LinkSuggestion> {
        let linked: HashSet<(QualifiedName, QualifiedName)> = self
            .workspace()
            .links
            .iter()
            .map(|l| (l.from.clone(), l.to.clone()))
            .collect();

        let mut named: Vec<(QualifiedName, Vec<String>)> = Vec::new();
        for name in self.def_names() {
            if let Some(fields) = self.fields_by_name(name) {
                named.push((name.clone(), fields.iter().map(|f| f.name.fold()).collect()));
            }
        }

        let mut out = Vec::new();
        for (i, (from, from_keys)) in named.iter().enumerate() {
            for (to, to_keys) in &named[i + 1..] {
                if from.universe == to.universe {
                    continue;
                }
                for (from, to) in [(from, to), (to, from)] {
                    if linked.contains(&(from.clone(), to.clone())) {
                        continue;
                    }
                    let score = if self.shape_hash(from) == self.shape_hash(to) {
                        1.0
                    } else {
                        overlap(from_keys, to_keys)
                    };
                    if score < min_score {
                        continue;
                    }
                    // A name-based hunch still has to survive derivation.
                    let Ok(kind) = self.derive_link(from, to, &[]) else {
                        continue;
                    };
                    out.push(LinkSuggestion {
                        from: from.clone(),
                        to: to.clone(),
                        lossy: matches!(&kind, LinkKind::FieldMap { dropped, .. } if !dropped.is_empty()),
                        kind,
                        score,
                    });
                }
            }
        }

        // Best first, then by name so that two runs agree.
        out.sort_by(|a, b| {
            b.score
                .total_cmp(&a.score)
                .then_with(|| (&a.from, &a.to).cmp(&(&b.from, &b.to)))
        });
        out
    }
}

/// The share of field names the two have in common.
fn overlap(a: &[String], b: &[String]) -> f32 {
    let a: HashSet<&String> = a.iter().collect();
    let b: HashSet<&String> = b.iter().collect();
    let union = a.union(&b).count();
    if union == 0 {
        return 0.0;
    }
    a.intersection(&b).count() as f32 / union as f32
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Workspace;
    use crate::check::check_workspace;

    fn ws(json: &str) -> CheckedWorkspace {
        check_workspace(Workspace::from_json(json).expect("reads")).expect("checks")
    }

    const TWO_UNIVERSES: &str = r#"{"universes":[
        {"name":"api","types":[
            {"k":"struct","name":"User","fields":[
                {"name":"userId","ty":"str"},{"name":"email","ty":"str"}]},
            {"k":"struct","name":"Session","fields":[
                {"name":"token","ty":"str"}]}]},
        {"name":"db","types":[
            {"k":"struct","name":"UserRow","fields":[
                {"name":"user_id","ty":"str"},{"name":"email","ty":"str"}]},
            {"k":"struct","name":"Widget","fields":[
                {"name":"colour","ty":"str"},{"name":"weight","ty":"float"}]}]}]}"#;

    #[test]
    fn the_same_data_described_twice_is_found_with_certainty() {
        let ws = ws(TWO_UNIVERSES);
        let found = ws.suggest_links(0.5);
        let best = found.first().expect("something should be suggested");
        assert_eq!(best.score, 1.0);
        assert_eq!(
            (best.from.to_string(), best.to.to_string()),
            ("api::User".into(), "db::UserRow".into()),
            "these hold the same data under different spellings"
        );
        assert!(
            matches!(best.kind, LinkKind::FieldMap { .. }),
            "the fields are spelled differently, so a mapping is needed: {:?}",
            best.kind
        );
    }

    #[test]
    fn unrelated_declarations_are_not_suggested() {
        let ws = ws(TWO_UNIVERSES);
        let pairs: Vec<String> = ws
            .suggest_links(0.5)
            .iter()
            .map(|s| format!("{} -> {}", s.from, s.to))
            .collect();
        assert!(
            !pairs.iter().any(|p| p.contains("Widget")),
            "a widget has nothing to do with a session: {pairs:?}"
        );
    }

    #[test]
    fn declarations_within_one_universe_are_never_suggested() {
        let ws = ws(r#"{"universes":[{"name":"a","types":[
            {"k":"struct","name":"X","fields":[{"name":"id","ty":"str"}]},
            {"k":"struct","name":"Y","fields":[{"name":"id","ty":"str"}]}]}]}"#);
        assert!(ws.suggest_links(0.0).is_empty());
    }

    #[test]
    fn a_correspondence_already_declared_is_not_suggested_again() {
        let with_link = TWO_UNIVERSES.trim_end_matches('}').to_string()
            + r#","links":[{"name":"user_row","from":"api::User","to":"db::UserRow",
                            "kind":{"k":"field_map","pairs":[
                                {"from":"userId","to":"user_id","coercion":{"k":"identity"}},
                                {"from":"email","to":"email","coercion":{"k":"identity"}}]}}]}"#;
        let ws = ws(&with_link);
        assert!(
            !ws.suggest_links(0.5)
                .iter()
                .any(|s| s.from.to_string() == "api::User" && s.to.to_string() == "db::UserRow"),
            "this one is settled"
        );
    }

    #[test]
    fn a_suggestion_that_would_lose_data_says_so() {
        let ws = ws(r#"{"universes":[
            {"name":"a","types":[{"k":"struct","name":"X","fields":[
                {"name":"id","ty":"str"},{"name":"email","ty":"str"},
                {"name":"secret","ty":"str"}]}]},
            {"name":"b","types":[{"k":"struct","name":"Y","fields":[
                {"name":"id","ty":"str"},{"name":"email","ty":"str"}]}]}]}"#);

        let lossy = ws
            .suggest_links(0.4)
            .into_iter()
            .find(|s| s.from.to_string() == "a::X")
            .expect("suggested");
        assert!(lossy.lossy, "dropping `secret` should be flagged");
    }

    #[test]
    fn suggestions_come_out_in_the_same_order_every_time() {
        let ws = ws(TWO_UNIVERSES);
        assert_eq!(ws.suggest_links(0.3), ws.suggest_links(0.3));
    }

    #[test]
    fn overlap_measures_what_the_two_have_in_common() {
        let a = ["id".to_string(), "email".to_string()];
        let b = ["id".to_string(), "email".to_string()];
        assert_eq!(overlap(&a, &b), 1.0);
        assert_eq!(overlap(&a, &["id".to_string()]), 0.5);
        assert_eq!(overlap(&a, &["colour".to_string()]), 0.0);
        assert_eq!(overlap(&[], &[]), 0.0);
    }
}
