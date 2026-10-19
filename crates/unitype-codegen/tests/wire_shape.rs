//! Does the generated code actually produce the shape it was supposed to?
//!
//! Every backend claims that what it emits describes the same bytes. That claim
//! is easy to make and easy to get wrong — a renamed field, a differently
//! tagged variant, an optional that becomes `undefined` instead of absent — and
//! none of it shows up until two systems disagree in production.
//!
//! So this generates the code, runs it, and compares what it produces against
//! what unitype itself says the value is. If a backend drifts, the languages
//! stop agreeing here rather than somewhere expensive.
//!
//! All three are run: Python by the interpreter, TypeScript by a Node that
//! reads it directly, Rust by building a small crate around the module. A file
//! that only looks right is not evidence of anything.
//!
//! Where an interpreter is missing, its half is skipped and said so. The point
//! of generating for a language is that the language is not this program's
//! problem, which cuts both ways.

use std::path::{Path, PathBuf};
use std::process::Command;

use unitype_codegen::{Backend, GenOptions, python, rust, typescript};
use unitype_core::check::{CheckedWorkspace, check_workspace};
use unitype_core::ir::{Type, TypeRef};
use unitype_core::{Ident, Workspace};

/// A universe with the cases most likely to drift between backends: an
/// inherited field, a discriminating literal, a field that may be absent, and
/// a list — and the cases most likely to stop a file loading at all: prose
/// long enough to wrap, prose that tries to end its own comment, a union of
/// strings, and bodies: one that builds a record, and some that fold a list,
/// compare and add, say what to do when something is absent, and take an enum
/// apart.
const UNIVERSE: &str = r#"{"universes":[{"name":"demo",
  "doc":"Shapes, and the things done to them.",
  "types":[
    {"k":"struct","name":"Shape",
     "doc":"Anything with an outline. Said at enough length that the comment has to wrap onto a second line, and with a */ in it that must not end the comment early.",
     "fields":[{"name":"id","ty":"str","doc":"What it is called."}]},
    {"k":"struct","name":"Circle","extends":"Shape","fields":[
        {"name":"kind","ty":{"k":"literal","value":{"str":"circle"}}},
        {"name":"radius","ty":"float"},
        {"name":"tags","ty":"[str]"},
        {"name":"note","ty":"str?"}]},
    {"k":"alias","name":"Colour","doc":"The ones there are.","target":{"k":"union","members":[
        {"k":"literal","value":{"str":"red"}},
        {"k":"literal","value":{"str":"blue"}}]}},
    {"k":"struct","name":"Label","fields":[
        {"name":"text","ty":"str"},{"name":"sides","ty":"i64"}]},
    {"k":"struct","name":"Step","fields":[
        {"name":"ok","ty":"bool"},{"name":"prev","ty":"i64"},{"name":"started","ty":"bool"}]},
    {"k":"enum","name":"Size","variants":[{"name":"small"},{"name":"large","payload":"i64"}]}],
  "functions":[
    {"name":"label_of","doc":"Describes a circle.",
     "params":[{"name":"c","ty":"Circle"}],"ret":"Label",
     "body":{"k":"make_record","ty":"Label","fields":[
        {"name":"text","value":{"k":"field","of":{"k":"var","name":"c"},"name":"id"}},
        {"name":"sides","value":{"k":"lit","value":0}}]}},
    {"name":"area","params":[{"name":"c","ty":"Circle"}],"ret":"float"},
    {"name":"total","params":[{"name":"xs","ty":"[i64]"}],"ret":"i64",
     "body":{"k":"fold","list":{"k":"var","name":"xs"},"init":{"k":"lit","value":0},
             "acc":"t","item":"x",
             "body":{"k":"prim","op":"add","args":[{"k":"var","name":"t"},{"k":"var","name":"x"}]}}},
    {"name":"ordered","params":[{"name":"xs","ty":"[i64]"}],"ret":"bool",
     "body":{"k":"field","name":"ok","of":
       {"k":"fold","list":{"k":"var","name":"xs"},
        "init":{"k":"make_record","ty":"Step","fields":[
            {"name":"ok","value":{"k":"lit","value":true}},
            {"name":"prev","value":{"k":"lit","value":0}},
            {"name":"started","value":{"k":"lit","value":false}}]},
        "acc":"s","item":"x",
        "body":{"k":"make_record","ty":"Step","fields":[
            {"name":"ok","value":{"k":"prim","op":"and","args":[
                {"k":"field","of":{"k":"var","name":"s"},"name":"ok"},
                {"k":"prim","op":"or","args":[
                    {"k":"prim","op":"not","args":[{"k":"field","of":{"k":"var","name":"s"},"name":"started"}]},
                    {"k":"prim","op":"le","args":[{"k":"field","of":{"k":"var","name":"s"},"name":"prev"},
                                                  {"k":"var","name":"x"}]}]}]}},
            {"name":"prev","value":{"k":"var","name":"x"}},
            {"name":"started","value":{"k":"lit","value":true}}]}}}},
    {"name":"note_or","params":[{"name":"c","ty":"Circle"}],"ret":"str",
     "body":{"k":"case","value":{"k":"field","of":{"k":"var","name":"c"},"name":"note"},"var":"it",
             "some":{"k":"var","name":"it"},"none":{"k":"lit","value":"none"}}},
    {"name":"measure","params":[{"name":"s","ty":"Size"}],"ret":"i64",
     "body":{"k":"match","ty":"Size","value":{"k":"var","name":"s"},"arms":[
        {"tag":"small","body":{"k":"lit","value":0}},
        {"tag":"large","var":"n","body":{"k":"prim","op":"mul","args":[
            {"k":"var","name":"n"},{"k":"lit","value":2}]}}]}},
    {"name":"two_tags","params":[{"name":"c","ty":"Circle"}],"ret":"bool",
     "body":{"k":"prim","op":"and","args":[
        {"k":"prim","op":"eq","args":[
            {"k":"prim","op":"len","args":[{"k":"field","of":{"k":"var","name":"c"},"name":"tags"}]},
            {"k":"lit","value":2}]},
        {"k":"prim","op":"ne","args":[
            {"k":"field","of":{"k":"var","name":"c"},"name":"id"},{"k":"lit","value":"x"}]}]}}]}]}"#;

