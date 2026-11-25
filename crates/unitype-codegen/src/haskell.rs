//! Writing a universe out as Haskell.
//!
//! Structs become records and enums become sums, each with a `ToJson` and a
//! `FromJson` instance that read and write exactly the wire form, so a value
//! built from the generated types is the value unitype says it is.
//!
//! # No JSON library
//!
//! GHC ships none, and the usual one is a build of its own dependency tree. So
//! the instances are written against a small module generated beside the
//! universes, `UnitypeSupport`, the way Rust's shared `Bytes` is: it reads and
//! writes JSON and base64 using only what comes with GHC — `base`,
//! `bytestring`, `containers`, `text`. A generated universe compiles with a
//! bare `ghc`, with no package database and no network.
//!
//! # Names
//!
//! Haskell puts every field, constructor and function in a module into one
//! space, where the other targets give each struct its own. Two structs that
//! both have an `id` cannot both have a field called `id`, and an inherited
//! one is carried by the child too. So a field is named after the type that
//! holds it — `circleId`, `circleRadius` — which is also what a reserved word
//! cannot be, and a variant after its enum — `SizeLarge`. The wire keeps its
//! own names, written into the instances. If two declarations would still come
//! out as one name, generation says so rather than choosing.
//!
//! # Inheritance
//!
//! Haskell has none, so a child carries every field it has, inherited or not,
//! as Rust does. An interface becomes a class of accessors, and a struct that
//! claims one gets an instance, so a bound on a generic parameter is a class
//! constraint.
//!
//! # Bodies
//!
//! A body is written with the Prelude qualified, so a parameter called `length`
//! or `map` cannot capture what the body means by them; nothing from the
//! Prelude is imported by name but the types and the operators. The checker
//! takes a `T` wherever a `T?` is wanted and Haskell does not, so a body is
//! written knowing what each part produces and what the place it stands in
//! wants, which is `Just` where a value that is there goes into one that may
//! not be.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use unitype_core::check::CheckedWorkspace;
use unitype_core::ir::{
    AliasDef, EnumDef, Expr, Field, FunctionSig, InterfaceDef, LiteralValue, PrimOp, StructDef,
    Type, TypeDef, TypeParamDecl, TypeRef, Universe,
};
use unitype_core::lang::{LanguageProfile, profile};
use unitype_core::value::Value;
use unitype_core::{Ident, QualifiedName};

use crate::writer::CodeWriter;
use crate::{
    Backend, GenError, GenOptions, GeneratedFile, header, ordered_defs, refs_in, selected,
};

/// The Haskell backend.
pub struct Haskell;

/// The module every universe's instances are written against.
const SUPPORT: &str = "UnitypeSupport";

/// The module that makes a filled-in file reachable as a proof.
const HARNESS: &str = "UnitypeHarness";

/// The largest tuple the support module can read and write.
const TUPLE_LIMIT: usize = 7;

impl Backend for Haskell {
    fn id(&self) -> &'static str {
        "haskell"
    }

    fn extension(&self) -> &'static str {
        "hs"
    }

    fn generate(
        &self,
        ws: &CheckedWorkspace,
        options: &GenOptions,
    ) -> Result<Vec<GeneratedFile>, GenError> {
        let profile = profile("haskell")?;
        crate::refuse_higher_order(ws, options, "haskell")?;
        let chosen = selected(ws, options);

        let mut modules: BTreeMap<String, &Ident> = BTreeMap::new();
        for universe in &chosen {
            let module = module_name(&universe.name);
            if module == SUPPORT || module == HARNESS {
                return Err(unsupported(
                    format!("a universe named `{}`", universe.name),
                    format!("that is the module `{module}`, which is written beside the universes"),
                ));
            }
            if let Some(other) = modules.insert(module.clone(), &universe.name) {
                return Err(unsupported(
                    format!("universes named `{other}` and `{}`", universe.name),
                    format!("both would be the module `{module}`"),
                ));
            }
        }

        check_cycles(ws, &chosen, options)?;

        let mut files = Vec::new();
        for universe in &chosen {
            files.push(GeneratedFile {
                path: format!("haskell/{}.hs", module_name(&universe.name)).into(),
                contents: universe_file(ws, universe, &profile, options)?,
            });
        }
        files.push(GeneratedFile {
            path: format!("haskell/{SUPPORT}.hs").into(),
            contents: SUPPORT_MODULE.to_string(),
        });
        if options.harness {
            files.push(GeneratedFile {
                path: format!("haskell/{HARNESS}.hs").into(),
                contents: HARNESS_MODULE.to_string(),
            });
        }
        Ok(files)
    }
}

/// What every generated universe is written against.
const SUPPORT_MODULE: &str = include_str!("haskell/UnitypeSupport.hs");

/// The harness that makes a filled-in module reachable as a proof.
const HARNESS_MODULE: &str = include_str!("haskell/UnitypeHarness.hs");

fn unsupported(what: String, detail: String) -> GenError {
    GenError::Unsupported {
        language: "haskell".into(),
        what,
        detail,
    }
}

// ---------------------------------------------------------------------------
// Names

fn ident(text: &str) -> Ident {
    Ident::new(text).expect("a name built out of identifiers is one")
}

fn upper_first(text: &str) -> String {
    let mut chars = text.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => String::new(),
    }
}

fn lower_first(text: &str) -> String {
    let mut chars = text.chars();
    match chars.next() {
        Some(first) => first.to_lowercase().chain(chars).collect(),
        None => String::new(),
    }
}

/// The module a universe is written to: its name, beginning with a capital.
fn module_name(universe: &Ident) -> String {
    let name = upper_first(universe.as_str());
    if name.starts_with(|c: char| c.is_ascii_uppercase()) {
        name
    } else {
        format!("U{name}")
    }
}

/// What a type is called. Haskell's begin with a capital.
fn type_name(profile: &LanguageProfile, name: &Ident) -> String {
    let named = upper_first(name.as_str());
    let named = if named.starts_with(|c: char| c.is_ascii_uppercase()) {
        named
    } else {
        format!("U{named}")
    };
    profile.name(&ident(&named))
}

/// What one alternative of an enum is called: after the enum, since a
/// constructor is named in a space shared with every other in the module.
fn variant_name(profile: &LanguageProfile, owner: &Ident, tag: &str) -> String {
    let owner = type_name(profile, owner);
    profile.name(&ident(&format!("{owner}{}", upper_first(tag))))
}

/// What a field is called: after the type that holds it.
fn field_name(profile: &LanguageProfile, owner: &Ident, field: &Ident) -> String {
    let owner = lower_first(&type_name(profile, owner));
    profile.name(&ident(&format!("{owner}{}", upper_first(field.as_str()))))
}

/// What a generic parameter is called. Haskell's begin with a lower-case letter.
fn tyvar(profile: &LanguageProfile, name: &Ident) -> String {
    let named = lower_first(name.as_str());
    let named = if named.starts_with('_') {
        format!("t{named}")
    } else {
        named
    };
    profile.name(&ident(&named))
}

/// What a function, a parameter or a local binding is called.
fn var(profile: &LanguageProfile, name: &Ident) -> String {
    profile.name(&ident(&lower_first(name.as_str())))
}

/// A name nothing else in the same declaration has taken, numbered if it has to
/// be.
fn distinct(name: String, taken: &mut BTreeSet<String>) -> String {
    let mut candidate = name.clone();
    let mut n = 2;
    while !taken.insert(candidate.clone()) {
        candidate = format!("{name}{n}");
        n += 1;
    }
    candidate
}

/// A declaration's parameters, as Haskell names them.
fn tyvars(profile: &LanguageProfile, params: &[TypeParamDecl]) -> Vec<String> {
    params.iter().map(|p| tyvar(profile, &p.name)).collect()
}

/// A declaration's head: its name, then its parameters.
fn head(name: &str, vars: &[String]) -> String {
    if vars.is_empty() {
        name.to_string()
    } else {
        format!("{name} {}", vars.join(" "))
    }
}

/// A declaration's head, bracketed to stand as an instance's subject.
fn subject(name: &str, vars: &[String]) -> String {
    if vars.is_empty() {
        name.to_string()
    } else {
        format!("({})", head(name, vars))
    }
}

// ---------------------------------------------------------------------------
// Spelling

/// How Haskell writes a type.
fn spell(profile: &LanguageProfile, ty: &Type) -> String {
    profile.spell(&named(profile, ty))
}

/// A type with every name it mentions as Haskell has it, so that the profile
/// spells it rather than the universe's own.
fn named(profile: &LanguageProfile, ty: &Type) -> Type {
    let kids: Vec<Type> = ty
        .children()
        .into_iter()
        .map(|c| named(profile, c))
        .collect();
    match ty.with_children(kids) {
        Type::Ref(mut r) => {
            r.name = ident(&type_name(profile, &r.name));
            Type::Ref(r)
        }
        Type::Param { name } => Type::Param {
            name: ident(&tyvar(profile, &name)),
        },
        Type::Map { key, value } => Type::Map {
            key: Box::new(map_key(*key)),
            value,
        },
        other => other,
    }
}

/// What the keys of a map are held as.
///
/// A key may be a literal, or a union of them, and Haskell has no type that is
/// only some strings. It is held as the kind of key it is made of, which is
/// also what it is on the wire: the text of a property name.
fn map_key(key: Type) -> Type {
    match key {
        Type::Literal { value } => value.base(),
        Type::Union { members } => {
            let held: Vec<Type> = members.into_iter().map(map_key).collect();
            if held.iter().all(|k| matches!(k, Type::I32 | Type::I64)) {
                Type::I64
            } else {
                Type::String
            }
        }
        other => other,
    }
}

/// Whether Haskell holds a type as parsed JSON, for want of a name for it.
fn is_json(ty: &Type) -> bool {
    matches!(
        ty,
        Type::Any | Type::Union { .. } | Type::Record { .. } | Type::Function { .. }
    )
}

fn is_string_literal(ty: &Type) -> bool {
    matches!(
        ty,
        Type::Literal {
            value: LiteralValue::Str(_)
        }
    )
}

/// An alternative of an untagged union, as the sum type written for it has it.
struct Member {
    /// The constructor.
    ctor: String,
    /// What it carries.
    ty: Type,
    /// The text it stands for, when the union is nothing but strings.
    text: Option<String>,
}

/// The alternatives of an alias that is a union.
///
/// A union of nothing but strings is a set of names, and Haskell has a word for
/// that: a sum whose constructors carry nothing, each one travelling as the
/// string it stands for. Any other union is tried in order.
fn union_members(profile: &LanguageProfile, def: &AliasDef) -> Vec<Member> {
    let Type::Union { members } = &def.target else {
        return Vec::new();
    };
    let owner = type_name(profile, &def.name);
    let names = members.iter().all(is_string_literal);
    let mut taken = BTreeSet::new();
    members
        .iter()
        .map(|member| {
            let (stem, text) = match member {
                Type::Literal {
                    value: LiteralValue::Str(text),
                } if names => (name_for(text), Some(text.clone())),
                Type::Ref(r) => (upper_first(r.name.as_str()), None),
                other => (name_for(&spell(profile, other)), None),
            };
            Member {
                ctor: distinct(profile.name(&ident(&format!("{owner}{stem}"))), &mut taken),
                ty: member.clone(),
                text,
            }
        })
        .collect()
}

/// A constructor's name for some text: its words, capitalised and run together.
fn name_for(text: &str) -> String {
    let mut out = String::new();
    let mut capitalize = true;
    for c in text.chars() {
        if c.is_ascii_alphanumeric() {
            if capitalize {
                out.extend(c.to_uppercase());
                capitalize = false;
            } else {
                out.push(c);
            }
        } else {
            capitalize = true;
        }
    }
    if out.is_empty() { "Other".into() } else { out }
}

/// Whether a declaration is a sum type written for a union.
fn is_union_alias(def: &TypeDef) -> bool {
    matches!(def, TypeDef::Alias(a) if matches!(a.target, Type::Union { .. }))
}

// ---------------------------------------------------------------------------
// A universe

