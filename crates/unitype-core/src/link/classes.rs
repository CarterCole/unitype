//! Equivalence classes: everything that is the same thing.
//!
//! Links relate types pairwise, but the interesting object is the class. When
//! `api::User` links to `db::UserRow` and `db::UserRow` links to
//! `events::UserPayload`, all three are one concept described three times, and
//! the useful question is not "is there a link between these two" but "which of
//! these are the same thing, and how do I get from any one to any other".
//!
//! Composition answers the second half. A path through the link graph composes
//! into a single conversion — the bodies chain, because a bridge is an ordinary
//! expression body and expressions compose. Nobody has to declare the `a` to
//! `c` link that `a` to `b` to `c` already implies.
//!
//! Two cautions the classes carry with them. A class is only lossless if every
//! link in it is: once one conversion drops a field, going around the class does
//! not bring it back. And membership is deliberately about *declared* links, not
//! about structural similarity — two types being shaped alike is a fact about
//! the types, whereas being the same thing is a claim about the world, and only
//! a person makes that claim.

use std::collections::{HashMap, HashSet, VecDeque};

use crate::check::CheckedWorkspace;
use crate::ident::QualifiedName;
use crate::ir::{Expr, FunctionSig, NamedParam, Type, TypeRef};
use crate::link::{CompileError, Link};
use crate::meta::Metadata;

/// A set of declarations that all describe the same thing.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Equivalence {
    /// The declarations in the class, sorted.
    pub members: Vec<QualifiedName>,
    /// True when no link within the class discards anything.
    ///
    /// A lossy class is still a class — the types still describe one thing —
    /// but going around it does not get you back what you started with.
    pub lossless: bool,
}

impl Equivalence {
    /// Whether a declaration is in this class.
    pub fn contains(&self, name: &QualifiedName) -> bool {
        self.members.binary_search(name).is_ok()
    }
}

impl CheckedWorkspace {
    /// Every set of declarations that the declared links say are one thing.
    ///
    /// Classes with a single member are left out: a type nobody linked to
    /// anything is not an interesting class.
    pub fn equivalence_classes(&self) -> Vec<Equivalence> {
        let mut groups: HashMap<QualifiedName, QualifiedName> = HashMap::new();

        // Direction does not matter for membership. A one-way conversion still
        // says the two are the same thing; it says something else about how
        // much of it survives the trip, which `lossless` records.
        for link in &self.workspace().links {
            let (a, b) = (find(&mut groups, &link.from), find(&mut groups, &link.to));
            if a != b {
                // Keep the smaller name as the representative, so that the same
                // workspace always produces the same classes.
                let (keep, merge) = if a < b { (a, b) } else { (b, a) };
                groups.insert(merge, keep);
            }
        }

        let mut by_root: HashMap<QualifiedName, Vec<QualifiedName>> = HashMap::new();
        let members: Vec<QualifiedName> = groups.keys().cloned().collect();
        for name in members {
            let root = find(&mut groups, &name);
            by_root.entry(root).or_default().push(name);
        }

        let mut classes: Vec<Equivalence> = by_root
            .into_values()
            .filter(|members| members.len() > 1)
            .map(|mut members| {
                members.sort();
                let lossless = self
                    .workspace()
                    .links
                    .iter()
                    .filter(|l| members.binary_search(&l.from).is_ok())
                    .all(|l| !l.is_lossy());
                Equivalence { members, lossless }
            })
            .collect();
        classes.sort_by(|a, b| a.members.cmp(&b.members));
        classes
    }

    /// The class a declaration belongs to, if it belongs to one.
    pub fn class_of(&self, name: &QualifiedName) -> Option<Equivalence> {
        self.equivalence_classes()
            .into_iter()
            .find(|c| c.contains(name))
    }

    /// The shortest chain of links leading from one declaration to another.
    ///
    /// Returns an empty chain when the two are the same declaration, and `None`
    /// when no sequence of declared links gets there. Ties are broken by link
    /// name so that two runs agree.
    pub fn bridge_path(&self, from: &QualifiedName, to: &QualifiedName) -> Option<Vec<&Link>> {
        if from == to {
            return Some(Vec::new());
        }

        let mut outgoing: HashMap<&QualifiedName, Vec<&Link>> = HashMap::new();
        for link in &self.workspace().links {
            outgoing.entry(&link.from).or_default().push(link);
        }
        for links in outgoing.values_mut() {
            links.sort_by(|a, b| a.name.cmp(&b.name));
        }

        // Breadth-first, so the chain found is the shortest — which matters,
        // since every extra hop is another chance to lose something.
        let mut seen: HashSet<&QualifiedName> = HashSet::from([from]);
        let mut queue: VecDeque<(&QualifiedName, Vec<&Link>)> =
            VecDeque::from([(from, Vec::new())]);

        while let Some((at, path)) = queue.pop_front() {
            for link in outgoing.get(at).into_iter().flatten() {
                if !seen.insert(&link.to) {
                    continue;
                }
                let mut next = path.clone();
                next.push(link);
                if &link.to == to {
                    return Some(next);
                }
                queue.push_back((&link.to, next));
            }
        }
        None
    }

