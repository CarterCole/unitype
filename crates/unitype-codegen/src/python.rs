//! Writing a universe out as Python.
//!
//! Structs become keyword-only dataclasses and inheritance becomes class
//! inheritance, so a `Circle` really is a `Shape` to anything asking. Keyword-
//! only is not a style choice: without it, a dataclass cannot have a field with
//! a default before one without, and a parent with an optional field would stop
//! a child from adding a required one. Making every field keyword-only removes
//! the ordering constraint, so declaration order survives from the universe to
//! the generated class.
//!
//! Enums are the one place this reaches for `TypedDict` instead. The wire tags
//! a variant with `$tag`, which is not a name a dataclass field can have, and
//! describing something other than what actually arrives would defeat the
//! purpose.
//!
//! # Generics
//!
//! Anything generic is written with the type parameter syntax of PEP 695 —
//! `class Box[T]:`, `def reverse[T](xs: list[T])`, `type Swap[A, B] = …` — so
//! generated code that declares one needs Python 3.12. Everything else still
//! reads on 3.10. The older way, a module-level `TypeVar`, cannot say which
//! order an alias takes its parameters in: `Swap = tuple[B, A]` would take `B`
//! first, because Python orders them by where they first appear. A bound is
//! written as one, and several bounds are left out with a comment saying so,
//! since Python has no way to say a type must be two things at once.
//!
//! A variant that carries a generic parameter is a class: a `TypedDict` made
//! the functional way holds the `$tag`, and a generic class inheriting from it
//! adds the payload, which is the only way a `TypedDict` with a `$tag` in it
//! can be generic.

use std::collections::{BTreeMap, BTreeSet};

use unitype_core::check::CheckedWorkspace;
use unitype_core::ir::{
    AliasDef, EnumDef, Expr, Field, FunctionSig, Functor, InterfaceDef, PrimOp, StructDef, Type,
    TypeDef, TypeParamDecl, Universe,
};
use unitype_core::lang::{LanguageProfile, profile};
use unitype_core::value::Value;
use unitype_core::{Ident, QualifiedName};

use crate::writer::CodeWriter;
use crate::{
    Backend, GenError, GenOptions, GeneratedFile, bodies_use, header, ordered_defs, selected,
};

/// The Python backend.
pub struct Python;

impl Backend for Python {
    fn id(&self) -> &'static str {
        "python"
    }

    fn extension(&self) -> &'static str {
        "py"
    }

    fn generate(
        &self,
        ws: &CheckedWorkspace,
        options: &GenOptions,
    ) -> Result<Vec<GeneratedFile>, GenError> {
        let profile = profile("python")?;
        let mut files = Vec::new();

        for universe in selected(ws, options) {
            files.push(GeneratedFile {
                path: format!("python/{}.py", universe.name).into(),
                contents: universe_file(ws, universe, &profile, options),
            });
        }

        if options.harness {
            files.push(GeneratedFile {
                path: "python/unitype_harness.py".into(),
                contents: HARNESS.to_string(),
            });
        }
        Ok(files)
    }
}

fn universe_file(
    ws: &CheckedWorkspace,
    universe: &Universe,
    profile: &LanguageProfile,
    options: &GenOptions,
) -> String {
    let mut w = CodeWriter::new();

    if options.header {
        for line in header("#", universe, ws) {
            w.line(line);
        }
        w.blank();
    }
    if let Some(doc) = &universe.doc {
        w.line("\"\"\"");
        w.comment("", doc);
        w.line("\"\"\"");
        w.blank();
    }

    // Postponed annotations, so a class may name one declared further down.
    w.line("from __future__ import annotations");
    w.blank();
    if options.stubs && bodies_use(universe, |e| matches!(e, Expr::Fold { .. })) {
        w.line("import functools");
    }
    w.line("import typing");
    w.line("from dataclasses import dataclass, field");
    for (from, names) in imports(universe) {
        w.write(format_args!("from .{from} import {}", names.join(", ")));
    }
    w.blank();

    for def in ordered_defs(universe) {
        match def {
            TypeDef::Struct(d) => emit_struct(&mut w, ws, d, profile),
            TypeDef::Interface(d) => emit_interface(&mut w, ws, d, profile),
            TypeDef::Enum(d) => emit_enum(&mut w, ws, d, profile),
            TypeDef::Alias(d) => emit_alias(&mut w, ws, d, profile),
        }
        w.blank();
    }

    if options.stubs {
        for sig in &universe.functions {
            emit_signature(&mut w, ws, universe, sig, profile);
            w.blank();
        }
    }

    w.finish()
}

