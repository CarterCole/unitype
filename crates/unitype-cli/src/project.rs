//! Finding and loading a project.
//!
//! A project is a `unitype.json` naming where the universes are, where the
//! manifests are, and where generated code should go. It is found by walking up
//! from wherever the command was run, the way `cargo` finds its manifest, so
//! that a command works from anywhere inside a project rather than only at the
//! top.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use unitype_core::check::{CheckedWorkspace, Diagnostics, check_workspace};
use unitype_core::{Universe, Workspace};
use unitype_runtime::Registry;

/// The project file format this build reads.
pub const PROJECT_VERSION: &str = "1.0";

/// What a project file says.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ProjectFile {
    /// The format version.
    #[serde(default = "default_version")]
    pub unitype: String,
    /// What the project is called.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Files or directories holding universes.
    #[serde(default = "default_universes")]
    pub universes: Vec<PathBuf>,
    /// Files or directories holding implementation manifests.
    #[serde(default = "default_impls")]
    pub impls: Vec<PathBuf>,
    /// Files or directories holding links between universes.
    ///
    /// Separate from the universes because a link belongs to neither end of
    /// itself: it is a claim about two universes, made by whoever noticed they
    /// were describing the same thing.
    #[serde(default = "default_links")]
    pub links: Vec<PathBuf>,
    /// Where generated code goes and for which languages.
    ///
    /// Spelled `gen` in the file, which reads better than the word Rust makes
    /// us use here.
    #[serde(default, rename = "gen")]
    pub generate: GenSettings,
}

/// Defaults for generation, which command-line flags override.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct GenSettings {
    /// Where to write.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub out: Option<PathBuf>,
    /// Which languages to write.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub langs: Vec<String>,
}

fn default_version() -> String {
    PROJECT_VERSION.to_string()
}

fn default_universes() -> Vec<PathBuf> {
    vec!["universes".into()]
}

fn default_impls() -> Vec<PathBuf> {
    vec!["impls".into()]
}

fn default_links() -> Vec<PathBuf> {
    vec!["links".into()]
}

/// A file of links, however it was written.
#[derive(Deserialize)]
#[serde(untagged)]
enum LinkFile {
    /// A bare array of links.
    Bare(Vec<unitype_core::Link>),
    /// An object with a `links` member, matching a workspace document.
    Wrapped {
        /// The links.
        links: Vec<unitype_core::Link>,
    },
}

impl LinkFile {
    fn links(self) -> Vec<unitype_core::Link> {
        match self {
            LinkFile::Bare(links) | LinkFile::Wrapped { links } => links,
        }
    }
}

impl Default for ProjectFile {
    fn default() -> Self {
        Self {
            unitype: default_version(),
            name: None,
            universes: default_universes(),
            impls: default_impls(),
            links: default_links(),
            generate: GenSettings::default(),
        }
    }
}

/// A loaded project.
pub struct Project {
    /// Where the project file is.
    pub root: PathBuf,
    /// What it said.
    pub file: ProjectFile,
    /// The universes, checked.
    ///
    /// Shared rather than owned, because the dispatcher needs it too and
    /// checking it twice would be work for nothing.
    pub workspace: Arc<CheckedWorkspace>,
    /// What proves what.
    ///
    /// Shared for a stronger reason: it holds the functions compiled into this
    /// process, and those cannot be rebuilt from anything on disk.
    pub registry: Arc<Registry>,
}

/// Why a project could not be loaded.
#[derive(thiserror::Error, Debug)]
pub enum ProjectError {
    /// No project file anywhere above here.
    #[error(
        "no `unitype.json` here or in any directory above `{0}`.\n\
         Create one, or point at a project with --project."
    )]
    NotFound(PathBuf),
    /// The project file was unreadable.
    #[error("could not read {path}: {source}")]
    Io {
        /// The file.
        path: String,
        /// What went wrong.
        #[source]
        source: std::io::Error,
    },
    /// The project file was not what it claimed.
    #[error("{path} is not a project file: {source}")]
    Malformed {
        /// The file.
        path: String,
        /// What was wrong.
        #[source]
        source: serde_json::Error,
    },
    /// The project declares a version this build does not read.
    #[error("{path} is version {found}; this build reads {expected}")]
    Version {
        /// The file.
        path: String,
        /// What it said.
        found: String,
        /// What this build knows.
        expected: &'static str,
    },
    /// A universe file was unreadable or not a universe.
    #[error("{path}: {detail}")]
    Universe {
        /// The file.
        path: String,
        /// What was wrong.
        detail: String,
    },
    /// The universes do not check.
    #[error("{0}")]
    Check(Diagnostics),
    /// A manifest could not be loaded.
    #[error("{0}")]
    Registry(#[from] unitype_runtime::RegistryError),
}

