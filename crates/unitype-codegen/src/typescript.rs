//! Writing a universe out as TypeScript.
//!
//! Structs become interfaces, inheritance becomes `extends`, and an enum
//! becomes a discriminated union tagged the way the wire tags it. Nothing here
//! invents a representation: what a generated interface describes is exactly
//! what arrives over a pipe, which is what lets a TypeScript proof and a Rust
//! proof of the same proposition agree without either being told about the
//! other.

use unitype_core::QualifiedName;
use unitype_core::check::CheckedWorkspace;
use unitype_core::ir::{
    AliasDef, EnumDef, Expr, Field, FunctionSig, InterfaceDef, PrimOp, StructDef, Type, TypeDef,
    Universe,
};
use unitype_core::lang::{LanguageProfile, profile};

use crate::writer::CodeWriter;
use crate::{
    Backend, GenError, GenOptions, GeneratedFile, bodies_use, header, ordered_defs, selected,
};

/// The TypeScript backend.
pub struct TypeScript;

impl Backend for TypeScript {
    fn id(&self) -> &'static str {
        "typescript"
    }

    fn extension(&self) -> &'static str {
        "ts"
    }

    fn generate(
        &self,
        ws: &CheckedWorkspace,
        options: &GenOptions,
    ) -> Result<Vec<GeneratedFile>, GenError> {
        let profile = profile("typescript")?;
        let mut files = Vec::new();

        for universe in selected(ws, options) {
            files.push(GeneratedFile {
                path: format!("typescript/{}.ts", universe.name).into(),
                contents: universe_file(ws, universe, &profile, options),
            });
        }

        if options.harness {
            files.push(GeneratedFile {
                path: "typescript/unitype_harness.ts".into(),
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
        for line in header("//", universe, ws) {
            w.line(line);
        }
        w.blank();
    }
    if let Some(doc) = &universe.doc {
        w.comment("// ", doc);
        w.blank();
    }

    for import in imports(universe) {
        w.write(format_args!(
            "import type {{ {} }} from \"./{}\";",
            import.1.join(", "),
            import.0
        ));
    }
    if !imports(universe).is_empty() {
        w.blank();
    }

    if options.stubs {
        emit_helpers(&mut w, universe);
    }

    for def in ordered_defs(universe) {
        match def {
            TypeDef::Struct(d) => emit_struct(&mut w, d, profile),
            TypeDef::Interface(d) => emit_interface(&mut w, d, profile),
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

/// What this universe needs from others, grouped by where it comes from.
fn imports(universe: &Universe) -> Vec<(String, Vec<String>)> {
    let mut found: std::collections::BTreeMap<String, std::collections::BTreeSet<String>> =
        Default::default();
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
    }
    for sig in &universe.functions {
        sig.params.iter().for_each(|p| note(&p.ty));
        note(&sig.ret);
    }

    found
        .into_iter()
        .map(|(from, names)| (from, names.into_iter().collect()))
        .collect()
}

/// Writes prose as a documentation comment.
///
/// A block comment has to be closed, and has to stay closed: prose that
/// happens to contain `*/` would end the comment early and leave the rest of
/// the sentence to be read as code.
fn jsdoc(w: &mut CodeWriter, text: &str) {
    let safe = text.replace("*/", "*\\/");
    let single = safe.trim();
    if !single.contains('\n') && single.len() < 68 {
        w.write(format_args!("/** {single} */"));
        return;
    }
    w.line("/**");
    w.comment(" * ", &safe);
    w.line(" */");
}

fn emit_struct(w: &mut CodeWriter, def: &StructDef, profile: &LanguageProfile) {
    if let Some(doc) = &def.doc {
        jsdoc(w, doc);
    }
    // A struct's parent and the interfaces it satisfies both become `extends`,
    // since TypeScript interfaces are structural and an interface may extend
    // any number of others.
    let mut parents: Vec<String> = def
        .extends
        .iter()
        .chain(&def.implements)
        .map(|r| profile.name(&r.name))
        .collect();
    parents.dedup();

    let head = if parents.is_empty() {
        format!("export interface {} {{", profile.name(&def.name))
    } else {
        format!(
            "export interface {} extends {} {{",
            profile.name(&def.name),
            parents.join(", ")
        )
    };

    // Only what this declaration adds: TypeScript inherits the rest.
    let own: Vec<&Field> = def.fields.iter().collect();
    if own.is_empty() {
        w.line(head.trim_end_matches(" {").to_string() + " {}");
        return;
    }
    w.block(head, "}", |w| emit_fields(w, &own, profile));
}

fn emit_interface(w: &mut CodeWriter, def: &InterfaceDef, profile: &LanguageProfile) {
    if let Some(doc) = &def.doc {
        jsdoc(w, doc);
    }
    let parents: Vec<String> = def.extends.iter().map(|r| profile.name(&r.name)).collect();
    let head = if parents.is_empty() {
        format!("export interface {} {{", profile.name(&def.name))
    } else {
        format!(
            "export interface {} extends {} {{",
            profile.name(&def.name),
            parents.join(", ")
        )
    };
    let own: Vec<&Field> = def.fields.iter().collect();
    if own.is_empty() {
        w.line(head.trim_end_matches(" {").to_string() + " {}");
        return;
    }
    w.block(head, "}", |w| emit_fields(w, &own, profile));
}

fn emit_fields(w: &mut CodeWriter, fields: &[&Field], profile: &LanguageProfile) {
    for field in fields {
        if let Some(doc) = &field.doc {
            jsdoc(w, doc);
        }
        for caveat in profile.caveats(&field.ty) {
            w.comment("// ", caveat);
        }
        // A field that may be absent is optional *and* nullable, because the
        // wire has one way of being absent and TypeScript has two.
        let optional = matches!(field.ty, Type::Optional { .. });
        w.write(format_args!(
            "{}{}: {};",
            profile.name(&field.name),
            if optional { "?" } else { "" },
            profile.spell(&field.ty)
        ));
    }
}

fn emit_enum(w: &mut CodeWriter, def: &EnumDef, profile: &LanguageProfile) {
    if let Some(doc) = &def.doc {
        jsdoc(w, doc);
    }
    // Tagged the way the wire tags it, so what a reader destructures is what
    // actually arrived.
    w.line(format!("export type {} =", profile.name(&def.name)));
    w.indented(|w| {
        for variant in &def.variants {
            let tag = format!("$tag: {:?}", variant.name.as_str());
            if matches!(variant.payload, Type::Unit) {
                w.write(format_args!("| {{ {tag} }}"));
            } else {
                w.write(format_args!(
                    "| {{ {tag}; value: {} }}",
                    profile.spell(&variant.payload)
                ));
            }
        }
    });
    w.line(";");
}

fn emit_alias(w: &mut CodeWriter, def: &AliasDef, profile: &LanguageProfile) {
    if let Some(doc) = &def.doc {
        jsdoc(w, doc);
    }
    let spelled = match &def.target {
        // A union has no single spelling in the profile, since how it is written
        // depends on the declaration it appears in. Here it is a union type.
        Type::Union { members } => members
            .iter()
            .map(|m| profile.spell(m))
            .collect::<Vec<_>>()
            .join(" | "),
        other => profile.spell(other),
    };
    w.write(format_args!(
        "export type {} = {spelled};",
        profile.name(&def.name)
    ));
}

fn emit_signature(
    w: &mut CodeWriter,
    ws: &CheckedWorkspace,
    universe: &Universe,
    sig: &FunctionSig,
    profile: &LanguageProfile,
) {
    if let Some(doc) = &sig.doc {
        jsdoc(w, doc);
    }

    let params: Vec<String> = sig
        .params
        .iter()
        .map(|p| format!("{}: {}", profile.name(&p.name), profile.spell(&p.ty)))
        .collect();
    let ret = profile.spell(&sig.ret);
    let name = profile.name(&sig.name);

    match &sig.body {
        // A body is a proof already: transpile it rather than leaving a hole.
        Some(body) => {
            w.block(
                format!("export function {name}({}): {ret} {{", params.join(", ")),
                "}",
                |w| {
                    w.write(format_args!(
                        "return {};",
                        crate::typescript::expression(body, profile)
                    ));
                },
            );
        }
        None => {
            w.comment(
                "// ",
                &format!(
                    "A proposition with no proof here. Implement it and register it \
                     as `{}`.",
                    QualifiedName::new(universe.name.clone(), sig.name.clone())
                ),
            );
            w.block(
                format!("export function {name}({}): {ret} {{", params.join(", ")),
                "}",
                |w| {
                    w.write(format_args!(
                        "throw new Error(\"{} is not implemented\");",
                        QualifiedName::new(universe.name.clone(), sig.name.clone())
                    ));
                },
            );
        }
    }
    let _ = ws;
}

/// Comparison as every other backend does it, written out once per module that
/// needs it.
///
/// `===` asks whether two arrays are the same array, and `<` orders strings by
/// UTF-16 unit; a body means the same values, and code point order. Neither
/// helper is exported, and both names are reserved, so a body cannot shadow
/// them.
const EQUAL: &str = r#"function unitypeEqual(a: unknown, b: unknown): boolean {
  if (a === b) return true;
  // Absent and null are one state on the wire, and so here.
  if (a === null || a === undefined || b === null || b === undefined) {
    return (a ?? null) === (b ?? null);
  }
  if (Array.isArray(a) || Array.isArray(b)) {
    return Array.isArray(a) && Array.isArray(b) && a.length === b.length &&
      a.every((x, i) => unitypeEqual(x, b[i]));
  }
  if (typeof a === "object" && typeof b === "object") {
    const x = a as Record<string, unknown>;
    const y = b as Record<string, unknown>;
    return [...new Set([...Object.keys(x), ...Object.keys(y)])].every((k) => unitypeEqual(x[k], y[k]));
  }
  return false;
}"#;

const COMPARE: &str = r#"function unitypeCompare(a: number | string, b: number | string): number {
  if (typeof a === "string" && typeof b === "string") {
    const x = Array.from(a, (c) => c.codePointAt(0) as number);
    const y = Array.from(b, (c) => c.codePointAt(0) as number);
    for (let i = 0; i < Math.min(x.length, y.length); i++) {
      if (x[i] !== y[i]) return x[i] - y[i];
    }
    return x.length - y.length;
  }
  return a < b ? -1 : a > b ? 1 : 0;
}"#;

fn emit_helpers(w: &mut CodeWriter, universe: &Universe) {
    let compares = |wanted: fn(PrimOp) -> bool| {
        bodies_use(
            universe,
            |e| matches!(e, Expr::Prim { op, .. } if wanted(*op)),
        )
    };
    if compares(|op| matches!(op, PrimOp::Eq | PrimOp::Ne)) {
        for line in EQUAL.lines() {
            w.line(line);
        }
        w.blank();
    }
    if compares(PrimOp::is_ordering) {
        for line in COMPARE.lines() {
            w.line(line);
        }
        w.blank();
    }
}

/// Writes an expression body as TypeScript.
///
/// The same body becomes Python and Rust elsewhere. A mapping written once in
/// the symbolic language is three functions, none of which anyone wrote.
pub(crate) fn expression(expr: &Expr, profile: &LanguageProfile) -> String {
    match expr {
        Expr::Lit { value } => crate::typescript::literal(value),
        Expr::Var { name } => profile.name(name),
        Expr::Field { of, name } => {
            format!("{}.{}", expression(of, profile), profile.name(name))
        }
        Expr::MakeRecord { fields, .. } => {
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
            format!("{{ {} }}", parts.join(", "))
        }
        Expr::MakeList { items } | Expr::MakeTuple { items } => {
            let parts: Vec<String> = items.iter().map(|i| expression(i, profile)).collect();
            format!("[{}]", parts.join(", "))
        }
        Expr::MakeVariant { tag, value, .. } => match value {
            Some(v) => format!(
                "{{ $tag: {:?}, value: {} }}",
                tag.as_str(),
                expression(v, profile)
            ),
            None => format!("{{ $tag: {:?} }}", tag.as_str()),
        },
        Expr::Call { sig, args } => {
            let parts: Vec<String> = args.iter().map(|a| expression(a, profile)).collect();
            format!("{}({})", profile.name(&sig.name), parts.join(", "))
        }
        // An immediately-applied function keeps this an expression, so a body
        // nests wherever an expression can go.
        Expr::Let { name, value, body } => format!(
            "(({} ) => {})({})",
            profile.name(name),
            arrow(expression(body, profile)),
            expression(value, profile)
        ),
        Expr::If {
            cond,
            then,
            otherwise,
        } => format!(
            "({} ? {} : {})",
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
            let inner = arrow(expression(body, profile));
            match functor {
                Functor::List => format!("{subject}.map(({bound}) => {inner})"),
                // Absent stays absent, which in TypeScript means checking.
                Functor::Optional => format!(
                    "(({bound}) => {bound} === null || {bound} === undefined ? null : {inner})({subject})"
                ),
                Functor::Future => format!("{subject}.then(({bound}) => {inner})"),
            }
        }
        Expr::Prim { op, args } => {
            let operands: Vec<String> = args.iter().map(|a| expression(a, profile)).collect();
            match (op, operands.as_slice()) {
                (PrimOp::Eq, [x, y]) => format!("unitypeEqual({x}, {y})"),
                (PrimOp::Ne, [x, y]) => format!("!unitypeEqual({x}, {y})"),
                (op, [x, y]) if op.is_ordering() => {
                    let sign = match op {
                        PrimOp::Lt => "<",
                        PrimOp::Le => "<=",
                        PrimOp::Gt => ">",
                        _ => ">=",
                    };
                    format!("(unitypeCompare({x}, {y}) {sign} 0)")
                }
                (PrimOp::Neg, [x]) => format!("(-{x})"),
                (PrimOp::Not, [x]) => format!("(!{x})"),
                (PrimOp::Len, [x]) => format!("{x}.length"),
                (op, [x, y]) => {
                    let sign = match op {
                        PrimOp::Add => "+",
                        PrimOp::Sub => "-",
                        PrimOp::Mul => "*",
                        PrimOp::And => "&&",
                        _ => "||",
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
            "{}.reduce(({}, {}) => {}, {})",
            expression(list, profile),
            profile.name(acc),
            profile.name(item),
            arrow(expression(body, profile)),
            expression(init, profile)
        ),
        // `$c` and `$m` cannot be the name of anything a body binds, since no
        // identifier may contain a `$`.
        Expr::Case {
            value,
            var,
            some,
            none,
        } => format!(
            "(($c) => $c === null || $c === undefined ? {} : (({}) => {})($c))({})",
            expression(none, profile),
            profile.name(var),
            arrow(expression(some, profile)),
            expression(value, profile)
        ),
        Expr::Match { value, arms, .. } => {
            let subject = expression(value, profile);
            let Some((last, rest)) = arms.split_last() else {
                return format!("(($m) => {{ throw new Error(\"no variant\"); }})({subject})");
            };
            let arm = |a: &unitype_core::ir::MatchArm| {
                let body = expression(&a.body, profile);
                match &a.var {
                    Some(var) => format!("(({}) => {})($m.value)", profile.name(var), arrow(body)),
                    None => body,
                }
            };
            // Every variant has an arm, so the last needs no test.
            let mut chain = arm(last);
            for a in rest.iter().rev() {
                chain = format!("$m.$tag === {:?} ? {} : {chain}", a.tag.as_str(), arm(a));
            }
            format!("(($m) => {chain})({subject})")
        }
    }
}

/// The body of an arrow function, bracketed if it would otherwise be read as a
/// block.
///
/// `(x) => { a: 1 }` is a function whose body is a statement labelled `a`, not
/// one returning a record, so a record has to be wrapped to be returned.
fn arrow(body: String) -> String {
    if body.starts_with('{') {
        format!("({body})")
    } else {
        body
    }
}

/// Writes a value as a TypeScript literal.
pub fn literal(value: &unitype_core::value::Value) -> String {
    use unitype_core::value::Value;
    match value {
        Value::Null => "null".into(),
        Value::Bool(b) => b.to_string(),
        Value::Int(i) => i.to_string(),
        Value::Float(f) => format_float(*f),
        Value::Str(s) => format!("{s:?}"),
        // Bytes travel as base64, and so does a literal one.
        Value::Bytes(b) => format!("{:?}", base64(b)),
        Value::List(items) | Value::Tuple(items) => {
            let parts: Vec<String> = items.iter().map(literal).collect();
            format!("[{}]", parts.join(", "))
        }
        Value::Record(fields) => {
            let parts: Vec<String> = fields
                .iter()
                .map(|(k, v)| format!("{}: {}", key(k), literal(v)))
                .collect();
            format!("{{ {} }}", parts.join(", "))
        }
        Value::Map(entries) => {
            let parts: Vec<String> = entries
                .iter()
                .map(|(k, v)| {
                    let k = match k {
                        Value::Str(s) => format!("{s:?}"),
                        other => format!("{:?}", literal(other)),
                    };
                    format!("{k}: {}", literal(v))
                })
                .collect();
            format!("{{ {} }}", parts.join(", "))
        }
        Value::Variant { tag, value } => match value.as_ref() {
            Value::Null => format!("{{ $tag: {tag:?} }}"),
            payload => format!("{{ $tag: {tag:?}, value: {} }}", literal(payload)),
        },
    }
}

/// A property name, quoted only when it has to be.
fn key(name: &str) -> String {
    let plain = !name.is_empty()
        && name
            .chars()
            .next()
            .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_');
    if plain {
        name.to_string()
    } else {
        format!("{name:?}")
    }
}

/// A float that reads back as a float rather than an integer.
fn format_float(f: f64) -> String {
    let plain = f.to_string();
    if plain.contains(['.', 'e', 'E']) {
        plain
    } else {
        format!("{plain}.0")
    }
}

fn base64(bytes: &[u8]) -> String {
    use base64::Engine as _;
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

/// The harness that makes a filled-in module reachable as a proof.
const HARNESS: &str = r#"// Generated by unitype. Do not edit.
//
// This is the whole of what it takes for a Node module to take part: read a
// message, do the work, write one back. One message per line, and reading lines
// serves both ways of being called — if the parent sends one message and closes
// the stream, this runs once and the process exits; if the parent keeps the
// stream open, it keeps answering. Nothing here has to know which is happening.
//
// Pass a table of what this module proves, keyed by the entry name a manifest
// points at:
//
//     import { unitypeMain } from "./unitype_harness.ts";
//     import { sort } from "./demo.ts";
//     unitypeMain({ bubble_sort: sort });
//
// Node 24 and later run that as it stands, which is why the paths say `.ts`.
// Behind a compiler or a bundler, write them the way it expects.

export type Proof = (...args: any[]) => unknown | Promise<unknown>;

export async function unitypeMain(proves: Record<string, Proof>): Promise<void> {
  let pending = "";
  for await (const chunk of process.stdin) {
    pending += chunk.toString("utf8");
    let at: number;
    while ((at = pending.indexOf("\n")) >= 0) {
      const line = pending.slice(0, at).trim();
      pending = pending.slice(at + 1);
      if (line) await answer(proves, line);
    }
  }
  // A parent that closed the stream without a trailing newline still sent a
  // message, so whatever is left is one.
  const last = pending.trim();
  if (last) await answer(proves, last);
}

async function answer(proves: Record<string, Proof>, line: string): Promise<void> {
  let request: any;
  try {
    request = JSON.parse(line);
  } catch (error) {
    return respond({ ok: false, error: { code: "bad_request", message: String(error) } });
  }

  if (request.unitype !== "1") {
    return respond({
      ok: false,
      error: { code: "bad_version", message: `this module speaks version 1, not ${request.unitype}` },
    });
  }
  if (request.kind !== "invoke") {
    return respond({ ok: false, error: { code: "bad_kind", message: `cannot do ${request.kind}` } });
  }

  const proof = proves[request.entry];
  if (proof === undefined) {
    const known = Object.keys(proves).sort().join(", ") || "nothing";
    return respond({
      ok: false,
      error: { code: "no_such_entry", message: `this module proves ${known}, not ${request.entry}` },
    });
  }

  try {
    respond({ ok: true, value: await proof(...(request.args ?? [])) });
  } catch (error) {
    // Reported as a failure of the proof, not of the process: exiting non-zero
    // would say the environment is broken, which it is not.
    respond({ ok: false, error: { code: "threw", message: String(error) } });
  }
}

function respond(response: unknown): void {
  // One line: a parent reading line by line needs the terminator to know the
  // message is finished.
  process.stdout.write(JSON.stringify(response) + "\n");
}
"#;

#[cfg(test)]
mod tests {
    use super::*;
    use unitype_core::Workspace;
    use unitype_core::check::check_workspace;

    fn generate(json: &str) -> String {
        let ws = check_workspace(Workspace::from_json(json).expect("reads")).expect("checks");
        let files = TypeScript
            .generate(&ws, &GenOptions::default())
            .expect("generates");
        files
            .into_iter()
            .find(|f| f.path.to_string_lossy().ends_with("demo.ts"))
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
        {"name":"area","params":[{"name":"s","ty":"Circle"}],"ret":"float"}]}]}"#;

    #[test]
    fn a_struct_becomes_an_interface() {
        let out = generate(SHAPES);
        assert!(
            out.contains("export interface Shape extends Identified {"),
            "{out}"
        );
        assert!(out.contains("id: string;"), "{out}");
    }

    #[test]
    fn inheritance_becomes_extends_rather_than_being_flattened() {
        let out = generate(SHAPES);
        assert!(
            out.contains("export interface Circle extends Shape {"),
            "{out}"
        );
        assert!(
            !out.contains("export interface Circle extends Shape {\n    id: string;"),
            "the inherited field should not be repeated: {out}"
        );
    }

    #[test]
    fn a_parent_is_declared_before_the_child_that_extends_it() {
        let out = generate(SHAPES);
        let shape = out.find("interface Shape").expect("declared");
        let circle = out.find("interface Circle").expect("declared");
        assert!(shape < circle, "{out}");
    }

    #[test]
    fn a_discriminating_literal_keeps_its_value() {
        let out = generate(SHAPES);
        assert!(
            out.contains(r#"kind: "circle";"#),
            "the discriminator is what lets a reader tell the union apart: {out}"
        );
    }

    #[test]
    fn a_field_that_may_be_absent_is_optional_and_nullable() {
        let out = generate(SHAPES);
        assert!(
            out.contains("note?: string | null;"),
            "the wire has one way of being absent and TypeScript has two: {out}"
        );
    }

    #[test]
    fn an_enum_becomes_a_union_tagged_the_way_the_wire_tags_it() {
        let out = generate(SHAPES);
        assert!(out.contains(r#"| { $tag: "Red" }"#), "{out}");
        assert!(
            out.contains(r#"| { $tag: "Custom"; value: string }"#),
            "{out}"
        );
    }

    #[test]
    fn an_alias_to_a_union_becomes_a_union_type() {
        let out = generate(SHAPES);
        assert!(
            out.contains("export type AnyShape = Circle | Rect;"),
            "{out}"
        );
    }

    #[test]
    fn a_proposition_with_no_proof_gets_a_skeleton_that_says_so() {
        let out = generate(SHAPES);
        assert!(
            out.contains("export function area(s: Circle): number {"),
            "{out}"
        );
        assert!(out.contains("is not implemented"), "{out}");
    }

    const COMPARING: &str = r#"{"universes":[{"name":"demo","functions":[
        {"name":"same","params":[{"name":"a","ty":"[i64]"},{"name":"b","ty":"[i64]"}],"ret":"bool",
         "body":{"k":"prim","op":"eq","args":[{"k":"var","name":"a"},{"k":"var","name":"b"}]}},
        {"name":"plain","params":[{"name":"a","ty":"i64"}],"ret":"i64","body":{"k":"var","name":"a"}}]}]}"#;

    #[test]
    fn equality_means_the_same_values_rather_than_the_same_array() {
        let out = generate(COMPARING);
        assert!(out.contains("unitypeEqual(a, b)"), "{out}");
        assert!(
            out.contains("function unitypeEqual("),
            "the helper ships with the module that uses it: {out}"
        );
        assert!(
            !out.contains("function unitypeCompare("),
            "and nothing that module does not use: {out}"
        );
    }

    #[test]
    fn a_module_that_compares_nothing_carries_no_helpers() {
        let out = generate(SHAPES);
        assert!(!out.contains("function unitype"), "{out}");
    }

    #[test]
    fn a_body_is_written_out_rather_than_left_as_a_hole() {
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
            out.contains("return { label: u.first, sides: 4 };"),
            "the body written once should become real TypeScript: {out}"
        );
        assert!(!out.contains("not implemented"), "{out}");
    }

    #[test]
    fn mapping_through_a_list_becomes_a_map() {
        let out = generate(
            r#"{"universes":[{"name":"demo","types":[
                {"k":"struct","name":"User","fields":[{"name":"first","ty":"str"}]}],
              "functions":[
                {"name":"names","params":[{"name":"us","ty":"[User]"}],"ret":"[str]",
                 "body":{"k":"map_through","functor":"list","value":{"k":"var","name":"us"},
                         "var":"u","body":{"k":"field","of":{"k":"var","name":"u"},"name":"first"}}}]}]}"#,
        );
        assert!(out.contains("us.map((u) => u.first)"), "{out}");
    }

    #[test]
    fn mapping_through_absence_checks_for_it() {
        let out = generate(
            r#"{"universes":[{"name":"demo","functions":[
                {"name":"trimmed","params":[{"name":"s","ty":"str?"}],"ret":"str?",
                 "body":{"k":"map_through","functor":"optional","value":{"k":"var","name":"s"},
                         "var":"x","body":{"k":"var","name":"x"}}}]}]}"#,
        );
        assert!(out.contains("=== null"), "absent has to stay absent: {out}");
    }

    #[test]
    fn where_precision_goes_is_written_next_to_the_field() {
        let out = generate(
            r#"{"universes":[{"name":"demo","types":[
                {"k":"struct","name":"Big","fields":[{"name":"n","ty":"i64"}]}]}]}"#,
        );
        assert!(
            out.contains("2^53"),
            "a reader deserves to know before they lose a digit: {out}"
        );
    }

    #[test]
    fn a_type_from_another_universe_is_imported() {
        let out = generate(
            r#"{"universes":[
                {"name":"geo","types":[{"k":"struct","name":"Point",
                 "fields":[{"name":"x","ty":"float"}]}]},
                {"name":"demo","types":[{"k":"struct","name":"Pin",
                 "fields":[{"name":"at","ty":"geo::Point"}]}]}]}"#,
        );
        assert!(
            out.contains(r#"import type { Point } from "./geo";"#),
            "{out}"
        );
    }

    #[test]
    fn a_reserved_word_is_not_emitted_as_a_name() {
        let out = generate(
            r#"{"universes":[{"name":"demo","types":[
                {"k":"struct","name":"Thing","fields":[{"name":"function","ty":"str"}]}]}]}"#,
        );
        assert!(out.contains("function_: string;"), "{out}");
    }

    #[test]
    fn generating_twice_from_the_same_workspace_gives_the_same_bytes() {
        assert_eq!(generate(SHAPES), generate(SHAPES));
    }

    #[test]
    fn the_header_says_what_this_came_from() {
        let out = generate(SHAPES);
        assert!(
            out.starts_with("// Generated by unitype from universe `demo`."),
            "{out}"
        );
        assert!(out.contains("Structural addresses:"), "{out}");
    }

    #[test]
    fn the_harness_ships_alongside() {
        let ws = check_workspace(Workspace::from_json(SHAPES).expect("reads")).expect("checks");
        let files = TypeScript
            .generate(&ws, &GenOptions::default())
            .expect("generates");
        assert!(
            files.iter().any(|f| f.path.ends_with("unitype_harness.ts")),
            "a filled-in module should be reachable without anyone writing glue"
        );
    }

    #[test]
    fn values_become_literals_that_read_back_as_themselves() {
        use unitype_core::value::Value;
        assert_eq!(literal(&Value::Int(3)), "3");
        assert_eq!(
            literal(&Value::Float(2.0)),
            "2.0",
            "otherwise it reads back as an integer"
        );
        assert_eq!(literal(&Value::Str("hi".into())), "\"hi\"");
        assert_eq!(literal(&Value::Null), "null");
        assert_eq!(literal(&Value::record([("a", Value::Int(1))])), "{ a: 1 }");
        assert_eq!(literal(&Value::unit_variant("Red")), r#"{ $tag: "Red" }"#);
    }

    #[test]
    fn a_property_name_is_quoted_only_when_it_has_to_be() {
        use unitype_core::value::Value;
        assert_eq!(
            literal(&Value::record([("ok", Value::Int(1))])),
            "{ ok: 1 }"
        );
        assert_eq!(
            literal(&Value::record([("not-ok", Value::Int(1))])),
            r#"{ "not-ok": 1 }"#
        );
    }
}
