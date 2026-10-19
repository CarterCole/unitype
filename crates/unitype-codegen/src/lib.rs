//! Writing the symbolic types out in a real language.
//!
//! A backend takes a checked workspace and produces files: type declarations
//! you can import, function skeletons to fill in, and a harness that makes a
//! filled-in file reachable as a proof without anyone writing glue.
//!
//! # What the backends share and what they do not
//!
//! How a type is *spelled* is data — it lives in that language's profile, along
//! with its reserved words and its caveats. What differs between backends is
//! how a *declaration* is written, because a TypeScript interface, a Python
//! dataclass, and a Rust struct genuinely are three different things. So the
//! spelling is shared and the declaring is not, which is roughly where the real
//! difference between languages sits.
//!
//! # Generated code has to be boring
//!
//! Output is deterministic: sorted, stably ordered, no timestamps, no host
//! names, no counters. Generating twice from an unchanged workspace produces
//! byte-identical files. Generated code gets committed and diffed, and a file
//! that appears to change when nothing did makes every diff worthless.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod literal;
pub mod python;
pub mod rust;
pub mod typescript;
pub mod writer;

use std::path::PathBuf;

use unitype_core::check::CheckedWorkspace;
use unitype_core::ir::{Expr, Type, TypeDef, TypeRef, Universe};
use unitype_core::lang::ProfileError;
use unitype_core::{Ident, QualifiedName};

pub use writer::CodeWriter;

/// A file a backend produced.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GeneratedFile {
    /// Where it goes, relative to wherever generation was pointed.
    pub path: PathBuf,
    /// What is in it.
    pub contents: String,
}

/// What to generate.
#[derive(Clone, Debug)]
pub struct GenOptions {
    /// Only these universes, or all of them.
    pub universes: Option<Vec<Ident>>,
    /// Emit skeletons for signatures nothing proves yet.
    pub stubs: bool,
    /// Emit the harness that makes a filled-in file reachable as a proof.
    pub harness: bool,
    /// Put a header on each file saying where it came from.
    pub header: bool,
}

impl Default for GenOptions {
    fn default() -> Self {
        Self {
            universes: None,
            stubs: true,
            harness: true,
            header: true,
        }
    }
}

/// Why generation failed.
#[derive(thiserror::Error, Debug)]
pub enum GenError {
    /// No profile ships for that language.
    #[error(transparent)]
    Profile(#[from] ProfileError),
    /// The workspace asked for something this backend cannot write.
    #[error("{language} cannot express {what}: {detail}")]
    Unsupported {
        /// The target language.
        language: String,
        /// What could not be written.
        what: String,
        /// Why not.
        detail: String,
    },
}

/// A target language this can generate for.
pub trait Backend {
    /// What this backend is called on a command line.
    fn id(&self) -> &'static str;

    /// The file extension it produces.
    fn extension(&self) -> &'static str;

    /// Writes out a whole workspace.
    fn generate(
        &self,
        ws: &CheckedWorkspace,
        options: &GenOptions,
    ) -> Result<Vec<GeneratedFile>, GenError>;
}

/// The backend for a language, if this build has one.
pub fn backend(language: &str) -> Option<Box<dyn Backend>> {
    match language {
        "typescript" | "ts" => Some(Box::new(typescript::TypeScript)),
        "python" | "py" => Some(Box::new(python::Python)),
        "rust" | "rs" => Some(Box::new(rust::Rust)),
        _ => None,
    }
}

/// How a language builds the things a typed literal needs.
///
/// Kept beside the backends because it is the same knowledge: what a struct
/// looks like when you make one, and what a variant is called.
pub fn constructors(language: &str) -> Option<literal::Constructors> {
    match language {
        "typescript" | "ts" => Some(literal::Constructors {
            record: |_name, fields| {
                // TypeScript has no constructor to call: an interface is
                // satisfied by an object literal.
                let parts: Vec<String> = fields
                    .iter()
                    .map(|(name, value)| format!("{name}: {value}"))
                    .collect();
                format!("{{ {} }}", parts.join(", "))
            },
            variant: |_name, tag, payload| match payload {
                Some(value) => format!("{{ $tag: {tag:?}, value: {value} }}"),
                None => format!("{{ $tag: {tag:?} }}"),
            },
            plain: typescript::literal,
        }),
        "python" | "py" => Some(literal::Constructors {
            record: |name, fields| {
                let parts: Vec<String> = fields
                    .iter()
                    .map(|(field, value)| format!("{field}={value}"))
                    .collect();
                format!("{name}({})", parts.join(", "))
            },
            // An enum is a TypedDict here, so a variant is written the way it
            // arrives rather than constructed.
            variant: |_name, tag, payload| match payload {
                Some(value) => format!("{{\"$tag\": {tag:?}, \"value\": {value}}}"),
                None => format!("{{\"$tag\": {tag:?}}}"),
            },
            plain: python::literal,
        }),
        "rust" | "rs" => Some(literal::Constructors {
            record: |name, fields| {
                let parts: Vec<String> = fields
                    .iter()
                    .map(|(field, value)| format!("{field}: {value}"))
                    .collect();
                format!("{name} {{ {} }}", parts.join(", "))
            },
            variant: |name, tag, payload| match payload {
                Some(value) => format!("{name}::{tag}({value})"),
                None => format!("{name}::{tag}"),
            },
            plain: rust::literal,
        }),
        _ => None,
    }
}

/// Every backend this build ships.
pub fn backends() -> Vec<Box<dyn Backend>> {
    ["typescript", "python", "rust"]
        .into_iter()
        .filter_map(backend)
        .collect()
}

/// The universes to write out, in a settled order.
pub(crate) fn selected<'a>(ws: &'a CheckedWorkspace, options: &GenOptions) -> Vec<&'a Universe> {
    let mut chosen: Vec<&Universe> = ws
        .workspace()
        .universes
        .iter()
        .filter(|u| {
            options
                .universes
                .as_ref()
                .is_none_or(|wanted| wanted.contains(&u.name))
        })
        .collect();
    chosen.sort_by(|a, b| a.name.cmp(&b.name));
    chosen
}