fn imports(universe: &Universe) -> Vec<(String, Vec<String>)> {
    let mut found: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    let mut note = |ty: &Type| {
        crate::refs_in(ty, &mut |r| {
            if let Some(from) = &r.universe {
                if from != &universe.name {
                    found
                        .entry(from.to_string())
                        .or_default()
                        .insert(r.name.to_string());
                }
            }
        });
    };
    for def in &universe.types {
        match def {
            TypeDef::Struct(d) => d.fields.iter().for_each(|f| note(&f.ty)),
            TypeDef::Interface(d) => d.fields.iter().for_each(|f| note(&f.ty)),
            TypeDef::Enum(d) => d.variants.iter().for_each(|v| note(&v.payload)),
            TypeDef::Alias(d) => note(&d.target),
        }
        for bound in def.params().iter().flat_map(|p| &p.bounds) {
            note(&Type::Ref(bound.clone()));
        }
    }
    for sig in &universe.functions {
        sig.params.iter().for_each(|p| note(&p.ty));
        note(&sig.ret);
        for bound in sig.params_decl.iter().flat_map(|p| &p.bounds) {
            note(&Type::Ref(bound.clone()));
        }
    }
    found
        .into_iter()
        .map(|(from, names)| (from, names.into_iter().collect()))
        .collect()
}

fn emit_struct(
    w: &mut CodeWriter,
    ws: &CheckedWorkspace,
    def: &StructDef,
    profile: &LanguageProfile,
) {
    let generics = type_params(w, &def.params, profile);
    // Keyword-only, so a parent's optional field cannot stop a child adding a
    // required one.
    w.line("@dataclass(kw_only=True)");
    let bases: Vec<String> = def
        .extends
        .iter()
        .chain(&def.implements)
        .map(|r| spell(ws, profile, &Type::Ref(r.clone())))
        .collect();
    let head = if bases.is_empty() {
        format!("class {}{generics}:", profile.name(&def.name))
    } else {
        format!(
            "class {}{generics}({}):",
            profile.name(&def.name),
            bases.join(", ")
        )
    };

    w.block(head, "", |w| {
        if let Some(doc) = &def.doc {
            docstring(w, doc);
        }
        if def.fields.is_empty() {
            if def.doc.is_none() {
                w.line("pass");
            }
            return;
        }
        emit_fields(w, ws, &def.fields, profile);
    });
}

fn emit_interface(
    w: &mut CodeWriter,
    ws: &CheckedWorkspace,
    def: &InterfaceDef,
    profile: &LanguageProfile,
) {
    // A field contract is a protocol: anything with these fields satisfies it,
    // which is what the interface actually says.
    let generics = type_params(w, &def.params, profile);
    let mut bases: Vec<String> = def
        .extends
        .iter()
        .map(|r| spell(ws, profile, &Type::Ref(r.clone())))
        .collect();
    if bases.is_empty() {
        bases.push("typing.Protocol".to_string());
    }
    w.block(
        format!(
            "class {}{generics}({}):",
            profile.name(&def.name),
            bases.join(", ")
        ),
        "",
        |w| {
            if let Some(doc) = &def.doc {
                docstring(w, doc);
            }
            if def.fields.is_empty() {
                if def.doc.is_none() {
                    w.line("pass");
                }
                return;
            }
            for field in &def.fields {
                w.write(format_args!(
                    "{}: {}",
                    profile.name(&field.name),
                    spell(ws, profile, &field.ty)
                ));
            }
        },
    );
}

fn emit_fields(
    w: &mut CodeWriter,
    ws: &CheckedWorkspace,
    fields: &[Field],
    profile: &LanguageProfile,
) {
    for f in fields {
        if profile.name(&f.name) != f.name.as_str() {
            w.comment(
                "# ",
                &format!(
                    "`{}` is a reserved word here, so this is renamed. It still \
                     travels under its own name.",
                    f.name
                ),
            );
        }
        for caveat in profile.caveats(&f.ty) {
            w.comment("# ", caveat);
        }
        if let Some(doc) = &f.doc {
            w.comment("# ", doc);
        }

        if has_integer_keys(ws, &f.ty) {
            w.comment("# ", INTEGER_KEYS);
        }
        let spelled = spell(ws, profile, &f.ty);
        // A field that may be absent defaults to absent, so a caller need not
        // spell out what they do not have. That is anything that expands to
        // `T?`, `null` or `any`, which is what a body may leave out.
        let absent = matches!(
            ws.expand_alias(&f.ty),
            Type::Optional { .. } | Type::Unit | Type::Any
        );
        match &f.ty {
            _ if absent => w.write(format_args!("{}: {spelled} = None", profile.name(&f.name))),
            Type::List { .. } => w.write(format_args!(
                "{}: {spelled} = field(default_factory=list)",
                profile.name(&f.name)
            )),
            _ => w.write(format_args!("{}: {spelled}", profile.name(&f.name))),
        }
    }
}

