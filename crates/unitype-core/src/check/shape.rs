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
//! the names of whatever it refers to, the names of its generic parameters, and
//! how its fields were distributed between itself and its parents. Only the
//! flattened result matters, because that is what a value of the type looks
//! like. Field *names* are kept — they are part of the data.
//!
//! # Views
//!
//! A struct's fields and a signature's parameters are both a list of names
//! with a type each, and there is more than one honest answer to when two such
//! lists are the same. Each [`View`] is one answer, and every declaration and
//! every signature gets an address under each:
//!
//! - **keyed** — names count, order does not. A JSON object: `{x, y}` is
//!   `{y, x}`. This is what the shape has always been.
//! - **positional** — order counts, names do not. A tuple, or an argument
//!   list: `{x: f64, y: f64}` lines up with `{lat: f64, long: f64}`.
//! - **bag** — neither. The same types, under any names, in any order: two
//!   lists share it exactly when some renaming and reordering makes them one.
//!
//! A view erases the names of a declaration's own fields and of whatever it
//! refers to, which is addressed under the same view, so the claim holds all
//! the way down. Two kinds of name survive every view: an enum's variant tags
//! and an anonymous record's keys. Both are what is on the wire, and neither
//! has a declared order to fall back on.
//!
//! The bag view needs an order that comes from nothing but structure, so its
//! fields are sorted the way union members are — by what each is, with any
//! mention inside a recursive group stood in for by its block. Unlike a
//! union's members, two fields of the same type are both kept: `(f64, f64)` is
//! not `(f64)`.
//!
//! # Recursion
//!
//! A type that mentions itself has no finite tree to hash. Declarations are
//! grouped into strongly connected components, and each is treated as a graph
//! with a node per member, labelled with what the member says apart from which
//! members of the group it mentions, and an edge for each mention. Anything
//! outside the group already has an address, so it is part of the label.
//!
//! The graph is minimized first, by partition refinement. Members start in
//! blocks keyed by their labels and are split by the blocks of what they
//! mention until no block splits further, which leaves two members in one block
//! exactly when they unfold to the same tree. That is equality of recursive
//! types as Amadio and Cardelli put it, and it is what makes an `A` that holds a
//! `B` that holds an `A` the same shape as a `T` that holds a `T`.
//!
//! A member is then addressed by a walk of the minimized graph from its own
//! block, which numbers blocks in the order it first reaches them and encodes
//! each with its mentions replaced by those numbers. Nothing in the walk
//! depends on a name or on the order declarations were written in, so two
//! groups that differ only in what things are called get the same addresses
//! member for member, and members of a group that unfold alike get one address
//! between them.
//!
//! Unions are where the walk needs more than the graph. Their members have no
//! order of their own, yet the order they are encoded in decides the
//! numbering, so they are ordered by what each would look like with every
//! mention replaced by the digest the refinement gave the block it reaches.
//! That order comes from structure alone too.

use std::collections::{HashMap, HashSet};
use std::fmt;
use std::str::FromStr;

use serde::Serialize;

use crate::canon::encode::canonical_bytes;
use crate::canon::hash::TypeId;
use crate::canon::normalize::normalize_type;
use crate::check::{CheckedWorkspace, DefIx, SigIx};
use crate::ident::{Ident, QualifiedName};
use crate::ir::{LiteralValue, Type, TypeDef};

/// Which of a list's names and order an address keeps.
///
/// See the [module documentation](self) for what each one means.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum View {
    /// Names count, order does not.
    Keyed,
    /// Order counts, names do not.
    Positional,
    /// Neither counts: the same types, in any order, under any names.
    Bag,
}

impl View {
    /// Every view, in a fixed order.
    pub const ALL: [View; 3] = [View::Keyed, View::Positional, View::Bag];

    pub(crate) fn index(self) -> usize {
        match self {
            View::Keyed => 0,
            View::Positional => 1,
            View::Bag => 2,
        }
    }

    /// The word for this view, as the command line spells it.
    pub fn as_str(self) -> &'static str {
        match self {
            View::Keyed => "keyed",
            View::Positional => "positional",
            View::Bag => "bag",
        }
    }

    /// Domain for a declaration's address under this view.
    ///
    /// The keyed domain is the one shapes have always been hashed in, so a
    /// shape address is a keyed address and nothing that was pinned moves.
    /// v1 ordered the members of a mutually recursive group by name, which put
    /// those names into every member's address.
    fn type_domain(self) -> &'static str {
        match self {
            View::Keyed => "unitype shape v2",
            View::Positional => "unitype layout v1",
            View::Bag => "unitype bag v1",
        }
    }

    /// Domain for a signature's address under this view.
    fn sig_domain(self) -> &'static str {
        match self {
            View::Keyed => "unitype sig keyed v1",
            View::Positional => "unitype sig positional v1",
            View::Bag => "unitype sig bag v1",
        }
    }

    /// Whether a list keeps its names under this view.
    fn keeps_names(self) -> bool {
        self == View::Keyed
    }
}

impl fmt::Display for View {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Why text did not name a view.
#[derive(thiserror::Error, Debug, Clone, PartialEq, Eq)]
#[error("`{0}` is not a view; the views are keyed, positional, and bag")]
pub struct ViewParseError(String);

impl FromStr for View {
    type Err = ViewParseError;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        View::ALL
            .into_iter()
            .find(|v| v.as_str() == s)
            .ok_or_else(|| ViewParseError(s.to_string()))
    }
}

/// Every address under one view, and what shares each.
#[derive(Default)]
pub(crate) struct Addresses {
    pub(crate) by_name: HashMap<QualifiedName, TypeId>,
    pub(crate) index: HashMap<TypeId, Vec<QualifiedName>>,
}

impl Addresses {
    fn new(by_name: HashMap<QualifiedName, TypeId>) -> Self {
        let mut index: HashMap<TypeId, Vec<QualifiedName>> = HashMap::new();
        for (name, address) in &by_name {
            index.entry(*address).or_default().push(name.clone());
        }
        for names in index.values_mut() {
            names.sort();
        }
        Self { by_name, index }
    }
}

/// Computes every declaration's and every signature's address under every
/// view, and indexes them.
pub(crate) fn compute(ws: &mut CheckedWorkspace) {
    for view in View::ALL {
        let hashes = declarations(ws, view);
        ws.addresses[view.index()] = Addresses::new(hashes);
    }
    for entry in &mut ws.defs {
        entry.shape = ws.addresses[View::Keyed.index()]
            .by_name
            .get(&entry.name)
            .copied();
    }
    for view in View::ALL {
        let hashes = (0..ws.sigs.len())
            .map(SigIx)
            .map(|ix| (ws.sig_name(ix).clone(), signature(ws, ix, view)))
            .collect();
        ws.sig_addresses[view.index()] = Addresses::new(hashes);
    }
}

