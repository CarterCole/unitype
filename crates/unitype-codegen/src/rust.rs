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

use std::collections::{BTreeMap, BTreeSet};

use unitype_core::QualifiedName;
use unitype_core::check::CheckedWorkspace;
use unitype_core::ir::{
    AliasDef, EnumDef, Expr, Field, FunctionSig, InterfaceDef, StructDef, Type, TypeDef, Universe,
};
use unitype_core::lang::{LanguageProfile, profile};
use unitype_core::value::Value;

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
            emit_signature(&mut w, universe, sig, profile);
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
            |w| emit_fields(w, &fields, profile),
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

fn emit_fields(w: &mut CodeWriter, fields: &[Field], profile: &LanguageProfile) {
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
        // sent nor has to arrive.
        if matches!(field.ty, Type::Optional { .. }) {
            w.line("#[serde(default, skip_serializing_if = \"Option::is_none\")]");
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
        // An untagged union has to be a real enum in Rust, tried in order.
        Type::Union { members } => {
            w.line(DERIVES);
            w.line("#[serde(untagged)]");
            w.block(
                format!("pub enum {} {{", profile.name(&def.name)),
                "}",
                |w| {
                    for member in members {
                        let spelled = profile.spell(member);
                        let case = variant_name(member, &spelled);
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
        Some(body) => w.block(head, "}", |w| w.line(expression(body, profile))),
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

/// Writes an expression body as Rust.
pub(crate) fn expression(expr: &Expr, profile: &LanguageProfile) -> String {
    match expr {
        Expr::Lit { value } => literal(value),
        Expr::Var { name } => profile.name(name),
        Expr::Field { of, name } => {
            // Cloned because a body produces a value rather than borrowing one,
            // and the alternative is deciding lifetimes for generated code.
            format!("{}.{}.clone()", expression(of, profile), profile.name(name))
        }
        Expr::MakeRecord { ty, fields } => {
            let parts: Vec<String> = fields
                .iter()
                .map(|f| {
                    format!(
                        "{}: {}",
                        profile.name(&f.name),
                        expression(&f.value, profile)
                    )
                })
                .collect();
            format!("{} {{ {} }}", profile.name(&ty.name), parts.join(", "))
        }
        Expr::MakeList { items } => {
            let parts: Vec<String> = items.iter().map(|i| expression(i, profile)).collect();
            format!("vec![{}]", parts.join(", "))
        }
        Expr::MakeTuple { items } => {
            let parts: Vec<String> = items.iter().map(|i| expression(i, profile)).collect();
            if parts.len() == 1 {
                format!("({},)", parts[0])
            } else {
                format!("({})", parts.join(", "))
            }
        }
        Expr::MakeVariant { ty, tag, value } => match value {
            Some(v) => format!(
                "{}::{tag}({})",
                profile.name(&ty.name),
                expression(v, profile)
            ),
            None => format!("{}::{tag}", profile.name(&ty.name)),
        },
        Expr::Call { sig, args } => {
            let parts: Vec<String> = args.iter().map(|a| expression(a, profile)).collect();
            format!("{}({})", profile.name(&sig.name), parts.join(", "))
        }
        Expr::Let { name, value, body } => format!(
            "{{ let {} = {}; {} }}",
            profile.name(name),
            expression(value, profile),
            expression(body, profile)
        ),
        Expr::If {
            cond,
            then,
            otherwise,
        } => format!(
            "if {} {{ {} }} else {{ {} }}",
            expression(cond, profile),
            expression(then, profile),
            expression(otherwise, profile)
        ),
        Expr::MapThrough {
            functor,
            value,
            var,
            body,
        } => {
            use unitype_core::ir::Functor;
            let subject = expression(value, profile);
            let bound = profile.name(var);
            let inner = expression(body, profile);
            match functor {
                Functor::List => {
                    format!("{subject}.into_iter().map(|{bound}| {inner}).collect()")
                }
                // Absent stays absent, which Rust already has a word for.
                Functor::Optional => format!("{subject}.map(|{bound}| {inner})"),
                Functor::Future => format!("{{ let {bound} = {subject}.await; {inner} }}"),
            }
        }
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