fn universe_file(
    ws: &CheckedWorkspace,
    universe: &Universe,
    profile: &LanguageProfile,
    options: &GenOptions,
) -> Result<String, GenError> {
    check_expressible(ws, universe)?;
    let defs = ordered_defs(universe);
    check_names(ws, profile, universe, &defs, options)?;

    let mut w = CodeWriter::with_indent("  ");
    if options.header {
        for line in header("--", universe, ws) {
            w.line(line);
        }
        w.blank();
    }

    w.line("{-# LANGUAGE OverloadedStrings #-}");
    if needs_classes_over_several_types(universe) {
        w.line("{-# LANGUAGE FlexibleContexts #-}");
        w.line("{-# LANGUAGE FlexibleInstances #-}");
        w.line("{-# LANGUAGE FunctionalDependencies #-}");
        w.line("{-# LANGUAGE MultiParamTypeClasses #-}");
    }
    // What is imported is what a universe might need, not what this one does.
    w.line("{-# OPTIONS_GHC -Wno-unused-imports #-}");
    if let Some(doc) = &universe.doc {
        haddock(&mut w, doc);
    }
    w.write(format_args!("module {} where", module_name(&universe.name)));
    w.blank();

    imports(&mut w, ws, universe, options);

    for def in defs {
        match def {
            TypeDef::Struct(d) => emit_struct(&mut w, ws, universe, d, profile),
            TypeDef::Interface(d) => emit_interface(&mut w, ws, universe, d, profile),
            TypeDef::Enum(d) => emit_enum(&mut w, d, profile),
            TypeDef::Alias(d) => emit_alias(&mut w, d, profile),
        }
        w.blank();
    }

    if options.stubs {
        for sig in &universe.functions {
            emit_signature(&mut w, ws, universe, sig, profile);
            w.blank();
        }
    }

    Ok(w.finish())
}

/// Refuses what the support module has no way to carry.
fn check_expressible(ws: &CheckedWorkspace, universe: &Universe) -> Result<(), GenError> {
    let mut refusal = None;
    for_each_type(universe, &mut |ty| {
        fn too_wide(ty: &Type) -> Option<usize> {
            match ty {
                Type::Tuple { items } if items.len() > TUPLE_LIMIT => Some(items.len()),
                _ => ty.children().into_iter().find_map(too_wide),
            }
        }
        if refusal.is_none() {
            refusal = too_wide(ty);
        }
    });
    if let Some(size) = refusal {
        return Err(unsupported(
            format!("a tuple of {size}"),
            format!("the support module reads and writes tuples of up to {TUPLE_LIMIT}"),
        ));
    }
    // A class is not a type: `Named` can be required of a parameter, but a
    // field of that type would have to be of some type it names, and which one
    // is not something JSON says.
    for (place, ty) in held(universe) {
        if let Some(interface) = interface_in(ws, ty) {
            return Err(unsupported(
                format!("`{place}`, whose type is the interface `{interface}`"),
                "a Haskell class is not a type, so nothing can have one for its type; \
                 bound a generic parameter by it instead"
                    .into(),
            ));
        }
    }
    for def in &universe.types {
        if matches!(def, TypeDef::Enum(d) if d.variants.is_empty()) {
            return Err(unsupported(
                format!("the enum `{}`", def.name()),
                "it has no variants, so there is nothing for a constructor to be".into(),
            ));
        }
        // What travels is read and written as JSON, and a wait is neither.
        let held: Vec<(String, &Type)> = match def {
            TypeDef::Struct(d) => d
                .fields
                .iter()
                .map(|f| (format!("{}.{}", d.name, f.name), &f.ty))
                .collect(),
            TypeDef::Enum(d) => d
                .variants
                .iter()
                .map(|v| (format!("{}.{}", d.name, v.name), &v.payload))
                .collect(),
            TypeDef::Alias(d) if matches!(d.target, Type::Union { .. }) => {
                vec![(d.name.to_string(), &d.target)]
            }
            _ => Vec::new(),
        };
        for (place, ty) in held {
            if waits(ws, ty) {
                return Err(unsupported(
                    format!("`{place}`, which holds a future"),
                    "a wait cannot be written as JSON, so a type that travels cannot hold one"
                        .into(),
                ));
            }
        }
    }
    Ok(())
}

/// Every place a universe holds a value of some type: a field, a payload, a
/// parameter, a result, and what an alias stands for. Where a type only bounds
/// a parameter is not one.
fn held(universe: &Universe) -> Vec<(String, &Type)> {
    let mut out: Vec<(String, &Type)> = Vec::new();
    for def in &universe.types {
        match def {
            TypeDef::Struct(d) => out.extend(
                d.fields
                    .iter()
                    .map(|f| (format!("{}.{}", d.name, f.name), &f.ty)),
            ),
            TypeDef::Interface(d) => out.extend(
                d.fields
                    .iter()
                    .map(|f| (format!("{}.{}", d.name, f.name), &f.ty)),
            ),
            TypeDef::Enum(d) => out.extend(
                d.variants
                    .iter()
                    .map(|v| (format!("{}.{}", d.name, v.name), &v.payload)),
            ),
            TypeDef::Alias(d) => out.push((d.name.to_string(), &d.target)),
        }
    }
    for sig in &universe.functions {
        out.extend(
            sig.params
                .iter()
                .map(|p| (format!("{}({})", sig.name, p.name), &p.ty)),
        );
        out.push((format!("{} returns", sig.name), &sig.ret));
    }
    out
}

/// The interface a type is, or holds, if there is one.
fn interface_in(ws: &CheckedWorkspace, ty: &Type) -> Option<Ident> {
    let ty = Resolver { ws }.resolve(ty);
    if let Type::Ref(r) = &ty {
        if let Some(TypeDef::Interface(_)) = ws.ref_target_of(r).and_then(|q| ws.def(&q)) {
            return Some(r.name.clone());
        }
    }
    ty.children()
        .into_iter()
        .find_map(|child| interface_in(ws, child))
}

/// Whether a type has a future anywhere in it, through any alias.
fn waits(ws: &CheckedWorkspace, ty: &Type) -> bool {
    let ty = Resolver { ws }.resolve(ty);
    matches!(ty, Type::Future { .. }) || ty.children().into_iter().any(|child| waits(ws, child))
}

/// Refuses a universe where two declarations would come out as one name.
///
/// Haskell keeps every type, every constructor and every field or function in a
/// module in a space of its own, so a clash is only a clash within one.
fn check_names(
    ws: &CheckedWorkspace,
    profile: &LanguageProfile,
    universe: &Universe,
    defs: &[&TypeDef],
    options: &GenOptions,
) -> Result<(), GenError> {
    let mut types: BTreeMap<String, String> = BTreeMap::new();
    let mut constructors: BTreeMap<String, String> = BTreeMap::new();
    let mut values: BTreeMap<String, String> = BTreeMap::new();

    let claim = |space: &mut BTreeMap<String, String>, name: String, what: String| match space
        .insert(name.clone(), what.clone())
    {
        Some(first) if first != what => Err(unsupported(
            format!("`{first}` and `{what}`"),
            format!(
                "both would be written as `{name}`, and Haskell has one \
                     space for them in a module"
            ),
        )),
        _ => Ok(()),
    };

    for def in defs {
        let what = |part: &str| format!("{}.{part}", def.name());
        claim(
            &mut types,
            type_name(profile, def.name()),
            def.name().to_string(),
        )?;
        match def {
            TypeDef::Struct(d) => {
                claim(
                    &mut constructors,
                    type_name(profile, &d.name),
                    def.name().to_string(),
                )?;
                // Every field it has, inherited or not, is a function of its own.
                for field in fields_of(ws, universe, def.name()) {
                    claim(
                        &mut values,
                        field_name(profile, &d.name, &field.name),
                        what(field.name.as_str()),
                    )?;
                }
            }
            TypeDef::Interface(d) => {
                for field in fields_of(ws, universe, def.name()) {
                    claim(
                        &mut values,
                        field_name(profile, &d.name, &field.name),
                        what(field.name.as_str()),
                    )?;
                }
            }
            TypeDef::Enum(d) => {
                for variant in &d.variants {
                    claim(
                        &mut constructors,
                        variant_name(profile, &d.name, variant.name.as_str()),
                        what(variant.name.as_str()),
                    )?;
                }
            }
            TypeDef::Alias(d) => {
                for member in union_members(profile, d) {
                    claim(&mut constructors, member.ctor, def.name().to_string())?;
                }
            }
        }
        let vars = tyvars(profile, def.params());
        let unique: BTreeSet<&String> = vars.iter().collect();
        if unique.len() != vars.len() {
            return Err(unsupported(
                format!("the parameters of `{}`", def.name()),
                "two of them would be written as one name".into(),
            ));
        }
    }
    if options.stubs {
        for sig in &universe.functions {
            claim(
                &mut values,
                var(profile, &sig.name),
                format!("the function `{}`", sig.name),
            )?;
            let params: BTreeSet<String> =
                sig.params.iter().map(|p| var(profile, &p.name)).collect();
            if params.len() != sig.params.len() {
                return Err(unsupported(
                    format!("the parameters of `{}`", sig.name),
                    "two of them would be written as one name".into(),
                ));
            }
        }
    }
    Ok(())
}

/// Every field a struct or an interface has, inherited ones included.
fn fields_of(ws: &CheckedWorkspace, universe: &Universe, name: &Ident) -> Vec<Field> {
    ws.flat_fields(&QualifiedName::new(universe.name.clone(), name.clone()))
        .map(<[Field]>::to_vec)
        .unwrap_or_default()
}

fn for_each_type(universe: &Universe, f: &mut impl FnMut(&Type)) {
    for def in &universe.types {
        match def {
            TypeDef::Struct(d) => d.fields.iter().for_each(|x| f(&x.ty)),
            TypeDef::Interface(d) => d.fields.iter().for_each(|x| f(&x.ty)),
            TypeDef::Enum(d) => d.variants.iter().for_each(|v| f(&v.payload)),
            TypeDef::Alias(d) => f(&d.target),
        }
        for bound in def.params().iter().flat_map(|p| &p.bounds) {
            f(&Type::Ref(bound.clone()));
        }
    }
    for sig in &universe.functions {
        sig.params.iter().for_each(|p| f(&p.ty));
        f(&sig.ret);
        for bound in sig.params_decl.iter().flat_map(|p| &p.bounds) {
            f(&Type::Ref(bound.clone()));
        }
    }
}

/// Whether anything here is a class over more than the type it is for: a
/// generic interface, or a bound or a claim that gives one arguments.
fn needs_classes_over_several_types(universe: &Universe) -> bool {
    let applied = |r: &TypeRef| !r.args.is_empty();
    universe.types.iter().any(|def| match def {
        TypeDef::Interface(d) => !d.params.is_empty(),
        TypeDef::Struct(d) => d.implements.iter().any(applied),
        _ => false,
    }) || universe
        .types
        .iter()
        .flat_map(|d| d.params())
        .chain(universe.functions.iter().flat_map(|f| &f.params_decl))
        .flat_map(|p| &p.bounds)
        .any(applied)
}

// ---------------------------------------------------------------------------
// Imports

/// A declaration another universe's module has to be asked for.
struct Wanted {
    /// Whether its constructors, fields or methods come with it.
    open: bool,
}

fn imports(w: &mut CodeWriter, ws: &CheckedWorkspace, universe: &Universe, options: &GenOptions) {
    // Only operators and types by name: every other word the Prelude has is
    // reached as `P.word`, so that nothing a universe declares can clash.
    w.line("import Prelude");
    w.indented(|w| {
        w.line("( Bool (..)");
        w.line(", Double");
        w.line(", Either (..)");
        w.line(", Eq");
        w.line(", IO");
        w.line(", Maybe (..)");
        w.line(", Ord");
        w.line(", Show");
        w.line(", (&&), (||), (==), (/=), (<), (<=), (>), (>=), (+), (-), (*), (<$>), (<*>)");
        w.line(")");
    });
    w.line("import qualified Prelude as P");
    w.line("import Data.ByteString (ByteString)");
    w.line("import qualified Data.ByteString as BS");
    w.line("import Data.Int (Int32, Int64)");
    w.line("import qualified Data.List");
    w.line("import Data.Map.Strict (Map)");
    w.line("import qualified Data.Map.Strict as Map");
    w.line("import Data.Text (Text)");
    w.line("import Data.Void (Void)");
    w.write(format_args!("import {SUPPORT} (Json (..))"));
    w.write(format_args!("import qualified {SUPPORT} as U"));

    // Asked for by module, in the order modules sort.
    let asked = needs(ws, universe, options);
    let mut modules: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for (from, wanted) in &asked.types {
        let names = modules.entry(module_name(from)).or_default();
        for (name, wanted) in wanted {
            names.push(if wanted.open {
                format!("{name} (..)")
            } else {
                name.clone()
            });
        }
    }
    for (from, called) in &asked.functions {
        modules
            .entry(module_name(from))
            .or_default()
            .extend(called.iter().cloned());
    }
    for (module, names) in modules {
        w.write(format_args!("import {module} ({})", names.join(", ")));
    }
    w.blank();
}