    /// A single conversion standing for a whole chain of links.
    ///
    /// `a` to `b` to `c` becomes one function from `a` to `c`, with the bodies
    /// chained. The result is an ordinary signature: it type-checks like any
    /// other, runs like any other, and generates like any other.
    pub fn compose_bridge(
        &self,
        from: &QualifiedName,
        to: &QualifiedName,
        name: crate::ident::Ident,
    ) -> Option<Result<FunctionSig, CompileError>> {
        let path = self.bridge_path(from, to)?;
        Some(self.compose_path(&path, from, to, name))
    }

    fn compose_path(
        &self,
        path: &[&Link],
        from: &QualifiedName,
        to: &QualifiedName,
        name: crate::ident::Ident,
    ) -> Result<FunctionSig, CompileError> {
        let subject = crate::ident::Ident::new_unchecked("value");
        let mut carried = Expr::Var {
            name: subject.clone(),
        };
        for (hop, link) in path.iter().enumerate() {
            carried = self.apply_link(link, carried, hop)?;
        }

        let hops: Vec<String> = path.iter().map(|l| l.name.to_string()).collect();
        let lossy = path.iter().any(|l| l.is_lossy());
        let doc = format!(
            "Converts a `{from}` into a `{to}`, through {}.{}",
            if hops.is_empty() {
                "no conversion at all".to_string()
            } else {
                hops.join(" then ")
            },
            if lossy {
                " Some fields are dropped along the way."
            } else {
                ""
            }
        );

        Ok(FunctionSig {
            name,
            doc: Some(doc),
            params_decl: Vec::new(),
            params: vec![NamedParam::new(subject, named(from))],
            ret: named(to),
            refines: None,
            body: Some(carried),
            meta: Metadata::default(),
        })
    }
}

/// Union-find with path compression, over qualified names.
fn find(groups: &mut HashMap<QualifiedName, QualifiedName>, name: &QualifiedName) -> QualifiedName {
    let parent = groups
        .entry(name.clone())
        .or_insert_with(|| name.clone())
        .clone();
    if &parent == name {
        return parent;
    }
    let root = find(groups, &parent);
    groups.insert(name.clone(), root.clone());
    root
}