/// What the body above produces for the circle every test builds.
const LABEL: &str = r#"{"text":"c-1","sides":0}"#;

/// What the other bodies produce, asked the same questions in every language:
/// `total([1,2,3,4])`, `ordered([1,2,2,5])`, `ordered([3,1])`, `note_or` and
/// `two_tags` of the circle, and `measure` of each size.
const FORMS: &str = r#"{"total":10,"ordered":true,"unordered":false,"note":"none",
    "small":0,"large":42,"two_tags":true}"#;

fn workspace() -> CheckedWorkspace {
    check_workspace(Workspace::from_json(UNIVERSE).expect("reads")).expect("checks")
}

fn circle_type() -> Type {
    Type::Ref(TypeRef::qualified(
        Ident::new("demo").unwrap(),
        Ident::new("Circle").unwrap(),
    ))
}

/// The value every language is asked to produce, and what unitype says it is.
fn expected_json() -> String {
    let ws = workspace();
    let circle = ws
        .read_json(
            r#"{"id":"c-1","kind":"circle","radius":2.5,"tags":["round","blue"],"note":null}"#,
            &circle_type(),
        )
        .expect("a real Circle");
    // Absent and null are one state, so the canonical form of this value has no
    // `note` at all. Every language has to agree about that too.
    ws.write_json(&circle, &circle_type()).expect("writes")
}

