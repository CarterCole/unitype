//! Writing a universe out as Rust.
//!
//! Rust has no inheritance, so there is a choice to make and it is worth being
//! explicit about. A child could hold its parent in a flattened field, keeping
//! the shape of the declaration; or it could carry every field it has,
//! inherited or not. This does the second.
//!
//! Flattening wins because the wire is what matters. `Circle` serializes to the
//! same object either way, but flattened it also *reads* like the object —
//! `circle.id` rather than `circle.base.id`, one level however deep the
//! hierarchy went. Rust was never going to give subtype polymorphism anyway;
//! what it gives is traits, and an interface becomes one, so the polymorphism
//! that does exist lands where Rust can actually use it.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use heck::ToUpperCamelCase;
use unitype_core::check::CheckedWorkspace;
use unitype_core::ir::{
    AliasDef, EnumDef, Expr, Field, FunctionSig, InterfaceDef, LiteralValue, PrimOp, StructDef,
    Type, TypeDef, Universe,
};
use unitype_core::lang::{LanguageProfile, profile};
use unitype_core::value::Value;
use unitype_core::{Ident, QualifiedName};

use crate::writer::CodeWriter;
use crate::{Backend, GenError, GenOptions, GeneratedFile, header, ordered_defs, selected};

/// What every generated type derives.
const DERIVES: &str = "#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]";

/// The Rust backend.
pub struct Rust;