/// What a universe asks of the others: the declarations it names and the
/// functions it calls, by the universe that has them and as Haskell names them.
#[derive(Default)]
struct Needs {
    types: BTreeMap<Ident, BTreeMap<String, Wanted>>,
    functions: BTreeMap<Ident, BTreeSet<String>>,
}

fn needs(ws: &CheckedWorkspace, universe: &Universe, options: &GenOptions) -> Needs {
    let profile = profile("haskell").expect("ships");
    let mut asked = Needs::default();

    let mut want_type = |r: &TypeRef| {
        let Some(from) = &r.universe else { return };
        if from == &universe.name {
            return;
        }
        let open = ws
            .def(&QualifiedName::new(from.clone(), r.name.clone()))
            .is_none_or(|def| !matches!(def, TypeDef::Alias(_)) || is_union_alias(def));
        asked
            .types
            .entry(from.clone())
            .or_default()
            .insert(type_name(&profile, &r.name), Wanted { open });
    };
    let mut note = |ty: &Type| refs_in(ty, &mut |r| want_type(r));
    for_each_type(universe, &mut |ty| note(ty));
    for def in &universe.types {
        match def {
            TypeDef::Struct(d) => d
                .extends
                .iter()
                .chain(&d.implements)
                .for_each(|r| note(&Type::Ref(r.clone()))),
            TypeDef::Interface(d) => d.extends.iter().for_each(|r| note(&Type::Ref(r.clone()))),
            _ => {}
        }
    }
    let mut called_elsewhere: Vec<&QualifiedName> = Vec::new();
    if options.stubs {
        for sig in &universe.functions {
            let Some(body) = &sig.body else { continue };
            body.walk(&mut |e| match e {
                Expr::MakeRecord { ty, .. }
                | Expr::MakeVariant { ty, .. }
                | Expr::Match { ty, .. } => note(&Type::Ref(ty.clone())),
                Expr::Call { sig: called, .. } if called.universe != universe.name => {
                    called_elsewhere.push(called);
                }
                _ => {}
            });
        }
    }
    for called in called_elsewhere {
        asked
            .functions
            .entry(called.universe.clone())
            .or_default()
            .insert(var(&profile, &called.name));
    }
    asked
}