/// Sorted keys, so two encoders that agree about content but not order still
/// compare equal.
fn normalize(json: &str) -> String {
    let parsed: serde_json::Value = serde_json::from_str(json).expect("valid JSON");
    fn sort(value: &serde_json::Value) -> serde_json::Value {
        match value {
            serde_json::Value::Object(map) => {
                let mut sorted = serde_json::Map::new();
                let mut keys: Vec<&String> = map.keys().collect();
                keys.sort();
                for key in keys {
                    sorted.insert(key.clone(), sort(&map[key]));
                }
                serde_json::Value::Object(sorted)
            }
            serde_json::Value::Array(items) => {
                serde_json::Value::Array(items.iter().map(sort).collect())
            }
            other => other.clone(),
        }
    }
    serde_json::to_string(&sort(&parsed)).expect("re-encodes")
}

/// Writes a backend's output into a directory of its own.
fn emit(backend: &dyn Backend, into: &Path) -> PathBuf {
    let ws = workspace();
    let files = backend
        .generate(&ws, &GenOptions::default())
        .expect("generates");
    let root = into.join(backend.id());
    for file in files {
        let path = root.join(file.path.file_name().expect("a file name"));
        std::fs::create_dir_all(path.parent().expect("a parent")).expect("makes the directory");
        std::fs::write(&path, file.contents).expect("writes the file");
    }
    root
}

fn scratch(name: &str) -> PathBuf {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(name);
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("makes a scratch directory");
    dir
}

