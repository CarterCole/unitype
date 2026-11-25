//! What each target language calls things.
//!
//! Every backend needs the same table: what is `i64` called here, how is a list
//! of them written, which identifiers are reserved. Written as code that table
//! is three near-identical match statements that drift apart; written as data
//! it is one thing, and adding a language is a file rather than a module.
//!
//! So each language ships as a **profile**: how the symbolic types are spelled,
//! and a small universe of the language's own builtin types — the ones the
//! symbolic layer has no exact counterpart for. `rust::u64`, `python::Decimal`,
//! `typescript::bigint` are real declarations here, addressable and linkable
//! like any others, which is what lets the awkward cases be stated rather than
//! glossed over.
//!
//! # Saying where precision goes
//!
//! The interesting content is the caveats. A 64-bit integer spelled as a
//! TypeScript `number` is exact only to 2^53; a Python `int` has no width at
//! all and will happily hold something the wire cannot carry. Those are facts
//! about a target, and a profile records them next to the spelling they qualify
//! rather than in a comment somewhere in a backend.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use crate::ident::Ident;
use crate::ir::{Bounds, Functor, Type, TypeDef, Universe};

/// The languages this build ships profiles for.
pub const LANGUAGES: &[&str] = &["rust", "typescript", "python", "haskell"];

/// How one target language spells things.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct LanguageProfile {
    /// The language's name.
    pub language: Ident,
    /// Prose describing the profile.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub doc: Option<String>,
    /// How each primitive is written, keyed by the type's kind token.
    pub scalars: BTreeMap<String, String>,
    /// How each constructor is written, with `{}` where its arguments go.
    ///
    /// `apply` is how a generic declaration is written applied to arguments:
    /// its name, then the arguments, comma-separated.
    pub constructors: BTreeMap<String, String>,
    /// Caveats, keyed by the kind token they qualify.
    #[serde(default)]
    pub notes: BTreeMap<String, String>,
    /// Identifiers the language will not let you use as names.
    #[serde(default)]
    pub reserved: BTreeSet<String>,
    /// How to rewrite a reserved identifier so it can be used, with `{}` for
    /// the original.
    #[serde(default = "default_escape")]
    pub escape: String,
    /// Constructors whose spelling is loose — an infix form with nothing round
    /// it, like `T | null` — and so needs bracketing where a template does not
    /// already bracket it. Without this, `T?` inside a list spells
    /// `string | null[]`, which is a different type.
    #[serde(default)]
    pub loose: BTreeSet<String>,
    /// How to bracket a loose spelling, with `{}` for what is bracketed.
    #[serde(default = "default_group")]
    pub group: String,
    /// Whether a generic declaration's arguments follow its name, separated by
    /// spaces — `Box Int64` — rather than being bracketed and comma-separated.
    /// Each argument that is itself loose is bracketed, as `Box (Maybe Int64)`
    /// needs.
    #[serde(default)]
    pub juxtaposed: bool,
    /// The language's own builtin types, as declarations.
    #[serde(default)]
    pub types: Vec<TypeDef>,
    /// Narrower integer spellings, for a bounded integer whose bounds fit one.
    ///
    /// `i32[0..255]` is a `u8` where there is such a thing. Tried in order, so
    /// list them narrowest first.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub integers: Vec<IntegerSpelling>,
    /// The longest list of an exact length the `array` constructor may spell,
    /// where the language has one and it stops working past some length.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub array_limit: Option<i64>,
}

/// A spelling for integers within a range.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct IntegerSpelling {
    /// The least integer the spelling holds.
    pub min: i64,
    /// The greatest.
    pub max: i64,
    /// What it is called.
    pub spelling: String,
}

fn default_escape() -> String {
    "{}_".to_string()
}

fn default_group() -> String {
    "({})".to_string()
}