/// Every declaration's address under one view.
fn declarations(ws: &CheckedWorkspace, view: View) -> HashMap<QualifiedName, TypeId> {
    let mut hashes: HashMap<QualifiedName, TypeId> = HashMap::new();

    // Components come out with everything they depend on already addressed.
    for component in components(ws) {
        let places: HashMap<QualifiedName, usize> = component
            .iter()
            .enumerate()
            .map(|(place, ix)| (ws.def_name(*ix).clone(), place))
            .collect();
        let group = Group::new(
            component
                .iter()
                .map(|ix| content(ws, *ix, &places, &hashes, view))
                .collect(),
            view,
        );
        for (place, ix) in component.iter().enumerate() {
            hashes.insert(
                ws.def_name(*ix).clone(),
                digest(view.type_domain(), &group.encode(place)),
            );
        }
    }

    // An alias is only a name. When it stands for a declaration and passes its
    // own parameters straight through, it shares that declaration's address
    // outright; otherwise it takes the address of the type it stands for, which
    // keeps `Box<i64>` and `Box<str>` apart from each other and from `Box`.
    let aliases: Vec<(QualifiedName, TypeId)> = (0..ws.defs.len())
        .map(DefIx)
        .filter_map(|ix| {
            let TypeDef::Alias(alias) = ws.def_at(ix) else {
                return None;
            };
            let params: Vec<Ident> = alias.params.iter().map(|p| p.name.clone()).collect();
            let lowering = Lowering {
                ws,
                params: &params,
                places: &HashMap::new(),
                hashes: &hashes,
                view,
            };
            let node = ordered(
                &lowering.lower(&alias.target),
                &|_| unreachable!("an alias is not part of any group"),
                view,
            );
            let address = match &node {
                ShapeNode::Id { h, args } if passes_through(args) => *h,
                other => digest(view.type_domain(), &canonical_bytes(other)),
            };
            Some((ws.def_name(ix).clone(), address))
        })
        .collect();
    hashes.extend(aliases);
    hashes
}

/// A signature's address under one view.
///
/// The view decides how the parameter list is compared. What each parameter
/// holds is compared by its keyed shape whatever the view, because a call hands
/// its arguments over as they are: a dispatcher can put them in a different
/// order, but it does not convert them.
fn signature(ws: &CheckedWorkspace, ix: SigIx, view: View) -> TypeId {
    let sig = ws.sig_at(ix);
    let generics: Vec<Ident> = sig.params_decl.iter().map(|p| p.name.clone()).collect();
    let lowering = Lowering {
        ws,
        params: &generics,
        places: &HashMap::new(),
        hashes: &ws.addresses[View::Keyed.index()].by_name,
        view: View::Keyed,
    };
    let lower = |t: &Type| {
        ordered(
            &lowering.lower(t),
            &|_| unreachable!("a signature is not part of any group"),
            View::Keyed,
        )
    };

    let params: Vec<Part> = sig
        .params
        .iter()
        .map(|p| Part {
            name: view.keeps_names().then(|| p.name.clone()),
            ty: lower(&p.ty),
        })
        .collect();
    let bounds: Vec<Vec<ShapeNode>> = sig
        .params_decl
        .iter()
        .map(|d| {
            d.bounds
                .iter()
                .map(|b| lower(&Type::Ref(b.clone())))
                .collect()
        })
        .collect();
    let ret = lower(&sig.ret);

    let encoded = |numbering: &[usize]| {
        canonical_bytes(&SigShape::new(&bounds, &params, &ret, numbering, view))
    };
    let bytes = match view {
        // The generic parameters are a list too, and a bag keeps no order of
        // anything, so they are numbered whichever way makes the encoding
        // least: `<A, B>(x: A, y: B)` and `<B, A>(x: A, y: B)` are one bag.
        // That is every numbering tried, which is cheap for the handful of
        // generic parameters a signature has.
        View::Bag => numberings(bounds.len())
            .iter()
            .map(|n| encoded(n))
            .min()
            .expect("there is always at least one numbering"),
        View::Keyed | View::Positional => encoded(&(0..bounds.len()).collect::<Vec<_>>()),
    };
    digest(view.sig_domain(), &bytes)
}

/// A signature in shape form.
#[derive(Serialize)]
struct SigShape {
    /// Each generic parameter, by number, with whatever bounds it.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    generics: Vec<Vec<ShapeNode>>,
    /// The parameters, arranged as the view says.
    params: Vec<Part>,
    ret: ShapeNode,
}

impl SigShape {
    /// A signature with its generic parameters numbered and its parameters
    /// arranged as the view says.
    ///
    /// The generic parameter declared `i`th becomes number `numbering[i]`.
    /// `bounds`, `params` and `ret` are in shape form with each generic
    /// parameter numbered by where it was declared.
    fn new(
        bounds: &[Vec<ShapeNode>],
        params: &[Part],
        ret: &ShapeNode,
        numbering: &[usize],
        view: View,
    ) -> SigShape {
        let mut generics = vec![Vec::new(); bounds.len()];
        for (i, bounds) in bounds.iter().enumerate() {
            let mut bounds: Vec<ShapeNode> =
                bounds.iter().map(|b| renumbered(b, numbering)).collect();
            // A parameter has to satisfy every one of its bounds, so in a bag
            // they are a set like everything else.
            if view == View::Bag {
                bounds.sort_by_cached_key(canonical_bytes);
                bounds.dedup();
            }
            generics[numbering[i]] = bounds;
        }
        let mut params: Vec<Part> = params
            .iter()
            .map(|p| Part {
                name: p.name.clone(),
                ty: renumbered(&p.ty, numbering),
            })
            .collect();
        match view {
            View::Keyed => params.sort_by(|a, b| a.name.cmp(&b.name)),
            View::Positional => {}
            View::Bag => params.sort_by_cached_key(|p| canonical_bytes(&p.ty)),
        }
        SigShape {
            generics,
            params,
            ret: renumbered(ret, numbering),
        }
    }
}

/// A type in shape form with generic parameter `i` renumbered `to[i]`.
///
/// A union's members are ordered by what each one is, and a parameter's
/// number is part of that, so every union in it is put back in order.
fn renumbered(node: &ShapeNode, to: &[usize]) -> ShapeNode {
    fn walk(node: &mut ShapeNode, to: &[usize]) {
        if let ShapeNode::Param { i } = node {
            *i = to[*i];
        }
        for child in node.children_mut() {
            walk(child, to);
        }
    }
    if to.iter().enumerate().all(|(i, &j)| i == j) {
        return node.clone();
    }
    let mut node = node.clone();
    walk(&mut node, to);
    ordered(
        &node,
        &|_| unreachable!("a signature is not part of any group"),
        View::Keyed,
    )
}

/// Every way of numbering `n` things, as `numbering[i]` for the `i`th: in
/// lexicographic order, so the first is each where it already was.
pub(crate) fn numberings(n: usize) -> Vec<Vec<usize>> {
    fn extend(prefix: &mut Vec<usize>, used: &mut [bool], out: &mut Vec<Vec<usize>>) {
        if prefix.len() == used.len() {
            out.push(prefix.clone());
            return;
        }
        for j in 0..used.len() {
            if !used[j] {
                used[j] = true;
                prefix.push(j);
                extend(prefix, used, out);
                prefix.pop();
                used[j] = false;
            }
        }
    }
    let mut out = Vec::new();
    extend(&mut Vec::with_capacity(n), &mut vec![false; n], &mut out);
    out
}

