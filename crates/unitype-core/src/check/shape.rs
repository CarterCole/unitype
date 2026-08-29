//! Structural addressing: what a declaration *is*, with every name erased.
//!
//! A declaration's [`decl_hash`](crate::canon::decl_hash) covers how it was
//! written. Its **shape** covers only what data it describes. Two structs in
//! different universes, called different things, reached through different
//! inheritance chains, share a shape address exactly when they hold the same
//! data — which is how a bridge between them suggests itself without anyone
//! having noticed the correspondence.
//!
//! What gets erased: the declaration's own name, the names of its supertypes,
//! and how its fields were distributed between itself and its parents. Only the
//! flattened result matters, because that is what a value of the type looks
//! like. Field *names* are kept — they are part of the data.
//!
//! # Recursion
//!
//! A type that mentions itself has no finite tree to hash. Declarations are
//! grouped into strongly connected components, each component is encoded once
//! with references inside it replaced by positional back references, and every
//! member is addressed from those shared bytes plus its own position. A
//! self-recursive declaration — much the commonest case — is therefore
//! addressed with no reference to its name at all.
//!
//! For a component with several mutually recursive members, member order has to
//! be fixed somehow, and v1 fixes it by sorting on qualified name, which puts
//! those names into the encoding. Two isomorphic mutually recursive groups with
//! different names will not share an address. This is a known limitation rather
//! than an oversight; such groups can still be linked explicitly.

use std::collections::HashMap;

use serde_json::Value as Json;

use crate::canon::hash::TypeId;
use crate::check::subst::substitute;
use crate::check::{CheckedWorkspace, DefIx};
use crate::ident::{Ident, QualifiedName};
use crate::ir::{Type, TypeDef};

/// Domain for structural addresses.
const DOMAIN_SHAPE: &str = "unitype shape v1";

/// Computes a structural address for every declaration, and indexes them.
pub(crate) fn compute(ws: &mut CheckedWorkspace) {
    let mut hashes: HashMap<QualifiedName, TypeId> = HashMap::new();

    // Components come out with everything they depend on already addressed.
    for component in components(ws) {
        let mut members: Vec<DefIx> = component;
        members.sort_by(|a, b| ws.def_name(*a).cmp(ws.def_name(*b)));
        let positions: HashMap<QualifiedName, usize> = members
            .iter()
            .enumerate()
            .map(|(i, ix)| (ws.def_name(*ix).clone(), i))
            .collect();

        let mut shared = Vec::new();
        for ix in &members {
            shared.extend(
                serde_json::to_vec(&encode_def(ws, *ix, &positions, &hashes))
                    .expect("shape encodings are plain JSON"),
            );
        }
        for (i, ix) in members.iter().enumerate() {
            let mut bytes = shared.clone();
            bytes.extend((i as u32).to_le_bytes());
            hashes.insert(ws.def_name(*ix).clone(), digest(&bytes));
        }
    }

    // An alias is only a name, so it shares its target's address outright when
    // the target is a declaration, and otherwise takes the address of the type
    // it stands for.
    for ix in (0..ws.defs.len()).map(DefIx) {
        if !matches!(ws.def_at(ix), TypeDef::Alias(_)) {
            continue;
        }
        let TypeDef::Alias(alias) = ws.def_at(ix) else {
            unreachable!("just matched an alias");
        };
        let expanded = expand_deep(ws, &alias.target);
        let address = match &expanded {
            Type::Ref(r) => ws
                .ref_target(r)
                .and_then(|q| hashes.get(&q).copied())
                .unwrap_or_else(|| digest(&type_bytes(ws, &expanded, &positions_none(), &hashes))),
            other => digest(&type_bytes(ws, other, &positions_none(), &hashes)),
        };
        hashes.insert(ws.def_name(ix).clone(), address);
    }

    let mut index: HashMap<TypeId, Vec<QualifiedName>> = HashMap::new();
    for entry in &mut ws.defs {
        entry.shape = hashes.get(&entry.name).copied();
        if let Some(shape) = entry.shape {
            index.entry(shape).or_default().push(entry.name.clone());
        }
    }
    for names in index.values_mut() {
        names.sort();
    }
    ws.shape_index = index;
}

fn digest(bytes: &[u8]) -> TypeId {
    TypeId::from_bytes(
        *blake3::Hasher::new_derive_key(DOMAIN_SHAPE)
            .update(bytes)
            .finalize()
            .as_bytes(),
    )
}

fn positions_none() -> HashMap<QualifiedName, usize> {
    HashMap::new()
}

