//! Checking a workspace, and what you get once it passes.
//!
//! [`check_workspace`] takes a workspace as written and returns a
//! [`CheckedWorkspace`]: the same declarations, normalized, with every
//! reference resolved, every inheritance chain validated and flattened, and
//! every declaration given a structural address. Everything downstream — the
//! runtime, the backends, bridge derivation — works from a checked workspace,
//! so none of them has to re-litigate whether the declarations make sense.
//!
//! Problems accumulate rather than stopping the check, because fixing them one
//! at a time is a poor way to spend an afternoon. Passes that would need to
//! chase a reference that does not exist are skipped, so a single unresolved
//! name does not produce a page of consequences.

mod apply;
mod bounds;
pub mod error;
mod inherit;
mod refine;
mod resolve;
mod shape;
mod subst;
mod subtype;

use std::collections::HashMap;

use crate::canon::hash::{TypeId, decl_hash, sig_hash};
use crate::canon::normalize::normalize_workspace;
use crate::canon::words::AliasIndex;
use crate::ident::{Ident, QualifiedName};
use crate::ir::{Field, FunctionSig, TypeDef, TypeRef, Workspace};

pub use apply::CallError;
pub use error::{CheckError, CheckErrorKind, Diagnostics, PathSeg, TypePath};
pub use subst::{Subst, bind, substitute, substitute_ref};
pub use subtype::SubtypeFailure;

/// A position in a checked workspace's declaration table.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct DefIx(pub(crate) usize);

/// A position in a checked workspace's signature table.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct SigIx(pub(crate) usize);

/// What the checker learned about one declaration.
pub(crate) struct DefEntry {
    pub(crate) name: QualifiedName,
    /// Where the declaration lives: universe index, then index within it.
    pub(crate) loc: (usize, usize),
    /// Every supertype, transitively, expressed using this declaration's own
    /// generic parameters so that arguments can be substituted at a use site.
    pub(crate) ancestors: Vec<TypeRef>,
    /// Every field including inherited ones, parents first, with overrides
    /// applied where they were declared.
    pub(crate) flat_fields: Vec<Field>,
    pub(crate) decl: TypeId,
    pub(crate) shape: Option<TypeId>,
}

/// What the checker learned about one signature.
pub(crate) struct SigEntry {
    pub(crate) name: QualifiedName,
    pub(crate) loc: (usize, usize),
    pub(crate) hash: TypeId,
}

/// A workspace that has passed the checker.
///
/// Immutable, and cheap to share: the runtime holds one behind an `Arc` and the
/// backends walk it directly.
pub struct CheckedWorkspace {
    ws: Workspace,
    pub(crate) defs: Vec<DefEntry>,
    def_index: HashMap<QualifiedName, DefIx>,
    pub(crate) sigs: Vec<SigEntry>,
    sig_index: HashMap<QualifiedName, SigIx>,
    shape_index: HashMap<TypeId, Vec<QualifiedName>>,
    refiners: HashMap<QualifiedName, Vec<QualifiedName>>,
}

/// Checks a workspace, returning it in usable form or saying why not.
pub fn check_workspace(ws: Workspace) -> Result<CheckedWorkspace, Diagnostics> {
    let mut diags = Diagnostics::default();
    let mut checked = CheckedWorkspace::skeleton(normalize_workspace(ws), &mut diags);

    resolve::run(&checked, &mut diags);
    // Later passes chase references. If one does not resolve, chasing it would
    // produce a page of consequences for a single typo.
    if !diags.is_ok() {
        return Err(diags);
    }

    inherit::build(&mut checked, &mut diags);
    if !diags.is_ok() {
        return Err(diags);
    }

    inherit::verify(&checked, &mut diags);
    bounds::verify(&checked, &mut diags);
    refine::verify(&mut checked, &mut diags);
    shape::compute(&mut checked);

    if diags.is_ok() {
        Ok(checked)
    } else {
        Err(diags)
    }
}