impl Backend for Rust {
    fn id(&self) -> &'static str {
        "rust"
    }

    fn extension(&self) -> &'static str {
        "rs"
    }

    fn generate(
        &self,
        ws: &CheckedWorkspace,
        options: &GenOptions,
    ) -> Result<Vec<GeneratedFile>, GenError> {
        let profile = profile("rust")?;
        let mut files = Vec::new();
        for universe in selected(ws, options) {
            files.push(GeneratedFile {
                path: format!("rust/{}.rs", universe.name).into(),
                contents: universe_file(ws, universe, &profile, options),
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
        for line in header("//", universe, ws) {
            w.line(line);
        }
        w.blank();
    }
    if let Some(doc) = &universe.doc {
        w.comment("//! ", doc);
        w.blank();
    }

    let uses = imports(universe);
    if !uses.is_empty() {
        for (from, names) in &uses {
            w.write(format_args!("use super::{from}::{{{}}};", names.join(", ")));
        }
        w.blank();
    }
    if mentions_map(universe) {
        w.line("use std::collections::BTreeMap;");
        w.blank();
    }

    for def in ordered_defs(universe) {
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

    w.finish()
}

fn imports(universe: &Universe) -> Vec<(String, Vec<String>)> {
    let mut found: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for_each_type(universe, &mut |ty| {
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
    });
    found
        .into_iter()
        .map(|(from, names)| (from, names.into_iter().collect()))
        .collect()
}

/// Whether anything here is a map, at any depth, so the import can be omitted
/// when nothing needs it.
fn mentions_map(universe: &Universe) -> bool {
    fn anywhere(ty: &Type) -> bool {
        matches!(ty, Type::Map { .. }) || ty.children().iter().any(|c| anywhere(c))
    }
    let mut found = false;
    for_each_type(universe, &mut |ty| found |= anywhere(ty));
    found
}

fn for_each_type(universe: &Universe, f: &mut impl FnMut(&Type)) {
    for def in &universe.types {
        match def {
            TypeDef::Struct(d) => d.fields.iter().for_each(|x| f(&x.ty)),
            TypeDef::Interface(d) => d.fields.iter().for_each(|x| f(&x.ty)),
            TypeDef::Enum(d) => d.variants.iter().for_each(|v| f(&v.payload)),
            TypeDef::Alias(d) => f(&d.target),
        }
    }
    for sig in &universe.functions {
        sig.params.iter().for_each(|p| f(&p.ty));
        f(&sig.ret);
    }
}

fn emit_struct(
    w: &mut CodeWriter,
    ws: &CheckedWorkspace,
    universe: &Universe,
    def: &StructDef,
    profile: &LanguageProfile,
) {
    let name = QualifiedName::new(universe.name.clone(), def.name.clone());
    // Every field it has, inherited or not: the wire has no notion of where a
    // field came from, and neither does the reader.
    let fields = ws
        .flat_fields(&name)
        .map(<[Field]>::to_vec)
        .unwrap_or_default();

    if let Some(doc) = &def.doc {
        w.comment("/// ", doc);
    }
    if let Some(parent) = &def.extends {
        w.comment(
            "/// ",
            &format!(
                "Inherits from `{}`. Rust has no inheritance, so the fields are \
                 carried here; the shape on the wire is unchanged.",
                parent.name
            ),
        );
    }
    w.line(DERIVES);

    if fields.is_empty() {
        w.write(format_args!("pub struct {};", profile.name(&def.name)));
    } else {
        w.block(
            format!("pub struct {} {{", profile.name(&def.name)),
            "}",
            |w| emit_fields(w, ws, &fields, profile),
        );
    }

    // What it claims to satisfy becomes what Rust can actually dispatch on.
    for iface in &def.implements {
        w.blank();
        w.block(
            format!(
                "impl {} for {} {{",
                profile.name(&iface.name),
                profile.name(&def.name)
            ),
            "}",
            |w| {
                let required = ws
                    .fields_by_name(&QualifiedName::new(
                        iface.universe.clone().unwrap_or(universe.name.clone()),
                        iface.name.clone(),
                    ))
                    .unwrap_or_default();
                for (i, field) in required.iter().enumerate() {
                    if i > 0 {
                        w.blank();
                    }
                    let spelled = profile.spell(&field.ty);
                    w.block(
                        format!("fn {}(&self) -> &{spelled} {{", profile.name(&field.name)),
                        "}",
                        |w| w.write(format_args!("&self.{}", profile.name(&field.name))),
                    );
                }
            },
        );
    }
}

fn emit_fields(
    w: &mut CodeWriter,
    ws: &CheckedWorkspace,
    fields: &[Field],
    profile: &LanguageProfile,
) {
    for field in fields {
        if let Some(doc) = &field.doc {
            w.comment("/// ", doc);
        }
        for caveat in profile.caveats(&field.ty) {
            w.comment("// ", caveat);
        }

        let renamed = profile.name(&field.name) != field.name.as_str();
        if renamed {
            // The escape keeps the name usable in Rust; the rename keeps it
            // correct on the wire.
            w.write(format_args!("#[serde(rename = {:?})]", field.name.as_str()));
        }
        // Absent and null are one state, so an absent field neither has to be
        // sent nor has to arrive. What may be absent is what the canonical
        // form leaves out: anything that expands to `T?`, `null` or `any`. A
        // `()` holds nothing else, so it is never sent at all.
        let skip = match ws.expand_alias(&field.ty) {
            Type::Optional { .. } => {
                Some("#[serde(default, skip_serializing_if = \"Option::is_none\")]")
            }
            Type::Any => {
                Some("#[serde(default, skip_serializing_if = \"serde_json::Value::is_null\")]")
            }
            Type::Unit => Some("#[serde(default, skip_serializing)]"),
            _ => None,
        };
        if let Some(attribute) = skip {
            w.line(attribute);
        }
        w.write(format_args!(
            "pub {}: {},",
            profile.name(&field.name),
            profile.spell(&field.ty)
        ));
    }
}

fn emit_interface(
    w: &mut CodeWriter,
    ws: &CheckedWorkspace,
    universe: &Universe,
    def: &InterfaceDef,
    profile: &LanguageProfile,
) {
    if let Some(doc) = &def.doc {
        w.comment("/// ", doc);
    }
    let name = QualifiedName::new(universe.name.clone(), def.name.clone());
    let fields = ws
        .flat_fields(&name)
        .map(<[Field]>::to_vec)
        .unwrap_or_default();

    // A field contract is a trait of accessors: the closest thing Rust has to
    // "anything with these fields".
    if fields.is_empty() {
        w.write(format_args!("pub trait {} {{}}", profile.name(&def.name)));
        return;
    }
    w.block(
        format!("pub trait {} {{", profile.name(&def.name)),
        "}",
        |w| {
            for (i, field) in fields.iter().enumerate() {
                if i > 0 {
                    w.blank();
                }
                if let Some(doc) = &field.doc {
                    w.comment("/// ", doc);
                }
                w.write(format_args!(
                    "fn {}(&self) -> &{};",
                    profile.name(&field.name),
                    profile.spell(&field.ty)
                ));
            }
        },
    );
}

fn emit_enum(w: &mut CodeWriter, def: &EnumDef, profile: &LanguageProfile) {
    if let Some(doc) = &def.doc {
        w.comment("/// ", doc);
    }
    w.line(DERIVES);
    // Adjacent tagging is exactly the wire form: a tag beside a payload.
    w.line("#[serde(tag = \"$tag\", content = \"value\")]");
    w.block(
        format!("pub enum {} {{", profile.name(&def.name)),
        "}",
        |w| {
            for variant in &def.variants {
                if let Some(doc) = &variant.doc {
                    w.comment("/// ", doc);
                }
                if matches!(variant.payload, Type::Unit) {
                    w.write(format_args!("{},", variant.name));
                } else {
                    w.write(format_args!(
                        "{}({}),",
                        variant.name,
                        profile.spell(&variant.payload)
                    ));
                }
            }
        },
    );
}

fn emit_alias(w: &mut CodeWriter, def: &AliasDef, profile: &LanguageProfile) {
    if let Some(doc) = &def.doc {
        w.comment("/// ", doc);
    }
    match &def.target {
        // A union of nothing but strings is a set of names, and Rust has a
        // word for that: an enum whose variants carry nothing, each one
        // travelling as the string it stands for. Anything else would accept
        // every string there is.
        Type::Union { members } if members.iter().all(is_string_literal) => {
            let mut taken = BTreeSet::new();
            w.line(DERIVES);
            w.block(
                format!("pub enum {} {{", profile.name(&def.name)),
                "}",
                |w| {
                    for member in members {
                        let Type::Literal {
                            value: LiteralValue::Str(text),
                        } = member
                        else {
                            continue;
                        };
                        let case = distinct(named_after(text), &mut taken);
                        w.write(format_args!("#[serde(rename = {text:?})]"));
                        w.write(format_args!("{case},"));
                    }
                },
            );
        }
        // Any other untagged union has to be a real enum in Rust, tried in
        // order.
        Type::Union { members } => {
            let mut taken = BTreeSet::new();
            w.line(DERIVES);
            w.line("#[serde(untagged)]");
            w.block(
                format!("pub enum {} {{", profile.name(&def.name)),
                "}",
                |w| {
                    for member in members {
                        let spelled = profile.spell(member);
                        // Two arms can spell alike — two literals of one base
                        // type do — and an enum cannot name two variants the
                        // same thing.
                        let case = distinct(variant_name(member, &spelled), &mut taken);
                        w.write(format_args!("{case}({spelled}),"));
                    }
                },
            );
        }
        other => w.write(format_args!(
            "pub type {} = {};",
            profile.name(&def.name),
            profile.spell(other)
        )),
    }
}

fn is_string_literal(ty: &Type) -> bool {
    matches!(
        ty,
        Type::Literal {
            value: LiteralValue::Str(_)
        }
    )
}

/// A variant name for a string a union admits.
///
/// The string itself goes on the wire, by way of a rename, so this only has to
/// be something Rust will accept and a reader will recognise.
fn named_after(text: &str) -> String {
    let name = text.to_upper_camel_case();
    match name.chars().next() {
        None => "Empty".to_string(),
        Some(first) if first.is_ascii_alphabetic() => name,
        Some(_) => format!("V{name}"),
    }
}

/// A name nothing else in the same enum has taken, numbered if it has to be.
fn distinct(name: String, taken: &mut BTreeSet<String>) -> String {
    let mut candidate = name.clone();
    let mut n = 2;
    while !taken.insert(candidate.clone()) {
        candidate = format!("{name}{n}");
        n += 1;
    }
    candidate
}

/// A variant name for one arm of an untagged union.
fn variant_name(member: &Type, spelled: &str) -> String {
    match member {
        Type::Ref(r) => r.name.to_string(),
        _ => {
            let mut out = String::new();
            let mut capitalize = true;
            for c in spelled.chars() {
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
    }
}

fn emit_signature(
    w: &mut CodeWriter,
    ws: &CheckedWorkspace,
    universe: &Universe,
    sig: &FunctionSig,
    profile: &LanguageProfile,
) {
    if let Some(doc) = &sig.doc {
        w.comment("/// ", doc);
    }
    let params: Vec<String> = sig
        .params
        .iter()
        .map(|p| format!("{}: {}", profile.name(&p.name), profile.spell(&p.ty)))
        .collect();
    let qualified = QualifiedName::new(universe.name.clone(), sig.name.clone());

    // A future in the return type is what `async` means here.
    let (keyword, ret) = match &sig.ret {
        Type::Future { of } => ("pub async fn", profile.spell(of)),
        other => ("pub fn", profile.spell(other)),
    };
    let head = format!(
        "{keyword} {}({}) -> {ret} {{",
        profile.name(&sig.name),
        params.join(", ")
    );

    match &sig.body {
        Some(body) => {
            let env: Env = sig
                .params
                .iter()
                .map(|p| (p.name.clone(), p.ty.clone()))
                .collect();
            let text = Body { ws, profile }.expression(body, &env, Some(&sig.ret));
            w.block(head, "}", |w| w.line(text));
        }
        None => {
            w.comment(
                "/// ",
                &format!("Implement this and register it as `{qualified}`."),
            );
            w.block(head, "}", |w| {
                w.write(format_args!("todo!({:?})", qualified.to_string()));
            });
        }
    }
}

/// What each name in scope holds, at some point in a body.
type Env = HashMap<Ident, Type>;

/// Writing a body as Rust.
///
/// The checker takes a `T` wherever a `T?` is wanted, because a value that is
/// there is one of the things a value that may be absent can be. Python and
/// TypeScript agree and write nothing for it. Rust does not: an `i64` is not an
/// `Option<i64>` until it is put in `Some`. So writing a body takes knowing what
/// each part of it produces and what the place it stands in wants, which the
/// workspace can say. Wherever nothing needs saying, the body comes out as it
/// would have without it.
struct Body<'a> {
    ws: &'a CheckedWorkspace,
    profile: &'a LanguageProfile,
}

impl Body<'_> {
    /// Writes `expr`, where `env` says what each name holds and `wanted` is
    /// what the place it stands in expects, when that place says.
    fn expression(&self, expr: &Expr, env: &Env, wanted: Option<&Type>) -> String {
        let profile = self.profile;
        match expr {
            // A binding, a conditional, and taking a value apart each hand on
            // what is wanted to whatever produces their result, so `Some` goes
            // on the one arm that needs it rather than round all of them.
            Expr::Let { name, value, body } => {
                let mut inner = env.clone();
                if let Some(bound) = self.ws.type_of_expr(value, env) {
                    inner.insert(name.clone(), bound);
                }
                format!(
                    "{{ let {} = {}; {} }}",
                    profile.name(name),
                    self.expression(value, env, None),
                    self.expression(body, &inner, wanted)
                )
            }
            Expr::If {
                cond,
                then,
                otherwise,
            } => {
                let arms = self.shared(wanted, expr, env);
                format!(
                    "if {} {{ {} }} else {{ {} }}",
                    self.expression(cond, env, None),
                    self.expression(then, env, arms.as_ref()),
                    self.expression(otherwise, env, arms.as_ref())
                )
            }
            Expr::Case {
                value,
                var,
                some,
                none,
            } => {
                let arms = self.shared(wanted, expr, env);
                let mut present = env.clone();
                let inside =
                    self.ws
                        .type_of_expr(value, env)
                        .map(|t| match self.ws.expand_alias(&t) {
                            Type::Optional { of } => *of,
                            // Only `null` gets here, so the present arm binds
                            // something that is never there.
                            _ => Type::Never,
                        });
                if let Some(inside) = inside {
                    present.insert(var.clone(), inside);
                }
                format!(
                    "match {} {{ Some({}) => {}, None => {} }}",
                    self.expression(value, env, None),
                    profile.name(var),
                    self.expression(some, &present, arms.as_ref()),
                    self.expression(none, env, arms.as_ref())
                )
            }
            Expr::Match { ty, value, arms } => {
                let wanted = self.shared(wanted, expr, env);
                let variants = self.ws.variants_of(ty).unwrap_or_default();
                let name = profile.name(&ty.name);
                let arms: Vec<String> = arms
                    .iter()
                    .map(|arm| {
                        let mut inner = env.clone();
                        let payload = variants.iter().find(|v| v.name == arm.tag);
                        if let (Some(var), Some(variant)) = (&arm.var, payload) {
                            inner.insert(var.clone(), variant.payload.clone());
                        }
                        let body = self.expression(&arm.body, &inner, wanted.as_ref());
                        match &arm.var {
                            Some(var) => {
                                format!("{name}::{}({}) => {body}", arm.tag, profile.name(var))
                            }
                            // Braces and `..` match a variant whatever it
                            // carries, so an arm that ignores the payload need
                            // not know whether there is one.
                            None => format!("{name}::{} {{ .. }} => {body}", arm.tag),
                        }
                    })
                    .collect();
                format!(
                    "match {} {{ {} }}",
                    self.expression(value, env, None),
                    arms.join(", ")
                )
            }
            _ => {
                // What is wanted of the value itself, once it is in `Some`.
                let within = wanted.map(|w| match self.ws.expand_alias(w) {
                    Type::Optional { of } => self.ws.expand_alias(&of),
                    other => other,
                });
                let text = self.value(expr, env, within.as_ref());
                if self.needs_some(expr, env, wanted) {
                    format!("Some({text})")
                } else {
                    text
                }
            }
        }
    }

    /// Writes an expression that is not a branch, where `wanted` is what is
    /// expected of it with any `Option` round it already accounted for.
    fn value(&self, expr: &Expr, env: &Env, wanted: Option<&Type>) -> String {
        let profile = self.profile;
        match expr {
            Expr::Lit { value } => {
                let target = wanted.cloned().or_else(|| self.ws.type_of_expr(expr, env));
                self.constant(value, target.as_ref())
            }
            Expr::Var { name } => profile.name(name),
            Expr::Field { of, name } => {
                // Cloned because a body produces a value rather than borrowing
                // one, and the alternative is deciding lifetimes for generated
                // code.
                format!(
                    "{}.{}.clone()",
                    self.expression(of, env, None),
                    profile.name(name)
                )
            }
            Expr::MakeRecord { ty, fields } => {
                let declared = self.ws.fields_of(ty).unwrap_or_default();
                let mut parts: Vec<String> = fields
                    .iter()
                    .map(|f| {
                        let field = declared.iter().find(|d| d.name == f.name);
                        format!(
                            "{}: {}",
                            profile.name(&f.name),
                            self.expression(&f.value, env, field.map(|d| &d.ty))
                        )
                    })
                    .collect();
                // The checker lets a body leave out a field that may be
                // absent, but a Rust struct literal names every field it has.
                // A field left out is one that is not there, which is `null`
                // written as that field wants it.
                parts.extend(
                    declared
                        .iter()
                        .filter(|d| !fields.iter().any(|f| f.name == d.name))
                        .filter(|d| {
                            matches!(
                                self.ws.expand_alias(&d.ty),
                                Type::Optional { .. } | Type::Unit | Type::Any
                            )
                        })
                        .map(|d| {
                            format!(
                                "{}: {}",
                                profile.name(&d.name),
                                self.constant(&Value::Null, Some(&d.ty))
                            )
                        }),
                );
                format!("{} {{ {} }}", profile.name(&ty.name), parts.join(", "))
            }
            Expr::MakeList { items } => {
                // Every element of a `Vec` is one type, so if one of them may
                // be absent they all have to be written as though they might.
                let elements = match wanted {
                    Some(Type::List { of }) if self.may_be_absent(of) => Some(of.as_ref().clone()),
                    _ => self.ws.type_of_expr(expr, env).and_then(|t| {
                        match self.ws.expand_alias(&t) {
                            Type::List { of } => Some(*of),
                            _ => None,
                        }
                    }),
                };
                let parts: Vec<String> = items
                    .iter()
                    .map(|i| self.expression(i, env, elements.as_ref()))
                    .collect();
                format!("vec![{}]", parts.join(", "))
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
                if parts.len() == 1 {
                    format!("({},)", parts[0])
                } else {
                    format!("({})", parts.join(", "))
                }
            }
            Expr::MakeVariant { ty, tag, value } => match value {
                Some(v) => {
                    let payload = self
                        .ws
                        .variants_of(ty)
                        .and_then(|vs| vs.into_iter().find(|x| &x.name == tag))
                        .map(|x| x.payload);
                    format!(
                        "{}::{tag}({})",
                        profile.name(&ty.name),
                        self.expression(v, env, payload.as_ref())
                    )
                }
                None => format!("{}::{tag}", profile.name(&ty.name)),
            },
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
                format!("{}({})", profile.name(&sig.name), parts.join(", "))
            }
            Expr::MapThrough {
                functor,
                value,
                var,
                body,
            } => {
                use unitype_core::ir::Functor;
                let mut inner = env.clone();
                let subject_ty = self.ws.type_of_expr(value, env);
                if let Some(subject_ty) = subject_ty {
                    if let Some((_, inside)) = Functor::split(&self.ws.expand_alias(&subject_ty)) {
                        inner.insert(var.clone(), inside.clone());
                    }
                }
                // What is wanted of each thing inside is whatever is wanted
                // inside the result. For an optional that is already what is
                // wanted here, since the `Option` round it is the map's own.
                let each = match functor {
                    Functor::Optional => wanted.cloned(),
                    _ => wanted.and_then(|w| match Functor::split(w) {
                        Some((f, inside)) if f == *functor => Some(inside.clone()),
                        _ => None,
                    }),
                };
                let subject = self.expression(value, env, None);
                let bound = profile.name(var);
                let inner = self.expression(body, &inner, each.as_ref());
                match functor {
                    Functor::List => {
                        format!("{subject}.into_iter().map(|{bound}| {inner}).collect()")
                    }
                    // Absent stays absent, which Rust already has a word for.
                    Functor::Optional => format!("{subject}.map(|{bound}| {inner})"),
                    Functor::Future => format!("{{ let {bound} = {subject}.await; {inner} }}"),
                }
            }
            Expr::Prim { op, args } => {
                // Rust compares an `Option` only with another, so a value
                // compared with something that may be absent is put in `Some`
                // to be compared at all.
                let others: Vec<Option<Type>> = match (op, args.as_slice()) {
                    (PrimOp::Eq | PrimOp::Ne, [x, y]) => {
                        vec![self.ws.type_of_expr(y, env), self.ws.type_of_expr(x, env)]
                    }
                    _ => vec![None; args.len()],
                };
                let operands: Vec<String> = args
                    .iter()
                    .zip(&others)
                    .map(|(a, other)| self.expression(a, env, other.as_ref()))
                    .collect();
                match (op, operands.as_slice()) {
                    (PrimOp::Neg, [x]) => format!("(-{x})"),
                    (PrimOp::Not, [x]) => format!("(!{x})"),
                    // A length is a `usize` in Rust and an `i64` everywhere
                    // else here.
                    (PrimOp::Len, [x]) => format!("({x}.len() as i64)"),
                    (op, [x, y]) => format!("({x} {} {y})", infix(*op)),
                    _ => unreachable!("the checker holds every operation to its arity"),
                }
            }
            // Borrowed and cloned element by element, so the list is still
            // there for anything else in the body that mentions it.
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
                let element =
                    self.ws
                        .type_of_expr(list, env)
                        .and_then(|t| match self.ws.expand_alias(&t) {
                            Type::List { of } => Some(*of),
                            _ => None,
                        });
                if let Some(element) = element {
                    inner.insert(item.clone(), element);
                }
                format!(
                    "{}.iter().cloned().fold({}, |{}, {}| {})",
                    self.expression(list, env, None),
                    self.expression(init, env, carried.as_ref()),
                    profile.name(acc),
                    profile.name(item),
                    self.expression(body, &inner, carried.as_ref())
                )
            }
            Expr::Let { .. } | Expr::If { .. } | Expr::Case { .. } | Expr::Match { .. } => {
                unreachable!("a branch is written by `expression`, which hands on what is wanted")
            }
        }
    }

    /// Writes a constant as `wanted` has it.
    ///
    /// [`literal`] has only the value to go on, so `[1, null]` comes out as
    /// `vec![1, None]`, which is two types to Rust. Where a `[i64?]` is
    /// wanted, the `1` is one that is there: `vec![Some(1), None]`. Whatever
    /// this has nothing to add to is written as [`literal`] writes it.
    fn constant(&self, value: &Value, wanted: Option<&Type>) -> String {
        let Some(wanted) = wanted else {
            return literal(value);
        };
        match (self.ws.expand_alias(wanted), value) {
            // `null` is the one value its type holds, and Rust spells both `()`.
            (Type::Unit, Value::Null) => "()".into(),
            // unitype takes an `any` that is absent and one that is null to
            // be the same state, and `serde_json::Value` has no way to be
            // absent but its `Null`.
            (Type::Any, Value::Null) => "serde_json::Value::Null".into(),
            (Type::Optional { .. }, Value::Null) => literal(value),
            (Type::Optional { of }, present) => {
                format!("Some({})", self.constant(present, Some(&of)))
            }
            (Type::List { of }, Value::List(items)) => {
                let parts: Vec<String> =
                    items.iter().map(|i| self.constant(i, Some(&of))).collect();
                format!("vec![{}]", parts.join(", "))
            }
            (Type::Tuple { items: slots }, Value::Tuple(items)) if slots.len() == items.len() => {
                let parts: Vec<String> = items
                    .iter()
                    .zip(&slots)
                    .map(|(i, slot)| self.constant(i, Some(slot)))
                    .collect();
                if parts.len() == 1 {
                    format!("({},)", parts[0])
                } else {
                    format!("({})", parts.join(", "))
                }
            }
            (Type::Map { key, value: of }, Value::Map(entries)) => {
                let parts: Vec<String> = entries
                    .iter()
                    .map(|(k, v)| {
                        format!(
                            "({}, {})",
                            self.constant(k, Some(&key)),
                            self.constant(v, Some(&of))
                        )
                    })
                    .collect();
                format!("BTreeMap::from([{}])", parts.join(", "))
            }
            _ => literal(value),
        }
    }

    /// What several alternatives are each written as, so that they come out as
    /// one Rust type: what the place wants, if that may be absent, and
    /// otherwise what they come to together.
    fn shared(&self, wanted: Option<&Type>, expr: &Expr, env: &Env) -> Option<Type> {
        match wanted {
            Some(w) if self.may_be_absent(w) => Some(w.clone()),
            _ => self.ws.type_of_expr(expr, env),
        }
    }

    /// Whether a type is one Rust writes as an `Option`.
    fn may_be_absent(&self, ty: &Type) -> bool {
        matches!(self.ws.expand_alias(ty), Type::Optional { .. })
    }

    /// Whether what `expr` produces has to be put in `Some` to be what is
    /// wanted.
    ///
    /// Something that may itself be absent is already an `Option`, and `null`
    /// is written `None`; anything else, where something that may be absent is
    /// wanted, has to be said to be there.
    fn needs_some(&self, expr: &Expr, env: &Env, wanted: Option<&Type>) -> bool {
        if !wanted.is_some_and(|w| self.may_be_absent(w)) {
            return false;
        }
        self.ws.type_of_expr(expr, env).is_some_and(|produced| {
            !matches!(
                self.ws.expand_alias(&produced),
                Type::Optional { .. } | Type::Unit
            )
        })
    }
}

/// How Rust writes a two-operand operation.
fn infix(op: PrimOp) -> &'static str {
    match op {
        PrimOp::Eq => "==",
        PrimOp::Ne => "!=",
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

/// Writes a value as a Rust literal.
pub fn literal(value: &Value) -> String {
    match value {
        Value::Null => "None".into(),
        Value::Bool(b) => b.to_string(),
        Value::Int(i) => i.to_string(),
        Value::Float(f) => {
            let plain = f.to_string();
            if plain.contains(['.', 'e', 'E']) {
                plain
            } else {
                format!("{plain}.0")
            }
        }
        Value::Str(s) => format!("{s:?}.to_string()"),
        Value::Bytes(b) => {
            let parts: Vec<String> = b.iter().map(|x| x.to_string()).collect();
            format!("vec![{}]", parts.join(", "))
        }
        Value::List(items) => {
            let parts: Vec<String> = items.iter().map(literal).collect();
            format!("vec![{}]", parts.join(", "))
        }
        Value::Tuple(items) => {
            let parts: Vec<String> = items.iter().map(literal).collect();
            if parts.len() == 1 {
                format!("({},)", parts[0])
            } else {
                format!("({})", parts.join(", "))
            }
        }
        // An anonymous record has no declared type to name here, so it is
        // written as the map it is on the wire.
        Value::Record(fields) => {
            let parts: Vec<String> = fields
                .iter()
                .map(|(k, v)| format!("({k:?}.to_string(), {})", literal(v)))
                .collect();
            format!("BTreeMap::from([{}])", parts.join(", "))
        }
        Value::Map(entries) => {
            let parts: Vec<String> = entries
                .iter()
                .map(|(k, v)| format!("({}, {})", literal(k), literal(v)))
                .collect();
            format!("BTreeMap::from([{}])", parts.join(", "))
        }
        Value::Variant { tag, value } => match value.as_ref() {
            Value::Null => tag.to_string(),
            payload => format!("{tag}({})", literal(payload)),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use unitype_core::Workspace;
    use unitype_core::check::check_workspace;

    fn generate(json: &str) -> String {
        let ws = check_workspace(Workspace::from_json(json).expect("reads")).expect("checks");
        Rust.generate(&ws, &GenOptions::default())
            .expect("generates")
            .into_iter()
            .find(|f| f.path.to_string_lossy().ends_with("demo.rs"))
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
        {"k":"struct","name":"Rect","extends":"Shape","fields":[
            {"name":"kind","ty":{"k":"literal","value":{"str":"rect"}}},
            {"name":"w","ty":"float"}]},
        {"k":"alias","name":"AnyShape","target":{"k":"union","members":["Circle","Rect"]}},
        {"k":"enum","name":"Colour","variants":[{"name":"Red"},{"name":"Custom","payload":"str"}]}],
      "functions":[
        {"name":"area","params":[{"name":"s","ty":"Circle"}],"ret":"float"},
        {"name":"load","params":[{"name":"id","ty":"str"}],"ret":{"k":"future","of":"Shape"}}]}]}"#;

    #[test]
    fn a_struct_carries_every_field_it_has() {
        let out = generate(SHAPES);
        let circle = out
            .split("pub struct Circle {")
            .nth(1)
            .expect("declared")
            .split('}')
            .next()
            .expect("closed");
        assert!(
            circle.contains("pub id: String"),
            "the inherited field is right here, not one level down: {circle}"
        );
        assert!(circle.contains("pub radius: f64"), "{circle}");
    }

    #[test]
    fn the_flattening_is_explained_rather_than_left_to_be_discovered() {
        let out = generate(SHAPES);
        assert!(out.contains("Rust has no inheritance"), "{out}");
    }

    #[test]
    fn an_interface_becomes_a_trait_and_the_struct_implements_it() {
        let out = generate(SHAPES);
        assert!(out.contains("pub trait Identified {"), "{out}");
        assert!(out.contains("fn id(&self) -> &String;"), "{out}");
        assert!(out.contains("impl Identified for Shape {"), "{out}");
    }

    #[test]
    fn a_field_that_may_be_absent_need_not_be_sent_or_arrive() {
        let out = generate(SHAPES);
        assert!(
            out.contains("#[serde(default, skip_serializing_if = \"Option::is_none\")]"),
            "{out}"
        );
        assert!(out.contains("pub note: Option<String>,"), "{out}");
    }

    #[test]
    fn an_enum_is_tagged_the_way_the_wire_tags_it() {
        let out = generate(SHAPES);
        assert!(
            out.contains(r#"#[serde(tag = "$tag", content = "value")]"#),
            "{out}"
        );
        assert!(out.contains("Custom(String),"), "{out}");
    }

    #[test]
    fn an_alias_to_a_union_becomes_an_untagged_enum() {
        let out = generate(SHAPES);
        assert!(out.contains("#[serde(untagged)]"), "{out}");
        assert!(out.contains("pub enum AnyShape {"), "{out}");
        assert!(out.contains("Circle(Circle),"), "{out}");
    }

    #[test]
    fn a_union_of_strings_becomes_the_names_it_admits() {
        let out = generate(
            r#"{"universes":[{"name":"demo","types":[
                {"k":"alias","name":"Method","target":{"k":"union","members":[
                    {"k":"literal","value":{"str":"GET"}},
                    {"k":"literal","value":{"str":"POST"}},
                    {"k":"literal","value":{"str":"get"}},
                    {"k":"literal","value":{"str":"2xx"}}]}}]}]}"#,
        );
        assert!(
            out.contains("#[serde(rename = \"GET\")]\n    Get,"),
            "{out}"
        );
        assert!(
            out.contains("#[serde(rename = \"POST\")]\n    Post,"),
            "{out}"
        );
        assert!(
            out.contains("#[serde(rename = \"get\")]\n    Get2,"),
            "two strings that fold to one name still need two variants: {out}"
        );
        assert!(
            out.contains("V2xx,"),
            "a name cannot start with a digit: {out}"
        );
        assert!(
            !out.contains("untagged"),
            "untagged over `String` would accept every string there is: {out}"
        );
    }

    #[test]
    fn arms_that_spell_alike_still_get_names_of_their_own() {
        let out = generate(
            r#"{"universes":[{"name":"demo","types":[
                {"k":"alias","name":"Level","target":{"k":"union","members":[
                    {"k":"literal","value":{"int":1}},
                    {"k":"literal","value":{"int":2}},
                    "str"]}}]}]}"#,
        );
        assert!(out.contains("I64(i64),"), "{out}");
        assert!(out.contains("I642(i64),"), "{out}");
        assert!(out.contains("String(String),"), "{out}");
    }

    #[test]
    fn a_promise_in_the_return_type_is_what_async_means() {
        let out = generate(SHAPES);
        assert!(
            out.contains("pub async fn load(id: String) -> Shape {"),
            "{out}"
        );
    }

    #[test]
    fn a_proposition_with_no_proof_leaves_a_hole_that_will_not_compile_quietly() {
        let out = generate(SHAPES);
        assert!(out.contains("todo!(\"demo::area\")"), "{out}");
    }

    #[test]
    fn a_body_is_written_out_as_rust() {
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
            out.contains("Badge { label: u.first.clone(), sides: 4 }"),
            "one body, three languages: {out}"
        );
    }

    #[test]
    fn mapping_through_absence_is_what_rust_already_calls_it() {
        let out = generate(
            r#"{"universes":[{"name":"demo","functions":[
                {"name":"trimmed","params":[{"name":"s","ty":"str?"}],"ret":"str?",
                 "body":{"k":"map_through","functor":"optional","value":{"k":"var","name":"s"},
                         "var":"x","body":{"k":"var","name":"x"}}}]}]}"#,
        );
        assert!(out.contains("s.map(|x| x)"), "{out}");
    }

    /// Bodies that each put a value where one that may be absent is wanted, or
    /// leave one out.
    const ABSENT: &str = r#"{"universes":[{"name":"demo","types":[
        {"k":"struct","name":"Mark","fields":[
            {"name":"at","ty":"i64"},{"name":"before","ty":"i64?"}]},
        {"k":"enum","name":"Size","variants":[{"name":"small"},{"name":"large","payload":"i64"}]},
        {"k":"alias","name":"Later","target":"i64?"},
        {"k":"struct","name":"Entry","extends":"Mark","fields":[
            {"name":"why","ty":"str?"},{"name":"when","ty":"Later"},
            {"name":"extra","ty":"any"},{"name":"nothing","ty":"null"}]}],
      "functions":[
        {"name":"entry","params":[{"name":"x","ty":"i64"}],"ret":"Entry",
         "body":{"k":"make_record","ty":"Entry","fields":[
            {"name":"why","value":{"k":"lit","value":"w"}},
            {"name":"at","value":{"k":"var","name":"x"}}]}},
        {"name":"gaps","params":[],"ret":"[i64?]","body":{"k":"lit","value":[1,null]}},
        {"name":"nested","params":[],"ret":"[[i64?]]","body":{"k":"lit","value":[[1,null],[2]]}},
        {"name":"pair","params":[],"ret":{"k":"tuple","items":["i64?","i64?","str"]},
         "body":{"k":"lit","value":{"$tuple":[3,null,"x"]}}},
        {"name":"counts","params":[],"ret":{"k":"map","key":"str","value":"i64?"},
         "body":{"k":"lit","value":{"$map":[["a",1],["b",null]]}}},
        {"name":"gap_count","params":[],"ret":"i64",
         "body":{"k":"let","name":"g","value":{"k":"lit","value":[1,null]},
                 "body":{"k":"prim","op":"len","args":[{"k":"var","name":"g"}]}}},
        {"name":"mark","params":[{"name":"x","ty":"i64"}],"ret":"Mark",
         "body":{"k":"make_record","ty":"Mark","fields":[
            {"name":"at","value":{"k":"var","name":"x"}},
            {"name":"before","value":{"k":"var","name":"x"}}]}},
        {"name":"last_of","params":[{"name":"xs","ty":"[i64]"}],"ret":"i64?",
         "body":{"k":"fold","list":{"k":"var","name":"xs"},"init":{"k":"lit","value":null},
                 "ty":"int?","acc":"p","item":"x","body":{"k":"var","name":"x"}}},
        {"name":"some_of","params":[{"name":"x","ty":"i64"}],"ret":"i64?",
         "body":{"k":"var","name":"x"}},
        {"name":"positive","params":[{"name":"x","ty":"i64"}],"ret":"i64?",
         "body":{"k":"if","cond":{"k":"prim","op":"gt","args":[
                     {"k":"var","name":"x"},{"k":"lit","value":0}]},
                 "then":{"k":"var","name":"x"},"else":{"k":"lit","value":null}}},
        {"name":"or_else","params":[{"name":"x","ty":"i64?"},{"name":"y","ty":"i64?"}],
         "ret":"i64?",
         "body":{"k":"case","value":{"k":"var","name":"x"},"var":"n",
                 "some":{"k":"var","name":"n"},"none":{"k":"var","name":"y"}}},
        {"name":"hint","params":[{"name":"s","ty":"Size"}],"ret":"i64?",
         "body":{"k":"match","ty":"Size","value":{"k":"var","name":"s"},"arms":[
            {"tag":"small","body":{"k":"lit","value":null}},
            {"tag":"large","var":"n","body":{"k":"var","name":"n"}}]}},
        {"name":"chosen","params":[{"name":"c","ty":"bool"},{"name":"x","ty":"i64"}],"ret":"i64",
         "body":{"k":"let","name":"y",
                 "value":{"k":"if","cond":{"k":"var","name":"c"},
                          "then":{"k":"var","name":"x"},"else":{"k":"lit","value":null}},
                 "body":{"k":"case","value":{"k":"var","name":"y"},"var":"n",
                         "some":{"k":"var","name":"n"},"none":{"k":"lit","value":0}}}},
        {"name":"is","params":[{"name":"x","ty":"i64?"},{"name":"y","ty":"i64"}],"ret":"bool",
         "body":{"k":"prim","op":"eq","args":[{"k":"var","name":"x"},{"k":"var","name":"y"}]}}]}]}"#;

    #[test]
    fn a_value_where_one_that_may_be_absent_is_wanted_is_put_in_some() {
        let out = generate(ABSENT);
        assert!(out.contains("Mark { at: x, before: Some(x) }"), "{out}");
        assert!(
            out.contains("xs.iter().cloned().fold(None, |p, x| Some(x))"),
            "the starting value and each step are both what is carried: {out}"
        );
        assert!(
            out.contains("pub fn some_of(x: i64) -> Option<i64> {\n    Some(x)\n}"),
            "{out}"
        );
    }

    #[test]
    fn only_the_arm_that_needs_it_is_put_in_some() {
        let out = generate(ABSENT);
        assert!(
            out.contains("if (x > 0) { Some(x) } else { None }"),
            "{out}"
        );
        assert!(
            out.contains("match x { Some(n) => Some(n), None => y }"),
            "an arm that may already be absent is an `Option` already: {out}"
        );
        assert!(
            out.contains("match s { Size::small { .. } => None, Size::large(n) => Some(n) }"),
            "{out}"
        );
        assert!(!out.contains("Some(None)"), "{out}");
    }

    #[test]
    fn arms_nothing_outside_asks_about_are_written_to_what_they_come_to_together() {
        let out = generate(ABSENT);
        assert!(
            out.contains("let y = if c { Some(x) } else { None };"),
            "one arm may be absent, so both are `Option`s: {out}"
        );
        assert!(out.contains("match y { Some(n) => n, None => 0 }"), "{out}");
    }

    #[test]
    fn a_value_compared_with_one_that_may_be_absent_is_compared_as_an_option() {
        let out = generate(ABSENT);
        assert!(out.contains("(x == Some(y))"), "{out}");
    }

    #[test]
    fn a_field_left_out_is_written_as_the_nothing_it_stands_for() {
        let out = generate(ABSENT);
        assert!(
            out.contains(
                "Entry { why: Some(\"w\".to_string()), at: x, before: None, when: None, \
                 extra: serde_json::Value::Null, nothing: () }"
            ),
            "what was given first, as given, then the rest as declared, \
             inherited ones included: {out}"
        );
    }

    #[test]
    fn an_alias_an_any_and_a_null_need_not_be_sent_or_arrive_either() {
        let out = generate(ABSENT);
        assert!(
            out.contains(
                "#[serde(default, skip_serializing_if = \"Option::is_none\")]\n    pub when: Later,"
            ),
            "an alias of an optional is still one: {out}"
        );
        assert!(
            out.contains(
                "#[serde(default, skip_serializing_if = \"serde_json::Value::is_null\")]\n    \
                 pub extra: serde_json::Value,"
            ),
            "{out}"
        );
        assert!(
            out.contains("#[serde(default, skip_serializing)]\n    pub nothing: (),"),
            "a `()` holds nothing else, so it is never sent: {out}"
        );
        assert!(
            out.contains("pub struct Entry {\n    pub at: i64,"),
            "a field that has to be there is still sent and still has to arrive: {out}"
        );
    }

    #[test]
    fn a_constant_is_written_as_what_is_wanted_of_it() {
        let out = generate(ABSENT);
        assert!(out.contains("vec![Some(1), None]\n}"), "{out}");
        assert!(
            out.contains("vec![vec![Some(1), None], vec![Some(2)]]"),
            "a list with nothing absent in it is still a list of what may be: {out}"
        );
        assert!(out.contains("(Some(3), None, \"x\".to_string())"), "{out}");
        assert!(
            out.contains(
                "BTreeMap::from([(\"a\".to_string(), Some(1)), (\"b\".to_string(), None)])"
            ),
            "{out}"
        );
        assert!(
            out.contains("{ let g = vec![Some(1), None]; (g.len() as i64) }"),
            "with nothing asked of it, a constant is what it comes to: {out}"
        );
        assert_eq!(
            literal(&Value::List(vec![Value::Int(1), Value::Null])),
            "vec![1, None]",
            "untyped, there is nothing to say a `1` might not have been there"
        );
    }

    #[test]
    fn a_reserved_word_is_escaped_and_still_travels_under_its_own_name() {
        let out = generate(
            r#"{"universes":[{"name":"demo","types":[
                {"k":"struct","name":"Thing","fields":[{"name":"type","ty":"str"}]}]}]}"#,
        );
        assert!(out.contains("pub r#type: String,"), "{out}");
        assert!(
            out.contains(r#"#[serde(rename = "type")]"#),
            "the escape must not change the wire: {out}"
        );
    }

    #[test]
    fn where_precision_goes_is_written_next_to_nothing_because_rust_holds_it() {
        let out = generate(
            r#"{"universes":[{"name":"demo","types":[
                {"k":"struct","name":"Big","fields":[{"name":"n","ty":"i64"}]}]}]}"#,
        );
        assert!(out.contains("pub n: i64,"), "{out}");
        assert!(
            !out.contains("2^53"),
            "Rust holds it exactly, so there is nothing to warn about: {out}"
        );
    }

    #[test]
    fn generating_twice_gives_the_same_bytes() {
        assert_eq!(generate(SHAPES), generate(SHAPES));
    }

    #[test]
    fn values_become_literals_rust_would_accept() {
        assert_eq!(literal(&Value::Str("hi".into())), "\"hi\".to_string()");
        assert_eq!(literal(&Value::Float(2.0)), "2.0");
        assert_eq!(
            literal(&Value::List(vec![Value::Int(1), Value::Int(2)])),
            "vec![1, 2]"
        );
        assert_eq!(literal(&Value::Null), "None");
    }
}