fn emit_enum(w: &mut CodeWriter, ws: &CheckedWorkspace, def: &EnumDef, profile: &LanguageProfile) {
    if let Some(doc) = &def.doc {
        w.comment("# ", doc);
    }
    // `$tag` is not a name a class attribute can have, so the functional form
    // of TypedDict is used. What it describes is exactly what arrives.
    let name = profile.name(&def.name);
    let mut variants = Vec::new();
    for variant in &def.variants {
        let variant_name = format!("{name}{}", variant.name);
        let tag = format!("\"$tag\": typing.Literal[{:?}]", variant.name.as_str());
        // The enum's generic parameters this variant's payload mentions, in
        // the order the enum declares them.
        let carried: Vec<TypeParamDecl> = def
            .params
            .iter()
            .filter(|p| mentions(&variant.payload, &p.name))
            .cloned()
            .collect();
        if carried.is_empty() {
            let members = if matches!(variant.payload, Type::Unit) {
                tag
            } else {
                format!("{tag}, \"value\": {}", spell(ws, profile, &variant.payload))
            };
            w.write(format_args!(
                "{variant_name} = typing.TypedDict({variant_name:?}, {{{members}}})"
            ));
            variants.push(variant_name);
            continue;
        }
        // Only a class can be generic, and a class cannot have a field called
        // `$tag`, so the tag goes in a functional `TypedDict` the class
        // inherits from.
        let tagged = format!("_{variant_name}");
        w.write(format_args!(
            "{tagged} = typing.TypedDict({tagged:?}, {{{tag}}})"
        ));
        let generics = type_params(w, &carried, profile);
        w.block(
            format!("class {variant_name}{generics}({tagged}):"),
            "",
            |w| {
                w.write(format_args!(
                    "value: {}",
                    spell(ws, profile, &variant.payload)
                ));
            },
        );
        let args: Vec<String> = carried.iter().map(|p| profile.name(&p.name)).collect();
        variants.push(format!("{variant_name}[{}]", args.join(", ")));
    }
    if def.params.is_empty() {
        w.write(format_args!("{name} = {}", variants.join(" | ")));
    } else {
        let generics = type_params(w, &def.params, profile);
        w.write(format_args!(
            "type {name}{generics} = {}",
            variants.join(" | ")
        ));
    }
}

/// Whether a generic parameter appears anywhere in a type.
fn mentions(ty: &Type, param: &Ident) -> bool {
    matches!(ty, Type::Param { name } if name == param)
        || ty.children().into_iter().any(|c| mentions(c, param))
}

/// Generic parameters in the syntax of PEP 695 — `[T, N: Named]` — or nothing
/// when there are none.
///
/// Python has no way to say a type must be several things at once, so a
/// parameter with more than one bound is left unbounded, and a comment says
/// what it was meant to be.
fn type_params(w: &mut CodeWriter, params: &[TypeParamDecl], profile: &LanguageProfile) -> String {
    if params.is_empty() {
        return String::new();
    }
    let spell = |b: &unitype_core::ir::TypeRef| profile.spell(&Type::Ref(b.clone()));
    let parts: Vec<String> = params
        .iter()
        .map(|p| {
            let name = profile.name(&p.name);
            match p.bounds.as_slice() {
                [] => name,
                [only] => format!("{name}: {}", spell(only)),
                several => {
                    let all: Vec<String> = several.iter().map(spell).collect();
                    w.comment(
                        "# ",
                        &format!(
                            "`{name}` has to be each of {}, which Python has no way to say, \
                             so it is left unbounded here.",
                            all.join(", ")
                        ),
                    );
                    name
                }
            }
        })
        .collect();
    format!("[{}]", parts.join(", "))
}

fn emit_alias(
    w: &mut CodeWriter,
    ws: &CheckedWorkspace,
    def: &AliasDef,
    profile: &LanguageProfile,
) {
    if let Some(doc) = &def.doc {
        w.comment("# ", doc);
    }
    let spelled = match &def.target {
        Type::Union { members } => members
            .iter()
            .map(|m| spell(ws, profile, m))
            .collect::<Vec<_>>()
            .join(" | "),
        other => spell(ws, profile, other),
    };
    // A generic alias says which order it takes its parameters in, which only
    // a `type` statement can.
    if def.params.is_empty() {
        w.write(format_args!("{} = {spelled}", profile.name(&def.name)));
    } else {
        let generics = type_params(w, &def.params, profile);
        w.write(format_args!(
            "type {}{generics} = {spelled}",
            profile.name(&def.name)
        ));
    }
}

fn emit_signature(
    w: &mut CodeWriter,
    ws: &CheckedWorkspace,
    universe: &Universe,
    sig: &FunctionSig,
    profile: &LanguageProfile,
) {
    let params: Vec<String> = sig
        .params
        .iter()
        .map(|p| format!("{}: {}", profile.name(&p.name), spell(ws, profile, &p.ty)))
        .collect();
    // A body that waits for something has to be a coroutine to be allowed to,
    // and what it returns is then what the future held.
    let awaits = sig.body.as_ref().is_some_and(|body| {
        let mut found = false;
        body.walk(&mut |e| {
            found |= matches!(
                e,
                Expr::MapThrough {
                    functor: Functor::Future,
                    ..
                }
            )
        });
        found
    });
    let (keyword, ret) = match &sig.ret {
        Type::Future { of } if awaits => ("async def", spell(ws, profile, of)),
        other => ("def", spell(ws, profile, other)),
    };
    let generics = type_params(w, &sig.params_decl, profile);
    let head = format!(
        "{keyword} {}{generics}({}) -> {ret}:",
        profile.name(&sig.name),
        params.join(", "),
    );
    let qualified = QualifiedName::new(universe.name.clone(), sig.name.clone());

    w.block(head, "", |w| {
        if let Some(doc) = &sig.doc {
            docstring(w, doc);
        }
        match &sig.body {
            // A body is a proof already, so it is written out rather than left
            // as a hole for someone to fill.
            Some(body) => w.write(format_args!("return {}", expression(body, profile))),
            None => {
                w.comment(
                    "# ",
                    &format!("Implement this and register it as `{qualified}`."),
                );
                w.write(format_args!(
                    "raise NotImplementedError({:?})",
                    qualified.to_string()
                ));
            }
        }
    });
}