/// The shape-relevant content of one declaration, name erased.
fn encode_def(
    ws: &CheckedWorkspace,
    ix: DefIx,
    positions: &HashMap<QualifiedName, usize>,
    hashes: &HashMap<QualifiedName, TypeId>,
) -> Json {
    let def = ws.def_at(ix);
    let params = positional_params(def.params().iter().map(|p| &p.name));

    let entries = |items: Vec<(String, &Type)>| -> Json {
        let mut list: Vec<(String, &Type)> = items;
        // Declaration order is semantic for generated code but says nothing
        // about the data, which is an unordered set of named parts.
        list.sort_by(|a, b| a.0.cmp(&b.0));
        Json::Array(
            list.into_iter()
                .map(|(name, ty)| {
                    let mut obj = serde_json::Map::new();
                    obj.insert("name".into(), Json::String(name));
                    obj.insert("ty".into(), shape_json(ws, ty, &params, positions, hashes));
                    Json::Object(obj)
                })
                .collect(),
        )
    };

    let mut obj = serde_json::Map::new();
    match def {
        TypeDef::Struct(_) | TypeDef::Interface(_) => {
            obj.insert(
                "k".into(),
                Json::String(
                    if def.as_struct().is_some() {
                        "fields"
                    } else {
                        "contract"
                    }
                    .into(),
                ),
            );
            obj.insert(
                "fields".into(),
                entries(
                    ws.defs[ix.0]
                        .flat_fields
                        .iter()
                        .map(|f| (f.name.to_string(), &f.ty))
                        .collect(),
                ),
            );
        }
        TypeDef::Enum(d) => {
            obj.insert("k".into(), Json::String("variants".into()));
            obj.insert(
                "variants".into(),
                entries(
                    d.variants
                        .iter()
                        .map(|v| (v.name.to_string(), &v.payload))
                        .collect(),
                ),
            );
        }
        TypeDef::Alias(_) => {
            // Handled separately: an alias is not a node in the graph.
            obj.insert("k".into(), Json::String("alias".into()));
        }
    }
    Json::Object(obj)
}

fn type_bytes(
    ws: &CheckedWorkspace,
    t: &Type,
    positions: &HashMap<QualifiedName, usize>,
    hashes: &HashMap<QualifiedName, TypeId>,
) -> Vec<u8> {
    serde_json::to_vec(&shape_json(ws, t, &HashMap::new(), positions, hashes))
        .expect("shape encodings are plain JSON")
}

/// A type in shape form: aliases expanded, parameters positional, references
/// replaced by the address of what they point at or a back reference.
fn shape_json(
    ws: &CheckedWorkspace,
    t: &Type,
    params: &HashMap<Ident, usize>,
    positions: &HashMap<QualifiedName, usize>,
    hashes: &HashMap<QualifiedName, TypeId>,
) -> Json {
    let expanded = expand_deep(ws, t);
    let renamed = rename_params(&expanded, params);
    let mut json = serde_json::to_value(&renamed).expect("the IR serializes infallibly");
    rewrite_refs(&mut json, positions, hashes);
    json
}

/// Replaces every reference in a serialized type.
///
/// Within a serialized type, any object tagged `ref` is genuinely a reference:
/// the only untagged sub-objects are literal payloads and field wrappers.
fn rewrite_refs(
    json: &mut Json,
    positions: &HashMap<QualifiedName, usize>,
    hashes: &HashMap<QualifiedName, TypeId>,
) {
    match json {
        Json::Array(items) => items
            .iter_mut()
            .for_each(|i| rewrite_refs(i, positions, hashes)),
        Json::Object(obj) => {
            for value in obj.values_mut() {
                rewrite_refs(value, positions, hashes);
            }
            if obj.get("k").and_then(Json::as_str) != Some("ref") {
                return;
            }
            let (Some(universe), Some(name)) = (
                obj.get("universe").and_then(Json::as_str),
                obj.get("name").and_then(Json::as_str),
            ) else {
                return;
            };
            let Ok(target) = format!("{universe}::{name}").parse::<QualifiedName>() else {
                return;
            };
            let args = obj.get("args").cloned();

            let mut replacement = serde_json::Map::new();
            if let Some(&position) = positions.get(&target) {
                replacement.insert("k".into(), Json::String("rec".into()));
                replacement.insert("i".into(), Json::Number(position.into()));
            } else if let Some(hash) = hashes.get(&target) {
                replacement.insert("k".into(), Json::String("id".into()));
                replacement.insert("h".into(), Json::String(hash.to_string()));
            } else {
                // Nothing to erase it to; leave the name in place rather than
                // silently producing a different type's address.
                return;
            }
            if let Some(args) = args {
                replacement.insert("args".into(), args);
            }
            *obj = replacement;
        }
        _ => {}
    }
}