/// What a type holds, as an address: two types share one exactly when they
/// accept the same values.
///
/// `generics` are the generic parameters in scope, which are numbered rather
/// than named.
pub(crate) fn data_address(ws: &CheckedWorkspace, t: &Type, generics: &[Ident]) -> TypeId {
    let lowering = Lowering {
        ws,
        params: generics,
        places: &HashMap::new(),
        hashes: &ws.addresses[View::Keyed.index()].by_name,
        view: View::Keyed,
    };
    let node = ordered(
        &lowering.lower(t),
        &|_| unreachable!("a lone type is not part of any group"),
        View::Keyed,
    );
    digest(View::Keyed.type_domain(), &canonical_bytes(&node))
}

fn digest(domain: &str, bytes: &[u8]) -> TypeId {
    TypeId::from_bytes(
        *blake3::Hasher::new_derive_key(domain)
            .update(bytes)
            .finalize()
            .as_bytes(),
    )
}

/// Whether `args` are a declaration's own parameters in order, which is what
/// an alias passes when it is only another name for what it points at.
fn passes_through(args: &[ShapeNode]) -> bool {
    args.iter()
        .enumerate()
        .all(|(i, arg)| *arg == ShapeNode::Param { i })
}

/// A type or declaration in shape form: names erased, parameters numbered, and
/// every reference replaced by what it refers to.
///
/// Encoded with [`canonical_bytes`] like everything else, so keys come out in
/// the order written here.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(tag = "k", rename_all = "snake_case")]
enum ShapeNode {
    Never,
    Any,
    Unit,
    Bool,
    I32,
    I64,
    F64,
    String,
    Bytes,
    Literal {
        value: LiteralValue,
    },
    Optional {
        of: Box<ShapeNode>,
    },
    List {
        of: Box<ShapeNode>,
    },
    Map {
        key: Box<ShapeNode>,
        value: Box<ShapeNode>,
    },
    Tuple {
        items: Vec<ShapeNode>,
    },
    Record {
        fields: Vec<Part>,
    },
    Union {
        members: Vec<ShapeNode>,
    },
    Function {
        params: Vec<ShapeNode>,
        ret: Box<ShapeNode>,
    },
    Future {
        of: Box<ShapeNode>,
    },
    /// A generic parameter of the declaration, by position.
    Param {
        i: usize,
    },
    /// A declaration outside the group being addressed, by its address.
    Id {
        h: TypeId,
        #[serde(skip_serializing_if = "Vec::is_empty")]
        args: Vec<ShapeNode>,
    },
    /// A declaration inside the group being addressed. While the group is
    /// being worked out, `i` is the member's place in it; in an address, it is
    /// the number the walk gave that member's block.
    Rec {
        i: usize,
        #[serde(skip_serializing_if = "Vec::is_empty")]
        args: Vec<ShapeNode>,
    },
    /// Something the checker should have resolved and did not. It keeps its
    /// name rather than be erased into some other type's address.
    Unresolved {
        name: String,
    },
    /// A struct: the data it holds.
    Fields {
        fields: Vec<Part>,
    },
    /// An interface: the data it promises.
    Contract {
        fields: Vec<Part>,
    },
    /// An enum: its alternatives.
    Variants {
        variants: Vec<Part>,
    },
}

/// A field, a parameter, or a variant: a name, and what it holds.
///
/// Nameless under a view that erases names. A keyed part encodes exactly as
/// it did before views existed, which is what keeps shape addresses where they
/// were.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
struct Part {
    #[serde(skip_serializing_if = "Option::is_none")]
    name: Option<Ident>,
    ty: ShapeNode,
}

impl ShapeNode {
    fn children(&self) -> Vec<&ShapeNode> {
        match self {
            ShapeNode::Optional { of } | ShapeNode::List { of } | ShapeNode::Future { of } => {
                vec![of.as_ref()]
            }
            ShapeNode::Map { key, value } => vec![key.as_ref(), value.as_ref()],
            ShapeNode::Tuple { items }
            | ShapeNode::Union { members: items }
            | ShapeNode::Id { args: items, .. }
            | ShapeNode::Rec { args: items, .. } => items.iter().collect(),
            ShapeNode::Function { params, ret } => {
                params.iter().chain(std::iter::once(ret.as_ref())).collect()
            }
            ShapeNode::Record { fields }
            | ShapeNode::Fields { fields }
            | ShapeNode::Contract { fields }
            | ShapeNode::Variants { variants: fields } => fields.iter().map(|p| &p.ty).collect(),
            ShapeNode::Never
            | ShapeNode::Any
            | ShapeNode::Unit
            | ShapeNode::Bool
            | ShapeNode::I32
            | ShapeNode::I64
            | ShapeNode::F64
            | ShapeNode::String
            | ShapeNode::Bytes
            | ShapeNode::Literal { .. }
            | ShapeNode::Param { .. }
            | ShapeNode::Unresolved { .. } => Vec::new(),
        }
    }

    fn children_mut(&mut self) -> Vec<&mut ShapeNode> {
        match self {
            ShapeNode::Optional { of } | ShapeNode::List { of } | ShapeNode::Future { of } => {
                vec![of.as_mut()]
            }
            ShapeNode::Map { key, value } => vec![key.as_mut(), value.as_mut()],
            ShapeNode::Tuple { items }
            | ShapeNode::Union { members: items }
            | ShapeNode::Id { args: items, .. }
            | ShapeNode::Rec { args: items, .. } => items.iter_mut().collect(),
            ShapeNode::Function { params, ret } => params
                .iter_mut()
                .chain(std::iter::once(ret.as_mut()))
                .collect(),
            ShapeNode::Record { fields }
            | ShapeNode::Fields { fields }
            | ShapeNode::Contract { fields }
            | ShapeNode::Variants { variants: fields } => {
                fields.iter_mut().map(|p| &mut p.ty).collect()
            }
            ShapeNode::Never
            | ShapeNode::Any
            | ShapeNode::Unit
            | ShapeNode::Bool
            | ShapeNode::I32
            | ShapeNode::I64
            | ShapeNode::F64
            | ShapeNode::String
            | ShapeNode::Bytes
            | ShapeNode::Literal { .. }
            | ShapeNode::Param { .. }
            | ShapeNode::Unresolved { .. } => Vec::new(),
        }
    }

    /// Every mention of a member of the group, in the order written: a
    /// mention before its arguments, a field's type before the next field's.
    fn mentions(&self) -> Vec<usize> {
        let mut out = Vec::new();
        let mut stack = vec![self];
        while let Some(node) = stack.pop() {
            if let ShapeNode::Rec { i, .. } = node {
                out.push(*i);
            }
            stack.extend(node.children().into_iter().rev());
        }
        out
    }

    /// Replaces every mention of a member of the group, outermost first.
    fn replace_mentions(&mut self, with: &impl Fn(usize, Vec<ShapeNode>) -> ShapeNode) {
        if let ShapeNode::Rec { i, args } = self {
            let replacement = with(*i, std::mem::take(args));
            *self = replacement;
        }
        for child in self.children_mut() {
            child.replace_mentions(with);
        }
    }
}