/// What a field holding an integer-keyed map says about its keys.
const INTEGER_KEYS: &str = "Integer keys arrive as text, because a JSON object's property names \
are text: {1: ...} is {\"1\": ...}.";

/// How Python spells a type: as the profile does, except that the keys of a map
/// that is keyed by integers are `str`.
///
/// A proof is handed what JSON gives it, and JSON has no object with an
/// integer for a name, so `{1: "one"}` arrives as `{"1": "one"}`. `dict[int,
/// str]` would be a type nothing ever has. The other direction already
/// agrees: `json.dumps({1: "one"})` writes `{"1": "one"}`.
fn spell(ws: &CheckedWorkspace, profile: &LanguageProfile, ty: &Type) -> String {
    profile.spell(&textual_keys(ws, ty))
}

fn textual_keys(ws: &CheckedWorkspace, ty: &Type) -> Type {
    let kids: Vec<Type> = ty
        .children()
        .into_iter()
        .map(|c| textual_keys(ws, c))
        .collect();
    match ty.with_children(kids) {
        Type::Map { key, value } if integer_keys(ws, &key) => Type::Map {
            key: Box::new(Type::String),
            value,
        },
        other => other,
    }
}

/// Whether every key a type admits is an integer.
fn integer_keys(ws: &CheckedWorkspace, key: &Type) -> bool {
    match ws.expand_alias(key) {
        Type::I32
        | Type::I64
        | Type::Literal {
            value: unitype_core::ir::LiteralValue::Int(_),
        } => true,
        Type::Union { members } => {
            !members.is_empty() && members.iter().all(|m| integer_keys(ws, m))
        }
        _ => false,
    }
}

/// Whether a map keyed by integers appears anywhere in a type.
fn has_integer_keys(ws: &CheckedWorkspace, ty: &Type) -> bool {
    matches!(ty, Type::Map { key, .. } if integer_keys(ws, key))
        || ty.children().into_iter().any(|c| has_integer_keys(ws, c))
}

fn docstring(w: &mut CodeWriter, text: &str) {
    let single = text.trim().replace('\n', " ");
    if single.len() < 68 && !single.contains('"') {
        w.write(format_args!("\"\"\"{single}\"\"\""));
        return;
    }
    w.line("\"\"\"");
    w.comment("", text);
    w.line("\"\"\"");
}