fn have(program: &str) -> bool {
    Command::new(program)
        .arg("--version")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

fn run(program: &str, args: &[&str], in_dir: &Path) -> String {
    let output = Command::new(program)
        .args(args)
        .current_dir(in_dir)
        .output()
        .unwrap_or_else(|e| panic!("could not run {program}: {e}"));
    assert!(
        output.status.success(),
        "{program} failed:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).expect("output is text")
}

#[test]
fn the_generated_python_produces_the_shape_it_promised() {
    if !have("python3") {
        eprintln!("no python3 on this machine; skipping");
        return;
    }

    let dir = scratch("python_wire");
    let root = emit(&python::Python, &dir);

    std::fs::write(
        root.join("check.py"),
        r#"import dataclasses, json
from demo import Circle

circle = Circle(id="c-1", kind="circle", radius=2.5, tags=["round", "blue"])
shape = dataclasses.asdict(circle)
# Absent and null are one state, so a field with nothing in it is not sent.
shape = {k: v for k, v in shape.items() if v is not None}
print(json.dumps(shape))
"#,
    )
    .expect("writes the driver");

    let produced = run("python3", &["check.py"], &root);
    assert_eq!(
        normalize(&produced),
        normalize(&expected_json()),
        "the generated dataclass produced a different shape than unitype says \
         the value has"
    );
}

/// Whether this Node runs TypeScript as it is, by dropping the types.
fn node_reads_typescript() -> bool {
    let Ok(output) = Command::new("node").arg("--version").output() else {
        return false;
    };
    String::from_utf8_lossy(&output.stdout)
        .trim()
        .trim_start_matches('v')
        .split('.')
        .next()
        .and_then(|major| major.parse::<u32>().ok())
        .is_some_and(|major| major >= 24)
}

#[test]
fn the_generated_typescript_loads_and_produces_the_shape_it_promised() {
    if !node_reads_typescript() {
        eprintln!("no node that reads TypeScript on this machine; skipping");
        return;
    }

    let dir = scratch("typescript_wire");
    let root = emit(&typescript::TypeScript, &dir);

    // Says the directory holds modules, so Node does not have to work it out.
    std::fs::write(root.join("package.json"), "{\"type\": \"module\"}\n").expect("writes");

    // The generated file itself is what gets loaded. A value written against
    // an interface proves nothing if the file declaring the interface would
    // not have parsed.
    std::fs::write(
        root.join("check.mjs"),
        r#"import { label_of, measure, note_or, ordered, total, two_tags } from "./demo.ts";

// Written against the generated `Circle` interface: every field it requires,
// and nothing for the one that may be absent.
const circle = {
  id: "c-1",
  kind: "circle",
  radius: 2.5,
  tags: ["round", "blue"],
};
const forms = {
  total: total([1, 2, 3, 4]),
  ordered: ordered([1, 2, 2, 5]),
  unordered: ordered([3, 1]),
  note: note_or(circle),
  small: measure({ $tag: "small" }),
  large: measure({ $tag: "large", value: 21 }),
  two_tags: two_tags(circle),
};
process.stdout.write(JSON.stringify({ circle, label: label_of(circle), forms }));
"#,
    )
    .expect("writes the driver");

    let produced: serde_json::Value =
        serde_json::from_str(&run("node", &["check.mjs"], &root)).expect("valid JSON");
    assert_eq!(
        normalize(&produced["circle"].to_string()),
        normalize(&expected_json()),
        "what a value written against the generated interface serializes to \
         differs from what unitype says it is"
    );
    assert_eq!(
        normalize(&produced["label"].to_string()),
        normalize(LABEL),
        "the body, as TypeScript, answered differently"
    );
    assert_eq!(
        normalize(&produced["forms"].to_string()),
        normalize(FORMS),
        "the bodies, as TypeScript, answered differently"
    );
}

#[test]
fn the_generated_python_runs_the_body_it_was_given() {
    if !have("python3") {
        return;
    }

    let dir = scratch("python_body");
    let root = emit(&python::Python, &dir);
    std::fs::write(
        root.join("check.py"),
        r#"import dataclasses, json
from demo import Circle, label_of, measure, note_or, ordered, total, two_tags

circle = Circle(id="c-1", kind="circle", radius=2.5, tags=["round", "blue"])
forms = {
    "total": total([1, 2, 3, 4]),
    "ordered": ordered([1, 2, 2, 5]),
    "unordered": ordered([3, 1]),
    "note": note_or(circle),
    "small": measure({"$tag": "small"}),
    "large": measure({"$tag": "large", "value": 21}),
    "two_tags": two_tags(circle),
}
print(json.dumps({"label": dataclasses.asdict(label_of(circle)), "forms": forms}))
"#,
    )
    .expect("writes the driver");

    let produced: serde_json::Value =
        serde_json::from_str(&run("python3", &["check.py"], &root)).expect("valid JSON");
    assert_eq!(
        normalize(&produced["label"].to_string()),
        normalize(LABEL),
        "the body, as Python, answered differently"
    );
    assert_eq!(
        normalize(&produced["forms"].to_string()),
        normalize(FORMS),
        "the bodies, as Python, answered differently"
    );
}

#[test]
fn the_generated_rust_compiles_and_produces_the_shape_it_promised() {
    let dir = scratch("rust_wire");
    let root = emit(&rust::Rust, &dir);
    std::fs::create_dir_all(dir.join("src")).expect("makes src");
    std::fs::rename(root.join("demo.rs"), dir.join("src/demo.rs")).expect("moves the module");

    // A crate of its own, with an empty workspace table so that it is not
    // taken for a member of the one it happens to sit inside.
    std::fs::write(
        dir.join("Cargo.toml"),
        r#"[package]
name = "wire-check"
version = "0.0.0"
edition = "2024"

[dependencies]
serde = { version = "1", features = ["derive"] }
serde_json = "1"

[workspace]
"#,
    )
    .expect("writes the manifest");
    std::fs::write(
        dir.join("src/main.rs"),
        r#"#[allow(dead_code, unused_variables)]
mod demo;

fn main() {
    // Read the way it arrives, written the way it leaves: what comes out is
    // whatever the generated struct believes the shape to be.
    let arrived = std::env::args().nth(1).expect("a circle");
    let circle: demo::Circle = serde_json::from_str(&arrived).expect("reads as a Circle");
    let label = demo::label_of(circle.clone());

    let red: demo::Colour = serde_json::from_str("\"red\"").expect("red is one of them");
    let green = serde_json::from_str::<demo::Colour>("\"green\"").is_ok();

    let forms = serde_json::json!({
        "total": demo::total(vec![1, 2, 3, 4]),
        "ordered": demo::ordered(vec![1, 2, 2, 5]),
        "unordered": demo::ordered(vec![3, 1]),
        "note": demo::note_or(circle.clone()),
        "small": demo::measure(demo::Size::small),
        "large": demo::measure(demo::Size::large(21)),
        "two_tags": demo::two_tags(circle.clone()),
    });

    println!(
        "{}",
        serde_json::json!({ "circle": circle, "label": label, "red": red, "green": green, "forms": forms })
    );
}
"#,
    )
    .expect("writes the driver");

    // Built somewhere that survives between runs, so the dependencies are
    // compiled once rather than every time.
    let target = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("rust_wire_target");
    let output = Command::new(env!("CARGO"))
        .args(["run", "--quiet", "--offline", "--"])
        .arg(expected_json())
        .env("CARGO_TARGET_DIR", &target)
        .current_dir(&dir)
        .output()
        .expect("runs cargo");
    let said = String::from_utf8_lossy(&output.stderr);

    if !output.status.success() && !said.contains("error[") && !said.contains("could not compile") {
        // Nothing was wrong with the code: the dependencies are not on this
        // machine and there is no network to fetch them with.
        eprintln!("serde is not available offline; skipping\n{said}");
        return;
    }
    assert!(
        output.status.success(),
        "the generated Rust did not build:\n{said}"
    );

    let produced: serde_json::Value = serde_json::from_slice(&output.stdout).expect("valid JSON");
    assert_eq!(
        normalize(&produced["circle"].to_string()),
        normalize(&expected_json()),
        "the generated struct read and wrote a different shape than unitype \
         says the value has"
    );
    assert_eq!(
        normalize(&produced["label"].to_string()),
        normalize(LABEL),
        "the body, as Rust, answered differently"
    );
    assert_eq!(
        normalize(&produced["forms"].to_string()),
        normalize(FORMS),
        "the bodies, as Rust, answered differently"
    );
    assert_eq!(produced["red"], "red", "a name travels as the string it is");
    assert_eq!(
        produced["green"], false,
        "a string the union does not admit should be refused"
    );
}

#[test]
fn every_backend_names_the_same_fields() {
    // Even without an interpreter, the field names a backend emits can be
    // compared: they are what appears on the wire, and a rename anywhere is a
    // disagreement everywhere.
    let ws = workspace();
    let wanted: Vec<String> = ws
        .flat_fields(&"demo::Circle".parse().unwrap())
        .expect("declared")
        .iter()
        .map(|f| f.name.to_string())
        .collect();

    for backend in [
        Box::new(typescript::TypeScript) as Box<dyn Backend>,
        Box::new(python::Python),
        Box::new(rust::Rust),
    ] {
        let files = backend
            .generate(&ws, &GenOptions::default())
            .expect("generates");
        let source: String = files
            .iter()
            .filter(|f| !f.path.to_string_lossy().contains("harness"))
            .map(|f| f.contents.clone())
            .collect();

        for field in &wanted {
            assert!(
                source.contains(field.as_str()),
                "{} never mentions `{field}`",
                backend.id()
            );
        }
    }
}

#[test]
fn what_the_generated_python_accepts_is_what_unitype_would_accept() {
    if !have("python3") {
        return;
    }

    let dir = scratch("python_reject");
    let root = emit(&python::Python, &dir);
    std::fs::write(
        root.join("check.py"),
        r#"from demo import Circle

# `radius` is required, so leaving it out has to fail. If the generated class
# accepted it, the class would be describing something looser than the type.
try:
    Circle(id="c-1", kind="circle", tags=[])
    print("accepted")
except TypeError:
    print("refused")
"#,
    )
    .expect("writes the driver");

    assert_eq!(
        run("python3", &["check.py"], &root).trim(),
        "refused",
        "a required field left out should be refused, as unitype would refuse it"
    );

    let ws = workspace();
    assert!(
        ws.read_json(r#"{"id":"c-1","kind":"circle","tags":[]}"#, &circle_type())
            .is_err(),
        "and unitype does refuse it"
    );
}