/// Expands every alias anywhere in a type.
fn expand_deep(ws: &CheckedWorkspace, t: &Type) -> Type {
    let expanded = ws.expand_alias(t);
    let kids: Vec<Type> = expanded
        .children()
        .into_iter()
        .map(|c| expand_deep(ws, c))
        .collect();
    expanded.with_children(kids)
}

fn positional_params<'a>(names: impl Iterator<Item = &'a Ident>) -> HashMap<Ident, usize> {
    names.enumerate().map(|(i, n)| (n.clone(), i)).collect()
}

/// Renames generic parameters to their positions, so that a declaration's
/// choice of parameter names is not part of what it is.
fn rename_params(t: &Type, params: &HashMap<Ident, usize>) -> Type {
    if params.is_empty() {
        return t.clone();
    }
    let subst = params
        .iter()
        .map(|(name, i)| {
            (
                name.clone(),
                Type::Param {
                    name: Ident::new(format!("_{i}")).expect("positional names are valid"),
                },
            )
        })
        .collect();
    substitute(t, &subst)
}

/// Strongly connected components of the declaration graph, each emitted after
/// everything it depends on.
fn components(ws: &CheckedWorkspace) -> Vec<Vec<DefIx>> {
    let count = ws.defs.len();
    let mut state = Tarjan {
        index: vec![None; count],
        low: vec![0; count],
        on_stack: vec![false; count],
        stack: Vec::new(),
        next: 0,
        out: Vec::new(),
    };

    let edges: Vec<Vec<DefIx>> = (0..count).map(|i| dependencies(ws, DefIx(i))).collect();
    for start in 0..count {
        if state.index[start].is_none() && !matches!(ws.def_at(DefIx(start)), TypeDef::Alias(_)) {
            state.visit(start, &edges);
        }
    }
    state.out
}

/// Every declaration whose shape this one's shape depends on.
fn dependencies(ws: &CheckedWorkspace, ix: DefIx) -> Vec<DefIx> {
    let mut out = Vec::new();
    let mut collect = |t: &Type| {
        let expanded = expand_deep(ws, t);
        gather_refs(ws, &expanded, &mut out);
    };
    match ws.def_at(ix) {
        TypeDef::Struct(_) | TypeDef::Interface(_) => {
            for field in &ws.defs[ix.0].flat_fields {
                collect(&field.ty);
            }
        }
        TypeDef::Enum(d) => {
            for variant in &d.variants {
                collect(&variant.payload);
            }
        }
        TypeDef::Alias(_) => {}
    }
    out.sort();
    out.dedup();
    out
}

fn gather_refs(ws: &CheckedWorkspace, t: &Type, out: &mut Vec<DefIx>) {
    if let Type::Ref(r) = t {
        out.extend(ws.ref_target(r).and_then(|q| ws.resolve(&q)));
    }
    for child in t.children() {
        gather_refs(ws, child, out);
    }
}

struct Tarjan {
    index: Vec<Option<usize>>,
    low: Vec<usize>,
    on_stack: Vec<bool>,
    stack: Vec<usize>,
    next: usize,
    out: Vec<Vec<DefIx>>,
}

impl Tarjan {
    fn visit(&mut self, at: usize, edges: &[Vec<DefIx>]) {
        self.index[at] = Some(self.next);
        self.low[at] = self.next;
        self.next += 1;
        self.stack.push(at);
        self.on_stack[at] = true;

        for next in &edges[at] {
            let next = next.0;
            match self.index[next] {
                None => {
                    self.visit(next, edges);
                    self.low[at] = self.low[at].min(self.low[next]);
                }
                Some(index) if self.on_stack[next] => {
                    self.low[at] = self.low[at].min(index);
                }
                Some(_) => {}
            }
        }

        if self.low[at] == self.index[at].expect("visited") {
            let mut component = Vec::new();
            while let Some(member) = self.stack.pop() {
                self.on_stack[member] = false;
                component.push(DefIx(member));
                if member == at {
                    break;
                }
            }
            self.out.push(component);
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::Workspace;
    use crate::check::{CheckedWorkspace, check_workspace};

    fn check(json: &str) -> CheckedWorkspace {
        check_workspace(Workspace::from_json(json).expect("reads")).expect("checks")
    }

    #[test]
    fn the_same_data_under_different_names_has_one_address() {
        let ws = check(
            r#"{"universes":[
                {"name":"geo","types":[{"k":"struct","name":"Point",
                 "fields":[{"name":"x","ty":"float"},{"name":"y","ty":"float"}]}]},
                {"name":"gfx","types":[{"k":"struct","name":"Vec2",
                 "fields":[{"name":"x","ty":"float"},{"name":"y","ty":"float"}]}]}]}"#,
        );
        let point = ws.shape_hash(&"geo::Point".parse().unwrap()).unwrap();
        let vec2 = ws.shape_hash(&"gfx::Vec2".parse().unwrap()).unwrap();
        assert_eq!(
            point, vec2,
            "two universes describing the same data should agree on its address"
        );
        assert_eq!(ws.defs_with_shape(&point).len(), 2);
    }