/// Refuses universes that import each other, which Haskell modules cannot.
fn check_cycles(
    ws: &CheckedWorkspace,
    chosen: &[&Universe],
    options: &GenOptions,
) -> Result<(), GenError> {
    let graph: BTreeMap<Ident, BTreeSet<Ident>> = chosen
        .iter()
        .map(|u| {
            let asked = needs(ws, u, options);
            let wanted = asked.types.into_keys().chain(asked.functions.into_keys());
            (u.name.clone(), wanted.collect())
        })
        .collect();

    fn visit<'a>(
        at: &'a Ident,
        graph: &'a BTreeMap<Ident, BTreeSet<Ident>>,
        path: &mut Vec<&'a Ident>,
        cleared: &mut BTreeSet<&'a Ident>,
    ) -> Option<Vec<&'a Ident>> {
        if let Some(start) = path.iter().position(|seen| *seen == at) {
            return Some(path[start..].to_vec());
        }
        if cleared.contains(at) {
            return None;
        }
        path.push(at);
        for next in graph.get(at).into_iter().flatten() {
            // A universe that was not asked for is not written, so cannot be
            // one end of a cycle.
            if graph.contains_key(next) {
                if let Some(cycle) = visit(next, graph, path, cleared) {
                    return Some(cycle);
                }
            }
        }
        path.pop();
        cleared.insert(at);
        None
    }

    let mut cleared = BTreeSet::new();
    for start in graph.keys() {
        if let Some(cycle) = visit(start, &graph, &mut Vec::new(), &mut cleared) {
            let mut names: Vec<String> = cycle.iter().map(|u| format!("`{u}`")).collect();
            names.push(format!("`{}`", cycle[0]));
            return Err(unsupported(
                format!("universes that refer to each other: {}", names.join(" -> ")),
                "each is a module that imports the next, and Haskell modules cannot import \
                 each other in a cycle"
                    .into(),
            ));
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Documentation

/// Writes prose as a Haddock comment: `-- |` once, then `--` for the rest.
fn haddock(w: &mut CodeWriter, text: &str) {
    w.comment("-- ", &format!("| {text}"));
}

/// Writes prose as a Haddock comment on what came before it: `-- ^`.
fn trailing(w: &mut CodeWriter, text: &str) {
    w.comment("-- ", &format!("^ {text}"));
}

// ---------------------------------------------------------------------------
// Instances

/// The context an instance of a class asks of a declaration's parameters.
fn context(class: &str, vars: &[String]) -> String {
    match vars {
        [] => String::new(),
        [only] => format!("{class} {only} => "),
        several => format!(
            "({}) => ",
            several
                .iter()
                .map(|v| format!("{class} {v}"))
                .collect::<Vec<_>>()
                .join(", ")
        ),
    }
}

fn instance_head(class: &str, name: &str, vars: &[String]) -> String {
    format!(
        "instance {}{class} {} where",
        context(class, vars),
        subject(name, vars)
    )
}

/// What a field is read as: one that may be left out is read from nothing.
///
/// Anything that expands to `T?`, `null` or `any` may be absent, and so may a
/// bare generic parameter, since the argument may be one of those.
fn may_be_left_out(ws: &CheckedWorkspace, ty: &Type) -> bool {
    matches!(
        Resolver { ws }.resolve(ty),
        Type::Optional { .. } | Type::Unit | Type::Any | Type::Param { .. }
    )
}

// ---------------------------------------------------------------------------
// Structs

fn emit_struct(
    w: &mut CodeWriter,
    ws: &CheckedWorkspace,
    universe: &Universe,
    def: &StructDef,
    profile: &LanguageProfile,
) {
    // Every field it has, inherited or not: the wire has no notion of where a
    // field came from, and neither does the reader.
    let fields = fields_of(ws, universe, &def.name);
    let type_ = type_name(profile, &def.name);
    let vars = tyvars(profile, &def.params);

    if let Some(doc) = &def.doc {
        haddock(w, doc);
    }
    if let Some(parent) = &def.extends {
        w.comment(
            "-- ",
            &format!(
                "Inherits from `{}`. Haskell has no inheritance, so the fields are \
                 carried here; the shape on the wire is unchanged.",
                parent.name
            ),
        );
    }

    if fields.is_empty() {
        w.write(format_args!(
            "data {} = {type_} deriving (Eq, Show)",
            head(&type_, &vars)
        ));
    } else {
        w.write(format_args!("data {} = {type_}", head(&type_, &vars)));
        w.indented(|w| {
            for (i, field) in fields.iter().enumerate() {
                let open = if i == 0 { "{" } else { "," };
                for caveat in profile.caveats(&field.ty) {
                    w.comment("-- ", &caveat);
                }
                w.write(format_args!(
                    "{open} {} :: {}",
                    field_name(profile, &def.name, &field.name),
                    spell(profile, &field.ty)
                ));
                if let Some(doc) = &field.doc {
                    w.indented(|w| trailing(w, doc));
                }
            }
            w.line("}");
            w.line("deriving (Eq, Show)");
        });
    }

    // How it travels.
    w.blank();
    w.line(instance_head("U.ToJson", &type_, &vars));
    w.indented(|w| {
        if fields.is_empty() {
            w.line("toJson _ = U.object []");
            return;
        }
        w.line("toJson x =");
        w.indented(|w| {
            w.line("U.object");
            w.indented(|w| {
                for (i, field) in fields.iter().enumerate() {
                    w.write(format_args!(
                        "{} U.field {:?} ({} x)",
                        if i == 0 { "[" } else { "," },
                        field.name.as_str(),
                        field_name(profile, &def.name, &field.name)
                    ));
                }
                w.line("]");
            });
        });
    });
    w.blank();
    w.line(instance_head("U.FromJson", &type_, &vars));
    w.indented(|w| {
        w.line("fromJson j = do");
        w.indented(|w| {
            if fields.is_empty() {
                w.write(format_args!("_ <- U.asObject {:?} j", type_));
                w.write(format_args!("P.pure {type_}"));
                return;
            }
            w.write(format_args!("o <- U.asObject {:?} j", type_));
            w.line(&type_);
            w.indented(|w| {
                for (i, field) in fields.iter().enumerate() {
                    let read = if may_be_left_out(ws, &field.ty) {
                        "optional"
                    } else {
                        "required"
                    };
                    w.write(format_args!(
                        "{} U.{read} {:?} o",
                        if i == 0 { "<$>" } else { "<*>" },
                        field.name.as_str()
                    ));
                }
            });
        });
    });

    // What it claims to satisfy becomes what Haskell can actually dispatch on.
    for iface in &def.implements {
        let mut applied = iface.clone();
        applied.universe = Some(iface.universe.clone().unwrap_or(universe.name.clone()));
        let this = Type::Ref(TypeRef {
            universe: Some(universe.name.clone()),
            name: def.name.clone(),
            args: def
                .params
                .iter()
                .map(|p| Type::Param {
                    name: p.name.clone(),
                })
                .collect(),
        });
        // The class is over the struct first, then whatever it was given.
        let mut over = vec![this];
        over.extend(applied.args.iter().cloned());
        let class = Type::Ref(TypeRef {
            universe: applied.universe.clone(),
            name: applied.name.clone(),
            args: over,
        });
        w.blank();
        w.write(format_args!("instance {} where", spell(profile, &class)));
        w.indented(|w| {
            for required in ws.fields_of(&applied).unwrap_or_default() {
                w.write(format_args!(
                    "{} = {}",
                    field_name(profile, &applied.name, &required.name),
                    field_name(profile, &def.name, &required.name)
                ));
            }
        });
    }
}

// ---------------------------------------------------------------------------
// Interfaces

fn emit_interface(
    w: &mut CodeWriter,
    ws: &CheckedWorkspace,
    universe: &Universe,
    def: &InterfaceDef,
    profile: &LanguageProfile,
) {
    if let Some(doc) = &def.doc {
        haddock(w, doc);
    }
    let fields = fields_of(ws, universe, &def.name);

    // A field contract is a class of accessors: the closest thing Haskell has
    // to "anything with these fields". It is over the type that has them, and
    // over whatever the interface is generic in, which that type decides.
    let vars = tyvars(profile, &def.params);
    let mut this = "self".to_string();
    while vars.contains(&this) {
        this.push('\'');
    }
    let mut over = vec![this.clone()];
    over.extend(vars.iter().cloned());
    let mut declared = format!("class {}", head(&type_name(profile, &def.name), &over));
    if !vars.is_empty() {
        declared.push_str(&format!(" | {this} -> {}", vars.join(" ")));
    }
    if fields.is_empty() {
        w.line(declared);
        return;
    }
    w.write(format_args!("{declared} where"));
    w.indented(|w| {
        for (i, field) in fields.iter().enumerate() {
            if i > 0 {
                w.blank();
            }
            w.write(format_args!(
                "{} :: {this} -> {}",
                field_name(profile, &def.name, &field.name),
                spell(profile, &field.ty)
            ));
            if let Some(doc) = &field.doc {
                w.indented(|w| trailing(w, doc));
            }
        }
    });
}

// ---------------------------------------------------------------------------
// Enums

fn emit_enum(w: &mut CodeWriter, def: &EnumDef, profile: &LanguageProfile) {
    if let Some(doc) = &def.doc {
        haddock(w, doc);
    }
    let type_ = type_name(profile, &def.name);
    let vars = tyvars(profile, &def.params);

    w.write(format_args!("data {}", head(&type_, &vars)));
    w.indented(|w| {
        for (i, variant) in def.variants.iter().enumerate() {
            let open = if i == 0 { "=" } else { "|" };
            let ctor = variant_name(profile, &def.name, variant.name.as_str());
            if matches!(variant.payload, Type::Unit) {
                w.write(format_args!("{open} {ctor}"));
            } else {
                w.write(format_args!(
                    "{open} {ctor} {}",
                    atom_type(profile, &variant.payload)
                ));
            }
            if let Some(doc) = &variant.doc {
                w.indented(|w| trailing(w, doc));
            }
        }
        w.line("deriving (Eq, Show)");
    });

    // The wire tags a variant `$tag` and puts what it carries beside it.
    w.blank();
    w.line(instance_head("U.ToJson", &type_, &vars));
    w.indented(|w| {
        for variant in &def.variants {
            let ctor = variant_name(profile, &def.name, variant.name.as_str());
            if matches!(variant.payload, Type::Unit) {
                w.write(format_args!(
                    "toJson {ctor} = U.variant {:?} Nothing",
                    variant.name.as_str()
                ));
            } else {
                w.write(format_args!(
                    "toJson ({ctor} x) = U.variant {:?} (Just (U.toJson x))",
                    variant.name.as_str()
                ));
            }
        }
    });
    w.blank();
    w.line(instance_head("U.FromJson", &type_, &vars));
    w.indented(|w| {
        w.line("fromJson j = do");
        w.indented(|w| {
            w.write(format_args!("(tag, carried) <- U.asVariant {:?} j", type_));
            w.line("case tag of");
            w.indented(|w| {
                for variant in &def.variants {
                    let ctor = variant_name(profile, &def.name, variant.name.as_str());
                    if matches!(variant.payload, Type::Unit) {
                        w.write(format_args!("{:?} -> P.pure {ctor}", variant.name.as_str()));
                    } else {
                        w.write(format_args!(
                            "{:?} -> {ctor} <$> U.payload {:?} carried",
                            variant.name.as_str(),
                            variant.name.as_str()
                        ));
                    }
                }
                w.write(format_args!("_ -> Left (U.unknownTag {:?} tag)", type_));
            });
        });
    });
}

/// A type written to stand as the argument of a constructor.
fn atom_type(profile: &LanguageProfile, ty: &Type) -> String {
    let spelled = spell(profile, ty);
    if spelled.contains(' ') && !is_bracketed(&spelled) {
        format!("({spelled})")
    } else {
        spelled
    }
}

/// Whether the whole of some text is one bracketed group.
fn is_bracketed(text: &str) -> bool {
    let close = match text.chars().next() {
        Some('(') => ')',
        Some('[') => ']',
        _ => return false,
    };
    let mut closed_at = None;
    scan(text, |i, c, nesting| {
        if closed_at.is_none() && nesting == 0 && c == close {
            closed_at = Some(i + c.len_utf8());
        }
    });
    closed_at == Some(text.len())
}

/// Walks some Haskell text, telling `visit` each character that is not inside
/// a string and how many brackets deep it is, counted before it.
fn scan(text: &str, mut visit: impl FnMut(usize, char, usize)) {
    let mut depth = 0usize;
    let mut in_string = false;
    let mut escaped = false;
    for (i, c) in text.char_indices() {
        if in_string {
            match (escaped, c) {
                (true, _) => escaped = false,
                (false, '\\') => escaped = true,
                (false, '"') => in_string = false,
                _ => {}
            }
            continue;
        }
        match c {
            '"' => in_string = true,
            '(' | '[' | '{' => {
                // An opening bracket is at the depth outside it.
                visit(i, c, depth);
                depth += 1;
            }
            ')' | ']' | '}' => {
                depth = depth.saturating_sub(1);
                visit(i, c, depth);
            }
            _ => visit(i, c, depth),
        }
    }
}

/// Whether some text has a comma that is not inside brackets or a string.
fn has_top_level_comma(text: &str) -> bool {
    let mut found = false;
    scan(text, |_, c, depth| found |= c == ',' && depth == 0);
    found
}

// ---------------------------------------------------------------------------
// Aliases

fn emit_alias(w: &mut CodeWriter, def: &AliasDef, profile: &LanguageProfile) {
    if let Some(doc) = &def.doc {
        haddock(w, doc);
    }
    let type_ = type_name(profile, &def.name);
    let vars = tyvars(profile, &def.params);

    let Type::Union { .. } = &def.target else {
        w.write(format_args!(
            "type {} = {}",
            head(&type_, &vars),
            spell(profile, &def.target)
        ));
        return;
    };
    let members = union_members(profile, def);
    let names = members.iter().all(|m| m.text.is_some());

    w.write(format_args!("data {}", head(&type_, &vars)));
    w.indented(|w| {
        for (i, member) in members.iter().enumerate() {
            let open = if i == 0 { "=" } else { "|" };
            if names {
                w.write(format_args!("{open} {}", member.ctor));
            } else {
                w.write(format_args!(
                    "{open} {} {}",
                    member.ctor,
                    atom_type(profile, &member.ty)
                ));
            }
        }
        w.line("deriving (Eq, Show)");
    });

    w.blank();
    w.line(instance_head("U.ToJson", &type_, &vars));
    w.indented(|w| {
        for member in &members {
            match &member.text {
                Some(text) if names => w.write(format_args!(
                    "toJson {} = U.JString {}",
                    member.ctor,
                    string_literal(text)
                )),
                _ => w.write(format_args!("toJson ({} x) = U.toJson x", member.ctor)),
            }
        }
    });
    w.blank();
    w.line(instance_head("U.FromJson", &type_, &vars));
    w.indented(|w| {
        if names {
            w.line("fromJson j = do");
            w.indented(|w| {
                w.write(format_args!("name <- U.asText {:?} j", type_));
                w.line("case name of");
                w.indented(|w| {
                    for member in &members {
                        if let Some(text) = &member.text {
                            w.write(format_args!(
                                "{} -> P.pure {}",
                                string_literal(text),
                                member.ctor
                            ));
                        }
                    }
                    w.write(format_args!("_ -> Left (U.unknownName {:?} name)", type_));
                });
            });
        } else {
            w.write(format_args!("fromJson j = U.firstOf {:?}", type_));
            w.indented(|w| {
                for (i, member) in members.iter().enumerate() {
                    let open = if i == 0 { "[" } else { "," };
                    w.write(format_args!("{open} {} <$> U.fromJson j", member.ctor));
                }
                w.line("]");
            });
        }
    });
}

// ---------------------------------------------------------------------------
// Signatures

fn emit_signature(
    w: &mut CodeWriter,
    ws: &CheckedWorkspace,
    universe: &Universe,
    sig: &FunctionSig,
    profile: &LanguageProfile,
) {
    let qualified = QualifiedName::new(universe.name.clone(), sig.name.clone());
    if let Some(doc) = &sig.doc {
        haddock(w, doc);
    }
    if sig.body.is_none() {
        w.comment(
            "-- ",
            &format!("Implement this and register it as `{qualified}`."),
        );
    }

    // A bound, being an interface, is a class constraint.
    let mut constraints: Vec<String> = Vec::new();
    for param in &sig.params_decl {
        for bound in &param.bounds {
            let mut over = vec![Type::Param {
                name: param.name.clone(),
            }];
            over.extend(bound.args.iter().cloned());
            constraints.push(spell(
                profile,
                &Type::Ref(TypeRef {
                    universe: bound.universe.clone(),
                    name: bound.name.clone(),
                    args: over,
                }),
            ));
        }
    }
    // A body may compare what it is handed.
    let compares = sig.body.as_ref().is_some_and(|body| {
        let mut found = false;
        body.walk(&mut |e| {
            found |= matches!(
                e,
                Expr::Prim {
                    op: PrimOp::Eq | PrimOp::Ne,
                    ..
                }
            )
        });
        found
    });
    if compares {
        for param in &sig.params_decl {
            let mentioned = sig.params.iter().any(|p| mentions(&p.ty, &param.name))
                || mentions(&sig.ret, &param.name);
            if mentioned {
                constraints.push(format!("Eq {}", tyvar(profile, &param.name)));
            }
        }
    }

    let mut parts: Vec<String> = sig.params.iter().map(|p| spell(profile, &p.ty)).collect();
    parts.push(spell(profile, &sig.ret));
    let name = var(profile, &sig.name);
    let context = match constraints.as_slice() {
        [] => String::new(),
        [only] => format!("{only} => "),
        several => format!("({}) => ", several.join(", ")),
    };
    w.write(format_args!("{name} :: {context}{}", parts.join(" -> ")));

    // A parameter the body never mentions is named so that GHC need not be told.
    let mut mentioned: BTreeSet<Ident> = BTreeSet::new();
    if let Some(body) = &sig.body {
        body.walk(&mut |e| {
            if let Expr::Var { name } = e {
                mentioned.insert(name.clone());
            }
        });
    }
    let params: Vec<String> = sig
        .params
        .iter()
        .map(|p| {
            let name = var(profile, &p.name);
            if sig.body.is_some() && !mentioned.contains(&p.name) {
                format!("_{name}")
            } else {
                name
            }
        })
        .collect();
    match &sig.body {
        // A body is a proof already, so it is written out rather than left as a
        // hole for someone to fill.
        Some(body) => {
            let env: Env = sig
                .params
                .iter()
                .map(|p| (p.name.clone(), p.ty.clone()))
                .collect();
            let text = Body {
                ws,
                profile,
                bounds: &sig.params_decl,
            }
            .expression(body, &env, Some(&sig.ret));
            w.write(format_args!(
                "{} = {}",
                head(&name, &params),
                strip_outer(&text)
            ));
        }
        None => {
            let holes = vec!["_".to_string(); params.len()];
            w.write(format_args!(
                "{} = P.error {}",
                head(&name, &holes),
                string_literal(&format!("not implemented: {qualified}"))
            ));
        }
    }
}

/// Whether a generic parameter appears anywhere in a type.
fn mentions(ty: &Type, param: &Ident) -> bool {
    matches!(ty, Type::Param { name } if name == param)
        || ty.children().into_iter().any(|c| mentions(c, param))
}

// ---------------------------------------------------------------------------
// Bodies

/// What each name in scope holds, at some point in a body.
type Env = HashMap<Ident, Type>;

/// Looking through a name for what it stands for.
struct Resolver<'a> {
    ws: &'a CheckedWorkspace,
}

impl Resolver<'_> {
    /// A type with its aliases expanded, except those that are a sum type of
    /// their own here, which have to be left as the name of one.
    fn resolve(&self, ty: &Type) -> Type {
        if self.is_sum(ty) {
            ty.clone()
        } else {
            self.ws.expand_alias(ty)
        }
    }

    /// Whether a name leads, through other names, to an alias that is a union.
    fn is_sum(&self, ty: &Type) -> bool {
        let mut current = ty.clone();
        for _ in 0..16 {
            let Type::Ref(r) = &current else {
                return false;
            };
            let Some(TypeDef::Alias(alias)) = self
                .ws
                .ref_target_of(r)
                .and_then(|target| self.ws.def(&target))
            else {
                return false;
            };
            if matches!(alias.target, Type::Union { .. }) {
                return true;
            }
            current = alias.target.clone();
        }
        false
    }
}

/// Writing a body as Haskell.
///
/// Every part that is not a single word is bracketed where it is made, so that
/// parts compose without anyone asking what binds tighter than what.
struct Body<'a> {
    ws: &'a CheckedWorkspace,
    profile: &'a LanguageProfile,
    /// The bounds of the signature's parameters, for the fields a bound gives.
    bounds: &'a [TypeParamDecl],
}

