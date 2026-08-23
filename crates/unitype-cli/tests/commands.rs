//! The commands, run as a person would run them.
//!
//! Unit tests cover what each command computes. What they cannot cover is
//! whether the command is *usable*: whether the flags fit together, whether a
//! mistake produces something you can act on, whether the exit code tells a
//! script what happened. That is what this checks, by running the binary
//! against the example project.

use std::path::PathBuf;
use std::process::{Command, Output};

fn demo() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../examples/demo")
}

fn unitype(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_unitype"))
        .arg("--project")
        .arg(demo())
        .args(args)
        .output()
        .expect("the command runs")
}

fn stdout(args: &[&str]) -> String {
    let output = unitype(args);
    assert!(
        output.status.success(),
        "`unitype {}` failed:\n{}",
        args.join(" "),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).expect("output is text")
}

fn json(args: &[&str]) -> serde_json::Value {
    let mut with_json = vec!["--json"];
    with_json.extend_from_slice(args);
    serde_json::from_str(&stdout(&with_json)).expect("valid JSON")
}

fn have_python() -> bool {
    Command::new("python3")
        .arg("--version")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

#[test]
fn the_example_project_checks() {
    let report = json(&["check"]);
    assert_eq!(report["project"], "demo");
    assert!(report["types"].as_u64().unwrap_or(0) > 0);
    assert_eq!(
        report["warnings"].as_array().map(Vec::len),
        Some(0),
        "the example should be clean: {report}"
    );
}

#[test]
fn the_same_data_in_two_universes_gets_the_same_words() {
    let point = json(&["words", "geo::Point"]);
    let vec2 = json(&["words", "gfx::Vec2"]);

    assert_eq!(
        point["words"], vec2["words"],
        "two universes describing the same thing should agree about what it is called"
    );
    assert_eq!(
        point["words"].as_str().map(|w| w.split('-').count()),
        Some(3),
        "and three words should be enough to say it"
    );
    assert!(
        vec2["same_shape"]
            .as_array()
            .is_some_and(|v| v.iter().any(|n| n == "geo::Point")),
        "each should know about the other: {vec2}"
    );
}

#[test]
fn asking_for_speed_and_asking_for_memory_choose_differently() {
    if !have_python() {
        eprintln!("no python3 on this machine; skipping");
        return;
    }
    let fast = json(&[
        "run",
        "demo::sort",
        "--args",
        "[[5,3,9,1]]",
        "--policy",
        "fastest",
    ]);
    let frugal = json(&[
        "run",
        "demo::sort",
        "--args",
        "[[5,3,9,1]]",
        "--policy",
        "min-memory",
    ]);

    assert_eq!(fast["implementation"], "sort.rust_merge");
    assert_eq!(fast["environment"], "here");
    assert_eq!(frugal["implementation"], "sort.py_bubble");
    assert_eq!(frugal["environment"], "python");
    assert_eq!(
        fast["value"], frugal["value"],
        "which proof ran should not change what is true"
    );
}

#[test]
fn a_value_crosses_between_universes_and_arrives_as_the_other_thing() {
    let moved = json(&[
        "bridge",
        "--from",
        "geo::Place",
        "--to",
        "gfx::Marker",
        "--data",
        r#"{"placeId":"p-1","displayName":"The Angel","at":{"lat":51.53,"lon":-0.1}}"#,
    ]);

    assert_eq!(moved["value"]["place_id"], "p-1");
    assert_eq!(moved["value"]["display_name"], "The Angel");
    assert_eq!(
        moved["value"]["at"]["lat"], 51.53,
        "the nested type crossed too: {moved}"
    );
    assert_eq!(moved["hops"].as_array().map(Vec::len), Some(1));
}

#[test]
fn the_route_can_be_seen_without_moving_anything() {
    let shown = stdout(&["bridge", "--from", "geo::Place", "--to", "gfx::Marker"]);
    assert!(shown.contains("place_is_a_marker"), "{shown}");
}

#[test]
fn a_value_comes_out_as_something_each_language_would_have_written() {
    let data = r#"{"id":"c-1","kind":"circle","radius":2.5}"#;
    let written = |lang: &str| {
        json(&[
            "cast",
            "--type",
            "demo::Circle",
            "--lang",
            lang,
            "--data",
            data,
        ])["literal"]
            .as_str()
            .expect("a literal")
            .to_string()
    };

    assert!(
        written("python").starts_with("Circle("),
        "{}",
        written("python")
    );
    assert!(
        written("rust").starts_with("Circle {"),
        "{}",
        written("rust")
    );
    assert!(
        written("typescript").starts_with("{ "),
        "{}",
        written("typescript")
    );
    assert!(written("json").starts_with("{\""), "{}", written("json"));
}

#[test]
fn sample_data_becomes_a_schema() {
    let universe = json(&[
        "infer",
        "--lift",
        "order",
        "--data",
        r#"{"id":"a","items":[{"sku":"x","qty":2}]}"#,
    ]);

    let names: Vec<&str> = universe["types"]
        .as_array()
        .expect("declarations")
        .iter()
        .filter_map(|d| d["name"].as_str())
        .collect();
    assert!(names.contains(&"Order"), "{names:?}");
    assert!(
        names.contains(&"OrderItem"),
        "the nested record should be named after where it sits: {names:?}"
    );
}

#[test]
fn data_that_is_not_of_the_type_says_where_and_says_so_to_a_script() {
    let output = unitype(&[
        "validate",
        "--type",
        "demo::Circle",
        "--data",
        r#"{"id":"c-1","kind":"square"}"#,
    ]);

    assert_eq!(
        output.status.code(),
        Some(2),
        "a script should be able to tell this apart from a crash"
    );
    let said = String::from_utf8_lossy(&output.stderr);
    assert!(said.contains("$.kind"), "{said}");
    assert!(
        said.contains("expected f64"),
        "types should read the way they would be written: {said}"
    );
}

#[test]
fn a_misspelled_name_suggests_the_right_one() {
    let output = unitype(&["words", "demo::circle"]);
    let said = String::from_utf8_lossy(&output.stderr);
    assert!(said.contains("Did you mean `demo::Circle`?"), "{said}");
}

#[test]
fn asking_from_somewhere_that_is_not_a_project_says_what_to_do() {
    let output = Command::new(env!("CARGO_BIN_EXE_unitype"))
        .arg("check")
        .current_dir(std::env::temp_dir())
        .output()
        .expect("runs");
    let said = String::from_utf8_lossy(&output.stderr);
    assert!(said.contains("unitype.json"), "{said}");
    assert!(said.contains("--project"), "{said}");
    assert_eq!(output.status.code(), Some(1));
}

#[test]
fn generation_can_be_asked_about_without_writing_anything() {
    let planned = json(&["gen", "--dry-run"]);
    assert_eq!(planned["dry_run"], true);
    let written = planned["written"].as_array().expect("a list");
    assert!(!written.is_empty());
    assert!(
        written
            .iter()
            .all(|f| !PathBuf::from(f["path"].as_str().unwrap_or("")).exists()),
        "a dry run should not have written anything"
    );
}

#[test]
fn what_the_project_says_is_the_same_thing_is_listed() {
    let classes = json(&["classes"]);
    let listed = classes.as_array().expect("a list");
    assert!(
        listed.iter().any(|c| {
            c["members"]
                .as_array()
                .is_some_and(|m| m.iter().any(|n| n == "geo::Point"))
        }),
        "{classes}"
    );
}