/// Finds the project containing `from`, or the one at `explicit`.
pub fn find(explicit: Option<&Path>, from: &Path) -> Result<PathBuf, ProjectError> {
    if let Some(given) = explicit {
        // A directory means the project file inside it.
        let candidate = if given.is_dir() {
            given.join("unitype.json")
        } else {
            given.to_path_buf()
        };
        return Ok(candidate);
    }

    let mut at = from;
    loop {
        let candidate = at.join("unitype.json");
        if candidate.is_file() {
            return Ok(candidate);
        }
        match at.parent() {
            Some(up) => at = up,
            None => return Err(ProjectError::NotFound(from.to_path_buf())),
        }
    }
}

impl Project {
    /// Loads the project whose file is at `path`.
    pub fn load(path: &Path) -> Result<Self, ProjectError> {
        let text = std::fs::read_to_string(path).map_err(|source| ProjectError::Io {
            path: path.display().to_string(),
            source,
        })?;
        let file: ProjectFile =
            serde_json::from_str(&text).map_err(|source| ProjectError::Malformed {
                path: path.display().to_string(),
                source,
            })?;
        if file.unitype != PROJECT_VERSION {
            return Err(ProjectError::Version {
                path: path.display().to_string(),
                found: file.unitype,
                expected: PROJECT_VERSION,
            });
        }

        let root = path.parent().unwrap_or(Path::new(".")).to_path_buf();

        let mut universes = Vec::new();
        for entry in &file.universes {
            for found in json_files(&root.join(entry)) {
                let text = std::fs::read_to_string(&found).map_err(|e| ProjectError::Universe {
                    path: found.display().to_string(),
                    detail: e.to_string(),
                })?;
                universes.push(
                    Universe::from_json(&text).map_err(|e| ProjectError::Universe {
                        path: found.display().to_string(),
                        detail: e.to_string(),
                    })?,
                );
            }
        }

        let mut workspace = Workspace::of(universes);
        for entry in &file.links {
            for found in json_files(&root.join(entry)) {
                let text = std::fs::read_to_string(&found).map_err(|e| ProjectError::Universe {
                    path: found.display().to_string(),
                    detail: e.to_string(),
                })?;
                let parsed: LinkFile =
                    serde_json::from_str(&text).map_err(|e| ProjectError::Universe {
                        path: found.display().to_string(),
                        detail: format!("this is not a file of links: {e}"),
                    })?;
                workspace.links.extend(parsed.links());
            }
        }

        let workspace = check_workspace(workspace).map_err(ProjectError::Check)?;

        let mut registry = Registry::new();
        crate::builtins::register(&mut registry);
        registry.register_bodies(&workspace)?;
        for entry in &file.impls {
            for found in json_files(&root.join(entry)) {
                registry.load_manifest_file(&found)?;
            }
        }

        Ok(Self {
            root,
            file,
            workspace: Arc::new(workspace),
            registry: Arc::new(registry),
        })
    }

    /// The project's name, or the directory it lives in.
    pub fn name(&self) -> String {
        self.file.name.clone().unwrap_or_else(|| {
            self.root
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| "unnamed".into())
        })
    }
}

