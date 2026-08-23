//! `unitype gen` — write the types out in another language.

use std::path::PathBuf;

use serde_json::json;
use unitype_codegen::{GenOptions, backend, backends};

use super::Output;
use crate::Failure;
use crate::project::Project;

/// Generates code for the languages asked for.
pub fn run(
    project: &Project,
    langs: Vec<String>,
    into: Option<PathBuf>,
    dry_run: bool,
    out: &Output,
) -> Result<(), Failure> {
    // What was asked for, then what the project says, then everything.
    let wanted: Vec<String> = if !langs.is_empty() {
        langs
    } else if !project.file.generate.langs.is_empty() {
        project.file.generate.langs.clone()
    } else {
        backends().iter().map(|b| b.id().to_string()).collect()
    };

    let root = into
        .or_else(|| project.file.generate.out.clone())
        .unwrap_or_else(|| "generated".into());
    let root = if root.is_absolute() {
        root
    } else {
        project.root.join(root)
    };

    let mut written = Vec::new();
    for lang in &wanted {
        let Some(backend) = backend(lang) else {
            return Err(Failure::usage(format!(
                "`{lang}` is not a language this writes. Try {}.",
                backends()
                    .iter()
                    .map(|b| b.id())
                    .collect::<Vec<_>>()
                    .join(", ")
            )));
        };

        let files = backend
            .generate(&project.workspace, &GenOptions::default())
            .map_err(|e| Failure::check(e.to_string()))?;

        for file in files {
            let path = root.join(&file.path);
            if !dry_run {
                if let Some(parent) = path.parent() {
                    std::fs::create_dir_all(parent).map_err(|e| {
                        Failure::run(format!("could not make {}: {e}", parent.display()))
                    })?;
                }
                std::fs::write(&path, &file.contents).map_err(|e| {
                    Failure::run(format!("could not write {}: {e}", path.display()))
                })?;
            }
            written.push(json!({
                "language": lang,
                "path": path.display().to_string(),
                "bytes": file.contents.len(),
            }));
        }
    }

    out.result(json!({ "written": written, "dry_run": dry_run }), || {
        out.say(if dry_run {
            format!(
                "{} file{} would be written:",
                written.len(),
                plural(written.len())
            )
        } else {
            format!("{} file{} written:", written.len(), plural(written.len()))
        });
        for file in &written {
            out.say(format!("  {}", file["path"].as_str().unwrap_or("")));
        }
    });
    Ok(())
}

fn plural(n: usize) -> &'static str {
    if n == 1 { "" } else { "s" }
}