impl Body<'_> {
    fn resolver(&self) -> Resolver<'_> {
        Resolver { ws: self.ws }
    }

    /// A name a body binds, written so that GHC is not told off for one the
    /// body never reads.
    fn bound(&self, name: &Ident, scope: &Expr) -> String {
        let mut read = false;
        scope.walk(&mut |e| read |= matches!(e, Expr::Var { name: n } if n == name));
        let written = var(self.profile, name);
        if read { written } else { format!("_{written}") }
    }

    /// Writes `expr`, where `env` says what each name holds and `wanted` is
    /// what the place it stands in expects, when that place says.
    fn expression(&self, expr: &Expr, env: &Env, wanted: Option<&Type>) -> String {
        let profile = self.profile;
        match expr {
            // A constant is written as what is wanted, which takes it into
            // `Just` and into parsed JSON itself.
            Expr::Lit { value } => {
                let target = wanted.cloned().or_else(|| self.ws.type_of_expr(expr, env));
                match target {
                    Some(target) => typed(self.ws, profile, value, &target),
                    None => untyped(value, true),
                }
            }
            // A binding, a conditional, and taking a value apart each hand on
            // what is wanted to whatever produces their result, so `Just` goes
            // on the one arm that needs it rather than round all of them.
            Expr::Let { name, value, body } => {
                let mut inner = env.clone();
                if let Some(bound) = self.ws.type_of_expr(value, env) {
                    inner.insert(name.clone(), bound);
                }
                // A lambda rather than `let`, which is recursive: a body that
                // rebinds a name to something made from it would never end.
                format!(
                    "((\\{} -> {}) {})",
                    self.bound(name, body),
                    self.expression(body, &inner, wanted),
                    self.expression(value, env, None)
                )
            }
            Expr::If {
                cond,
                then,
                otherwise,
            } => {
                let arms = self.shared(wanted, expr, env);
                format!(
                    "(if {} then {} else {})",
                    self.expression(cond, env, None),
                    self.expression(then, env, arms.as_ref()),
                    self.expression(otherwise, env, arms.as_ref())
                )
            }
            Expr::Case {
                value,
                var: bound,
                some,
                none,
            } => {
                let arms = self.shared(wanted, expr, env);
                let mut present = env.clone();
                let inside =
                    self.ws
                        .type_of_expr(value, env)
                        .map(|t| match self.resolver().resolve(&t) {
                            Type::Optional { of } => *of,
                            // Only `null` gets here, so the present arm binds
                            // something that is never there.
                            _ => Type::Never,
                        });
                if let Some(inside) = inside {
                    present.insert(bound.clone(), inside);
                }
                format!(
                    "(case {} of {{ Just {} -> {}; Nothing -> {} }})",
                    self.expression(value, env, None),
                    self.bound(bound, some),
                    self.expression(some, &present, arms.as_ref()),
                    self.expression(none, env, arms.as_ref())
                )
            }
            Expr::Match { ty, value, arms } => {
                let wanted = self.shared(wanted, expr, env);
                let variants = self.ws.variants_of(ty).unwrap_or_default();
                let arms: Vec<String> = arms
                    .iter()
                    .map(|arm| {
                        let mut inner = env.clone();
                        let variant = variants.iter().find(|v| v.name == arm.tag);
                        let carries = variant.is_some_and(|v| !matches!(v.payload, Type::Unit));
                        if let (Some(bound), Some(variant)) = (&arm.var, variant) {
                            inner.insert(bound.clone(), variant.payload.clone());
                        }
                        let body = self.expression(&arm.body, &inner, wanted.as_ref());
                        let ctor = variant_name(profile, &ty.name, arm.tag.as_str());
                        match (&arm.var, carries) {
                            (Some(bound), _) => {
                                format!("{ctor} {} -> {body}", self.bound(bound, &arm.body))
                            }
                            // An arm that ignores the payload need not name it.
                            (None, true) => format!("{ctor} _ -> {body}"),
                            (None, false) => format!("{ctor} -> {body}"),
                        }
                    })
                    .collect();
                format!(
                    "(case {} of {{ {} }})",
                    self.expression(value, env, None),
                    arms.join("; ")
                )
            }
            _ => {
                // What is wanted of the value itself, once it is in `Just`.
                let within = wanted.map(|w| match self.resolver().resolve(w) {
                    Type::Optional { of } => self.resolver().resolve(&of),
                    other => other,
                });
                let text = self.value(expr, env, within.as_ref());
                let produced = self.ws.type_of_expr(expr, env);
                self.adapt(text, produced.as_ref(), wanted)
            }
        }
    }

    /// Writes an expression that is not a branch or a constant, where `wanted`
    /// is what is expected of it with any `Maybe` round it already accounted
    /// for.
    fn value(&self, expr: &Expr, env: &Env, wanted: Option<&Type>) -> String {
        let profile = self.profile;
        match expr {
            Expr::Var { name } => var(profile, name),
            Expr::Field { of, name } => {
                let subject = self.ws.type_of_expr(of, env);
                format!(
                    "({} {})",
                    self.accessor(subject.as_ref(), name),
                    self.expression(of, env, None)
                )
            }
            Expr::MakeRecord { ty, fields } => {
                let declared = self.ws.fields_of(ty).unwrap_or_default();
                let mut parts: Vec<String> = fields
                    .iter()
                    .map(|f| {
                        let field = declared.iter().find(|d| d.name == f.name);
                        format!(
                            "{} = {}",
                            field_name(profile, &ty.name, &f.name),
                            self.expression(&f.value, env, field.map(|d| &d.ty))
                        )
                    })
                    .collect();
                // The checker lets a body leave out a field that may be
                // absent, but a record names every field it has, and one left
                // out would fail when something read it.
                parts.extend(
                    declared
                        .iter()
                        .filter(|d| !fields.iter().any(|f| f.name == d.name))
                        .filter(|d| {
                            matches!(
                                self.resolver().resolve(&d.ty),
                                Type::Optional { .. } | Type::Unit | Type::Any
                            )
                        })
                        .map(|d| {
                            format!(
                                "{} = {}",
                                field_name(profile, &ty.name, &d.name),
                                typed(self.ws, profile, &Value::Null, &d.ty)
                            )
                        }),
                );
                let ctor = type_name(profile, &ty.name);
                if parts.is_empty() {
                    ctor
                } else {
                    format!("({ctor} {{ {} }})", parts.join(", "))
                }
            }
            Expr::MakeList { items } => {
                // Every element of a list is one type, so if one of them may be
                // absent they all have to be written as though they might.
                let elements = match wanted {
                    Some(Type::List { of }) if self.may_be_absent(of) => Some(of.as_ref().clone()),
                    _ => self.ws.type_of_expr(expr, env).and_then(|t| {
                        match self.resolver().resolve(&t) {
                            Type::List { of } => Some(*of),
                            _ => None,
                        }
                    }),
                };
                let parts: Vec<String> = items
                    .iter()
                    .map(|i| self.expression(i, env, elements.as_ref()))
                    .collect();
                format!("[{}]", parts.join(", "))
            }
            Expr::MakeTuple { items } => {
                let slots = match wanted {
                    Some(Type::Tuple { items: slots }) if slots.len() == items.len() => {
                        Some(slots.as_slice())
                    }
                    _ => None,
                };
                let parts: Vec<String> = items
                    .iter()
                    .enumerate()
                    .map(|(n, i)| self.expression(i, env, slots.map(|s| &s[n])))
                    .collect();
                match parts.as_slice() {
                    [] => "()".to_string(),
                    // There is no tuple of one, and the canonical form has none.
                    [only] => only.clone(),
                    _ => format!("({})", parts.join(", ")),
                }
            }
            Expr::MakeVariant { ty, tag, value } => {
                let ctor = variant_name(profile, &ty.name, tag.as_str());
                match value {
                    Some(v) => {
                        let payload = self
                            .ws
                            .variants_of(ty)
                            .and_then(|vs| vs.into_iter().find(|x| &x.name == tag))
                            .map(|x| x.payload);
                        format!("({ctor} {})", self.expression(v, env, payload.as_ref()))
                    }
                    None => ctor,
                }
            }
            Expr::Call { sig, args } => {
                let params = self.ws.sig(sig).map(|s| s.params.as_slice());
                let parts: Vec<String> = args
                    .iter()
                    .enumerate()
                    .map(|(n, a)| {
                        let param = params.and_then(|ps| ps.get(n)).map(|p| &p.ty);
                        self.expression(a, env, param)
                    })
                    .collect();
                let name = var(profile, &sig.name);
                if parts.is_empty() {
                    name
                } else {
                    format!("({name} {})", parts.join(" "))
                }
            }
            Expr::MapThrough {
                functor,
                value,
                var: bound,
                body,
            } => {
                use unitype_core::ir::Functor;
                let mut inner = env.clone();
                if let Some(subject) = self.ws.type_of_expr(value, env) {
                    if let Some((_, inside)) = Functor::split(&self.resolver().resolve(&subject)) {
                        inner.insert(bound.clone(), inside.clone());
                    }
                }
                // What is wanted of each thing inside is whatever is wanted
                // inside the result. For an optional that is already what is
                // wanted here, since the `Maybe` round it is the map's own.
                let each = match functor {
                    Functor::Optional => wanted.cloned(),
                    _ => wanted.and_then(|w| match Functor::split(w) {
                        Some((f, inside)) if f == *functor => Some(inside.clone()),
                        _ => None,
                    }),
                };
                // A list, a value that may be absent and a wait are all
                // things `fmap` goes through.
                format!(
                    "(P.fmap (\\{} -> {}) {})",
                    self.bound(bound, body),
                    self.expression(body, &inner, each.as_ref()),
                    self.expression(value, env, None)
                )
            }
            Expr::Prim { op, args } => {
                // Haskell compares a `Maybe` only with another, so a value
                // compared with something that may be absent is put in `Just`
                // to be compared at all.
                let produced: Vec<Option<Type>> =
                    args.iter().map(|a| self.ws.type_of_expr(a, env)).collect();
                let others: Vec<Option<Type>> = match (op, produced.as_slice()) {
                    (PrimOp::Eq | PrimOp::Ne, [x, y]) => vec![y.clone(), x.clone()],
                    _ => vec![None; args.len()],
                };
                let mut operands: Vec<String> = args
                    .iter()
                    .zip(&others)
                    .map(|(a, other)| self.expression(a, env, other.as_ref()))
                    .collect();
                // A 32-bit integer beside a 64-bit one is the narrower one
                // widened, which Haskell will not do unasked.
                if let [x, y] = operands.as_mut_slice() {
                    let kinds: Vec<Option<Type>> = produced
                        .iter()
                        .map(|t| t.as_ref().map(|t| self.resolver().resolve(t)))
                        .collect();
                    match kinds.as_slice() {
                        [Some(Type::I32), Some(Type::I64)] => *x = format!("(P.fromIntegral {x})"),
                        [Some(Type::I64), Some(Type::I32)] => *y = format!("(P.fromIntegral {y})"),
                        _ => {}
                    }
                }
                match (op, operands.as_slice()) {
                    (PrimOp::Neg, [x]) => format!("(P.negate {x})"),
                    (PrimOp::Not, [x]) => format!("(P.not {x})"),
                    // A length is an `Int` in Haskell and an `i64` everywhere
                    // else here.
                    (PrimOp::Len, [x]) => format!("(P.fromIntegral (P.length {x}) :: Int64)"),
                    (op, [x, y]) => format!("({x} {} {y})", infix(*op)),
                    _ => unreachable!("the checker holds every operation to its arity"),
                }
            }
            Expr::Fold {
                list,
                init,
                acc,
                item,
                body,
                ..
            } => {
                // The starting value and every step are one type, the one
                // carried, which is also what the fold as a whole produces.
                let carried = self.ws.type_of_expr(expr, env);
                let mut inner = env.clone();
                if let Some(carried) = &carried {
                    inner.insert(acc.clone(), carried.clone());
                }
                let element = self.ws.type_of_expr(list, env).and_then(|t| {
                    match self.resolver().resolve(&t) {
                        Type::List { of } => Some(*of),
                        _ => None,
                    }
                });
                if let Some(element) = element {
                    inner.insert(item.clone(), element);
                }
                format!(
                    "(Data.List.foldl' (\\{} {} -> {}) {} {})",
                    self.bound(acc, body),
                    self.bound(item, body),
                    self.expression(body, &inner, carried.as_ref()),
                    self.expression(init, env, carried.as_ref()),
                    self.expression(list, env, None)
                )
            }
            Expr::Lit { .. }
            | Expr::Let { .. }
            | Expr::If { .. }
            | Expr::Case { .. }
            | Expr::Match { .. } => {
                unreachable!("a branch is written by `expression`, which hands on what is wanted")
            }
        }
    }

    /// The function that reads a field out of something of type `subject`.
    fn accessor(&self, subject: Option<&Type>, field: &Ident) -> String {
        let profile = self.profile;
        match subject.map(|t| self.resolver().resolve(t)) {
            Some(Type::Ref(r)) => field_name(profile, &r.name, field),
            // What a bound says a parameter has is read through its class.
            Some(Type::Param { name }) => self
                .bounds
                .iter()
                .filter(|p| p.name == name)
                .flat_map(|p| &p.bounds)
                .find(|bound| {
                    self.ws
                        .fields_of(bound)
                        .is_some_and(|fs| fs.iter().any(|f| &f.name == field))
                })
                .map(|bound| field_name(profile, &bound.name, field))
                .unwrap_or_else(|| field.to_string()),
            _ => field.to_string(),
        }
    }

    /// What several alternatives are each written as, so that they come out as
    /// one Haskell type: what the place wants, if that may be absent, and
    /// otherwise what they come to together.
    fn shared(&self, wanted: Option<&Type>, expr: &Expr, env: &Env) -> Option<Type> {
        match wanted {
            Some(w) if self.may_be_absent(w) => Some(w.clone()),
            _ => self.ws.type_of_expr(expr, env),
        }
    }

    /// Whether a type is one Haskell writes as a `Maybe`.
    fn may_be_absent(&self, ty: &Type) -> bool {
        matches!(self.resolver().resolve(ty), Type::Optional { .. })
    }

    /// Makes what an expression produces what the place it stands in wants,
    /// for the three ways the checker lets one fit where Haskell will not: a
    /// value that is there where one that may be absent is wanted, a 32-bit
    /// integer where a wider number is, and anything where parsed JSON is.
    fn adapt(&self, text: String, produced: Option<&Type>, wanted: Option<&Type>) -> String {
        let (Some(produced), Some(wanted)) = (produced, wanted) else {
            return text;
        };
        let produced = self.resolver().resolve(produced);
        let wanted = self.resolver().resolve(wanted);
        // Bounds do not change what a value is, so a bounded `i32` widens like
        // any other; and a bounded type never wraps an optional, so looking
        // through them loses nothing the arms below ask about.
        match (produced.unbounded(), wanted.unbounded()) {
            // Already one, or the `null` that is the absence of one.
            (Type::Optional { .. } | Type::Unit | Type::Never, Type::Optional { .. }) => text,
            (_, Type::Optional { of }) => {
                format!("(Just {})", self.adapt(text, Some(&produced), Some(of)))
            }
            (Type::I32, Type::I64 | Type::F64) => format!("(P.fromIntegral {text})"),
            (_, w) if is_json(w) && !is_json(&produced) => format!("(U.toJson {text})"),
            _ => text,
        }
    }
}