/// What a declaration says, names erased, with each mention of a member of its
/// group left as a [`ShapeNode::Rec`] holding that member's place.
fn content(
    ws: &CheckedWorkspace,
    ix: DefIx,
    places: &HashMap<QualifiedName, usize>,
    hashes: &HashMap<QualifiedName, TypeId>,
    view: View,
) -> ShapeNode {
    let def = ws.def_at(ix);
    let params: Vec<Ident> = def.params().iter().map(|p| p.name.clone()).collect();
    let lowering = Lowering {
        ws,
        params: &params,
        places,
        hashes,
        view,
    };
    let fields = || lowering.fields(ws.defs[ix.0].flat_fields.iter().map(|f| (&f.name, &f.ty)));
    match def {
        TypeDef::Struct(_) => ShapeNode::Fields { fields: fields() },
        TypeDef::Interface(_) => ShapeNode::Contract { fields: fields() },
        TypeDef::Enum(d) => ShapeNode::Variants {
            variants: lowering.variants(d.variants.iter().map(|v| (&v.name, &v.payload))),
        },
        TypeDef::Alias(_) => unreachable!("an alias is a name, not a node in the graph"),
    }
}

/// Puts the types in one declaration into shape form.
struct Lowering<'a> {
    ws: &'a CheckedWorkspace,
    /// The declaration's generic parameters, which become their positions so
    /// that what they were called is not part of what the declaration is.
    params: &'a [Ident],
    /// The members of the group being addressed, by place.
    places: &'a HashMap<QualifiedName, usize>,
    /// Every declaration addressed so far.
    hashes: &'a HashMap<QualifiedName, TypeId>,
    /// Which names and which order a struct's fields keep.
    view: View,
}

impl Lowering<'_> {
    /// A type in shape form, with every alias in it expanded.
    fn lower(&self, t: &Type) -> ShapeNode {
        // Expanding an alias can leave something normalization would have
        // rewritten — an optional directly inside a union, say — so the result
        // is normalized again.
        self.node(&normalize_type(&expand_deep(self.ws, t)))
    }

    /// A struct's or an interface's fields, as the view arranges them.
    ///
    /// Keyed, they are in name order: the declared order is for the code
    /// generated from them, which says nothing about the data. Positional, they
    /// keep the declared order and lose their names. In a bag they lose both,
    /// and are put in order by [`ordered`] once mentions inside the group can be
    /// stood in for.
    fn fields<'t>(&self, fields: impl Iterator<Item = (&'t Ident, &'t Type)>) -> Vec<Part> {
        let mut parts: Vec<Part> = fields
            .map(|(name, ty)| Part {
                name: self.view.keeps_names().then(|| name.clone()),
                ty: self.lower(ty),
            })
            .collect();
        if self.view.keeps_names() {
            parts.sort_by(|a, b| a.name.cmp(&b.name));
        }
        parts
    }

    /// An enum's variants, in name order under every view: a tag is what is on
    /// the wire, and there is no declared order to fall back on.
    fn variants<'t>(&self, variants: impl Iterator<Item = (&'t Ident, &'t Type)>) -> Vec<Part> {
        let mut parts: Vec<Part> = variants
            .map(|(name, ty)| Part {
                name: Some(name.clone()),
                ty: self.lower(ty),
            })
            .collect();
        parts.sort_by(|a, b| a.name.cmp(&b.name));
        parts
    }

    fn node(&self, t: &Type) -> ShapeNode {
        let one = |t: &Type| Box::new(self.node(t));
        let all = |ts: &[Type]| ts.iter().map(|t| self.node(t)).collect();
        match t {
            Type::Never => ShapeNode::Never,
            Type::Any => ShapeNode::Any,
            Type::Unit => ShapeNode::Unit,
            Type::Bool => ShapeNode::Bool,
            Type::I32 => ShapeNode::I32,
            Type::I64 => ShapeNode::I64,
            Type::F64 => ShapeNode::F64,
            Type::String => ShapeNode::String,
            Type::Bytes => ShapeNode::Bytes,
            Type::Literal { value } => ShapeNode::Literal {
                value: value.clone(),
            },
            Type::Optional { of } => ShapeNode::Optional { of: one(of) },
            Type::List { of } => ShapeNode::List { of: one(of) },
            Type::Future { of } => ShapeNode::Future { of: one(of) },
            Type::Map { key, value } => ShapeNode::Map {
                key: one(key),
                value: one(value),
            },
            Type::Tuple { items } => ShapeNode::Tuple { items: all(items) },
            Type::Union { members } => ShapeNode::Union {
                members: all(members),
            },
            Type::Function { params, ret } => ShapeNode::Function {
                params: all(params),
                ret: one(ret),
            },
            // Already in name order: normalization sorts a record's fields.
            Type::Record { fields } => ShapeNode::Record {
                fields: fields
                    .iter()
                    .map(|f| Part {
                        name: Some(f.name.clone()),
                        ty: self.node(&f.ty),
                    })
                    .collect(),
            },
            Type::Param { name } => match self.params.iter().position(|p| p == name) {
                Some(i) => ShapeNode::Param { i },
                None => ShapeNode::Unresolved {
                    name: t.to_string(),
                },
            },
            Type::Ref(r) => {
                let args = all(&r.args);
                let target = self.ws.ref_target(r);
                let target = target.as_ref();
                if let Some(&i) = target.and_then(|q| self.places.get(q)) {
                    ShapeNode::Rec { i, args }
                } else if let Some(&h) = target.and_then(|q| self.hashes.get(q)) {
                    ShapeNode::Id { h, args }
                } else {
                    ShapeNode::Unresolved {
                        name: t.to_string(),
                    }
                }
            }
        }
    }
}

/// A strongly connected component, as a graph with a node per member.
struct Group {
    /// What each member says, with mentions of members left as their places.
    contents: Vec<ShapeNode>,
    /// `rounds[k][m]` is a digest of member `m` as far as `k` mentions deep.
    /// Members with equal digests in the last round are in one block.
    rounds: Vec<Vec<TypeId>>,
    /// The view the members were lowered under, which decides what else has
    /// to be put in order from structure.
    view: View,
}

impl Group {
    /// Refines the members into blocks until no block splits.
    fn new(contents: Vec<ShapeNode>, view: View) -> Self {
        let everyone = 0..contents.len();
        // Before anything is known about where a mention leads, every member
        // looks the same from outside.
        let unknown = TypeId::from_bytes([0; 32]);
        let mut rounds = vec![refine(&contents, &|_| unknown, view)];
        loop {
            let last = rounds.last().expect("there is a first round");
            let next = refine(&contents, &|m| last[m], view);
            let split = distinct(&next, everyone.clone()) > distinct(last, everyone.clone());
            rounds.push(next);
            if !split {
                break;
            }
        }
        Self {
            contents,
            rounds,
            view,
        }
    }