impl CheckedWorkspace {
    /// Builds the lookup tables and reports names that collide.
    fn skeleton(ws: Workspace, diags: &mut Diagnostics) -> Self {
        let mut defs = Vec::new();
        let mut def_index = HashMap::new();
        let mut sigs = Vec::new();
        let mut sig_index = HashMap::new();
        let mut seen_universes: HashMap<&Ident, ()> = HashMap::new();

        for (ui, universe) in ws.universes.iter().enumerate() {
            let at = TypePath::root().then(PathSeg::Universe(universe.name.clone()));
            if seen_universes.insert(&universe.name, ()).is_some() {
                diags.error(
                    CheckErrorKind::DuplicateUniverse(universe.name.clone()),
                    at.clone(),
                );
            }

            for (di, def) in universe.types.iter().enumerate() {
                let name = QualifiedName::new(universe.name.clone(), def.name().clone());
                if def_index.contains_key(&name) {
                    diags.error(
                        CheckErrorKind::DuplicateDef(def.name().clone()),
                        at.then(PathSeg::Def(def.name().clone())),
                    );
                    continue;
                }
                def_index.insert(name.clone(), DefIx(defs.len()));
                defs.push(DefEntry {
                    name,
                    loc: (ui, di),
                    ancestors: Vec::new(),
                    flat_fields: Vec::new(),
                    decl: decl_hash(def),
                    shape: None,
                });
            }

            for (fi, sig) in universe.functions.iter().enumerate() {
                let name = QualifiedName::new(universe.name.clone(), sig.name.clone());
                if sig_index.contains_key(&name) {
                    diags.error(
                        CheckErrorKind::DuplicateFunction(sig.name.clone()),
                        at.then(PathSeg::Sig(sig.name.clone())),
                    );
                    continue;
                }
                sig_index.insert(name.clone(), SigIx(sigs.len()));
                sigs.push(SigEntry {
                    name,
                    loc: (ui, fi),
                    hash: sig_hash(sig),
                });
            }
        }

        Self {
            ws,
            defs,
            def_index,
            sigs,
            sig_index,
            shape_index: HashMap::new(),
            refiners: HashMap::new(),
        }
    }

    /// The normalized workspace.
    pub fn workspace(&self) -> &Workspace {
        &self.ws
    }

    /// Every declaration's qualified name, in checked order.
    pub fn def_names(&self) -> impl Iterator<Item = &QualifiedName> {
        self.defs.iter().map(|d| &d.name)
    }

    /// Every signature's qualified name, in checked order.
    pub fn sig_names(&self) -> impl Iterator<Item = &QualifiedName> {
        self.sigs.iter().map(|s| &s.name)
    }

    /// Where a name is in the declaration table.
    pub fn resolve(&self, name: &QualifiedName) -> Option<DefIx> {
        self.def_index.get(name).copied()
    }

    /// Where a name is in the signature table.
    pub fn resolve_sig(&self, name: &QualifiedName) -> Option<SigIx> {
        self.sig_index.get(name).copied()
    }

    /// The declaration at a position.
    pub fn def_at(&self, ix: DefIx) -> &TypeDef {
        let (u, d) = self.defs[ix.0].loc;
        &self.ws.universes[u].types[d]
    }

    /// The signature at a position.
    pub fn sig_at(&self, ix: SigIx) -> &FunctionSig {
        let (u, f) = self.sigs[ix.0].loc;
        &self.ws.universes[u].functions[f]
    }

    /// The name of the declaration at a position.
    pub fn def_name(&self, ix: DefIx) -> &QualifiedName {
        &self.defs[ix.0].name
    }

    /// The name of the signature at a position.
    pub fn sig_name(&self, ix: SigIx) -> &QualifiedName {
        &self.sigs[ix.0].name
    }

    /// A declaration, by name.
    pub fn def(&self, name: &QualifiedName) -> Option<&TypeDef> {
        self.resolve(name).map(|ix| self.def_at(ix))
    }