/// Writes an expression body as Python.
pub(crate) fn expression(expr: &Expr, profile: &LanguageProfile) -> String {
    match expr {
        Expr::Lit { value } => literal(value),
        Expr::Var { name } => profile.name(name),
        Expr::Field { of, name } => {
            format!("{}.{}", expression(of, profile), profile.name(name))
        }
        Expr::MakeRecord { ty, fields } => {
            let parts: Vec<String> = fields
                .iter()
                .map(|f| {
                    format!(
                        "{}={}",
                        profile.name(&f.name),
                        expression(&f.value, profile)
                    )
                })
                .collect();
            format!("{}({})", profile.name(&ty.name), parts.join(", "))
        }
        Expr::MakeList { items } => {
            let parts: Vec<String> = items.iter().map(|i| expression(i, profile)).collect();
            format!("[{}]", parts.join(", "))
        }
        Expr::MakeTuple { items } => {
            let parts: Vec<String> = items.iter().map(|i| expression(i, profile)).collect();
            // A one-element tuple needs the comma, or it is just brackets.
            if parts.len() == 1 {
                format!("({},)", parts[0])
            } else {
                format!("({})", parts.join(", "))
            }
        }
        Expr::MakeVariant { tag, value, .. } => match value {
            Some(v) => format!(
                "{{\"$tag\": {:?}, \"value\": {}}}",
                tag.as_str(),
                expression(v, profile)
            ),
            None => format!("{{\"$tag\": {:?}}}", tag.as_str()),
        },
        Expr::Call { sig, args } => {
            let parts: Vec<String> = args.iter().map(|a| expression(a, profile)).collect();
            format!("{}({})", profile.name(&sig.name), parts.join(", "))
        }
        // Python has no expression-level binding other than this, and a body is
        // one expression, so this is the honest translation.
        Expr::Let { name, value, body } => format!(
            "(lambda {}: {})({})",
            profile.name(name),
            expression(body, profile),
            expression(value, profile)
        ),
        Expr::If {
            cond,
            then,
            otherwise,
        } => format!(
            "({} if {} else {})",
            expression(then, profile),
            expression(cond, profile),
            expression(otherwise, profile)
        ),
        Expr::MapThrough {
            functor,
            value,
            var,
            body,
        } => {
            let subject = expression(value, profile);
            let bound = profile.name(var);
            let inner = expression(body, profile);
            match functor {
                Functor::List => format!("[{inner} for {bound} in {subject}]"),
                Functor::Optional => {
                    format!("(lambda {bound}: None if {bound} is None else {inner})({subject})")
                }
                Functor::Future => {
                    format!("(lambda {bound}: {inner})(await {subject})")
                }
            }
        }
        Expr::Prim { op, args } => {
            let operands: Vec<String> = args.iter().map(|a| expression(a, profile)).collect();
            match (op, operands.as_slice()) {
                (PrimOp::Neg, [x]) => format!("(-{x})"),
                (PrimOp::Not, [x]) => format!("(not {x})"),
                (PrimOp::Len, [x]) => format!("len({x})"),
                (op, [x, y]) => {
                    let sign = match op {
                        PrimOp::Eq => "==",
                        PrimOp::Ne => "!=",
                        PrimOp::Lt => "<",
                        PrimOp::Le => "<=",
                        PrimOp::Gt => ">",
                        PrimOp::Ge => ">=",
                        PrimOp::Add => "+",
                        PrimOp::Sub => "-",
                        PrimOp::Mul => "*",
                        PrimOp::And => "and",
                        _ => "or",
                    };
                    format!("({x} {sign} {y})")
                }
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
        } => format!(
            "functools.reduce(lambda {}, {}: {}, {}, {})",
            profile.name(acc),
            profile.name(item),
            expression(body, profile),
            expression(list, profile),
            expression(init, profile)
        ),
        // `_case` and `_match` are reserved in this profile, so a body that
        // binds either name has it escaped rather than captured.
        Expr::Case {
            value,
            var,
            some,
            none,
        } => format!(
            "(lambda _case: {} if _case is None else (lambda {}: {})(_case))({})",
            expression(none, profile),
            profile.name(var),
            expression(some, profile),
            expression(value, profile)
        ),
        Expr::Match { value, arms, .. } => {
            let subject = expression(value, profile);
            let Some((last, rest)) = arms.split_last() else {
                return format!("(lambda _match: None)({subject})");
            };
            let arm = |a: &unitype_core::ir::MatchArm| {
                let body = expression(&a.body, profile);
                match &a.var {
                    Some(var) => format!(
                        "(lambda {}: {body})(_match.get(\"value\"))",
                        profile.name(var)
                    ),
                    None => body,
                }
            };
            // Every variant has an arm, so the last needs no test.
            let mut chain = arm(last);
            for a in rest.iter().rev() {
                chain = format!(
                    "({} if _match[\"$tag\"] == {:?} else {chain})",
                    arm(a),
                    a.tag.as_str()
                );
            }
            format!("(lambda _match: {chain})({subject})")
        }
    }
}

/// Writes a value as a Python literal.
pub fn literal(value: &Value) -> String {
    match value {
        Value::Null => "None".into(),
        Value::Bool(b) => if *b { "True" } else { "False" }.into(),
        Value::Int(i) => i.to_string(),
        Value::Float(f) => {
            let plain = f.to_string();
            if plain.contains(['.', 'e', 'E']) {
                plain
            } else {
                format!("{plain}.0")
            }
        }
        Value::Str(s) => format!("{s:?}"),
        // Base64 text, which is what a Python proof is handed and what it
        // writes back (see the profile's note on `bytes`).
        Value::Bytes(b) => {
            use base64::Engine as _;
            format!("{:?}", base64::engine::general_purpose::STANDARD.encode(b))
        }
        Value::List(items) => {
            let parts: Vec<String> = items.iter().map(literal).collect();
            format!("[{}]", parts.join(", "))
        }
        Value::Tuple(items) => {
            let parts: Vec<String> = items.iter().map(literal).collect();
            if parts.len() == 1 {
                format!("({},)", parts[0])
            } else {
                format!("({})", parts.join(", "))
            }
        }
        Value::Record(fields) => {
            let parts: Vec<String> = fields
                .iter()
                .map(|(k, v)| format!("{k:?}: {}", literal(v)))
                .collect();
            format!("{{{}}}", parts.join(", "))
        }
        Value::Map(entries) => {
            let parts: Vec<String> = entries
                .iter()
                .map(|(k, v)| {
                    // A property name is text, whatever the key was.
                    let name = match k {
                        Value::Int(i) => format!("{:?}", i.to_string()),
                        other => literal(other),
                    };
                    format!("{name}: {}", literal(v))
                })
                .collect();
            format!("{{{}}}", parts.join(", "))
        }
        Value::Variant { tag, value } => match value.as_ref() {
            Value::Null => format!("{{\"$tag\": {tag:?}}}"),
            payload => format!("{{\"$tag\": {tag:?}, \"value\": {}}}", literal(payload)),
        },
    }
}

/// A name that does not collide with a generated class.
#[allow(dead_code)]
fn distinct(name: &Ident, taken: &BTreeSet<String>) -> String {
    let mut candidate = name.to_string();
    while taken.contains(&candidate) {
        candidate.push('_');
    }
    candidate
}

/// The harness that makes a filled-in module reachable as a proof.
const HARNESS: &str = r#"# Generated by unitype. Do not edit.
"""Makes a Python module reachable as a proof, over standard input and output.

This is the whole of what it takes to take part: read a message, do the work,
write one back. One message per line, and reading lines serves both ways of
being called — if the parent sends one message and closes the stream, this runs
once and the process exits; if the parent keeps the stream open, it keeps
answering. Nothing here has to know which is happening.

Pass a table of what this module proves, keyed by the entry name a manifest
points at:

    from unitype_harness import unitype_main
    from demo import sort

    if __name__ == "__main__":
        unitype_main({"bubble_sort": sort})
"""

import json
import sys


def unitype_main(proves):
    for line in sys.stdin:
        line = line.strip()
        if line:
            _answer(proves, line)


def _answer(proves, line):
    try:
        request = json.loads(line)
    except ValueError as exc:
        return _fail("bad_request", f"could not read the request: {exc}")

    if request.get("unitype") != "1":
        return _fail(
            "bad_version",
            f"this module speaks version 1, not {request.get('unitype')!r}",
        )
    if request.get("kind") != "invoke":
        return _fail("bad_kind", f"cannot do {request.get('kind')!r}")

    entry = request.get("entry")
    proof = proves.get(entry)
    if proof is None:
        known = ", ".join(sorted(proves)) or "nothing"
        return _fail("no_such_entry", f"this module proves {known}, not {entry!r}")

    try:
        return _succeed(proof(*request.get("args", [])))
    except Exception as exc:  # noqa: BLE001 — whatever it raises is its answer
        return _fail(type(exc).__name__, str(exc))


def _succeed(value):
    _send({"ok": True, "value": value})


def _fail(code, message):
    # Reported as a failure of the proof, not of the process: exiting non-zero
    # would say the environment is broken, which it is not.
    _send({"ok": False, "error": {"code": code, "message": message}})


def _send(response):
    # One line, flushed: a parent waiting on the next line would otherwise wait
    # on a buffer that never fills.
    json.dump(response, sys.stdout)
    sys.stdout.write("\n")
    sys.stdout.flush()
"#;

#[cfg(test)]
mod tests {
    use super::*;
    use unitype_core::Workspace;
    use unitype_core::check::check_workspace;

    fn generate(json: &str) -> String {
        let ws = check_workspace(Workspace::from_json(json).expect("reads")).expect("checks");
        Python
            .generate(&ws, &GenOptions::default())
            .expect("generates")
            .into_iter()
            .find(|f| f.path.to_string_lossy().ends_with("demo.py"))
            .expect("a file for the demo universe")
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
        {"k":"alias","name":"Either","target":{"k":"union","members":["Circle","Shape"]}}],
      "functions":[
        {"name":"area","params":[{"name":"s","ty":"Circle"}],"ret":"float"}]}]}"#;

    #[test]
    fn a_struct_becomes_a_keyword_only_dataclass() {
        let out = generate(SHAPES);
        assert!(out.contains("@dataclass(kw_only=True)"), "{out}");
        assert!(out.contains("class Shape(Identified):"), "{out}");
        assert!(out.contains("id: str"), "{out}");
    }

    #[test]
    fn keyword_only_is_what_lets_a_child_add_a_required_field() {
        // Circle inherits an optional-free parent but adds `note`, which has a
        // default. Without kw_only, adding a required field in a later subclass
        // would be impossible.
        let out = generate(SHAPES);
        assert!(out.contains("note: str | None = None"), "{out}");
        assert!(
            out.matches("@dataclass(kw_only=True)").count() >= 2,
            "every dataclass needs it, not just the awkward one: {out}"
        );
    }

    #[test]
    fn a_parent_is_defined_before_the_child() {
        let out = generate(SHAPES);
        let shape = out.find("class Shape").expect("declared");
        let circle = out.find("class Circle").expect("declared");
        assert!(
            shape < circle,
            "Python cannot inherit from what is later: {out}"
        );
    }

    #[test]
    fn an_interface_becomes_a_protocol() {
        let out = generate(SHAPES);
        assert!(out.contains("class Identified(typing.Protocol):"), "{out}");
    }

    #[test]
    fn a_discriminating_literal_keeps_its_value() {
        let out = generate(SHAPES);
        assert!(out.contains(r#"kind: typing.Literal["circle"]"#), "{out}");
    }

    #[test]
    fn an_enum_is_described_as_what_actually_arrives() {
        let out = generate(SHAPES);
        assert!(
            out.contains(r#""$tag": typing.Literal["Red"]"#),
            "the wire tags it `$tag`, which no attribute could be called: {out}"
        );
        assert!(out.contains("Colour = ColourRed | ColourCustom"), "{out}");
    }

    #[test]
    fn an_alias_an_any_and_a_null_default_to_absent_too() {
        let out = generate(
            r#"{"universes":[{"name":"demo","types":[
                {"k":"alias","name":"Later","target":"i64?"},
                {"k":"struct","name":"Entry","fields":[
                    {"name":"at","ty":"i64"},{"name":"when","ty":"Later"},
                    {"name":"extra","ty":"any"},{"name":"nothing","ty":"null"}]}],
              "functions":[
                {"name":"entry","params":[{"name":"x","ty":"i64"}],"ret":"Entry",
                 "body":{"k":"make_record","ty":"Entry","fields":[
                    {"name":"at","value":{"k":"var","name":"x"}}]}}]}]}"#,
        );
        assert!(out.contains("when: Later = None"), "{out}");
        assert!(out.contains("extra: typing.Any = None"), "{out}");
        assert!(out.contains("nothing: None = None"), "{out}");
        assert!(
            out.contains("return Entry(at=x)"),
            "a body may leave all three out, so the class has to let it: {out}"
        );
        assert!(out.contains("    at: int\n"), "{out}");
    }

    #[test]
    fn a_list_field_defaults_to_an_empty_list_rather_than_a_shared_one() {
        let out = generate(
            r#"{"universes":[{"name":"demo","types":[
                {"k":"struct","name":"Bag","fields":[{"name":"items","ty":"[str]"}]}]}]}"#,
        );
        assert!(
            out.contains("field(default_factory=list)"),
            "a mutable default would be shared between instances: {out}"
        );
    }

    #[test]
    fn a_proposition_with_no_proof_raises_rather_than_returning_nothing() {
        let out = generate(SHAPES);
        assert!(out.contains("def area(s: Circle) -> float:"), "{out}");
        assert!(out.contains("raise NotImplementedError"), "{out}");
    }

    #[test]
    fn a_fold_brings_its_import_and_nothing_else_does() {
        let folding = generate(
            r#"{"universes":[{"name":"demo","functions":[
                {"name":"total","params":[{"name":"xs","ty":"[i64]"}],"ret":"i64",
                 "body":{"k":"fold","list":{"k":"var","name":"xs"},"init":{"k":"lit","value":0},
                         "acc":"t","item":"x",
                         "body":{"k":"prim","op":"add","args":[{"k":"var","name":"t"},{"k":"var","name":"x"}]}}}]}]}"#,
        );
        assert!(folding.contains("import functools"), "{folding}");
        assert!(
            folding.contains("functools.reduce(lambda t, x: (t + x), xs, 0)"),
            "{folding}"
        );
        assert!(!generate(SHAPES).contains("functools"));
    }

    #[test]
    fn a_body_that_waits_is_a_coroutine() {
        let out = generate(
            r#"{"universes":[{"name":"demo","functions":[
                {"name":"later","params":[{"name":"n","ty":{"k":"future","of":"i64"}}],
                 "ret":{"k":"future","of":"i64"},
                 "body":{"k":"map_through","functor":"future","value":{"k":"var","name":"n"},
                         "var":"x","body":{"k":"var","name":"x"}}}]}]}"#,
        );
        assert!(
            out.contains("async def later(n: typing.Awaitable[int]) -> int:"),
            "an `await` is only allowed inside `async def`: {out}"
        );
    }

    #[test]
    fn a_name_the_generated_code_uses_for_itself_is_escaped_in_a_body() {
        let out = generate(
            r#"{"universes":[{"name":"demo","functions":[
                {"name":"f","params":[{"name":"_case","ty":"i64?"}],"ret":"i64",
                 "body":{"k":"case","value":{"k":"var","name":"_case"},"var":"it",
                         "some":{"k":"var","name":"it"},"none":{"k":"lit","value":0}}}]}]}"#,
        );
        assert!(
            out.contains("(lambda _case: 0 if _case is None else (lambda it: it)(_case))(_case_)"),
            "{out}"
        );
    }

    #[test]
    fn a_body_is_written_out_as_python() {
        let out = generate(
            r#"{"universes":[{"name":"demo","types":[
                {"k":"struct","name":"User","fields":[{"name":"first","ty":"str"}]},
                {"k":"struct","name":"Badge","fields":[
                    {"name":"label","ty":"str"},{"name":"sides","ty":"i64"}]}],
              "functions":[
                {"name":"badge_of","params":[{"name":"u","ty":"User"}],"ret":"Badge",
                 "body":{"k":"make_record","ty":"Badge","fields":[
                    {"name":"label","value":{"k":"field","of":{"k":"var","name":"u"},"name":"first"}},
                    {"name":"sides","value":{"k":"lit","value":4}}]}}]}]}"#,
        );
        assert!(
            out.contains("return Badge(label=u.first, sides=4)"),
            "the same body that became TypeScript should become Python: {out}"
        );
    }

    #[test]
    fn mapping_through_a_list_becomes_a_comprehension() {
        let out = generate(
            r#"{"universes":[{"name":"demo","types":[
                {"k":"struct","name":"User","fields":[{"name":"first","ty":"str"}]}],
              "functions":[
                {"name":"names","params":[{"name":"us","ty":"[User]"}],"ret":"[str]",
                 "body":{"k":"map_through","functor":"list","value":{"k":"var","name":"us"},
                         "var":"u","body":{"k":"field","of":{"k":"var","name":"u"},"name":"first"}}}]}]}"#,
        );
        assert!(out.contains("[u.first for u in us]"), "{out}");
    }

    #[test]
    fn a_reserved_word_is_renamed_and_the_rename_is_explained() {
        let out = generate(
            r#"{"universes":[{"name":"demo","types":[
                {"k":"struct","name":"Thing","fields":[{"name":"class","ty":"str"}]}]}]}"#,
        );
        assert!(out.contains("class_: str"), "{out}");
        assert!(
            out.contains("reserved word"),
            "the rename changes what the reader sees, so it should be explained: {out}"
        );
    }

    #[test]
    fn annotations_are_postponed_so_order_never_bites() {
        let out = generate(SHAPES);
        assert!(out.contains("from __future__ import annotations"), "{out}");
    }

    #[test]
    fn generating_twice_gives_the_same_bytes() {
        assert_eq!(generate(SHAPES), generate(SHAPES));
    }

    #[test]
    fn values_become_literals_python_would_accept() {
        assert_eq!(literal(&Value::Bool(true)), "True");
        assert_eq!(literal(&Value::Null), "None");
        assert_eq!(literal(&Value::Float(2.0)), "2.0");
        assert_eq!(
            literal(&Value::Tuple(vec![Value::Int(1)])),
            "(1,)",
            "without the comma it is just brackets"
        );
        assert_eq!(
            literal(&Value::record([("a", Value::Int(1))])),
            r#"{"a": 1}"#
        );
    }

    #[test]
    fn generic_declarations_and_signatures_use_type_parameter_syntax() {
        let py = generate(crate::GENERIC);
        for expected in [
            "class Box[T]:",
            "class Labelled[T, N: Named](Box[T]):",
            "class Tagged[T](Holder[T]):",
            "class Holder[T](typing.Protocol):",
            "type Swap[A, B] = tuple[B, A]",
            "outcome: Outcome[int, str]",
            "def pick[T: Named](xs: list[T]) -> T | None:",
            "def konst[A, B](x: A, y: B) -> A:",
        ] {
            assert!(py.contains(expected), "missing `{expected}` in:\n{py}");
        }
    }

    #[test]
    fn a_variant_that_carries_a_generic_parameter_is_a_class_on_top_of_its_tag() {
        let py = generate(crate::GENERIC);
        for expected in [
            "_OutcomeOk = typing.TypedDict(\"_OutcomeOk\", {\"$tag\": typing.Literal[\"Ok\"]})",
            "class OutcomeOk[T](_OutcomeOk):\n    value: T",
            "class OutcomeErr[E](_OutcomeErr):\n    value: E",
            "OutcomePending = typing.TypedDict(\"OutcomePending\", {\"$tag\": typing.Literal[\"Pending\"]})",
            "type Outcome[T, E] = OutcomeOk[T] | OutcomeErr[E] | OutcomePending",
        ] {
            assert!(py.contains(expected), "missing `{expected}` in:\n{py}");
        }
    }

    #[test]
    fn several_bounds_are_left_out_and_said_to_be() {
        let py = generate(
            r#"{"universes":[{"name":"demo","types":[
                {"k":"interface","name":"Named","fields":[{"name":"name","ty":"str"}]},
                {"k":"interface","name":"Sized","fields":[{"name":"size","ty":"i64"}]}],
              "functions":[{"name":"f","params_decl":[{"name":"T","bounds":["Named","Sized"]}],
                "params":[{"name":"x","ty":{"k":"param","name":"T"}}],"ret":"str"}]}]}"#,
        );
        assert!(py.contains("def f[T](x: T) -> str:"), "{py}");
        assert!(py.contains("`T` has to be each of Named, Sized"), "{py}");
    }

    #[test]
    fn nothing_that_is_not_generic_needs_a_newer_python() {
        let py = generate(SHAPES);
        // A `type` statement, or type parameters before a class's bases or a
        // function's parameters.
        let newer = |line: &str| {
            line.starts_with("type ")
                || ["class ", "def "].iter().any(|kw| {
                    line.strip_prefix(kw).is_some_and(|rest| {
                        rest.split(['(', ':']).next().unwrap_or("").contains('[')
                    })
                })
        };
        assert!(!py.lines().any(newer), "{py}");
    }

    #[test]
    fn what_a_proof_is_handed_is_what_the_annotations_say() {
        let py = generate(
            r#"{"universes":[{"name":"demo","types":[
                {"k":"struct","name":"Doc","fields":[
                    {"name":"blob","ty":"bytes"},
                    {"name":"by_number","ty":"map<i64, [bytes]>"},
                    {"name":"by_name","ty":"map<str, i64>"}]}]}]}"#,
        );
        // Base64 text for bytes, and text for the keys of a map that is
        // keyed by integers, because that is what JSON can hold.
        assert!(py.contains("blob: str"), "{py}");
        assert!(py.contains("by_number: dict[str, list[str]]"), "{py}");
        assert!(py.contains("by_name: dict[str, int]"), "{py}");
        assert_eq!(
            py.matches("Integer keys arrive as text").count(),
            1,
            "said once, beside the one field it is true of: {py}"
        );
    }

    #[test]
    fn a_literal_bytes_value_is_the_text_it_travels_as() {
        assert_eq!(literal(&Value::Bytes(vec![1, 2, 3])), "\"AQID\"");
        assert_eq!(
            literal(&Value::Map(vec![(Value::Int(1), Value::Str("one".into()))])),
            "{\"1\": \"one\"}"
        );
    }
}