    /// The bytes that address one member: each block its walk reaches, in the
    /// order it reaches them, with mentions replaced by those positions.
    fn encode(&self, member: usize) -> Vec<u8> {
        let blocks = self.rounds.last().expect("there is a last round");
        let key = &self.rounds[self.settled_for(member)];

        let mut numbers: HashMap<TypeId, usize> = HashMap::new();
        let mut reached: Vec<ShapeNode> = Vec::new();
        self.walk(member, key, blocks, &mut numbers, &mut reached);

        for content in &mut reached {
            content.replace_mentions(&|m, args| ShapeNode::Rec {
                i: numbers[&blocks[m]],
                args,
            });
        }
        canonical_bytes(&reached)
    }

    /// Depth first from `member`'s block, numbering each block as it is first
    /// reached.
    fn walk(
        &self,
        member: usize,
        key: &[TypeId],
        blocks: &[TypeId],
        numbers: &mut HashMap<TypeId, usize>,
        reached: &mut Vec<ShapeNode>,
    ) {
        numbers.insert(blocks[member], reached.len());
        let content = ordered(&self.contents[member], &|m| key[m], self.view);
        let next = content.mentions();
        reached.push(content);
        for m in next {
            if !numbers.contains_key(&blocks[m]) {
                self.walk(m, key, blocks, numbers, reached);
            }
        }
    }

    /// The first round after which nothing `member` can reach splits.
    ///
    /// That round's digests tell apart every block the member's walk can
    /// reach, and depend on nothing but what the member unfolds to. The round
    /// the whole group settles in would serve as well for telling blocks apart,
    /// but it depends on what else is in the group.
    fn settled_for(&self, member: usize) -> usize {
        let reach = self.reachable(member);
        (0..self.rounds.len() - 1)
            .find(|&k| {
                distinct(&self.rounds[k], reach.iter().copied())
                    == distinct(&self.rounds[k + 1], reach.iter().copied())
            })
            .expect("the group as a whole settles in its last round")
    }

    /// Every member that `member` mentions, directly or not, and itself.
    fn reachable(&self, member: usize) -> Vec<usize> {
        let mut seen = vec![false; self.contents.len()];
        let mut stack = vec![member];
        let mut out = Vec::new();
        while let Some(m) = stack.pop() {
            if std::mem::replace(&mut seen[m], true) {
                continue;
            }
            out.push(m);
            stack.extend(self.contents[m].mentions());
        }
        out
    }
}

/// Each member's digest, when every member it mentions is stood in for by the
/// digest `stand_in` gives it.
fn refine(contents: &[ShapeNode], stand_in: &impl Fn(usize) -> TypeId, view: View) -> Vec<TypeId> {
    contents
        .iter()
        .map(|content| {
            digest(
                view.type_domain(),
                &canonical_bytes(&standing_in(&ordered(content, stand_in, view), stand_in)),
            )
        })
        .collect()
}

fn distinct(digests: &[TypeId], among: impl Iterator<Item = usize>) -> usize {
    among.map(|m| digests[m]).collect::<HashSet<_>>().len()
}

/// `node` with each mention of a member of the group stood in for by a digest,
/// as though that member were a declaration outside it with that address.
fn standing_in(node: &ShapeNode, stand_in: &impl Fn(usize) -> TypeId) -> ShapeNode {
    let mut node = node.clone();
    node.replace_mentions(&|m, args| ShapeNode::Id {
        h: stand_in(m),
        args,
    });
    node
}

/// `node` with everything that has no order of its own put in an order that
/// comes from structure alone.
///
/// A union's members, always: normalization ordered them by their encoding as
/// written, names and all. Here each is ordered by its encoding with every
/// mention of a member of the group stood in for by `stand_in`, and those that
/// encode alike are one member. A union left with one member is that member.
///
/// A struct's or an interface's fields too, in the bag view, by the same key.
/// Those that encode alike are all kept, since a bag of two numbers is not a
/// bag of one.
fn ordered(node: &ShapeNode, stand_in: &impl Fn(usize) -> TypeId, view: View) -> ShapeNode {
    let mut node = node.clone();
    order_structure(&mut node, stand_in, view);
    node
}

