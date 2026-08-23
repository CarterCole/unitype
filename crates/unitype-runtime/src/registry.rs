//! What proves what, and where it runs.
//!
//! Nothing here is discovered. An environment loads a module, and the module
//! registers what it can prove — either programmatically, for code compiled
//! into this process, or by handing over a manifest, for everything else. That
//! is the same act in both cases, which is why an embedder and a standalone
//! binary need no different arrangement.
//!
//! Signatures that carry a body register themselves, since a constructive proof
//! needs no one to supply it.

use std::collections::{BTreeMap, HashMap};
use std::path::Path;
use std::sync::Arc;

use unitype_core::canon::sig_hash;
use unitype_core::check::CheckedWorkspace;
use unitype_core::value::Value;
use unitype_core::{Ident, QualifiedName};

use crate::environment::{Environment, Reach};
use crate::error::ExecError;
use crate::implementation::{ImplId, Implementation, Manifest, trivial_cost};
use crate::interpret::BoxFuture;

/// The entry name given to a signature proved by its own body.
pub const INTRINSIC_ENTRY: &str = "intrinsic";

/// A function compiled into this process.
pub type NativeFn =
    Arc<dyn Fn(Vec<Value>) -> BoxFuture<'static, Result<Value, ExecError>> + Send + Sync>;

/// Why something could not be registered.
#[derive(thiserror::Error, Debug)]
pub enum RegistryError {
    /// Two implementations were given the same name.
    #[error("`{0}` is already registered")]
    DuplicateImplementation(ImplId),
    /// Two environments were given the same name.
    #[error("environment `{0}` is already registered")]
    DuplicateEnvironment(Ident),
    /// An implementation named an environment that was never introduced.
    #[error("`{id}` runs in `{environment}`, which is not a registered environment")]
    UnknownEnvironment {
        /// The implementation.
        id: ImplId,
        /// The environment it named.
        environment: Ident,
    },
    /// A manifest could not be read.
    #[error("invalid manifest: {0}")]
    Manifest(#[from] serde_json::Error),
    /// A manifest file could not be read from disk.
    #[error("could not read {path}: {source}")]
    Io {
        /// The file.
        path: String,
        /// What went wrong.
        #[source]
        source: std::io::Error,
    },
}

/// Something that was noticed but did not stop registration.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RegistryWarning {
    /// An implementation proves a signature the workspace does not have.
    ProvesNothing {
        /// The implementation.
        id: ImplId,
        /// What it claimed to prove.
        sig: QualifiedName,
    },
    /// An implementation was written against a signature that has since
    /// changed, so it is proving something nobody is asking any more.
    Stale {
        /// The implementation.
        id: ImplId,
        /// The signature as it now stands.
        sig: QualifiedName,
    },
}

impl std::fmt::Display for RegistryWarning {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RegistryWarning::ProvesNothing { id, sig } => {
                write!(f, "`{id}` claims to prove `{sig}`, which is not declared")
            }
            RegistryWarning::Stale { id, sig } => write!(
                f,
                "`{id}` was written against an earlier `{sig}`; the proposition has changed"
            ),
        }
    }
}

/// Everything known about who proves what.
#[derive(Default)]
pub struct Registry {
    environments: BTreeMap<Ident, Environment>,
    implementations: Vec<Implementation>,
    by_sig: HashMap<QualifiedName, Vec<usize>>,
    by_id: HashMap<ImplId, usize>,
    native: HashMap<String, NativeFn>,
}

impl Registry {
    /// A registry that knows only about this process.
    pub fn new() -> Self {
        let mut registry = Self::default();
        registry
            .add_environment(Environment::here())
            .expect("a fresh registry has room for it");
        registry
    }

    /// Introduces a place implementations can run.
    pub fn add_environment(&mut self, environment: Environment) -> Result<(), RegistryError> {
        if self.environments.contains_key(&environment.id) {
            return Err(RegistryError::DuplicateEnvironment(environment.id));
        }
        self.environments
            .insert(environment.id.clone(), environment);
        Ok(())
    }

    /// Records that something proves a proposition.
    pub fn register(&mut self, implementation: Implementation) -> Result<(), RegistryError> {
        if self.by_id.contains_key(&implementation.id) {
            return Err(RegistryError::DuplicateImplementation(implementation.id));
        }
        if !self.environments.contains_key(&implementation.environment) {
            return Err(RegistryError::UnknownEnvironment {
                id: implementation.id.clone(),
                environment: implementation.environment.clone(),
            });
        }

        let at = self.implementations.len();
        self.by_id.insert(implementation.id.clone(), at);
        self.by_sig
            .entry(implementation.proves.clone())
            .or_default()
            .push(at);
        self.implementations.push(implementation);
        Ok(())
    }