    /// A signature, by name.
    pub fn sig(&self, name: &QualifiedName) -> Option<&FunctionSig> {
        self.resolve_sig(name).map(|ix| self.sig_at(ix))
    }

    /// Every field of a struct or interface, inherited ones included.
    pub fn flat_fields(&self, name: &QualifiedName) -> Option<&[Field]> {
        self.resolve(name)
            .map(|ix| self.defs[ix.0].flat_fields.as_slice())
    }

    /// The address of a declaration as authored.
    pub fn decl_hash(&self, name: &QualifiedName) -> Option<TypeId> {
        self.resolve(name).map(|ix| self.defs[ix.0].decl)
    }

    /// The structural address of a declaration: what it is, with names erased.
    ///
    /// Two declarations in different universes, called different things, share
    /// this address exactly when they describe the same data.
    pub fn shape_hash(&self, name: &QualifiedName) -> Option<TypeId> {
        self.resolve(name).and_then(|ix| self.defs[ix.0].shape)
    }

    /// The address of a proposition.
    pub fn sig_hash(&self, name: &QualifiedName) -> Option<TypeId> {
        self.resolve_sig(name).map(|ix| self.sigs[ix.0].hash)
    }

    /// Every declaration sharing a structural address.
    ///
    /// More than one means the same data is described in more than one place —
    /// which is where a bridge suggests itself.
    pub fn defs_with_shape(&self, shape: &TypeId) -> &[QualifiedName] {
        self.shape_index
            .get(shape)
            .map(Vec::as_slice)
            .unwrap_or_default()
    }

    /// Signatures that strengthen this one, and so whose proofs also prove it.
    pub fn refiners_of(&self, name: &QualifiedName) -> &[QualifiedName] {
        self.refiners
            .get(name)
            .map(Vec::as_slice)
            .unwrap_or_default()
    }

    /// Word aliases for every declaration in this workspace.
    ///
    /// Shortening is relative to what is loaded: an alias is as short as it can
    /// be while still picking out one declaration here.
    pub fn alias_index(&self) -> AliasIndex {
        AliasIndex::new(self.defs.iter().filter_map(|d| d.shape))
    }

    /// The declaration a reference points at, if it points at anything.
    pub(crate) fn ref_target(&self, r: &TypeRef) -> Option<QualifiedName> {
        Some(QualifiedName::new(r.universe.clone()?, r.name.clone()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn check(json: &str) -> Result<CheckedWorkspace, Diagnostics> {
        check_workspace(Workspace::from_json(json).expect("reads"))
    }

    /// The one-line summary of the first error, for readable assertions.
    fn first_error(json: &str) -> String {
        check(json).err().expect("should not check").errors[0].to_string()
    }

    #[test]
    fn a_workspace_with_nothing_wrong_checks() {
        let ws = check(
            r#"{"universes":[{"name":"demo","types":[
                {"k":"struct","name":"Shape","fields":[{"name":"id","ty":"str"}]}]}]}"#,
        )
        .expect("checks");
        assert_eq!(ws.def_names().count(), 1);
    }

    #[test]
    fn two_declarations_with_one_name_are_reported() {
        let msg = first_error(
            r#"{"universes":[{"name":"demo","types":[
                {"k":"struct","name":"S","fields":[]},
                {"k":"struct","name":"S","fields":[]}]}]}"#,
        );
        assert!(msg.contains("declared more than once"), "{msg}");
    }

    #[test]
    fn declarations_are_reachable_by_name_after_checking() {
        let ws = check(
            r#"{"universes":[{"name":"geo","types":[
                {"k":"struct","name":"Point","fields":[
                    {"name":"x","ty":"float"},{"name":"y","ty":"float"}]}]}]}"#,
        )
        .expect("checks");
        let point: QualifiedName = "geo::Point".parse().unwrap();
        assert!(ws.def(&point).is_some());
        assert_eq!(ws.flat_fields(&point).unwrap().len(), 2);
        assert!(ws.shape_hash(&point).is_some());
    }
}