/// Why a profile could not be read.
#[derive(thiserror::Error, Debug)]
#[allow(clippy::enum_variant_names)]
pub enum ProfileError {
    /// The document was not a profile.
    #[error("invalid language profile: {0}")]
    Json(#[from] serde_json::Error),
    /// Shorthand in the document could not be expanded.
    #[error(transparent)]
    Sugar(#[from] crate::ir::SugarError),
    /// No profile ships for that language.
    #[error("no profile for `{language}`; this build has {known}")]
    Unknown {
        /// The language that was asked for.
        language: String,
        /// The ones that do ship.
        known: String,
    },
}

/// The profile for one of the languages this build ships.
pub fn profile(language: &str) -> Result<LanguageProfile, ProfileError> {
    let source = match language {
        "rust" => include_str!("rust.json"),
        "typescript" => include_str!("typescript.json"),
        "python" => include_str!("python.json"),
        "haskell" => include_str!("haskell.json"),
        other => {
            return Err(ProfileError::Unknown {
                language: other.to_string(),
                known: LANGUAGES.join(", "),
            });
        }
    };
    LanguageProfile::from_json(source)
}

/// Every profile this build ships.
pub fn profiles() -> Vec<LanguageProfile> {
    LANGUAGES
        .iter()
        .map(|name| profile(name).expect("shipped profiles are valid"))
        .collect()
}

impl LanguageProfile {
    /// Reads a profile, so that a language this build has never heard of can be
    /// added by writing one.
    pub fn from_json(text: &str) -> Result<Self, ProfileError> {
        let mut json: serde_json::Value = serde_json::from_str(text)?;
        if let Some(types) = json
            .get_mut("types")
            .and_then(serde_json::Value::as_array_mut)
        {
            for def in types {
                let mut wrapper = serde_json::json!({ "types": [def.clone()] });
                crate::ir::sugar::desugar_universe(&mut wrapper)?;
                *def = wrapper["types"][0].clone();
            }
        }
        Ok(serde_json::from_value(json)?)
    }

    /// This language's builtin types as a universe, so they can be loaded into
    /// a workspace, addressed, and linked like anything else.
    pub fn universe(&self) -> Universe {
        Universe {
            name: self.language.clone(),
            doc: self.doc.clone(),
            types: self.types.clone(),
            functions: Vec::new(),
            meta: Default::default(),
        }
    }

    /// How a type is written in this language.
    ///
    /// Compositional: a list of optionals is spelled by spelling the optional
    /// and putting it through the list's template. Named types are spelled as
    /// their own name, since that is what the generated declaration will be
    /// called, and a generic one applied to arguments goes through `apply`.
    pub fn spell(&self, ty: &Type) -> String {
        self.spelling(ty).text
    }

    fn spelling(&self, ty: &Type) -> Spelling {
        match ty {
            Type::Literal { value } => Spelling::tight(self.spell_literal(value)),
            Type::Optional { of } => self.wrap("optional", &[self.spelling(of)]),
            Type::List { of } => self.wrap("list", &[self.spelling(of)]),
            Type::Future { of } => self.wrap("future", &[self.spelling(of)]),
            Type::Map { key, value } => {
                self.wrap("map", &[self.spelling(key), self.spelling(value)])
            }
            Type::Tuple { items } => {
                let inner: Vec<String> = items.iter().map(|t| self.spelling(t).text).collect();
                self.wrap("tuple", &[Spelling::tight(inner.join(", "))])
            }
            Type::Bounded { of, value, len, .. } => self.spell_bounded(of, *value, *len),
            Type::Ref(r) if r.args.is_empty() || !self.constructors.contains_key("apply") => {
                Spelling::tight(self.name(&r.name))
            }
            Type::Ref(r) => {
                let args: Vec<String> = r
                    .args
                    .iter()
                    .map(|a| {
                        let spelled = self.spelling(a);
                        if self.juxtaposed && spelled.loose {
                            self.group.replacen("{}", &spelled.text, 1)
                        } else {
                            spelled.text
                        }
                    })
                    .collect();
                let separator = if self.juxtaposed { " " } else { ", " };
                self.wrap(
                    "apply",
                    &[
                        Spelling::tight(self.name(&r.name)),
                        Spelling::tight(args.join(separator)),
                    ],
                )
            }
            Type::Param { name } => Spelling::tight(self.name(name)),
            // A higher-order parameter applied, spelled as an application of the
            // parameter to its argument. A backend that cannot express one refuses
            // the declaration before it is spelled, so this is what a reader sees
            // in a diagnostic rather than what gets generated.
            Type::Apply { name, args } => {
                let args: Vec<String> = args.iter().map(|a| self.spelling(a).text).collect();
                let separator = if self.juxtaposed { " " } else { ", " };
                self.wrap(
                    "apply",
                    &[
                        Spelling::tight(self.name(name)),
                        Spelling::tight(args.join(separator)),
                    ],
                )
            }
            Type::Constructor { functor } => Spelling::tight(
                match functor {
                    crate::ir::Functor::List => "list",
                    crate::ir::Functor::Optional => "optional",
                    crate::ir::Functor::Future => "future",
                }
                .to_string(),
            ),
            // A union or an anonymous record has no one spelling: how it is
            // written depends on the backend's choices about declarations, so
            // the backend does that part.
            Type::Union { .. } | Type::Record { .. } | Type::Function { .. } => {
                Spelling::tight(self.scalar("any"))
            }
            leaf => Spelling::tight(self.scalar(leaf.kind_name())),
        }
    }

    /// How a bounded type is written: as its type, except where the language
    /// has a narrower word for exactly that — `u8` for `i32[0..255]`, `[T; 4]`
    /// for a list of exactly four. The bounds themselves are checked at the
    /// boundary whatever is written, so the word only has to be no wider than
    /// the type it stands in for.
    fn spell_bounded(&self, of: &Type, value: Option<Bounds>, len: Option<Bounds>) -> Spelling {
        match (of, value, len) {
            (
                Type::I32 | Type::I64,
                Some(Bounds {
                    min: Some(lo),
                    max: Some(hi),
                }),
                _,
            ) => match self.integers.iter().find(|i| i.min <= lo && hi <= i.max) {
                Some(narrow) => Spelling::tight(narrow.spelling.clone()),
                None => self.spelling(of),
            },
            (
                Type::List { of: item },
                _,
                Some(Bounds {
                    min: Some(lo),
                    max: Some(hi),
                }),
            ) if lo == hi
                && self.constructors.contains_key("array")
                && self.array_limit.is_none_or(|limit| hi <= limit) =>
            {
                self.wrap(
                    "array",
                    &[self.spelling(item), Spelling::tight(lo.to_string())],
                )
            }
            _ => self.spelling(of),
        }
    }

    /// How a literal type is written, which is only sometimes possible.
    fn spell_literal(&self, value: &crate::ir::LiteralValue) -> String {
        use crate::ir::LiteralValue;
        // A language with singleton types writes the value; one without falls
        // back to the base type, and the discriminating is done at runtime.
        match (self.language.as_str(), value) {
            ("typescript", LiteralValue::Str(s)) => format!("{s:?}"),
            ("typescript", LiteralValue::Int(i)) => i.to_string(),
            ("typescript", LiteralValue::Bool(b)) => b.to_string(),
            ("python", LiteralValue::Str(s)) => format!("typing.Literal[{s:?}]"),
            ("python", LiteralValue::Int(i)) => format!("typing.Literal[{i}]"),
            (_, other) => self.spell(&other.base()),
        }
    }

    /// The spelling of a primitive, or a plain fallback if the profile is
    /// silent about it.
    fn scalar(&self, kind: &str) -> String {
        self.scalars
            .get(kind)
            .cloned()
            .unwrap_or_else(|| kind.to_string())
    }

    /// Fills a constructor's template with its arguments, bracketing any that
    /// would otherwise be read as part of what surrounds it.
    fn wrap(&self, kind: &str, args: &[Spelling]) -> Spelling {
        let Some(template) = self.constructors.get(kind) else {
            return Spelling::tight(
                args.iter()
                    .map(|a| a.text.clone())
                    .collect::<Vec<_>>()
                    .join(", "),
            );
        };

        let mut out = String::with_capacity(template.len() + 16);
        let mut rest = template.as_str();
        let mut args = args.iter();
        while let Some(at) = rest.find("{}") {
            let before = rest[..at].trim_end().chars().last();
            let after = rest[at + 2..].trim_start().chars().next();
            let bracketed = matches!(before, Some('<' | '(' | '[' | '{' | ','))
                && matches!(after, Some('>' | ')' | ']' | '}' | ','));

            out.push_str(&rest[..at]);
            match args.next() {
                Some(arg) if arg.loose && !bracketed => {
                    out.push_str(&self.group.replacen("{}", &arg.text, 1));
                }
                Some(arg) => out.push_str(&arg.text),
                None => {}
            }
            rest = &rest[at + 2..];
        }
        out.push_str(rest);

        Spelling {
            text: out,
            loose: self.loose.contains(kind),
        }
    }

    /// An identifier as this language will accept it.
    pub fn name(&self, ident: &Ident) -> String {
        if self.reserved.contains(ident.as_str()) {
            self.escape.replacen("{}", ident.as_str(), 1)
        } else {
            ident.to_string()
        }
    }

    /// What is worth knowing about how this language holds a type.
    ///
    /// Returns the caveats gathered from the type and everything inside it, so
    /// that a `[i64]` in TypeScript reports what a bare `i64` would.
    ///
    /// Bounds are reported too, since the generated type is wider than the
    /// declared one and a reader should know by how much.
    pub fn caveats(&self, ty: &Type) -> Vec<String> {
        let mut out = Vec::new();
        self.gather(ty, &mut out);
        out.dedup();
        out
    }

    fn gather(&self, ty: &Type, out: &mut Vec<String>) {
        let kind = match Functor::of(ty) {
            Some(Functor::List) => "list",
            Some(Functor::Optional) => "optional",
            Some(Functor::Future) => "future",
            None => ty.kind_name(),
        };
        if let Some(note) = self.notes.get(kind) {
            out.push(note.clone());
        }
        if let Type::Bounded {
            of,
            value,
            len,
            size,
            pattern,
            wait,
        } = ty
        {
            out.push(describe_bounds(of, value, len, size, pattern, wait));
        }
        for child in ty.children() {
            self.gather(child, out);
        }
    }
}

/// What a type's bounds ask for, in a sentence: "between 0 and 255", "exactly
/// 4 items", "at most 63 scalar values and at most 253 bytes".
fn describe_bounds(
    of: &Type,
    value: &Option<Bounds>,
    len: &Option<Bounds>,
    size: &Option<Bounds>,
    pattern: &Option<String>,
    wait: &Option<Bounds>,
) -> String {
    let counted = match of {
        Type::List { .. } => "items",
        Type::Map { .. } => "entries",
        Type::Bytes => "bytes",
        Type::String => "scalar values",
        _ => "",
    };
    let range = |bounds: &Bounds, unit: &str| {
        let unit = if unit.is_empty() {
            String::new()
        } else {
            format!(" {unit}")
        };
        match (bounds.min, bounds.max) {
            (Some(a), Some(z)) if a == z => format!("exactly {a}{unit}"),
            (Some(a), Some(z)) => format!("between {a} and {z}{unit}"),
            (Some(a), None) => format!("at least {a}{unit}"),
            (None, Some(z)) => format!("at most {z}{unit}"),
            (None, None) => String::new(),
        }
    };
    let mut parts = Vec::new();
    if let Some(bounds) = value {
        parts.push(range(bounds, ""));
    }
    if let Some(bounds) = len {
        parts.push(range(bounds, counted));
    }
    if let Some(bounds) = size {
        parts.push(range(bounds, "bytes"));
    }
    if let Some(pattern) = pattern {
        parts.push(format!("matching {pattern:?}"));
    }
    if let Some(bounds) = wait {
        parts.push(format!("delivered {} ms after the call", range(bounds, "")));
    }
    parts.retain(|p| !p.is_empty());
    parts.join(" and ")
}

/// A spelling, and whether it needs bracketing where something follows it.
struct Spelling {
    text: String,
    loose: bool,
}

impl Spelling {
    /// A spelling that reads the same wherever it is put.
    fn tight(text: String) -> Self {
        Self { text, loose: false }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir::TypeRef;

    fn rust() -> LanguageProfile {
        profile("rust").expect("ships")
    }

    fn ts() -> LanguageProfile {
        profile("typescript").expect("ships")
    }

    fn py() -> LanguageProfile {
        profile("python").expect("ships")
    }

    fn hs() -> LanguageProfile {
        profile("haskell").expect("ships")
    }

    fn id(s: &str) -> Ident {
        Ident::new(s).unwrap()
    }

    #[test]
    fn a_bounded_integer_gets_the_narrowest_word_a_language_has_for_it() {
        let octet = Type::I32.valued(Bounds::between(0, 255));
        assert_eq!(rust().spell(&octet), "u8");
        assert_eq!(
            rust().spell(&Type::I32.valued(Bounds::between(0, 65535))),
            "u16"
        );
        assert_eq!(
            rust().spell(&Type::I64.valued(Bounds::between(0, 4294967295))),
            "u32"
        );
        assert_eq!(
            rust().spell(&Type::I32.valued(Bounds::between(-1, 5))),
            "i32",
            "a negative end is no unsigned type"
        );
        assert_eq!(
            rust().spell(&Type::I32.valued(Bounds::at_most(5))),
            "i32",
            "an open end fits nothing narrower"
        );
        assert_eq!(ts().spell(&octet), "number");
        assert_eq!(py().spell(&octet), "int");
    }

    #[test]
    fn a_list_of_an_exact_length_is_an_array_where_the_language_has_one() {
        let four = Type::I32
            .valued(Bounds::between(0, 255))
            .list()
            .counted(Bounds::exactly(4));
        assert_eq!(rust().spell(&four), "[u8; 4]");
        assert_eq!(
            rust().spell(&Type::String.list().counted(Bounds::exactly(40))),
            "Vec<String>",
            "past what serde's arrays reach"
        );
        assert_eq!(
            rust().spell(&Type::String.list().counted(Bounds::between(1, 8))),
            "Vec<String>"
        );
        assert_eq!(ts().spell(&four), "number[]");
        assert_eq!(py().spell(&four), "list[int]");
    }

    #[test]
    fn caveats_say_what_the_bounds_ask_for() {
        let addr = Type::I32
            .valued(Bounds::between(0, 255))
            .list()
            .counted(Bounds::exactly(4));
        let said = ts().caveats(&addr);
        assert!(said.iter().any(|c| c == "exactly 4 items"), "{said:?}");
        assert!(said.iter().any(|c| c == "between 0 and 255"), "{said:?}");

        let label = Type::String
            .counted(Bounds::between(1, 63))
            .sized(Bounds::at_most(253));
        let said = rust().caveats(&label);
        assert!(
            said.iter()
                .any(|c| c == "between 1 and 63 scalar values and at most 253 bytes"),
            "{said:?}"
        );
    }

    #[test]
    fn every_shipped_profile_reads() {
        assert_eq!(profiles().len(), LANGUAGES.len());
        for p in profiles() {
            assert!(!p.scalars.is_empty(), "{} has no spellings", p.language);
            assert!(!p.reserved.is_empty(), "{} reserves nothing", p.language);
        }
    }

    #[test]
    fn a_language_this_build_never_heard_of_is_reported_not_guessed() {
        assert!(matches!(
            profile("cobol"),
            Err(ProfileError::Unknown { .. })
        ));
    }

    #[test]
    fn primitives_are_spelled_the_way_each_language_spells_them() {
        assert_eq!(rust().spell(&Type::String), "String");
        assert_eq!(ts().spell(&Type::String), "string");
        assert_eq!(py().spell(&Type::String), "str");
        assert_eq!(hs().spell(&Type::String), "Text");

        assert_eq!(rust().spell(&Type::I64), "i64");
        assert_eq!(ts().spell(&Type::I64), "number");
        assert_eq!(py().spell(&Type::I64), "int");
        assert_eq!(hs().spell(&Type::I64), "Int64");
    }

    #[test]
    fn constructors_compose_rather_than_being_enumerated() {
        let nested = Type::String.optional().list();
        assert_eq!(rust().spell(&nested), "Vec<Option<String>>");
        assert_eq!(py().spell(&nested), "list[str | None]");
        assert_eq!(
            ts().spell(&nested),
            "(string | null)[]",
            "without the brackets this would say `string | null[]`, which is a \
             different type"
        );
        assert_eq!(hs().spell(&nested), "[Maybe Text]");
    }

    #[test]
    fn an_application_is_bracketed_where_it_is_the_argument_of_another() {
        // `Maybe Map Text Int64` would be a different type, and a kind error.
        let counts = Type::I64.map_from(Type::String);
        assert_eq!(hs().spell(&counts), "Map Text Int64");
        assert_eq!(
            hs().spell(&counts.clone().optional()),
            "Maybe (Map Text Int64)"
        );
        assert_eq!(
            hs().spell(&Type::I64.optional().map_from(Type::String)),
            "Map Text (Maybe Int64)"
        );
        assert_eq!(hs().spell(&counts.list()), "[Map Text Int64]");
        assert_eq!(
            hs().spell(&Type::String.optional().future()),
            "IO (Maybe Text)"
        );
    }

    #[test]
    fn a_loose_spelling_is_left_alone_where_something_already_brackets_it() {
        // `list[...]` already delimits, so bracketing again would be noise.
        assert_eq!(
            py().spell(&Type::String.optional().list()),
            "list[str | None]"
        );
        assert_eq!(
            ts().spell(&Type::String.optional().map_from(Type::String)),
            "Record<string, string | null>"
        );
    }

    #[test]
    fn a_generic_declaration_is_spelled_with_its_arguments() {
        let boxed = Type::Ref(crate::ir::TypeRef {
            universe: None,
            name: Ident::new("Box").unwrap(),
            args: vec![
                Type::String.optional(),
                Type::Param {
                    name: Ident::new("T").unwrap(),
                },
            ],
        });
        assert_eq!(rust().spell(&boxed), "Box<Option<String>, T>");
        assert_eq!(ts().spell(&boxed), "Box<string | null, T>");
        assert_eq!(py().spell(&boxed.list()), "list[Box[str | None, T]]");
    }

    #[test]
    fn a_juxtaposed_application_follows_its_name_and_brackets_what_is_loose() {
        let apply = |args: Vec<Type>| {
            Type::Ref(crate::ir::TypeRef {
                universe: None,
                name: Ident::new("Box").unwrap(),
                args,
            })
        };
        let t = Type::Param {
            name: Ident::new("t").unwrap(),
        };
        assert_eq!(
            hs().spell(&apply(vec![Type::String.optional(), t.clone()])),
            "Box (Maybe Text) t"
        );
        // An application inside an application is bracketed too, and a list
        // already is.
        assert_eq!(
            hs().spell(&apply(vec![apply(vec![t.clone()]), Type::I64.list()])),
            "Box (Box t) [Int64]"
        );
        assert_eq!(
            hs().spell(&apply(vec![t.clone()]).optional()),
            "Maybe (Box t)"
        );
        assert!(!hs().spell(&apply(vec![t])).contains(','));
    }

    #[test]
    fn a_map_fills_both_halves_of_its_template() {
        let counts = Type::I64.map_from(Type::String);
        assert_eq!(rust().spell(&counts), "BTreeMap<String, i64>");
        assert_eq!(ts().spell(&counts), "Record<string, number>");
        assert_eq!(py().spell(&counts), "dict[str, int]");
    }

    #[test]
    fn a_promise_is_spelled_the_way_each_language_waits() {
        let later = Type::Bool.future();
        assert_eq!(ts().spell(&later), "Promise<boolean>");
        assert_eq!(py().spell(&later), "typing.Awaitable[bool]");
        assert!(rust().spell(&later).contains("Future"));
        assert_eq!(hs().spell(&later), "IO Bool");
    }

    #[test]
    fn a_named_type_is_spelled_as_its_name() {
        let user = Type::Ref(TypeRef::qualified(id("app"), id("User")));
        for p in profiles() {
            assert_eq!(p.spell(&user), "User", "in {}", p.language);
        }
    }

    #[test]
    fn a_reserved_word_is_rewritten_rather_than_emitted() {
        assert_eq!(rust().name(&id("type")), "r#type");
        assert_eq!(py().name(&id("class")), "class_");
        assert_eq!(ts().name(&id("function")), "function_");
        assert_eq!(rust().name(&id("radius")), "radius");
        assert_eq!(hs().name(&id("where")), "where_");
        // Names the generated code imports are reserved too, or a universe's own
        // `Map` would be taken for the one it imports.
        assert_eq!(hs().name(&id("Map")), "Map_");
        assert_eq!(hs().name(&id("radius")), "radius");
    }

    #[test]
    fn where_precision_goes_is_recorded_next_to_the_spelling() {
        let ts = ts();
        let caveats = ts.caveats(&Type::I64);
        assert!(
            caveats.iter().any(|c| c.contains("2^53")),
            "a 64-bit integer in TypeScript deserves a warning: {caveats:?}"
        );
        let rust = rust();
        assert!(
            rust.caveats(&Type::I64).is_empty(),
            "Rust holds it exactly, so there is nothing to say"
        );
    }

    #[test]
    fn a_caveat_inside_a_container_still_surfaces() {
        let ts = ts();
        let caveats = ts.caveats(&Type::I64.list().optional());
        assert!(
            caveats.iter().any(|c| c.contains("2^53")),
            "the problem does not go away because it is in a list: {caveats:?}"
        );
    }

    #[test]
    fn a_language_declares_the_types_the_symbolic_layer_has_no_word_for() {
        let rust = rust().universe();
        let names: Vec<String> = rust.types.iter().map(|d| d.name().to_string()).collect();
        assert!(names.contains(&"u64".to_string()), "{names:?}");

        // And they are ordinary declarations, so they load, check, and get
        // addresses like anything else.
        let ws = crate::check::check_workspace(crate::Workspace::of(
            profiles().iter().map(LanguageProfile::universe),
        ))
        .expect("the language universes check");
        assert!(
            ws.shape_hash(&"rust::u64".parse().unwrap()).is_some(),
            "a language's own types are addressable too"
        );
    }

    #[test]
    fn an_awkward_type_says_how_it_travels() {
        let rust = rust().universe();
        let u64 = rust.type_def(&id("u64")).expect("declared");
        assert!(
            u64.doc().unwrap().contains("as text"),
            "the honest answer is that it does not fit, and this should say so"
        );
    }

    #[test]
    fn a_profile_can_be_written_for_a_language_this_build_does_not_ship() {
        let profile = LanguageProfile::from_json(
            r#"{"language":"go",
                "scalars":{"i64":"int64","string":"string","bool":"bool"},
                "constructors":{"list":"[]{}","map":"map[{}]{}"},
                "reserved":["func","type","range"],
                "escape":"{}_",
                "types":[{"k":"alias","name":"rune","target":"str"}]}"#,
        )
        .expect("reads");

        assert_eq!(profile.spell(&Type::I64.list()), "[]int64");
        assert_eq!(
            profile.spell(&Type::String.map_from(Type::String)),
            "map[string]string"
        );
        assert_eq!(profile.name(&id("range")), "range_");
    }
}