    #[test]
    fn where_a_field_came_from_is_not_part_of_what_a_type_is() {
        let ws = check(
            r#"{"universes":[
                {"name":"a","types":[
                    {"k":"struct","name":"Base","fields":[{"name":"id","ty":"str"}]},
                    {"k":"struct","name":"Circle","extends":"Base",
                     "fields":[{"name":"radius","ty":"float"}]}]},
                {"name":"b","types":[
                    {"k":"struct","name":"Flat","fields":[
                        {"name":"id","ty":"str"},{"name":"radius","ty":"float"}]}]}]}"#,
        );
        assert_eq!(
            ws.shape_hash(&"a::Circle".parse().unwrap()),
            ws.shape_hash(&"b::Flat".parse().unwrap()),
            "inheritance is how a type was written, not what it holds"
        );
    }

    #[test]
    fn different_data_has_different_addresses() {
        let ws = check(
            r#"{"universes":[{"name":"demo","types":[
                {"k":"struct","name":"A","fields":[{"name":"x","ty":"i64"}]},
                {"k":"struct","name":"B","fields":[{"name":"x","ty":"str"}]},
                {"k":"struct","name":"C","fields":[{"name":"y","ty":"i64"}]}]}]}"#,
        );
        let a = ws.shape_hash(&"demo::A".parse().unwrap()).unwrap();
        let b = ws.shape_hash(&"demo::B".parse().unwrap()).unwrap();
        let c = ws.shape_hash(&"demo::C".parse().unwrap()).unwrap();
        assert_ne!(a, b, "the field type matters");
        assert_ne!(a, c, "the field name matters");
    }

    #[test]
    fn a_struct_and_an_interface_with_the_same_fields_stay_distinct() {
        let ws = check(
            r#"{"universes":[{"name":"demo","types":[
                {"k":"struct","name":"S","fields":[{"name":"id","ty":"str"}]},
                {"k":"interface","name":"I","fields":[{"name":"id","ty":"str"}]}]}]}"#,
        );
        assert_ne!(
            ws.shape_hash(&"demo::S".parse().unwrap()),
            ws.shape_hash(&"demo::I".parse().unwrap()),
            "a contract is not a thing"
        );
    }

    #[test]
    fn a_self_recursive_type_is_addressed_without_using_its_name() {
        let ws = check(
            r#"{"universes":[
                {"name":"a","types":[{"k":"struct","name":"Tree","fields":[
                    {"name":"kids","ty":"[Tree]"},{"name":"value","ty":"i64"}]}]},
                {"name":"b","types":[{"k":"struct","name":"Node","fields":[
                    {"name":"kids","ty":"[Node]"},{"name":"value","ty":"i64"}]}]}]}"#,
        );
        assert_eq!(
            ws.shape_hash(&"a::Tree".parse().unwrap()),
            ws.shape_hash(&"b::Node".parse().unwrap()),
            "the same recursive structure under two names is one shape"
        );
    }

    #[test]
    fn an_alias_shares_the_address_of_what_it_names() {
        let ws = check(
            r#"{"universes":[{"name":"demo","types":[
                {"k":"struct","name":"Point","fields":[{"name":"x","ty":"float"}]},
                {"k":"alias","name":"Coord","target":"Point"}]}]}"#,
        );
        assert_eq!(
            ws.shape_hash(&"demo::Coord".parse().unwrap()),
            ws.shape_hash(&"demo::Point".parse().unwrap())
        );
    }

    #[test]
    fn a_generic_is_addressed_by_position_not_by_parameter_name() {
        let ws = check(
            r#"{"universes":[
                {"name":"a","types":[{"k":"struct","name":"Box","params":[{"name":"T"}],
                 "fields":[{"name":"item","ty":{"k":"param","name":"T"}}]}]},
                {"name":"b","types":[{"k":"struct","name":"Holder","params":[{"name":"V"}],
                 "fields":[{"name":"item","ty":{"k":"param","name":"V"}}]}]}]}"#,
        );
        assert_eq!(
            ws.shape_hash(&"a::Box".parse().unwrap()),
            ws.shape_hash(&"b::Holder".parse().unwrap())
        );
    }

    #[test]
    fn word_aliases_follow_the_structure_across_universes() {
        let ws = check(
            r#"{"universes":[
                {"name":"geo","types":[{"k":"struct","name":"Point",
                 "fields":[{"name":"x","ty":"float"},{"name":"y","ty":"float"}]}]},
                {"name":"gfx","types":[{"k":"struct","name":"Vec2",
                 "fields":[{"name":"x","ty":"float"},{"name":"y","ty":"float"}]}]}]}"#,
        );
        let index = ws.alias_index();
        let point = ws.shape_hash(&"geo::Point".parse().unwrap()).unwrap();
        let vec2 = ws.shape_hash(&"gfx::Vec2".parse().unwrap()).unwrap();
        assert_eq!(index.alias_for(&point), index.alias_for(&vec2));
        assert_eq!(
            index.alias_for(&point).unwrap().split('-').count(),
            3,
            "three words should be plenty here"
        );
    }

    #[test]
    fn a_proposition_can_be_said_out_loud_too() {
        // A signature has an address, so it should have words. Without this a
        // person can say the name of a type and not the name of a call, which
        // is the half of the vocabulary they actually have to look up.
        let ws = check(
            r#"{"universes":[{"name":"demo","functions":[
                {"name":"sort","params":[{"name":"xs","ty":"[i64]"}],"ret":"[i64]"}]}]}"#,
        );
        let sort = ws.sig_hash(&"demo::sort".parse().unwrap()).unwrap();
        let words = ws
            .alias_index()
            .alias_for(&sort)
            .expect("a proposition is in the index");
        assert_eq!(words.split('-').count(), 3, "{words}");
    }

    #[test]
    fn the_same_call_asked_twice_is_one_proposition() {
        // Two universes asking for the same thing arrive at one address,
        // whatever each of them called it — the point being that neither had
        // to know about the other.
        let ws = check(
            r#"{"universes":[
                {"name":"a","functions":[
                    {"name":"sort","params":[{"name":"xs","ty":"[i64]"}],"ret":"[i64]"}]},
                {"name":"b","functions":[
                    {"name":"order","params":[{"name":"ys","ty":"[i64]"}],"ret":"[i64]"}]}]}"#,
        );
        let sort = ws.sig_hash(&"a::sort".parse().unwrap()).unwrap();
        assert_eq!(ws.sig_hash(&"b::order".parse().unwrap()), Some(sort));
        assert_eq!(
            ws.sigs_with_hash(&sort)
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>(),
            ["a::sort", "b::order"]
        );
    }

    #[test]
    fn a_proposition_over_named_types_belongs_to_its_universe() {
        // The limit of the above, stated on purpose rather than discovered.
        // A signature mentioning a named type carries that name in its address,
        // so two universes each with their own `Req` ask two different
        // questions however alike the two `Req`s are. Sharing a proposition
        // across universes means sharing the types it is over — which is what
        // a universe like `http` is for.
        let ws = check(
            r#"{"universes":[
                {"name":"a","types":[{"k":"struct","name":"Req",
                    "fields":[{"name":"url","ty":"str"}]}],
                 "functions":[{"name":"send","params":[{"name":"r","ty":"Req"}],"ret":"i32"}]},
                {"name":"b","types":[{"k":"struct","name":"Req",
                    "fields":[{"name":"url","ty":"str"}]}],
                 "functions":[{"name":"send","params":[{"name":"r","ty":"Req"}],"ret":"i32"}]}]}"#,
        );
        let a = ws.sig_hash(&"a::send".parse().unwrap()).unwrap();
        let b = ws.sig_hash(&"b::send".parse().unwrap()).unwrap();
        assert_ne!(a, b);

        // The data they carry is the same, though, and that much is visible.
        assert_eq!(
            ws.shape_hash(&"a::Req".parse().unwrap()),
            ws.shape_hash(&"b::Req".parse().unwrap()),
        );
    }
}