    /// Registers a function compiled into this process, under an entry name
    /// that an implementation can point at.
    ///
    /// This is a module registering itself as an execution source, in the case
    /// where the module happens to be linked in.
    pub fn register_native<F, Fut>(&mut self, entry: &str, f: F)
    where
        F: Fn(Vec<Value>) -> Fut + Send + Sync + 'static,
        Fut: std::future::Future<Output = Result<Value, ExecError>> + Send + 'static,
    {
        self.native
            .insert(entry.to_string(), Arc::new(move |args| Box::pin(f(args))));
    }

    /// The same, for a function that does not need to wait for anything.
    pub fn register_native_sync<F>(&mut self, entry: &str, f: F)
    where
        F: Fn(Vec<Value>) -> Result<Value, ExecError> + Send + Sync + 'static,
    {
        let f = Arc::new(f);
        self.native.insert(
            entry.to_string(),
            Arc::new(move |args| {
                let f = Arc::clone(&f);
                Box::pin(async move { f(args) })
            }),
        );
    }

    /// Registers every signature that carries its own proof.
    ///
    /// A body is an implementation already: it needs no environment beyond this
    /// process and no one to write it. Derived bridges arrive this way.
    pub fn register_bodies(&mut self, ws: &CheckedWorkspace) -> Result<usize, RegistryError> {
        let here = Environment::here().id;
        let mut added = 0;
        for (name, sig) in ws.workspace().iter_functions() {
            if sig.body.is_none() {
                continue;
            }
            let id = ImplId::new(format!("{name}#body"));
            if self.by_id.contains_key(&id) {
                continue;
            }
            let mut implementation = Implementation::new(
                id,
                name.clone(),
                here.clone(),
                INTRINSIC_ENTRY,
                trivial_cost(),
            );
            implementation.sig_hash = Some(sig_hash(sig));
            implementation.notes = Some("Proved by its own body.".to_string());
            self.register(implementation)?;
            added += 1;
        }
        Ok(added)
    }

    /// Takes in a module's declaration of what it can prove.
    ///
    /// Relative paths inside are resolved against `base`, which is the
    /// directory the declaration came from, so that a manifest means the same
    /// thing wherever it is loaded from.
    pub fn load_manifest(&mut self, text: &str, base: &Path) -> Result<usize, RegistryError> {
        let manifest: Manifest = serde_json::from_str(text)?;

        for mut environment in manifest.environments {
            resolve_paths(&mut environment.reach, base);
            // An environment already introduced is not an error here: two
            // modules may both know they need Python.
            if !self.environments.contains_key(&environment.id) {
                self.add_environment(environment)?;
            }
        }

        let count = manifest.implementations.len();
        for implementation in manifest.implementations {
            self.register(implementation)?;
        }
        Ok(count)
    }

    /// Reads a manifest from a file.
    pub fn load_manifest_file(&mut self, path: &Path) -> Result<usize, RegistryError> {
        let text = std::fs::read_to_string(path).map_err(|source| RegistryError::Io {
            path: path.display().to_string(),
            source,
        })?;
        let base = path.parent().unwrap_or(Path::new("."));
        self.load_manifest(&text, base)
    }

    /// Everything that claims to prove a proposition, in registration order.
    pub fn proofs_of(&self, sig: &QualifiedName) -> Vec<&Implementation> {
        self.by_sig
            .get(sig)
            .map(|indices| indices.iter().map(|i| &self.implementations[*i]).collect())
            .unwrap_or_default()
    }

    /// One implementation, by name.
    pub fn implementation(&self, id: &ImplId) -> Option<&Implementation> {
        self.by_id.get(id).map(|i| &self.implementations[*i])
    }

    /// Every implementation, in registration order.
    pub fn implementations(&self) -> &[Implementation] {
        &self.implementations
    }

    /// One environment, by name.
    pub fn environment(&self, id: &Ident) -> Option<&Environment> {
        self.environments.get(id)
    }

    /// Every environment, by name.
    pub fn environments(&self) -> impl Iterator<Item = &Environment> {
        self.environments.values()
    }

    /// A function compiled into this process, by entry name.
    pub fn native(&self, entry: &str) -> Option<&NativeFn> {
        self.native.get(entry)
    }