/// How Haskell writes a two-operand operation.
fn infix(op: PrimOp) -> &'static str {
    match op {
        PrimOp::Eq => "==",
        PrimOp::Ne => "/=",
        PrimOp::Lt => "<",
        PrimOp::Le => "<=",
        PrimOp::Gt => ">",
        PrimOp::Ge => ">=",
        PrimOp::Add => "+",
        PrimOp::Sub => "-",
        PrimOp::Mul => "*",
        PrimOp::And => "&&",
        PrimOp::Or => "||",
        PrimOp::Neg | PrimOp::Not | PrimOp::Len => unreachable!("not a two-operand operation"),
    }
}

/// Takes off one pair of brackets round the whole of some text, if it has one.
///
/// A tuple keeps its own, and so does `()`.
fn strip_outer(text: &str) -> &str {
    if text.starts_with('(') && is_bracketed(text) {
        let inner = &text[1..text.len() - 1];
        if !inner.is_empty() && !has_top_level_comma(inner) {
            return inner;
        }
    }
    text
}

// ---------------------------------------------------------------------------
// Literals

/// Writes a Haskell string literal.
///
/// Anything outside printable ASCII is written as its code point in decimal,
/// which is how Haskell spells a character it cannot see; `\&` keeps a digit
/// that follows from being read as part of one.
pub fn string_literal(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 2);
    out.push('"');
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\t' => out.push_str("\\t"),
            '\r' => out.push_str("\\r"),
            ' '..='~' => out.push(c),
            other => {
                out.push_str(&format!("\\{}", other as u32));
                if chars.peek().is_some_and(char::is_ascii_digit) {
                    out.push_str("\\&");
                }
            }
        }
    }
    out.push('"');
    out
}

/// Writes a number so that it stands as an argument: a negative one is
/// bracketed, because `f -1` is a subtraction.
fn number(text: String) -> String {
    if text.starts_with('-') {
        format!("({text})")
    } else {
        text
    }
}

fn float(f: f64) -> String {
    let plain = f.to_string();
    number(if plain.contains(['.', 'e', 'E']) {
        plain
    } else {
        format!("{plain}.0")
    })
}

/// Writes a value as a Haskell literal, with nothing but the value to go on.
///
/// What a string is, or a list of them, is up to whatever it is put in, which
/// is what `OverloadedStrings` is for. `annotated` says it is `Text`, for the
/// places that have nothing else to settle it.
fn untyped(value: &Value, annotated: bool) -> String {
    match value {
        Value::Null => "Nothing".into(),
        Value::Bool(b) => if *b { "True" } else { "False" }.into(),
        Value::Int(i) => number(i.to_string()),
        Value::Float(f) => float(*f),
        Value::Str(s) if annotated => format!("({} :: Text)", string_literal(s)),
        Value::Str(s) => string_literal(s),
        Value::Bytes(b) => {
            let parts: Vec<String> = b.iter().map(u8::to_string).collect();
            format!("(BS.pack [{}])", parts.join(", "))
        }
        Value::List(items) => {
            let parts: Vec<String> = items.iter().map(|i| untyped(i, annotated)).collect();
            format!("[{}]", parts.join(", "))
        }
        Value::Tuple(items) => {
            let parts: Vec<String> = items.iter().map(|i| untyped(i, annotated)).collect();
            match parts.as_slice() {
                [only] => only.clone(),
                _ => format!("({})", parts.join(", ")),
            }
        }
        // An anonymous record has no declared type to name here, so it is
        // written as the map it is on the wire.
        Value::Record(fields) => {
            let parts: Vec<String> = fields
                .iter()
                .map(|(k, v)| format!("({}, {})", string_literal(k), untyped(v, annotated)))
                .collect();
            format!("(Map.fromList [{}])", parts.join(", "))
        }
        Value::Map(entries) => {
            let parts: Vec<String> = entries
                .iter()
                .map(|(k, v)| format!("({}, {})", untyped(k, annotated), untyped(v, annotated)))
                .collect();
            format!("(Map.fromList [{}])", parts.join(", "))
        }
        Value::Variant { .. } => json(value),
    }
}

/// Writes a value as the parsed JSON that holds it, which is what an `any` is
/// and what a type with no name of its own is held as.
///
/// With no type to say, the shapes JSON has no word for take the escapes the
/// protocol gives them.
fn json(value: &Value) -> String {
    let object = |entries: Vec<(&str, String)>| {
        let parts: Vec<String> = entries
            .into_iter()
            .map(|(k, v)| format!("({}, {v})", string_literal(k)))
            .collect();
        format!("(JObject (Map.fromList [{}]))", parts.join(", "))
    };
    match value {
        Value::Null => "JNull".into(),
        Value::Bool(b) => format!("(JBool {})", if *b { "True" } else { "False" }),
        Value::Int(i) => format!("(JInt {})", number(i.to_string())),
        Value::Float(f) => format!("(JFloat {})", float(*f)),
        Value::Str(s) => format!("(JString {})", string_literal(s)),
        Value::Bytes(b) => {
            use base64::Engine as _;
            let text = base64::engine::general_purpose::STANDARD.encode(b);
            object(vec![(
                "$bytes",
                format!("(JString {})", string_literal(&text)),
            )])
        }
        Value::List(items) => {
            let parts: Vec<String> = items.iter().map(json).collect();
            format!("(JArray [{}])", parts.join(", "))
        }
        Value::Tuple(items) => {
            let parts: Vec<String> = items.iter().map(json).collect();
            object(vec![("$tuple", format!("(JArray [{}])", parts.join(", ")))])
        }
        Value::Record(fields) => {
            let parts: Vec<String> = fields
                .iter()
                .map(|(k, v)| format!("({}, {})", string_literal(k), json(v)))
                .collect();
            format!("(JObject (Map.fromList [{}]))", parts.join(", "))
        }
        Value::Map(entries) => {
            let pairs: Vec<String> = entries
                .iter()
                .map(|(k, v)| format!("(JArray [{}, {}])", json(k), json(v)))
                .collect();
            object(vec![("$map", format!("(JArray [{}])", pairs.join(", ")))])
        }
        Value::Variant { tag, value } => {
            let mut entries = vec![("$tag", format!("(JString {})", string_literal(tag)))];
            if !matches!(value.as_ref(), Value::Null) {
                entries.push(("value", json(value)));
            }
            object(entries)
        }
    }
}

/// Writes a value as a Haskell literal of `ty`.
///
/// This is the language's way of writing a literal against its type, and the
/// body writer's way of writing a constant. Every result stands as an argument.
pub fn typed_literal(ws: &CheckedWorkspace, value: &Value, ty: &Type) -> String {
    typed(ws, &profile("haskell").expect("ships"), value, ty)
}

fn typed(ws: &CheckedWorkspace, profile: &LanguageProfile, value: &Value, ty: &Type) -> String {
    let resolver = Resolver { ws };
    let ty = resolver.resolve(ty);
    match (&ty, value) {
        (Type::Ref(r), _) => named_literal(ws, profile, r, value),
        // Bounds change nothing about how a value is written.
        (Type::Bounded { of, .. }, _) => typed(ws, profile, value, of),
        (Type::Optional { .. }, Value::Null) => "Nothing".into(),
        (Type::Optional { of }, present) => {
            format!("(Just {})", typed(ws, profile, present, of))
        }
        (Type::Unit, Value::Null) => "()".into(),
        (t, v) if is_json(t) => json(v),
        (Type::Never, _) => "(P.error \"no value has this type\")".into(),
        // Absence, where the type is not one that says so, is still `Nothing`:
        // a body reaches this with the type inside a `Maybe` it is about to add.
        (_, Value::Null) => "Nothing".into(),
        (Type::Literal { value: lit }, v) => typed(ws, profile, v, &lit.base()),
        (Type::Bool, Value::Bool(_)) | (Type::I32 | Type::I64, Value::Int(_)) => {
            untyped(value, false)
        }
        (Type::F64, Value::Int(i)) => float(*i as f64),
        (Type::F64, Value::Float(_)) | (Type::String, Value::Str(_)) => untyped(value, false),
        (Type::Bytes, Value::Bytes(_)) => untyped(value, false),
        (Type::List { of }, Value::List(items)) => {
            let parts: Vec<String> = items.iter().map(|i| typed(ws, profile, i, of)).collect();
            format!("[{}]", parts.join(", "))
        }
        (Type::Tuple { items: types }, Value::Tuple(items)) if types.len() == items.len() => {
            let parts: Vec<String> = types
                .iter()
                .zip(items)
                .map(|(t, i)| typed(ws, profile, i, t))
                .collect();
            format!("({})", parts.join(", "))
        }
        (Type::Map { key, value: of }, Value::Map(entries)) => {
            let key = map_key(resolver.resolve(key));
            let parts: Vec<String> = entries
                .iter()
                .map(|(k, v)| {
                    format!(
                        "({}, {})",
                        typed(ws, profile, k, &key),
                        typed(ws, profile, v, of)
                    )
                })
                .collect();
            format!("(Map.fromList [{}])", parts.join(", "))
        }
        // An object read against a map type whose keys are text.
        (Type::Map { key, value: of }, Value::Record(entries)) => {
            let key = map_key(resolver.resolve(key));
            let parts: Vec<String> = entries
                .iter()
                .map(|(k, v)| {
                    format!(
                        "({}, {})",
                        typed(ws, profile, &Value::Str(k.clone()), &key),
                        typed(ws, profile, v, of)
                    )
                })
                .collect();
            format!("(Map.fromList [{}])", parts.join(", "))
        }
        _ => untyped(value, false),
    }
}