/// Whether any body in a universe contains an expression `wanted` picks out.
///
/// A backend that needs a helper or an import for some form asks this, so a
/// module that never uses the form does not carry what it would have needed.
pub(crate) fn bodies_use(universe: &Universe, wanted: impl Fn(&Expr) -> bool) -> bool {
    universe.functions.iter().any(|sig| {
        let mut found = false;
        if let Some(body) = &sig.body {
            body.walk(&mut |e| found |= wanted(e));
        }
        found
    })
}

/// Declarations ordered so that nothing is mentioned before it exists.
///
/// Python needs this — a class cannot inherit from one defined later in the
/// file — and it costs the other targets nothing. Ties are broken by name so
/// that two runs agree.
pub(crate) fn ordered_defs(universe: &Universe) -> Vec<&TypeDef> {
    let mut pending: Vec<&TypeDef> = universe.types.iter().collect();
    pending.sort_by(|a, b| a.name().cmp(b.name()));

    let mut placed: Vec<Ident> = Vec::new();
    let mut out: Vec<&TypeDef> = Vec::with_capacity(pending.len());

    while !pending.is_empty() {
        let (ready, waiting): (Vec<&TypeDef>, Vec<&TypeDef>) =
            pending.into_iter().partition(|def| {
                requires(universe, def)
                    .iter()
                    .all(|needed| placed.contains(needed))
            });

        // The checker has already refused inheritance cycles, so this cannot
        // happen for a workspace that came through the front door. If something
        // gets here anyway, emitting it is better than looping.
        if ready.is_empty() {
            out.extend(waiting);
            break;
        }

        placed.extend(ready.iter().map(|d| d.name().clone()));
        out.extend(ready);
        pending = waiting;
    }
    out
}

/// What a declaration is built out of, within its own universe.
///
/// Only what it *extends* or, for an alias, what it stands for. A field merely
/// mentioning another type does not constrain the order in any target here.
fn requires(universe: &Universe, def: &TypeDef) -> Vec<Ident> {
    let mut out = Vec::new();
    let mut take = |r: &TypeRef| {
        if r.universe.as_ref() == Some(&universe.name) {
            out.push(r.name.clone());
        }
    };
    match def {
        TypeDef::Struct(d) => {
            d.extends.iter().for_each(&mut take);
            d.implements.iter().for_each(&mut take);
        }
        TypeDef::Interface(d) => d.extends.iter().for_each(&mut take),
        TypeDef::Alias(d) => refs_in(&d.target, &mut take),
        TypeDef::Enum(_) => {}
    }
    out
}

/// Every reference a type makes, at any depth.
fn refs_in(ty: &Type, f: &mut impl FnMut(&TypeRef)) {
    if let Type::Ref(r) = ty {
        f(r);
    }
    for child in ty.children() {
        refs_in(child, f);
    }
}

/// The header put at the top of a generated file.
///
/// Names the universe and its structural address, so that a file can be traced
/// back to what it was generated from — and so that regenerating an unchanged
/// universe produces an unchanged header.
pub(crate) fn header(comment: &str, universe: &Universe, ws: &CheckedWorkspace) -> Vec<String> {
    let mut addresses: Vec<String> = universe
        .types
        .iter()
        .filter_map(|d| {
            let name = QualifiedName::new(universe.name.clone(), d.name().clone());
            Some(format!("{}: {}", d.name(), ws.shape_hash(&name)?.short()))
        })
        .collect();
    addresses.sort();

    let mut out = vec![
        format!(
            "{comment} Generated by unitype from universe `{}`.",
            universe.name
        ),
        format!("{comment} Do not edit; edit the universe and generate again."),
    ];
    if !addresses.is_empty() {
        out.push(comment.trim_end().to_string());
        out.push(format!("{comment} Structural addresses:"));
        out.extend(addresses.iter().map(|a| format!("{comment}   {a}")));
    }
    out
}