    /// Checks what is registered against what the workspace actually declares.
    ///
    /// Catches the two ways a registry drifts: an implementation proving
    /// something nobody declares, and one written against a proposition that
    /// has since changed out from under it.
    pub fn check_against(&self, ws: &CheckedWorkspace) -> Vec<RegistryWarning> {
        let mut out = Vec::new();
        for implementation in &self.implementations {
            let Some(sig) = ws.sig(&implementation.proves) else {
                out.push(RegistryWarning::ProvesNothing {
                    id: implementation.id.clone(),
                    sig: implementation.proves.clone(),
                });
                continue;
            };
            if let Some(claimed) = implementation.sig_hash {
                if claimed != sig_hash(sig) {
                    out.push(RegistryWarning::Stale {
                        id: implementation.id.clone(),
                        sig: implementation.proves.clone(),
                    });
                }
            }
        }
        out
    }
}

/// Makes a manifest's relative paths mean the same thing wherever it is loaded.
fn resolve_paths(reach: &mut Reach, base: &Path) {
    match reach {
        Reach::Subprocess { cwd, .. } => {
            if let Some(dir) = cwd {
                if dir.is_relative() {
                    *dir = base.join(&*dir);
                }
            } else {
                *cwd = Some(base.to_path_buf());
            }
        }
        Reach::Wasm { module } => {
            if module.is_relative() {
                *module = base.join(&*module);
            }
        }
        Reach::Here | Reach::Remote { .. } => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use unitype_core::check::check_workspace;
    use unitype_core::{BigO, ImplProfile, Workspace};

    fn ws(json: &str) -> CheckedWorkspace {
        check_workspace(Workspace::from_json(json).expect("reads")).expect("checks")
    }

    fn q(s: &str) -> QualifiedName {
        s.parse().unwrap()
    }

    fn here() -> Ident {
        Ident::new("here").unwrap()
    }

    fn sort_workspace() -> CheckedWorkspace {
        ws(r#"{"universes":[{"name":"demo","functions":[
            {"name":"sort","params":[{"name":"xs","ty":"[i64]"}],"ret":"[i64]"}]}]}"#)
    }

    fn proof(id: &str, time: BigO, space: BigO) -> Implementation {
        Implementation::new(
            id,
            q("demo::sort"),
            here(),
            id,
            ImplProfile::new(time, space),
        )
    }

    #[test]
    fn a_fresh_registry_knows_about_this_process() {
        let registry = Registry::new();
        assert!(registry.environment(&here()).is_some());
        assert_eq!(registry.environments().count(), 1);
    }

    #[test]
    fn one_proposition_can_have_several_proofs() {
        let mut registry = Registry::new();
        registry
            .register(proof("merge", BigO::ONLogN, BigO::ON))
            .expect("registers");
        registry
            .register(proof("bubble", BigO::ON2, BigO::O1))
            .expect("registers");

        assert_eq!(registry.proofs_of(&q("demo::sort")).len(), 2);
        assert!(registry.proofs_of(&q("demo::missing")).is_empty());
    }

    #[test]
    fn two_proofs_cannot_share_a_name() {
        let mut registry = Registry::new();
        registry
            .register(proof("same", BigO::ON, BigO::ON))
            .unwrap();
        assert!(matches!(
            registry.register(proof("same", BigO::O1, BigO::O1)),
            Err(RegistryError::DuplicateImplementation(_))
        ));
    }

    #[test]
    fn a_proof_cannot_run_somewhere_that_was_never_introduced() {
        let mut registry = Registry::new();
        let mut nowhere = proof("stray", BigO::ON, BigO::ON);
        nowhere.environment = Ident::new("mars").unwrap();
        assert!(matches!(
            registry.register(nowhere),
            Err(RegistryError::UnknownEnvironment { .. })
        ));
    }

    #[tokio::test]
    async fn a_module_compiled_in_registers_itself() {
        let mut registry = Registry::new();
        registry.register_native_sync("reverse", |args| {
            let Some(Value::List(items)) = args.into_iter().next() else {
                return Err(ExecError::Untyped("expected a list".into()));
            };
            Ok(Value::List(items.into_iter().rev().collect()))
        });

        let f = registry.native("reverse").expect("registered");
        let out = f(vec![Value::List(vec![Value::Int(1), Value::Int(2)])])
            .await
            .expect("runs");
        assert_eq!(out, Value::List(vec![Value::Int(2), Value::Int(1)]));
    }

    #[test]
    fn a_signature_with_a_body_needs_nobody_to_prove_it() {
        let ws = ws(r#"{"universes":[{"name":"demo","functions":[
            {"name":"sort","params":[{"name":"xs","ty":"[i64]"}],"ret":"[i64]"},
            {"name":"echo","params":[{"name":"s","ty":"str"}],"ret":"str",
             "body":{"k":"var","name":"s"}}]}]}"#);

        let mut registry = Registry::new();
        assert_eq!(registry.register_bodies(&ws).expect("registers"), 1);
        assert_eq!(registry.proofs_of(&q("demo::echo")).len(), 1);
        assert!(
            registry.proofs_of(&q("demo::sort")).is_empty(),
            "sorting still needs somebody to write it"
        );
    }

    #[test]
    fn registering_bodies_twice_does_not_double_them() {
        let ws = ws(r#"{"universes":[{"name":"demo","functions":[
            {"name":"echo","params":[{"name":"s","ty":"str"}],"ret":"str",
             "body":{"k":"var","name":"s"}}]}]}"#);
        let mut registry = Registry::new();
        registry.register_bodies(&ws).unwrap();
        assert_eq!(registry.register_bodies(&ws).unwrap(), 0);
        assert_eq!(registry.proofs_of(&q("demo::echo")).len(), 1);
    }

    #[test]
    fn a_manifest_introduces_an_environment_and_what_runs_there() {
        let mut registry = Registry::new();
        let count = registry
            .load_manifest(
                r#"{"environments":[
                        {"id":"py","language":"python",
                         "reach":{"k":"subprocess","command":"python3","args":["impls.py"]}}],
                    "implementations":[
                        {"id":"demo.sort.bubble","proves":"demo::sort","environment":"py",
                         "entry":"bubble_sort","cost":{"time":"o_n2","space":"o1"}}]}"#,
                Path::new("/project/impls"),
            )
            .expect("loads");

        assert_eq!(count, 1);
        assert!(registry.environment(&Ident::new("py").unwrap()).is_some());
        assert_eq!(registry.proofs_of(&q("demo::sort")).len(), 1);
    }

    #[test]
    fn a_manifest_means_the_same_thing_wherever_it_is_loaded_from() {
        let mut registry = Registry::new();
        registry
            .load_manifest(
                r#"{"environments":[{"id":"py","language":"python",
                    "reach":{"k":"subprocess","command":"python3","args":["impls.py"]}}]}"#,
                Path::new("/project/impls"),
            )
            .expect("loads");

        let env = registry.environment(&Ident::new("py").unwrap()).unwrap();
        let Reach::Subprocess { cwd, .. } = &env.reach else {
            panic!("expected a subprocess");
        };
        assert_eq!(
            cwd.as_deref(),
            Some(Path::new("/project/impls")),
            "a relative command should run where its manifest lives"
        );
    }

    #[test]
    fn two_modules_may_both_know_they_need_python() {
        let manifest = r#"{"environments":[{"id":"py","language":"python",
            "reach":{"k":"subprocess","command":"python3"}}]}"#;
        let mut registry = Registry::new();
        registry.load_manifest(manifest, Path::new(".")).unwrap();
        assert!(
            registry.load_manifest(manifest, Path::new(".")).is_ok(),
            "the second module should not have to know the first went first"
        );
    }

    #[test]
    fn a_proof_of_something_nobody_declares_is_reported() {
        let ws = sort_workspace();
        let mut registry = Registry::new();
        let mut stray = proof("stray", BigO::ON, BigO::ON);
        stray.proves = q("demo::nonexistent");
        registry.register(stray).unwrap();

        assert!(matches!(
            registry.check_against(&ws).as_slice(),
            [RegistryWarning::ProvesNothing { .. }]
        ));
    }

    #[test]
    fn a_proof_of_a_proposition_that_has_since_changed_is_reported() {
        let ws = sort_workspace();
        let mut registry = Registry::new();
        let mut written_earlier = proof("merge", BigO::ONLogN, BigO::ON);
        written_earlier.sig_hash = Some(unitype_core::canon::local_hash(
            &unitype_core::ir::Type::Bool,
        ));
        registry.register(written_earlier).unwrap();

        let warnings = registry.check_against(&ws);
        assert!(
            matches!(warnings.as_slice(), [RegistryWarning::Stale { .. }]),
            "{warnings:?}"
        );
        assert!(warnings[0].to_string().contains("has changed"));
    }

    #[test]
    fn a_proof_that_still_matches_its_proposition_is_not_reported() {
        let ws = sort_workspace();
        let mut registry = Registry::new();
        let mut current = proof("merge", BigO::ONLogN, BigO::ON);
        current.sig_hash = ws.sig_hash(&q("demo::sort"));
        registry.register(current).unwrap();

        assert!(registry.check_against(&ws).is_empty());
    }
}