/// Writes a value of a declared type: a record, an alternative of an enum, or
/// one of the names a union stands for.
fn named_literal(
    ws: &CheckedWorkspace,
    profile: &LanguageProfile,
    r: &TypeRef,
    value: &Value,
) -> String {
    let Some(def) = ws.ref_target_of(r).and_then(|q| ws.def(&q)) else {
        return untyped(value, false);
    };
    match (def, value) {
        (TypeDef::Struct(_), Value::Record(entries)) => {
            let declared = ws.fields_of(r).unwrap_or_default();
            // Declaration order, not whatever order the value arrived in:
            // generated code should look the way the declaration does.
            let parts: Vec<String> = declared
                .iter()
                .filter_map(|field| {
                    let present = entries.get(field.name.as_str());
                    let written = match present {
                        Some(entry) if !matches!(entry, Value::Null) => {
                            typed(ws, profile, entry, &field.ty)
                        }
                        // A field that is not there is written as nothing, if
                        // that is something its type can hold. A record names
                        // every field it has.
                        _ if may_be_left_out(ws, &field.ty) => {
                            typed(ws, profile, &Value::Null, &field.ty)
                        }
                        _ => return None,
                    };
                    Some(format!(
                        "{} = {written}",
                        field_name(profile, &r.name, &field.name)
                    ))
                })
                .collect();
            let ctor = type_name(profile, &r.name);
            if parts.is_empty() {
                ctor
            } else {
                format!("({ctor} {{ {} }})", parts.join(", "))
            }
        }
        (
            TypeDef::Enum(_),
            Value::Variant {
                tag,
                value: payload,
            },
        ) => {
            let ctor = variant_name(profile, &r.name, tag);
            let carried = ws
                .variants_of(r)
                .unwrap_or_default()
                .into_iter()
                .find(|v| v.name.as_str() == tag)
                .filter(|v| !matches!(v.payload, Type::Unit));
            match carried {
                Some(variant) => {
                    format!("({ctor} {})", typed(ws, profile, payload, &variant.payload))
                }
                None => ctor,
            }
        }
        (TypeDef::Alias(alias), v) if matches!(alias.target, Type::Union { .. }) => {
            let members = union_members(profile, alias);
            let names = members.iter().all(|m| m.text.is_some());
            let chosen = if names {
                members
                    .iter()
                    .find(|m| v.as_str().is_some() && m.text.as_deref() == v.as_str())
            } else {
                // Whichever alternative it actually is decides how it is
                // written, which is what makes a union come out right.
                members.iter().find(|m| ws.validate(v, &m.ty).is_ok())
            };
            match chosen {
                Some(member) if names => member.ctor.clone(),
                Some(member) => format!("({} {})", member.ctor, typed(ws, profile, v, &member.ty)),
                None => untyped(v, false),
            }
        }
        _ => untyped(value, false),
    }
}

/// Builds a named struct from its `field = value` parts.
pub fn record(name: &str, fields: &[(String, String)]) -> String {
    let parts: Vec<String> = fields.iter().map(|(f, v)| format!("{f} = {v}")).collect();
    format!("{name} {{ {} }}", parts.join(", "))
}

/// Builds one alternative of a named enum.
pub fn variant(name: &str, tag: &str, payload: Option<&str>) -> String {
    match payload {
        Some(value) => format!("{name}{} {}", upper_first(tag), bracketed(value)),
        None => format!("{name}{}", upper_first(tag)),
    }
}

fn bracketed(text: &str) -> String {
    if text.contains(' ') && !is_bracketed(text) && !text.starts_with('"') {
        format!("({text})")
    } else {
        text.to_string()
    }
}

