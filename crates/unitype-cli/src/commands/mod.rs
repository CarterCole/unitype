//! What each command does.

pub mod bridge;
pub mod cast;
pub mod check;
pub mod classes;
pub mod conform;
pub mod generate;
pub mod infer;
pub mod ls;
pub mod reflect;
pub mod run;
pub mod suggest;
pub mod validate;
pub mod words;

use std::path::{Path, PathBuf};

use unitype_core::QualifiedName;
use unitype_core::check::CheckedWorkspace;
use unitype_core::ir::Type;
use unitype_core::value::Value;

use crate::Failure;
use crate::project::Project;

/// Where results go, and in what form.
///
/// Two audiences: a person reading a terminal, and a script reading a pipe.
/// Every command answers both rather than making the second parse the first.
pub struct Output {
    machine: bool,
}

impl Output {
    /// An output that prints for a person, or as JSON.
    pub fn new(machine: bool) -> Self {
        Self { machine }
    }

    /// Whether output is going to a script.
    pub fn is_machine(&self) -> bool {
        self.machine
    }

    /// One line, for a person. Nothing when output is for a script.
    pub fn say(&self, text: impl AsRef<str>) {
        if !self.machine {
            println!("{}", text.as_ref());
        }
    }

    /// A heading, for a person.
    pub fn heading(&self, text: impl AsRef<str>) {
        if !self.machine {
            println!("\n{}", text.as_ref());
        }
    }

    /// The result, whichever form was asked for.
    pub fn result(&self, machine: serde_json::Value, human: impl FnOnce()) {
        if self.machine {
            println!(
                "{}",
                serde_json::to_string_pretty(&machine).expect("encodes")
            );
        } else {
            human();
        }
    }
}

/// Reads a qualified name, saying what a good one looks like if it is not one.
pub fn qualified(text: &str, what: &str) -> Result<QualifiedName, Failure> {
    text.parse().map_err(|_| {
        Failure::usage(format!(
            "`{text}` is not a {what}. Write it as `universe::Name`."
        ))
    })
}

/// The type a name refers to, checked to exist.
pub fn named_type(project: &Project, text: &str) -> Result<(QualifiedName, Type), Failure> {
    let name = qualified(text, "type name")?;
    if project.workspace.def(&name).is_none() {
        return Err(Failure::check(format!(
            "`{name}` is not declared. {}",
            nearby(&project.workspace, &name)
        )));
    }
    Ok((
        name.clone(),
        Type::Ref(unitype_core::ir::TypeRef::qualified(
            name.universe,
            name.name,
        )),
    ))
}

/// A hint about what was probably meant.
pub fn nearby(ws: &CheckedWorkspace, wanted: &QualifiedName) -> String {
    let folded = wanted.name.fold();
    let close: Vec<String> = ws
        .def_names()
        .filter(|n| n.name.fold() == folded || n.name == wanted.name)
        .map(ToString::to_string)
        .collect();
    match close.as_slice() {
        [] => "Try `unitype ls types` to see what is.".to_string(),
        [one] => format!("Did you mean `{one}`?"),
        many => format!("Did you mean one of {}?", many.join(", ")),
    }
}

/// Data from wherever the caller put it.
///
/// A file of one JSON document per line is read as several samples, which is
/// the shape logs and exports usually come in.
pub fn read_data(data: Option<String>, file: Option<PathBuf>) -> Result<Vec<Value>, Failure> {
    let text = match (data, file) {
        (Some(inline), _) => inline,
        (None, Some(path)) => read_file(&path)?,
        (None, None) => {
            return Err(Failure::usage(
                "nothing to read. Pass --data '<json>' or --file <path>.",
            ));
        }
    };

    let trimmed = text.trim();
    // One document per line is how exports and logs arrive, and reading it as
    // several samples is what makes inference useful on them.
    if trimmed.lines().count() > 1
        && trimmed
            .lines()
            .all(|l| l.trim().is_empty() || l.trim_start().starts_with(['{', '[']))
        && serde_json::from_str::<serde_json::Value>(trimmed).is_err()
    {
        return trimmed
            .lines()
            .filter(|l| !l.trim().is_empty())
            .map(|line| {
                Value::parse_json(line).map_err(|e| Failure::check(format!("{}: {e}", line.trim())))
            })
            .collect();
    }

    Ok(vec![
        Value::parse_json(trimmed).map_err(|e| Failure::check(e.to_string()))?,
    ])
}

fn read_file(path: &Path) -> Result<String, Failure> {
    std::fs::read_to_string(path)
        .map_err(|e| Failure::usage(format!("could not read {}: {e}", path.display())))
}

/// One value, where exactly one was expected.
pub fn read_one(data: Option<String>, file: Option<PathBuf>) -> Result<Value, Failure> {
    let mut values = read_data(data, file)?;
    match values.len() {
        1 => Ok(values.remove(0)),
        n => Err(Failure::usage(format!(
            "this takes one value, but {n} were given."
        ))),
    }
}