fn order_structure(node: &mut ShapeNode, stand_in: &impl Fn(usize) -> TypeId, view: View) {
    for child in node.children_mut() {
        order_structure(child, stand_in, view);
    }
    let key = |n: &ShapeNode| canonical_bytes(&standing_in(n, stand_in));
    match node {
        ShapeNode::Union { members } => {
            let mut keyed: Vec<(Vec<u8>, ShapeNode)> = std::mem::take(members)
                .into_iter()
                .map(|m| (key(&m), m))
                .collect();
            keyed.sort_by(|a, b| a.0.cmp(&b.0));
            keyed.dedup_by(|a, b| a.0 == b.0);
            let mut members: Vec<ShapeNode> = keyed.into_iter().map(|(_, m)| m).collect();
            *node = match members.len() {
                1 => members.pop().expect("length checked"),
                _ => ShapeNode::Union { members },
            };
        }
        ShapeNode::Fields { fields } | ShapeNode::Contract { fields } if view == View::Bag => {
            fields.sort_by_cached_key(|part| key(&part.ty));
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

/// Every declaration whose shape this one's shape depends on: exactly those its
/// content mentions, since whether a mention is inside the group or out of it
/// decides how it is encoded.
///
/// Never an alias: aliases are expanded wherever they appear, so one left
/// behind is one that could not be, and is not a node of the graph.
fn dependencies(ws: &CheckedWorkspace, ix: DefIx) -> Vec<DefIx> {
    let mut out = Vec::new();
    let mut collect = |t: &Type| {
        // As in `Lowering::lower`, so that a reference normalization removes —
        // a member of a union with `any`, say — is no edge either.
        let expanded = normalize_type(&expand_deep(ws, t));
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
    out.retain(|target| !matches!(ws.def_at(*target), TypeDef::Alias(_)));
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
    use std::collections::HashSet;

    use crate::Workspace;
    use crate::canon::hash::TypeId;
    use crate::check::{CheckedWorkspace, check_workspace};

    fn check(json: &str) -> CheckedWorkspace {
        check_workspace(Workspace::from_json(json).expect("reads")).expect("checks")
    }

    fn shape(ws: &CheckedWorkspace, name: &str) -> TypeId {
        ws.shape_hash(&name.parse().unwrap()).expect("addressed")
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

    /// A pair whose names sort one way in `a` and the other way in `b`, so that
    /// any dependence on name order would show.
    const TWO_CYCLES: &str = r#"{"universes":[
        {"name":"a","types":[
            {"k":"struct","name":"A","fields":[
                {"name":"value","ty":"i64"},{"name":"next","ty":"B?"}]},
            {"k":"struct","name":"B","fields":[
                {"name":"label","ty":"str"},{"name":"back","ty":"[A]"}]}]},
        {"name":"b","types":[
            {"k":"struct","name":"Y","fields":[
                {"name":"value","ty":"i64"},{"name":"next","ty":"X?"}]},
            {"k":"struct","name":"X","fields":[
                {"name":"label","ty":"LABEL"},{"name":"back","ty":"[Y]"}]}]}]}"#;

    #[test]
    fn two_groups_that_differ_only_in_naming_share_addresses_member_for_member() {
        let ws = check(&TWO_CYCLES.replace("LABEL", "str"));
        assert_eq!(shape(&ws, "a::A"), shape(&ws, "b::Y"));
        assert_eq!(shape(&ws, "a::B"), shape(&ws, "b::X"));
        assert_ne!(
            shape(&ws, "a::A"),
            shape(&ws, "a::B"),
            "the two members of a group are still two different things"
        );
    }

    #[test]
    fn groups_that_differ_in_one_field_do_not_share_addresses() {
        let ws = check(&TWO_CYCLES.replace("LABEL", "i64"));
        assert_ne!(shape(&ws, "a::B"), shape(&ws, "b::X"), "the field differs");
        assert_ne!(
            shape(&ws, "a::A"),
            shape(&ws, "b::Y"),
            "and what holds it differs with it, two levels down"
        );
    }

    #[test]
    fn a_three_member_cycle_is_addressed_the_same_way_whatever_it_is_called() {
        // Sorted by name, `a` comes out paper-rock-scissors and `b` comes out
        // paper-scissors-rock.
        let ws = check(
            r#"{"universes":[
                {"name":"a","types":[
                    {"k":"struct","name":"Rock","fields":[
                        {"name":"r","ty":"i64"},{"name":"beats","ty":"Scissors?"}]},
                    {"k":"struct","name":"Scissors","fields":[
                        {"name":"s","ty":"str"},{"name":"beats","ty":"Paper?"}]},
                    {"k":"struct","name":"Paper","fields":[
                        {"name":"p","ty":"bool"},{"name":"beats","ty":"Rock?"}]}]},
                {"name":"b","types":[
                    {"k":"struct","name":"Ma","fields":[
                        {"name":"r","ty":"i64"},{"name":"beats","ty":"Lo?"}]},
                    {"k":"struct","name":"Lo","fields":[
                        {"name":"s","ty":"str"},{"name":"beats","ty":"Ka?"}]},
                    {"k":"struct","name":"Ka","fields":[
                        {"name":"p","ty":"bool"},{"name":"beats","ty":"Ma?"}]}]}]}"#,
        );
        for (one, other) in [
            ("a::Rock", "b::Ma"),
            ("a::Scissors", "b::Lo"),
            ("a::Paper", "b::Ka"),
        ] {
            assert_eq!(shape(&ws, one), shape(&ws, other), "{one} and {other}");
        }
        let distinct: HashSet<_> = ["a::Rock", "a::Scissors", "a::Paper"]
            .iter()
            .map(|name| shape(&ws, name))
            .collect();
        assert_eq!(distinct.len(), 3);
    }

    #[test]
    fn members_of_one_group_that_unfold_alike_share_an_address() {
        // `Even` holds an `Odd` holds an `Even`: unfolded, that is a `Loop`
        // holding a `Loop`, however the group was cut up to say it.
        let ws = check(
            r#"{"universes":[
                {"name":"a","types":[
                    {"k":"struct","name":"Even","fields":[{"name":"next","ty":"Odd?"}]},
                    {"k":"struct","name":"Odd","fields":[{"name":"next","ty":"Even?"}]}]},
                {"name":"b","types":[
                    {"k":"struct","name":"Loop","fields":[{"name":"next","ty":"Loop?"}]}]}]}"#,
        );
        let even = shape(&ws, "a::Even");
        assert_eq!(shape(&ws, "a::Odd"), even);
        assert_eq!(
            shape(&ws, "b::Loop"),
            even,
            "and it does not matter that the other group has only one member"
        );
        assert_eq!(ws.defs_with_shape(&even).len(), 3);
    }

    #[test]
    fn a_union_is_ordered_by_what_its_members_are_not_what_they_are_called() {
        let ws = check(
            r#"{"universes":[
                {"name":"a","types":[
                    {"k":"struct","name":"Doc","fields":[
                        {"name":"part","ty":{"k":"union","members":["Image","Para"]}}]},
                    {"k":"struct","name":"Image","fields":[{"name":"src","ty":"str"}]},
                    {"k":"struct","name":"Para","fields":[{"name":"text","ty":"str"}]}]},
                {"name":"b","types":[
                    {"k":"struct","name":"Page","fields":[
                        {"name":"part","ty":{"k":"union","members":["Copy","Photo"]}}]},
                    {"k":"struct","name":"Copy","fields":[{"name":"text","ty":"str"}]},
                    {"k":"struct","name":"Photo","fields":[{"name":"src","ty":"str"}]}]}]}"#,
        );
        assert_eq!(shape(&ws, "a::Doc"), shape(&ws, "b::Page"));
    }

    #[test]
    fn a_union_inside_a_group_is_ordered_by_what_its_members_are() {
        // Both members of each union are a struct holding the tree and one
        // more field, so nothing short of following the mentions tells them
        // apart; and by name they sort in opposite orders.
        let ws = check(
            r#"{"universes":[
                {"name":"a","types":[
                    {"k":"struct","name":"Tree","fields":[
                        {"name":"kids","ty":{"k":"list","of":
                            {"k":"union","members":["Fork","Spoon"]}}}]},
                    {"k":"struct","name":"Fork","fields":[
                        {"name":"tree","ty":"Tree"},{"name":"n","ty":"i64"}]},
                    {"k":"struct","name":"Spoon","fields":[
                        {"name":"tree","ty":"Tree"},{"name":"s","ty":"str"}]}]},
                {"name":"b","types":[
                    {"k":"struct","name":"Root","fields":[
                        {"name":"kids","ty":{"k":"list","of":
                            {"k":"union","members":["Prong","Bowl"]}}}]},
                    {"k":"struct","name":"Prong","fields":[
                        {"name":"tree","ty":"Root"},{"name":"n","ty":"i64"}]},
                    {"k":"struct","name":"Bowl","fields":[
                        {"name":"tree","ty":"Root"},{"name":"s","ty":"str"}]}]}]}"#,
        );
        assert_eq!(shape(&ws, "a::Tree"), shape(&ws, "b::Root"));
        assert_eq!(shape(&ws, "a::Fork"), shape(&ws, "b::Prong"));
        assert_eq!(shape(&ws, "a::Spoon"), shape(&ws, "b::Bowl"));
    }

    #[test]
    fn a_union_of_two_names_for_the_same_data_is_that_data() {
        // An untagged union of two things that look alike cannot say which of
        // them a value is, so it holds no more than either.
        let ws = check(
            r#"{"universes":[{"name":"demo","types":[
                {"k":"struct","name":"Point","fields":[{"name":"x","ty":"float"}]},
                {"k":"struct","name":"Vec1","fields":[{"name":"x","ty":"float"}]},
                {"k":"struct","name":"Either","fields":[
                    {"name":"at","ty":{"k":"union","members":["Point","Vec1"]}}]},
                {"k":"struct","name":"Just","fields":[{"name":"at","ty":"Point"}]}]}]}"#,
        );
        assert_eq!(shape(&ws, "demo::Either"), shape(&ws, "demo::Just"));
    }

    #[test]
    fn an_alias_expanded_inside_a_union_is_normalized_like_anything_else() {
        let ws = check(
            r#"{"universes":[{"name":"demo","types":[
                {"k":"alias","name":"MaybeInt","target":"i64?"},
                {"k":"struct","name":"Spelled","fields":[
                    {"name":"x","ty":{"k":"union","members":["MaybeInt","str"]}}]},
                {"k":"struct","name":"Written","fields":[
                    {"name":"x","ty":{"k":"optional","of":
                        {"k":"union","members":["i64","str"]}}}]}]}]}"#,
        );
        assert_eq!(shape(&ws, "demo::Spelled"), shape(&ws, "demo::Written"));
    }

    #[test]
    fn an_alias_is_addressed_by_position_not_by_parameter_name() {
        let ws = check(
            r#"{"universes":[
                {"name":"a","types":[{"k":"alias","name":"Pair","params":[{"name":"T"}],
                 "target":{"k":"tuple","items":[
                    {"k":"param","name":"T"},{"k":"param","name":"T"}]}}]},
                {"name":"b","types":[{"k":"alias","name":"Twice","params":[{"name":"U"}],
                 "target":{"k":"tuple","items":[
                    {"k":"param","name":"U"},{"k":"param","name":"U"}]}}]}]}"#,
        );
        assert_eq!(shape(&ws, "a::Pair"), shape(&ws, "b::Twice"));
    }

    #[test]
    fn an_alias_that_fixes_a_parameter_is_not_the_generic_it_names() {
        let ws = check(
            r#"{"universes":[{"name":"demo","types":[
                {"k":"struct","name":"Box","params":[{"name":"T"}],
                 "fields":[{"name":"item","ty":{"k":"param","name":"T"}}]},
                {"k":"alias","name":"IntBox","target":{"k":"ref","name":"Box","args":["i64"]}},
                {"k":"alias","name":"StrBox","target":{"k":"ref","name":"Box","args":["str"]}},
                {"k":"alias","name":"Crate","params":[{"name":"V"}],
                 "target":{"k":"ref","name":"Box","args":[{"k":"param","name":"V"}]}}]}]}"#,
        );
        let generic = shape(&ws, "demo::Box");
        assert_ne!(shape(&ws, "demo::IntBox"), shape(&ws, "demo::StrBox"));
        assert_ne!(shape(&ws, "demo::IntBox"), generic);
        assert_eq!(
            shape(&ws, "demo::Crate"),
            generic,
            "passing its own parameter straight through, an alias is only a name"
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

    fn under(ws: &CheckedWorkspace, name: &str, view: super::View) -> TypeId {
        ws.address(&name.parse().unwrap(), view).expect("addressed")
    }

    /// The same numbers four ways: as written, swapped, renamed, and both.
    const FOUR_POINTS: &str = r#"{"universes":[
        {"name":"a","types":[{"k":"struct","name":"P","fields":[
            {"name":"x","ty":"float"},{"name":"n","ty":"i64"}]}]},
        {"name":"b","types":[{"k":"struct","name":"Swapped","fields":[
            {"name":"n","ty":"i64"},{"name":"x","ty":"float"}]}]},
        {"name":"c","types":[{"k":"struct","name":"Renamed","fields":[
            {"name":"lat","ty":"float"},{"name":"count","ty":"i64"}]}]},
        {"name":"d","types":[{"k":"struct","name":"Both","fields":[
            {"name":"count","ty":"i64"},{"name":"lat","ty":"float"}]}]}]}"#;

    #[test]
    fn each_view_counts_what_it_says_it_counts() {
        use super::View::{Bag, Keyed, Positional};
        let ws = check(FOUR_POINTS);
        let same = |a: &str, b: &str, view| under(&ws, a, view) == under(&ws, b, view);

        assert!(
            same("a::P", "b::Swapped", Keyed),
            "a key's place is not part of it"
        );
        assert!(!same("a::P", "c::Renamed", Keyed), "but its name is");

        assert!(
            same("a::P", "c::Renamed", Positional),
            "a place's name is not part of it"
        );
        assert!(
            !same("a::P", "b::Swapped", Positional),
            "but where it is, is"
        );

        for other in ["b::Swapped", "c::Renamed", "d::Both"] {
            assert!(same("a::P", other, Bag), "a bag is neither: {other}");
        }
        assert_eq!(
            under(&ws, "a::P", Keyed),
            shape(&ws, "a::P"),
            "keyed is the shape there always was"
        );
    }

    #[test]
    fn a_bag_counts_how_many_of_each() {
        use super::View::Bag;
        let ws = check(
            r#"{"universes":[
                {"name":"a","types":[{"k":"struct","name":"Two","fields":[
                    {"name":"x","ty":"float"},{"name":"y","ty":"float"}]}]},
                {"name":"b","types":[{"k":"struct","name":"One","fields":[
                    {"name":"x","ty":"float"}]}]}]}"#,
        );
        assert_ne!(
            under(&ws, "a::Two", Bag),
            under(&ws, "b::One", Bag),
            "two numbers are not one number"
        );
    }

    #[test]
    fn a_view_holds_all_the_way_down() {
        use super::View::{Bag, Keyed, Positional};
        let ws = check(
            r#"{"universes":[
                {"name":"a","types":[
                    {"k":"struct","name":"Point","fields":[{"name":"x","ty":"float"},{"name":"y","ty":"float"}]},
                    {"k":"struct","name":"Line","fields":[{"name":"start","ty":"Point"},{"name":"end","ty":"Point"}]}]},
                {"name":"b","types":[
                    {"k":"struct","name":"LatLong","fields":[{"name":"lat","ty":"float"},{"name":"long","ty":"float"}]},
                    {"k":"struct","name":"Leg","fields":[{"name":"from","ty":"LatLong"},{"name":"to","ty":"LatLong"}]}]}]}"#,
        );
        assert_ne!(under(&ws, "a::Line", Keyed), under(&ws, "b::Leg", Keyed));
        assert_eq!(
            under(&ws, "a::Line", Positional),
            under(&ws, "b::Leg", Positional),
            "the points inside are compared by position too"
        );
        assert_eq!(under(&ws, "a::Line", Bag), under(&ws, "b::Leg", Bag));
    }

    #[test]
    fn a_tag_and_a_record_key_keep_their_names_in_every_view() {
        for view in super::View::ALL {
            let ws = check(
                r#"{"universes":[
                    {"name":"a","types":[
                        {"k":"enum","name":"E","variants":[{"name":"Red"}]},
                        {"k":"struct","name":"R","fields":[{"name":"m","ty":{"k":"record","fields":[
                            {"name":"a","ty":"i64"}]}}]}]},
                    {"name":"b","types":[
                        {"k":"enum","name":"E","variants":[{"name":"Blue"}]},
                        {"k":"struct","name":"R","fields":[{"name":"m","ty":{"k":"record","fields":[
                            {"name":"b","ty":"i64"}]}}]}]}]}"#,
            );
            assert_ne!(under(&ws, "a::E", view), under(&ws, "b::E", view), "{view}");
            assert_ne!(under(&ws, "a::R", view), under(&ws, "b::R", view), "{view}");
        }
    }

    #[test]
    fn a_struct_and_an_interface_stay_apart_in_every_view() {
        for view in super::View::ALL {
            let ws = check(
                r#"{"universes":[{"name":"demo","types":[
                    {"k":"struct","name":"S","fields":[{"name":"id","ty":"str"}]},
                    {"k":"interface","name":"I","fields":[{"name":"id","ty":"str"}]}]}]}"#,
            );
            assert_ne!(
                under(&ws, "demo::S", view),
                under(&ws, "demo::I", view),
                "{view}"
            );
        }
    }

    #[test]
    fn a_recursive_type_reordered_and_renamed_is_the_same_bag() {
        use super::View::{Bag, Positional};
        let ws = check(
            r#"{"universes":[
                {"name":"a","types":[{"k":"struct","name":"Node","fields":[
                    {"name":"value","ty":"i64"},{"name":"next","ty":"Node?"}]}]},
                {"name":"b","types":[{"k":"struct","name":"Cell","fields":[
                    {"name":"tail","ty":"Cell?"},{"name":"head","ty":"i64"}]}]}]}"#,
        );
        assert_eq!(under(&ws, "a::Node", Bag), under(&ws, "b::Cell", Bag));
        assert_ne!(
            under(&ws, "a::Node", Positional),
            under(&ws, "b::Cell", Positional)
        );
    }

    #[test]
    fn a_signature_is_addressed_by_its_parameters_three_ways() {
        use super::View::{Bag, Keyed, Positional};
        let ws = check(
            r#"{"universes":[
                {"name":"node","functions":[{"name":"request","ret":"unit","params":[
                    {"name":"url","ty":"str"},
                    {"name":"cb","ty":{"k":"function","params":["str"],"ret":"unit"}}]},
                  {"name":"swapped","ret":"unit","params":[
                    {"name":"cb","ty":{"k":"function","params":["str"],"ret":"unit"}},
                    {"name":"url","ty":"str"}]}]},
                {"name":"browser","functions":[{"name":"request","ret":"unit","params":[
                    {"name":"callback","ty":{"k":"function","params":["str"],"ret":"unit"}},
                    {"name":"getUrl","ty":"str"}]}]}]}"#,
        );
        let at = |name: &str, view| ws.sig_address(&name.parse().unwrap(), view).unwrap();

        assert_eq!(at("node::request", Keyed), at("node::swapped", Keyed));
        assert_ne!(
            at("node::request", Positional),
            at("node::swapped", Positional)
        );
        assert_eq!(
            at("node::swapped", Positional),
            at("browser::request", Positional),
            "a callback then a string, whatever they are called"
        );
        assert_eq!(at("node::request", Bag), at("browser::request", Bag));
        assert_ne!(at("node::request", Keyed), at("browser::request", Keyed));
    }

    #[test]
    fn a_bag_does_not_care_what_order_generic_parameters_were_declared_in() {
        use super::View::{Bag, Positional};
        let p = |n: &str| format!(r#"{{"k":"param","name":"{n}"}}"#);
        let list = |n: &str| format!(r#"{{"k":"list","of":{}}}"#, p(n));
        let sig = |name: &str, generics: [&str; 2], params: [(&str, String); 2], ret: String| {
            format!(
                r#"{{"name":"{name}","params_decl":[{{"name":"{}"}},{{"name":"{}"}}],
                    "params":[{{"name":"{}","ty":{}}},{{"name":"{}","ty":{}}}],"ret":{ret}}}"#,
                generics[0], generics[1], params[0].0, params[0].1, params[1].0, params[1].1
            )
        };
        let ws = check(&format!(
            r#"{{"universes":[{{"name":"a","functions":[{},{},{},{}]}}]}}"#,
            sig("konst", ["A", "B"], [("x", p("A")), ("y", p("B"))], p("A")),
            sig(
                "flipped",
                ["Y", "X"],
                [("ignored", p("Y")), ("kept", p("X"))],
                p("X")
            ),
            sig(
                "plain",
                ["A", "B"],
                [("x", p("A")), ("ys", list("B"))],
                p("A")
            ),
            sig(
                "element",
                ["A", "B"],
                [("x", p("A")), ("ys", list("B"))],
                p("B")
            ),
        ));
        let at = |name: &str, view| {
            ws.sig_address(&format!("a::{name}").parse().unwrap(), view)
                .unwrap()
        };

        assert_eq!(
            at("konst", Bag),
            at("flipped", Bag),
            "a value of one type and a value of another, answering the first"
        );
        assert_ne!(
            at("konst", Positional),
            at("flipped", Positional),
            "a call names the generic parameters in order, so position keeps it"
        );
        assert_ne!(
            at("plain", Bag),
            at("element", Bag),
            "the one on its own, or one out of the list, whatever the order"
        );
    }

    #[test]
    fn a_bag_takes_a_generic_parameters_bounds_as_a_set() {
        let ws = check(
            r#"{"universes":[{"name":"a",
                "types":[{"k":"interface","name":"Named","fields":[{"name":"name","ty":"str"}]},
                         {"k":"interface","name":"Sized","fields":[{"name":"size","ty":"i64"}]}],
                "functions":[
                  {"name":"f","params_decl":[{"name":"T","bounds":["Named","Sized"]}],
                   "params":[{"name":"x","ty":{"k":"param","name":"T"}}],"ret":"str"},
                  {"name":"g","params_decl":[{"name":"T","bounds":["Sized","Named"]}],
                   "params":[{"name":"x","ty":{"k":"param","name":"T"}}],"ret":"str"}]}]}"#,
        );
        let at = |name: &str, view| {
            ws.sig_address(&format!("a::{name}").parse().unwrap(), view)
                .unwrap()
        };
        assert_eq!(at("f", super::View::Bag), at("g", super::View::Bag));
        assert_ne!(
            at("f", super::View::Positional),
            at("g", super::View::Positional)
        );
    }

    #[test]
    fn numberings_start_where_everything_already_is() {
        assert_eq!(super::numberings(0), [Vec::<usize>::new()]);
        assert_eq!(
            super::numberings(3),
            [
                [0, 1, 2],
                [0, 2, 1],
                [1, 0, 2],
                [1, 2, 0],
                [2, 0, 1],
                [2, 1, 0]
            ]
        );
    }

    #[test]
    fn structurally_a_proposition_over_named_types_is_the_data_it_takes() {
        // Where `sig_hash` keeps the names of the types a proposition is over,
        // its structural addresses compare what those types hold.
        let ws = check(
            r#"{"universes":[
                {"name":"a","types":[{"k":"struct","name":"Req",
                    "fields":[{"name":"url","ty":"str"}]}],
                 "functions":[{"name":"send","params":[{"name":"r","ty":"Req"}],"ret":"i32"}]},
                {"name":"b","types":[{"k":"struct","name":"Req",
                    "fields":[{"name":"url","ty":"str"}]}],
                 "functions":[{"name":"send","params":[{"name":"r","ty":"Req"}],"ret":"i32"}]}]}"#,
        );
        for view in super::View::ALL {
            assert_eq!(
                ws.sig_address(&"a::send".parse().unwrap(), view),
                ws.sig_address(&"b::send".parse().unwrap(), view),
                "{view}"
            );
        }
    }

    #[test]
    fn a_view_reads_back_from_its_own_name() {
        for view in super::View::ALL {
            assert_eq!(view.as_str().parse::<super::View>(), Ok(view));
        }
        assert!("sorted".parse::<super::View>().is_err());
    }
}