/// Writes a value as a Haskell literal, with no type to guide it.
pub fn literal(value: &Value) -> String {
    untyped(value, false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use unitype_core::Workspace;
    use unitype_core::check::check_workspace;

    fn checked(json: &str) -> CheckedWorkspace {
        check_workspace(Workspace::from_json(json).expect("reads")).expect("checks")
    }

    fn generate(json: &str) -> String {
        generate_module(json, "Demo.hs")
    }

    fn generate_module(json: &str, file: &str) -> String {
        Haskell
            .generate(&checked(json), &GenOptions::default())
            .expect("generates")
            .into_iter()
            .find(|f| f.path.to_string_lossy().ends_with(file))
            .unwrap_or_else(|| panic!("a file called {file}"))
            .contents
    }

    const SHAPES: &str = r#"{"universes":[{"name":"demo","types":[
        {"k":"interface","name":"Identified","fields":[{"name":"id","ty":"str"}]},
        {"k":"struct","name":"Shape","implements":["Identified"],
         "fields":[{"name":"id","ty":"str"}]},
        {"k":"struct","name":"Circle","extends":"Shape","fields":[
            {"name":"kind","ty":{"k":"literal","value":{"str":"circle"}}},
            {"name":"radius","ty":"float"},
            {"name":"note","ty":"str?"}]},
        {"k":"enum","name":"Colour","variants":[{"name":"Red"},{"name":"Custom","payload":"str"}]},
        {"k":"alias","name":"Either","target":{"k":"union","members":["Circle","Shape"]}},
        {"k":"alias","name":"Tone","target":{"k":"union","members":[
            {"k":"literal","value":{"str":"light"}},{"k":"literal","value":{"str":"dark"}}]}}],
      "functions":[
        {"name":"area","params":[{"name":"s","ty":"Circle"}],"ret":"float"}]}]}"#;

    #[test]
    fn a_struct_becomes_a_record_whose_fields_are_named_after_it() {
        let out = generate(SHAPES);
        assert!(
            out.contains("data Shape = Shape\n  { shapeId :: Text\n  }"),
            "{out}"
        );
        assert!(out.contains("{ circleId :: Text"), "{out}");
        assert!(out.contains(", circleRadius :: Double"), "{out}");
        assert!(out.contains(", circleNote :: Maybe Text"), "{out}");
        assert!(out.contains("deriving (Eq, Show)"), "{out}");
    }

    #[test]
    fn a_child_carries_the_fields_it_inherited() {
        let out = generate(SHAPES);
        let circle = out.find("data Circle").expect("declared");
        let end = circle + out[circle..].find("deriving").expect("derives");
        assert!(out[circle..end].contains("circleId :: Text"), "{out}");
        assert!(
            out[..circle]
                .trim_end()
                .ends_with("the shape on the wire is unchanged."),
            "it says where the fields came from: {out}"
        );
    }

    #[test]
    fn an_interface_is_a_class_of_accessors_and_a_claim_is_an_instance() {
        let out = generate(SHAPES);
        assert!(
            out.contains("class Identified self where\n  identifiedId :: self -> Text"),
            "{out}"
        );
        assert!(
            out.contains("instance Identified Shape where\n  identifiedId = shapeId"),
            "{out}"
        );
    }

    #[test]
    fn an_enum_is_a_sum_tagged_the_way_the_wire_tags_it() {
        let out = generate(SHAPES);
        assert!(
            out.contains(
                "data Colour\n  = ColourRed\n  | ColourCustom Text\n  deriving (Eq, Show)"
            ),
            "{out}"
        );
        assert!(
            out.contains("toJson ColourRed = U.variant \"Red\" Nothing"),
            "{out}"
        );
        assert!(
            out.contains("\"Custom\" -> ColourCustom <$> U.payload \"Custom\" carried"),
            "{out}"
        );
    }

    #[test]
    fn a_union_of_names_is_a_sum_of_nothing() {
        let out = generate(SHAPES);
        assert!(
            out.contains("data Tone\n  = ToneDark\n  | ToneLight\n  deriving (Eq, Show)"),
            "{out}"
        );
        assert!(
            out.contains("toJson ToneLight = U.JString \"light\""),
            "{out}"
        );
        assert!(
            !out.contains("MapKey"),
            "the checker admits no such key: {out}"
        );
    }

    #[test]
    fn any_other_union_is_tried_in_order() {
        let out = generate(SHAPES);
        assert!(
            out.contains("data Either_\n  = Either_Circle Circle\n  | Either_Shape Shape"),
            "a universe's own `Either` must not be taken for the Prelude's: {out}"
        );
        assert!(
            out.contains("fromJson j = U.firstOf \"Either_\"\n    [ Either_Circle <$> U.fromJson j\n    , Either_Shape <$> U.fromJson j\n    ]"),
            "{out}"
        );
    }

    #[test]
    fn a_proposition_with_no_proof_is_an_error_rather_than_nothing() {
        let out = generate(SHAPES);
        assert!(out.contains("area :: Circle -> Double"), "{out}");
        assert!(
            out.contains("area _ = P.error \"not implemented: demo::area\""),
            "{out}"
        );
        assert!(
            out.contains("Implement this and register it as `demo::area`."),
            "{out}"
        );
    }

    #[test]
    fn fields_that_may_be_left_out_are_read_from_nothing_and_not_sent() {
        let out = generate(SHAPES);
        assert!(out.contains("<*> U.optional \"note\" o"), "{out}");
        assert!(out.contains("<*> U.required \"radius\" o"), "{out}");
    }

    #[test]
    fn a_reserved_word_cannot_be_a_field_because_a_field_is_prefixed() {
        let out = generate(
            r#"{"universes":[{"name":"demo","types":[
                {"k":"struct","name":"Thing","fields":[{"name":"class","ty":"str"}]}]}]}"#,
        );
        assert!(out.contains("thingClass :: Text"), "{out}");
        assert!(
            out.contains("U.field \"class\" (thingClass x)"),
            "the wire keeps its own name: {out}"
        );
    }

    #[test]
    fn a_type_named_for_something_that_is_imported_is_renamed() {
        let out = generate(
            r#"{"universes":[{"name":"demo","types":[
                {"k":"struct","name":"Map","fields":[{"name":"cells","ty":"[str]"}]},
                {"k":"struct","name":"World","fields":[{"name":"map","ty":"Map"}]}]}]}"#,
        );
        assert!(out.contains("data Map_ = Map_"), "{out}");
        assert!(out.contains("worldMap :: Map_"), "{out}");
    }

    #[test]
    fn two_declarations_that_would_be_one_name_are_refused_rather_than_merged() {
        let ws = checked(
            r#"{"universes":[{"name":"demo","types":[
                {"k":"struct","name":"Foo","fields":[{"name":"barBaz","ty":"str"}]},
                {"k":"struct","name":"FooBar","fields":[{"name":"baz","ty":"str"}]}]}]}"#,
        );
        let refused = Haskell.generate(&ws, &GenOptions::default());
        assert!(
            matches!(&refused, Err(GenError::Unsupported { what, .. }) if what.contains("barBaz") || what.contains("baz")),
            "{refused:?}"
        );
    }

    #[test]
    fn a_record_that_holds_a_wait_is_refused_because_a_wait_cannot_travel() {
        let ws = checked(
            r#"{"universes":[{"name":"demo","types":[
                {"k":"alias","name":"Soon","target":{"k":"future","of":"i64"}},
                {"k":"struct","name":"Pending","fields":[{"name":"later","ty":"[Soon]"}]}]}]}"#,
        );
        assert!(matches!(
            Haskell.generate(&ws, &GenOptions::default()),
            Err(GenError::Unsupported { what, .. }) if what.contains("Pending.later")
        ));
    }

    #[test]
    fn a_wait_is_fine_where_it_does_not_have_to_travel() {
        let out = generate(
            r#"{"universes":[{"name":"demo","types":[
                {"k":"alias","name":"Soon","target":{"k":"future","of":"i64"}}],
              "functions":[
                {"name":"fetch","params":[{"name":"url","ty":"str"}],"ret":"Soon"}]}]}"#,
        );
        assert!(out.contains("type Soon = IO Int64"), "{out}");
        assert!(out.contains("fetch :: Text -> Soon"), "{out}");
    }

    #[test]
    fn universes_that_import_each_other_are_refused_because_modules_cannot() {
        let ws = checked(
            r#"{"universes":[
                {"name":"a","types":[{"k":"struct","name":"Pa","fields":[{"name":"other","ty":"b::Pb?"}]}]},
                {"name":"b","types":[{"k":"struct","name":"Pb","fields":[{"name":"back","ty":"a::Pa?"}]}]}]}"#,
        );
        let refused = Haskell.generate(&ws, &GenOptions::default());
        assert!(
            matches!(&refused, Err(GenError::Unsupported { what, .. }) if what.contains("`a` -> `b` -> `a`")),
            "{refused:?}"
        );

        // Asked for one of them alone, there is no cycle to write.
        let alone = GenOptions {
            universes: Some(vec![ident("a")]),
            ..GenOptions::default()
        };
        assert!(Haskell.generate(&ws, &alone).is_ok());
    }

    #[test]
    fn a_universe_that_imports_another_one_way_is_not_a_cycle() {
        let ws = checked(
            r#"{"universes":[
                {"name":"a","types":[{"k":"struct","name":"Pa","fields":[{"name":"other","ty":"b::Pb?"}]}]},
                {"name":"b","types":[{"k":"struct","name":"Pb","fields":[{"name":"n","ty":"i64"}]}]}]}"#,
        );
        assert!(Haskell.generate(&ws, &GenOptions::default()).is_ok());
    }

    #[test]
    fn a_value_typed_by_an_interface_is_refused_but_a_bound_is_not() {
        let ws = checked(
            r#"{"universes":[{"name":"demo","types":[
                {"k":"interface","name":"Named","fields":[{"name":"name","ty":"str"}]},
                {"k":"struct","name":"Holder","fields":[{"name":"who","ty":"[Named]"}]}]}]}"#,
        );
        assert!(matches!(
            Haskell.generate(&ws, &GenOptions::default()),
            Err(GenError::Unsupported { what, .. }) if what.contains("Holder.who")
        ));

        // Bounding a parameter by it is what an interface is for.
        let out = generate(crate::GENERIC);
        assert!(out.contains("pick :: Named t => [t] -> Maybe t"), "{out}");
    }

    #[test]
    fn a_tuple_the_support_module_cannot_carry_is_refused() {
        let ws = checked(
            r#"{"universes":[{"name":"demo","types":[
                {"k":"struct","name":"Wide","fields":[{"name":"all","ty":{"k":"tuple","items":
                    ["i64","i64","i64","i64","i64","i64","i64","i64"]}}]}]}]}"#,
        );
        assert!(matches!(
            Haskell.generate(&ws, &GenOptions::default()),
            Err(GenError::Unsupported { .. })
        ));
    }

    #[test]
    fn generic_declarations_and_signatures_keep_their_parameters() {
        let hs = generate(crate::GENERIC);
        for expected in [
            "data Box t = Box",
            "{ boxValue :: t",
            "data Labelled t n = Labelled",
            "{ labelledValue :: t",
            ", labelledLabel :: n",
            "instance (U.ToJson t, U.ToJson n) => U.ToJson (Labelled t n) where",
            "data Outcome t e\n  = OutcomeOk t\n  | OutcomeErr e\n  | OutcomePending",
            "type Swap a b = (b, a)",
            "usesOutcome :: Outcome Int64 Text",
            "pick :: Named t => [t] -> Maybe t",
            "konst :: a -> b -> a",
            "konst x _y = x",
        ] {
            assert!(hs.contains(expected), "missing `{expected}` in:\n{hs}");
        }
    }

    #[test]
    fn a_generic_interface_is_a_class_over_the_type_that_has_it_and_what_it_is_given() {
        let hs = generate(crate::GENERIC);
        for expected in [
            "class Holder self t | self -> t where\n  holderHeld :: self -> t",
            "instance Holder (Tagged t) t where\n  holderHeld = taggedHeld",
            "{-# LANGUAGE FunctionalDependencies #-}",
        ] {
            assert!(hs.contains(expected), "missing `{expected}` in:\n{hs}");
        }
        assert!(
            !generate(SHAPES).contains("FunctionalDependencies"),
            "a universe with nothing generic does not need the extension"
        );
    }

    #[test]
    fn a_parameter_nothing_uses_is_still_one() {
        // Haskell accepts a phantom parameter, so nothing is left out.
        let hs = generate(crate::GENERIC);
        assert!(hs.contains("data Ghost t = Ghost"), "{hs}");
        assert!(hs.contains("usesGhost :: Ghost Int64"), "{hs}");
    }

    #[test]
    fn bodies_are_written_with_the_prelude_qualified() {
        let out = generate(
            r#"{"universes":[{"name":"demo","functions":[
                {"name":"length","params":[{"name":"xs","ty":"[i64]"}],"ret":"i64",
                 "body":{"k":"prim","op":"len","args":[{"k":"var","name":"xs"}]}}]}]}"#,
        );
        assert!(
            out.contains("length xs = P.fromIntegral (P.length xs) :: Int64"),
            "a function called `length` must not capture the one the body means: {out}"
        );
    }

    #[test]
    fn a_value_that_is_there_goes_in_just_where_one_that_may_not_be_is_wanted() {
        let out = generate(
            r#"{"universes":[{"name":"demo","types":[
                {"k":"struct","name":"Mark","fields":[
                    {"name":"at","ty":"i64"},{"name":"before","ty":"i64?"}]}],
              "functions":[
                {"name":"mark","params":[{"name":"x","ty":"i64"}],"ret":"Mark",
                 "body":{"k":"make_record","ty":"Mark","fields":[
                    {"name":"at","value":{"k":"var","name":"x"}},
                    {"name":"before","value":{"k":"var","name":"x"}}]}},
                {"name":"bare","params":[{"name":"x","ty":"i64"}],"ret":"Mark",
                 "body":{"k":"make_record","ty":"Mark","fields":[
                    {"name":"at","value":{"k":"var","name":"x"}}]}}]}]}"#,
        );
        assert!(
            out.contains("mark x = Mark { markAt = x, markBefore = (Just x) }"),
            "{out}"
        );
        assert!(
            out.contains("bare x = Mark { markAt = x, markBefore = Nothing }"),
            "a record names every field it has: {out}"
        );
    }

    #[test]
    fn a_fold_binding_and_a_branch_are_written_without_recursion() {
        let out = generate(
            r#"{"universes":[{"name":"demo","functions":[
                {"name":"total","params":[{"name":"xs","ty":"[i64]"}],"ret":"i64",
                 "body":{"k":"fold","list":{"k":"var","name":"xs"},"init":{"k":"lit","value":0},
                         "acc":"t","item":"x",
                         "body":{"k":"prim","op":"add","args":[{"k":"var","name":"t"},{"k":"var","name":"x"}]}}},
                {"name":"twice","params":[{"name":"x","ty":"i64"}],"ret":"i64",
                 "body":{"k":"let","name":"x","value":{"k":"prim","op":"add","args":[
                            {"k":"var","name":"x"},{"k":"var","name":"x"}]},
                         "body":{"k":"var","name":"x"}}}]}]}"#,
        );
        assert!(
            out.contains("total xs = Data.List.foldl' (\\t x -> (t + x)) 0 xs"),
            "{out}"
        );
        assert!(
            out.contains("twice x = (\\x -> x) (x + x)"),
            "`let x = x + x` would never end, so a binding is a lambda: {out}"
        );
    }

    #[test]
    fn a_body_that_takes_an_enum_apart_names_the_constructor_and_ignores_what_it_may() {
        let out = generate(
            r#"{"universes":[{"name":"demo","types":[
                {"k":"enum","name":"Size","variants":[{"name":"small"},{"name":"large","payload":"i64"}]}],
              "functions":[
                {"name":"is_large","params":[{"name":"s","ty":"Size"}],"ret":"bool",
                 "body":{"k":"match","ty":"Size","value":{"k":"var","name":"s"},"arms":[
                    {"tag":"small","body":{"k":"lit","value":false}},
                    {"tag":"large","body":{"k":"lit","value":true}}]}}]}]}"#,
        );
        assert!(
            out.contains("is_large s = case s of { SizeSmall -> False; SizeLarge _ -> True }"),
            "{out}"
        );
    }

    #[test]
    fn a_thirty_two_bit_integer_is_widened_where_a_wider_number_is_wanted() {
        let out = generate(
            r#"{"universes":[{"name":"demo","functions":[
                {"name":"widen","params":[{"name":"x","ty":"i32"}],"ret":"i64",
                 "body":{"k":"var","name":"x"}}]}]}"#,
        );
        assert!(out.contains("widen x = P.fromIntegral x"), "{out}");
    }

    #[test]
    fn string_literals_are_written_so_that_haskell_reads_the_same_text() {
        assert_eq!(string_literal("plain"), "\"plain\"");
        assert_eq!(string_literal("a \"q\" \\ b"), "\"a \\\"q\\\" \\\\ b\"");
        assert_eq!(string_literal("line\nbreak"), "\"line\\nbreak\"");
        // A character outside ASCII is its code point, and a digit after it
        // must not be read as more of the number.
        assert_eq!(string_literal("é1"), "\"\\233\\&1\"");
        assert_eq!(string_literal("😀"), "\"\\128512\"");
    }

    #[test]
    fn values_become_literals_haskell_would_accept() {
        assert_eq!(literal(&Value::Bool(true)), "True");
        assert_eq!(literal(&Value::Null), "Nothing");
        assert_eq!(literal(&Value::Float(2.0)), "2.0");
        assert_eq!(
            literal(&Value::Int(-3)),
            "(-3)",
            "`f -3` is a subtraction, so a negative number is bracketed"
        );
        assert_eq!(
            literal(&Value::Tuple(vec![Value::Int(1), Value::Str("a".into())])),
            "(1, \"a\")"
        );
        assert_eq!(literal(&Value::Bytes(vec![1, 2, 3])), "(BS.pack [1, 2, 3])");
    }

    #[test]
    fn a_universe_is_a_module_named_for_it_beside_the_support() {
        let ws = checked(SHAPES);
        let files = Haskell
            .generate(&ws, &GenOptions::default())
            .expect("generates");
        let paths: Vec<String> = files
            .iter()
            .map(|f| f.path.to_string_lossy().into_owned())
            .collect();
        assert_eq!(
            paths,
            [
                "haskell/Demo.hs",
                "haskell/UnitypeSupport.hs",
                "haskell/UnitypeHarness.hs"
            ]
        );
        assert!(generate(SHAPES).contains("module Demo where"));
    }

    #[test]
    fn a_universe_cannot_take_the_name_of_the_module_written_beside_it() {
        let ws = checked(r#"{"universes":[{"name":"unitypeSupport"}]}"#);
        assert!(matches!(
            Haskell.generate(&ws, &GenOptions::default()),
            Err(GenError::Unsupported { .. })
        ));
    }

    #[test]
    fn nothing_is_written_beside_a_universe_that_was_not_asked_for() {
        let ws = checked(SHAPES);
        let files = Haskell
            .generate(
                &ws,
                &GenOptions {
                    harness: false,
                    ..GenOptions::default()
                },
            )
            .expect("generates");
        assert!(!files.iter().any(|f| f.path.ends_with("UnitypeHarness.hs")));
    }

    #[test]
    fn generating_twice_gives_the_same_bytes() {
        assert_eq!(generate(SHAPES), generate(SHAPES));
    }

    #[test]
    fn what_another_universe_declares_is_imported_with_what_comes_with_it() {
        let ws = checked(
            r#"{"universes":[
                {"name":"geo","types":[
                    {"k":"struct","name":"Point","fields":[{"name":"x","ty":"f64"}]},
                    {"k":"alias","name":"Pair","target":{"k":"tuple","items":["f64","f64"]}}]},
                {"name":"map","types":[
                    {"k":"struct","name":"Pin","fields":[
                        {"name":"at","ty":"geo::Point"},{"name":"span","ty":"geo::Pair"}]}]}]}"#,
        );
        let out = Haskell
            .generate(&ws, &GenOptions::default())
            .expect("generates")
            .into_iter()
            .find(|f| f.path.ends_with("Map.hs"))
            .expect("a module for it")
            .contents;
        assert!(out.contains("import Geo (Pair, Point (..))"), "{out}");
        assert!(out.contains("module Map where"), "{out}");
    }
}