fn named(name: &QualifiedName) -> Type {
    Type::Ref(TypeRef::qualified(name.universe.clone(), name.name.clone()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Workspace;
    use crate::check::check_workspace;
    use crate::ident::Ident;

    fn ws(json: &str) -> CheckedWorkspace {
        check_workspace(Workspace::from_json(json).expect("reads")).expect("checks")
    }

    fn q(s: &str) -> QualifiedName {
        s.parse().unwrap()
    }

    /// One concept described in three universes, linked end to end.
    const CHAIN: &str = r#"{"universes":[
        {"name":"api","types":[{"k":"struct","name":"User","fields":[
            {"name":"id","ty":"str"},{"name":"email","ty":"str"}]}]},
        {"name":"db","types":[{"k":"struct","name":"UserRow","fields":[
            {"name":"id","ty":"str"},{"name":"email","ty":"str"}]}]},
        {"name":"events","types":[{"k":"struct","name":"UserPayload","fields":[
            {"name":"id","ty":"str"},{"name":"email","ty":"str"}]}]},
        {"name":"other","types":[{"k":"struct","name":"Widget","fields":[
            {"name":"colour","ty":"str"}]}]}],
      "links":[
        {"name":"api_to_db","from":"api::User","to":"db::UserRow","kind":{"k":"identical"}},
        {"name":"db_to_events","from":"db::UserRow","to":"events::UserPayload",
         "kind":{"k":"identical"}}]}"#;

    #[test]
    fn types_linked_end_to_end_form_one_class() {
        let classes = ws(CHAIN).equivalence_classes();
        assert_eq!(classes.len(), 1);
        assert_eq!(
            classes[0]
                .members
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>(),
            ["api::User", "db::UserRow", "events::UserPayload"],
            "three descriptions of one thing"
        );
        assert!(classes[0].lossless);
    }

    #[test]
    fn a_type_nobody_linked_is_not_a_class_of_its_own() {
        let classes = ws(CHAIN).equivalence_classes();
        assert!(
            !classes.iter().any(|c| c.contains(&q("other::Widget"))),
            "a widget on its own is not an equivalence"
        );
    }

    #[test]
    fn a_class_says_whether_going_around_it_loses_anything() {
        let lossy = r#"{"universes":[
            {"name":"a","types":[{"k":"struct","name":"X","fields":[
                {"name":"id","ty":"str"},{"name":"secret","ty":"str"}]}]},
            {"name":"b","types":[{"k":"struct","name":"Y","fields":[
                {"name":"id","ty":"str"}]}]}],
          "links":[{"name":"x_to_y","from":"a::X","to":"b::Y",
                    "kind":{"k":"field_map","dropped":["secret"],
                            "pairs":[{"from":"id","to":"id","coercion":{"k":"identity"}}]}}]}"#;
        let classes = ws(lossy).equivalence_classes();
        assert!(
            !classes[0].lossless,
            "`secret` does not come back, and the class should say so"
        );
    }

    #[test]
    fn a_path_is_found_through_intermediate_types() {
        let ws = ws(CHAIN);
        let path = ws
            .bridge_path(&q("api::User"), &q("events::UserPayload"))
            .expect("reachable");
        assert_eq!(
            path.iter().map(|l| l.name.to_string()).collect::<Vec<_>>(),
            ["api_to_db", "db_to_events"]
        );
    }

    #[test]
    fn a_path_to_somewhere_unreachable_is_not_invented() {
        let ws = ws(CHAIN);
        assert!(
            ws.bridge_path(&q("api::User"), &q("other::Widget"))
                .is_none()
        );
        assert!(
            ws.bridge_path(&q("events::UserPayload"), &q("api::User"))
                .is_none(),
            "these links only run one way, and a round trip is two links"
        );
    }

    #[test]
    fn a_type_reaches_itself_without_converting_anything() {
        let ws = ws(CHAIN);
        assert_eq!(
            ws.bridge_path(&q("api::User"), &q("api::User")),
            Some(Vec::new())
        );
    }

    #[test]
    fn a_chain_composes_into_one_conversion() {
        let ws = ws(CHAIN);
        let composed = ws
            .compose_bridge(
                &q("api::User"),
                &q("events::UserPayload"),
                Ident::new("api_to_events").unwrap(),
            )
            .expect("reachable")
            .expect("composes");

        assert_eq!(
            crate::canon::canonical_string(&composed.params[0].ty),
            crate::canon::canonical_string(&named(&q("api::User")))
        );
        assert_eq!(
            crate::canon::canonical_string(&composed.ret),
            crate::canon::canonical_string(&named(&q("events::UserPayload")))
        );
        assert!(
            composed
                .doc
                .as_ref()
                .unwrap()
                .contains("api_to_db then db_to_events"),
            "the composition should say what it went through: {:?}",
            composed.doc
        );
    }

    #[test]
    fn the_composed_conversion_type_checks_like_any_other() {
        let ws = ws(CHAIN);
        let composed = ws
            .compose_bridge(
                &q("api::User"),
                &q("events::UserPayload"),
                Ident::new("api_to_events").unwrap(),
            )
            .expect("reachable")
            .expect("composes");

        // Nobody declared this conversion; it fell out of two that were
        // declared. It still has to survive the ordinary checker.
        let mut workspace = ws.workspace().clone();
        workspace.universes[0].functions.push(composed);
        assert!(
            check_workspace(workspace).is_ok(),
            "a conversion nobody wrote should be as sound as one somebody did"
        );
    }

    #[test]
    fn a_composition_that_loses_something_says_so() {
        let lossy = r#"{"universes":[
            {"name":"a","types":[{"k":"struct","name":"X","fields":[
                {"name":"id","ty":"str"},{"name":"secret","ty":"str"}]}]},
            {"name":"b","types":[{"k":"struct","name":"Y","fields":[
                {"name":"id","ty":"str"}]}]},
            {"name":"c","types":[{"k":"struct","name":"Z","fields":[
                {"name":"id","ty":"str"}]}]}],
          "links":[
            {"name":"x_to_y","from":"a::X","to":"b::Y",
             "kind":{"k":"field_map","dropped":["secret"],
                     "pairs":[{"from":"id","to":"id","coercion":{"k":"identity"}}]}},
            {"name":"y_to_z","from":"b::Y","to":"c::Z","kind":{"k":"identical"}}]}"#;
        let ws = ws(lossy);
        let composed = ws
            .compose_bridge(&q("a::X"), &q("c::Z"), Ident::new("x_to_z").unwrap())
            .expect("reachable")
            .expect("composes");
        assert!(
            composed.doc.as_ref().unwrap().contains("dropped"),
            "a lossy chain should warn: {:?}",
            composed.doc
        );
    }

    #[test]
    fn classes_come_out_the_same_way_every_time() {
        let ws = ws(CHAIN);
        assert_eq!(ws.equivalence_classes(), ws.equivalence_classes());
    }
}