/// Every `.json` file at a path: itself if it is one, everything in it if it is
/// a directory. Sorted, so a project loads the same way twice.
fn json_files(at: &Path) -> Vec<PathBuf> {
    if at.is_file() {
        return vec![at.to_path_buf()];
    }
    let Ok(entries) = std::fs::read_dir(at) else {
        return Vec::new();
    };
    let mut found: Vec<PathBuf> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.is_file() && p.extension().is_some_and(|e| e == "json"))
        .collect();
    found.sort();
    found
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join("unitype-cli-tests").join(name);
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("makes a directory");
        dir
    }

    fn write(path: &Path, text: &str) {
        std::fs::create_dir_all(path.parent().expect("a parent")).expect("makes a directory");
        std::fs::write(path, text).expect("writes");
    }

    fn a_project(dir: &Path) {
        write(dir.join("unitype.json").as_path(), r#"{"name":"demo"}"#);
        write(
            dir.join("universes/demo.json").as_path(),
            r#"{"name":"demo","types":[
                {"k":"struct","name":"Shape","fields":[{"name":"id","ty":"str"}]}],
              "functions":[
                {"name":"sort","params":[{"name":"xs","ty":"[int]"}],"ret":"[int]"}]}"#,
        );
    }

    #[test]
    fn a_project_is_found_from_anywhere_inside_it() {
        let dir = scratch("find_project");
        a_project(&dir);
        std::fs::create_dir_all(dir.join("deep/deeper")).expect("makes directories");

        let found = find(None, &dir.join("deep/deeper")).expect("walks up");
        assert_eq!(found, dir.join("unitype.json"));
    }

    #[test]
    fn somewhere_that_is_not_a_project_says_so_and_says_what_to_do() {
        let dir = scratch("no_project");
        let err = find(None, &dir).unwrap_err();
        assert!(err.to_string().contains("--project"), "{err}");
    }

    #[test]
    fn a_directory_given_explicitly_means_the_file_inside_it() {
        let dir = scratch("explicit_project");
        a_project(&dir);
        assert_eq!(
            find(Some(&dir), Path::new("/")).expect("takes what it was given"),
            dir.join("unitype.json")
        );
    }

    #[test]
    fn a_project_loads_its_universes_and_checks_them() {
        let dir = scratch("load_project");
        a_project(&dir);
        let project = Project::load(&dir.join("unitype.json")).expect("loads");

        assert_eq!(project.name(), "demo");
        assert_eq!(project.workspace.def_names().count(), 1);
        assert!(
            project
                .workspace
                .workspace()
                .universe(&"demo".parse().unwrap())
                .is_some()
        );
    }

    #[test]
    fn universes_that_do_not_check_are_reported_rather_than_half_loaded() {
        let dir = scratch("bad_project");
        write(dir.join("unitype.json").as_path(), "{}");
        write(
            dir.join("universes/demo.json").as_path(),
            r#"{"name":"demo","types":[
                {"k":"struct","name":"S","fields":[{"name":"f","ty":"Missing"}]}]}"#,
        );

        let err = match Project::load(&dir.join("unitype.json")) {
            Err(e) => e,
            Ok(_) => panic!("a universe that does not check should not load"),
        };
        assert!(err.to_string().contains("unknown type"), "{err}");
    }

    #[test]
    fn links_are_loaded_from_their_own_files() {
        let dir = scratch("linked_project");
        write(dir.join("unitype.json").as_path(), "{}");
        write(
            dir.join("universes/geo.json").as_path(),
            r#"{"name":"geo","types":[{"k":"struct","name":"Point",
                "fields":[{"name":"x","ty":"float"},{"name":"y","ty":"float"}]}]}"#,
        );
        write(
            dir.join("universes/gfx.json").as_path(),
            r#"{"name":"gfx","types":[{"k":"struct","name":"Vec2",
                "fields":[{"name":"x","ty":"float"},{"name":"y","ty":"float"}]}]}"#,
        );
        write(
            dir.join("links/geo_gfx.json").as_path(),
            r#"{"links":[{"name":"point_is_a_vec2","from":"geo::Point","to":"gfx::Vec2",
                "kind":{"k":"identical"}}]}"#,
        );

        let project = Project::load(&dir.join("unitype.json")).expect("loads");
        assert_eq!(project.workspace.workspace().links.len(), 1);
        assert!(
            project
                .workspace
                .bridge_path(
                    &"geo::Point".parse().unwrap(),
                    &"gfx::Vec2".parse().unwrap()
                )
                .is_some(),
            "and the route across is there"
        );
    }

    #[test]
    fn a_link_file_may_be_a_bare_array() {
        let dir = scratch("bare_links");
        write(dir.join("unitype.json").as_path(), "{}");
        write(
            dir.join("universes/a.json").as_path(),
            r#"{"name":"a","types":[{"k":"struct","name":"X","fields":[{"name":"id","ty":"str"}]}]}"#,
        );
        write(
            dir.join("universes/b.json").as_path(),
            r#"{"name":"b","types":[{"k":"struct","name":"Y","fields":[{"name":"id","ty":"str"}]}]}"#,
        );
        write(
            dir.join("links/all.json").as_path(),
            r#"[{"name":"x_is_y","from":"a::X","to":"b::Y","kind":{"k":"identical"}}]"#,
        );

        let project = Project::load(&dir.join("unitype.json")).expect("loads");
        assert_eq!(project.workspace.workspace().links.len(), 1);
    }

    #[test]
    fn a_project_from_a_later_version_is_refused_rather_than_guessed_at() {
        let dir = scratch("future_project");
        write(dir.join("unitype.json").as_path(), r#"{"unitype":"9.9"}"#);
        let err = match Project::load(&dir.join("unitype.json")) {
            Err(e) => e,
            Ok(_) => panic!("a later version should not load"),
        };
        assert!(matches!(err, ProjectError::Version { .. }), "{err}");
    }

    #[test]
    fn a_signature_with_a_body_is_registered_without_being_asked() {
        let dir = scratch("bodies_project");
        write(dir.join("unitype.json").as_path(), "{}");
        write(
            dir.join("universes/demo.json").as_path(),
            r#"{"name":"demo","functions":[
                {"name":"echo","params":[{"name":"s","ty":"str"}],"ret":"str",
                 "body":{"k":"var","name":"s"}}]}"#,
        );

        let project = Project::load(&dir.join("unitype.json")).expect("loads");
        assert_eq!(
            project
                .registry
                .proofs_of(&"demo::echo".parse().unwrap())
                .len(),
            1
        );
    }

    #[test]
    fn a_project_loads_the_same_way_twice() {
        let dir = scratch("stable_project");
        a_project(&dir);
        write(
            dir.join("universes/other.json").as_path(),
            r#"{"name":"other","types":[
                {"k":"struct","name":"T","fields":[]}]}"#,
        );

        let first = Project::load(&dir.join("unitype.json")).expect("loads");
        let second = Project::load(&dir.join("unitype.json")).expect("loads");
        let names = |p: &Project| {
            p.workspace
                .def_names()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
        };
        assert_eq!(names(&first), names(&second));
    }
}
