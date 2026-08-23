//! The `unitype` command.
//!
//! Everything the libraries can do, from a terminal: check a project, look at
//! what is in it, work out a type from sample data, move a value between
//! universes, write the types out in another language, and run a proposition
//! and see which proof answered.

#![forbid(unsafe_code)]

mod builtins;
mod commands;
mod project;

use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Parser, Subcommand};

/// What went wrong, and what that should mean to a shell.
///
/// Separate codes so that a script can tell "your data is wrong" from "your
/// project is wrong" from "the thing you asked for broke".
mod exit {
    /// Something was asked for that does not make sense.
    pub const USAGE: u8 = 1;
    /// A project, a type, or a value did not check out.
    pub const CHECK: u8 = 2;
    /// Something ran and failed.
    pub const RUN: u8 = 3;
}

#[derive(Parser)]
#[command(
    name = "unitype",
    version,
    about = "A universal type system: one canonical form, one address, many proofs.",
    long_about = None,
)]
struct Cli {
    /// The project to work in. Found by walking up from here if not given.
    #[arg(long, short, global = true, value_name = "PATH")]
    project: Option<PathBuf>,

    /// Print results as JSON rather than for a person.
    #[arg(long, global = true)]
    json: bool,

    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Check that the project's universes and manifests hold together.
    Check,

    /// List what is in the project.
    Ls {
        /// What to list.
        #[arg(value_enum, default_value_t = commands::ls::What::All)]
        what: commands::ls::What,
    },

    /// Show a declaration's address and the words that name it.
    Words {
        /// The declaration, as `universe::Name`.
        name: String,
    },

    /// Work out a type from sample data.
    Infer {
        /// The data, as JSON.
        #[arg(long, conflicts_with = "file")]
        data: Option<String>,
        /// A file of data, or of one JSON document per line.
        #[arg(long)]
        file: Option<PathBuf>,
        /// Read string fields as the set of values seen, rather than as strings.
        #[arg(long)]
        tags: bool,
        /// Turn the result into named declarations under this name.
        #[arg(long, value_name = "NAME")]
        lift: Option<String>,
    },

    /// Check that data is of a type.
    Validate {
        /// The type, as `universe::Name`.
        #[arg(long, value_name = "TYPE")]
        r#type: String,
        /// The data, as JSON.
        #[arg(long, conflicts_with = "file")]
        data: Option<String>,
        /// A file of data.
        #[arg(long)]
        file: Option<PathBuf>,
    },

    /// Read data as a type and write it out as a literal in some language.
    Cast {
        /// The type, as `universe::Name`.
        #[arg(long, value_name = "TYPE")]
        r#type: String,
        /// The data, as JSON.
        #[arg(long, conflicts_with = "file")]
        data: Option<String>,
        /// A file of data.
        #[arg(long)]
        file: Option<PathBuf>,
        /// The language to write it in.
        #[arg(long, default_value = "json")]
        lang: String,
    },

    /// Write the types out in another language.
    Gen {
        /// Languages to write, comma-separated.
        #[arg(long, value_delimiter = ',')]
        lang: Vec<String>,
        /// Where to write.
        #[arg(long, value_name = "DIR")]
        out: Option<PathBuf>,
        /// Say what would be written without writing it.
        #[arg(long)]
        dry_run: bool,
    },

    /// Run a proposition and see which proof answered.
    Run {
        /// The proposition, as `universe::name`.
        signature: String,
        /// The arguments, as a JSON array.
        #[arg(long, default_value = "[]")]
        args: String,
        /// How to choose among the proofs.
        #[arg(long, value_enum, default_value_t = commands::run::Choice::Default)]
        policy: commands::run::Choice,
        /// Use this proof and no other.
        #[arg(long, value_name = "ID")]
        r#impl: Option<String>,
        /// Show every proof that was considered.
        #[arg(long, short)]
        verbose: bool,
    },

    /// Move a value from one universe to another.
    Bridge {
        /// The type it is now, as `universe::Name`.
        #[arg(long, value_name = "TYPE")]
        from: String,
        /// The type to make it, as `universe::Name`.
        #[arg(long, value_name = "TYPE")]
        to: String,
        /// The value, as JSON. Without it, only the route is shown.
        #[arg(long)]
        data: Option<String>,
    },

    /// Show which declarations the project says are the same thing.
    Classes,

    /// Suggest links between universes that describe the same data.
    Suggest {
        /// How much of a match to report, from 0 to 1.
        #[arg(long, default_value_t = 0.5)]
        min: f32,
    },
}

#[tokio::main]
async fn main() -> ExitCode {
    let cli = Cli::parse();
    match run(cli).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(failure) => {
            eprintln!("{}", failure.message);
            ExitCode::from(failure.code)
        }
    }
}

/// Something that stops the command, and what the shell should make of it.
pub struct Failure {
    message: String,
    code: u8,
}

impl Failure {
    /// Something was asked for that does not make sense.
    pub fn usage(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            code: exit::USAGE,
        }
    }

    /// Something did not check out.
    pub fn check(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            code: exit::CHECK,
        }
    }

    /// Something ran and failed.
    pub fn run(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            code: exit::RUN,
        }
    }
}

async fn run(cli: Cli) -> Result<(), Failure> {
    let here = std::env::current_dir().map_err(|e| Failure::usage(e.to_string()))?;
    let path =
        project::find(cli.project.as_deref(), &here).map_err(|e| Failure::usage(e.to_string()))?;
    let project = project::Project::load(&path).map_err(|e| Failure::check(e.to_string()))?;

    let out = commands::Output::new(cli.json);

    match cli.command {
        Command::Check => commands::check::run(&project, &out),
        Command::Ls { what } => commands::ls::run(&project, what, &out),
        Command::Words { name } => commands::words::run(&project, &name, &out),
        Command::Infer {
            data,
            file,
            tags,
            lift,
        } => commands::infer::run(&project, data, file, tags, lift, &out),
        Command::Validate { r#type, data, file } => {
            commands::validate::run(&project, &r#type, data, file, &out)
        }
        Command::Cast {
            r#type,
            data,
            file,
            lang,
        } => commands::cast::run(&project, &r#type, data, file, &lang, &out),
        Command::Gen {
            lang,
            out: into,
            dry_run,
        } => commands::generate::run(&project, lang, into, dry_run, &out),
        Command::Run {
            signature,
            args,
            policy,
            r#impl,
            verbose,
        } => commands::run::run(&project, &signature, &args, policy, r#impl, verbose, &out).await,
        Command::Bridge { from, to, data } => {
            commands::bridge::run(&project, &from, &to, data, &out).await
        }
        Command::Classes => commands::classes::run(&project, &out),
        Command::Suggest { min } => commands::suggest::run(&project, min, &out),
    }
}
