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

fn http() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../examples/http")
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

/// A project of its own, so a test can register proofs that disagree without
/// putting a wrong one in the example anybody reads.
fn scratch_project(name: &str, extra_proof: Option<&str>) -> PathBuf {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(name);
    let _ = std::fs::remove_dir_all(&dir);
    let write = |at: PathBuf, text: &str| {
        std::fs::create_dir_all(at.parent().expect("a parent")).expect("makes a directory");
        std::fs::write(at, text).expect("writes");
    };

    write(dir.join("unitype.json"), r#"{"name":"scratch"}"#);
    write(
        dir.join("universes/demo.json"),
        r#"{"name":"demo","functions":[
            {"name":"sort","params":[{"name":"xs","ty":"[int]"}],"ret":"[int]"},
            {"name":"reverse","params_decl":[{"name":"T"}],
             "params":[{"name":"xs","ty":{"k":"list","of":{"k":"param","name":"T"}}}],
             "ret":{"k":"list","of":{"k":"param","name":"T"}}}]}"#,
    );
    write(
        dir.join("impls/demo.json"),
        &format!(
            r#"{{"implementations":[
                {{"id":"sort.merge","proves":"demo::sort","environment":"here",
                  "entry":"builtin::merge_sort",
                  "cost":{{"time":"o_n_log_n","space":"o_n"}}}}{}]}}"#,
            extra_proof.unwrap_or("")
        ),
    );
    write(
        dir.join("conform/sort.json"),
        r#"{"signature":"demo::sort","vectors":[
            {"name":"backwards","args":[[3,2,1]],"expect":[1,2,3]},
            {"name":"already in order","args":[[1,2,3]],"expect":[1,2,3]},
            {"name":"no answer given","args":[[5,1,4,2]]}]}"#,
    );
    dir
}

fn in_project(dir: &PathBuf, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_unitype"))
        .arg("--project")
        .arg(dir)
        .args(args)
        .output()
        .expect("the command runs")
}

#[test]
fn proofs_of_the_demo_agree_with_each_other() {
    if !have_python() {
        eprintln!("no python3 on this machine; skipping");
        return;
    }
    let report = json(&["conform"]);
    let sort = &report["suites"].as_array().expect("a list")[0];
    assert_eq!(sort["signature"], "demo::sort");
    assert_eq!(sort["agreed"], true, "{sort}");
    // Named rather than counted: which proofs take part depends on how this
    // binary was built — the sandboxed one needs the `wasm` feature — and the
    // claim worth making is that two languages actually ran, not how many
    // proofs exist.
    let took_part: Vec<&str> = sort["proofs"]
        .as_array()
        .expect("a list")
        .iter()
        .filter_map(|p| p.as_str())
        .collect();
    for expected in ["sort.rust_merge", "sort.py_bubble"] {
        assert!(
            took_part.contains(&expected),
            "{expected} should have taken part: {sort}"
        );
    }
    // And every proof that did take part answered every case, rather than
    // being quietly skipped into agreement with nobody.
    for case in sort["cases"].as_array().expect("a list") {
        for proof in &took_part {
            assert!(
                case["answers"].get(proof).is_some(),
                "{proof} did not answer `{}`: {sort}",
                case["name"]
            );
        }
    }
}

#[test]
fn a_proof_of_the_wrong_thing_is_caught_and_says_so_to_a_build() {
    // Reversing has exactly the type sorting has, so nothing about the
    // signature separates them. Only running both does.
    let dir = scratch_project(
        "conform_disagree",
        Some(
            r#",{"id":"sort.broken","proves":"demo::sort","environment":"here",
                "entry":"builtin::reverse","cost":{"time":"o_n","space":"o1"}}"#,
        ),
    );
    let output = in_project(&dir, &["conform"]);

    assert_eq!(
        output.status.code(),
        Some(4),
        "a build should be able to tell this apart from a crash"
    );
    let said = String::from_utf8_lossy(&output.stdout);
    assert!(said.contains("sort.broken"), "{said}");
    assert!(
        said.contains("already in order"),
        "the case it got wrong should be named: {said}"
    );
    assert!(
        !said.contains("  backwards\n"),
        "reversing happens to be right for this one, and should not be blamed: {said}"
    );
}

#[test]
fn a_disagreement_is_found_even_where_nobody_said_what_the_answer_should_be() {
    let dir = scratch_project(
        "conform_no_expectation",
        Some(
            r#",{"id":"sort.broken","proves":"demo::sort","environment":"here",
                "entry":"builtin::reverse","cost":{"time":"o_n","space":"o1"}}"#,
        ),
    );
    let output = in_project(&dir, &["--json", "conform"]);
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).expect("valid JSON");

    let case = report["suites"][0]["cases"]
        .as_array()
        .expect("cases")
        .iter()
        .find(|c| c["name"] == "no answer given")
        .expect("the case with no expected answer");
    assert_eq!(
        case["agreed"], false,
        "two proofs saying different things is a disagreement whether or not \
         anybody worked out the right answer first: {case}"
    );
}

#[test]
fn a_generic_proposition_is_checked_without_a_vector_file() {
    // Nothing under `conform/` mentions `reverse`. Its signature is enough.
    let report = json(&["conform", "--sig", "demo::reverse"]);
    assert_eq!(
        report["suites"].as_array().map(Vec::len),
        Some(0),
        "no file, so no suite: {report}"
    );
    let theorem = &report["theorems"].as_array().expect("a list")[0];
    assert_eq!(theorem["signature"], "demo::reverse");
    assert_eq!(theorem["params"], serde_json::json!(["T"]));
    assert_eq!(theorem["held"], true, "{theorem}");
    let proofs = theorem["proofs"].as_array().expect("a list");
    assert!(
        proofs.iter().any(|p| p == "reverse.rust"),
        "the compiled-in proof should have been held to it: {theorem}"
    );
    if have_python() {
        assert!(
            proofs.iter().any(|p| p == "reverse.py"),
            "and so should the Python one: {theorem}"
        );
        for trial in theorem["trials"].as_array().expect("a list") {
            assert_eq!(
                trial["verdicts"]["reverse.py"], "held",
                "ran, rather than being skipped into holding: {trial}"
            );
        }
    }
}

#[test]
fn a_proof_that_only_works_at_one_type_has_not_proved_the_generic_claim() {
    // Sorting integers is a fine thing to do to a `[i64]`. Claiming it proves
    // `reverse<T>` says it works at every `T`, and at `str` it does not — and
    // even where it ran, it sorted, which a proof that cannot see the elements
    // could not have done.
    let dir = scratch_project(
        "conform_not_parametric",
        Some(
            r#",{"id":"reverse.cheat","proves":"demo::reverse","environment":"here",
                "entry":"builtin::merge_sort","cost":{"time":"o_n_log_n","space":"o_n"}}"#,
        ),
    );
    let output = in_project(&dir, &["conform"]);

    assert_eq!(
        output.status.code(),
        Some(4),
        "the same exit as a disagreement"
    );
    let said = String::from_utf8_lossy(&output.stdout);
    assert!(said.contains("demo::reverse<T>"), "{said}");
    assert!(said.contains("reverse.cheat"), "{said}");
    assert!(
        said.contains("naturality"),
        "the law it broke is named: {said}"
    );
    assert!(
        said.contains("demo::sort  1 proof, 3 cases  — all agree"),
        "the other proposition is still reported on its own terms: {said}"
    );
    let complained = String::from_utf8_lossy(&output.stderr);
    assert!(
        complained.contains("free theorem"),
        "and the summary says what kind of failure it was: {complained}"
    );
}

#[test]
fn one_proof_on_its_own_has_nothing_to_disagree_with() {
    let dir = scratch_project("conform_alone", None);
    let output = in_project(&dir, &["conform"]);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn a_route_says_what_carries_out_each_step_and_where() {
    // A conversion that somebody had to write runs wherever its proof runs,
    // which may be another language. A route that did not say so would be one
    // you have to guess at.
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("route_languages");
    let _ = std::fs::remove_dir_all(&dir);
    let write = |at: PathBuf, text: &str| {
        std::fs::create_dir_all(at.parent().expect("a parent")).expect("makes a directory");
        std::fs::write(at, text).expect("writes");
    };

    write(dir.join("unitype.json"), r#"{"name":"routes"}"#);
    write(
        dir.join("universes/a.json"),
        r#"{"name":"a","types":[{"k":"struct","name":"Raw",
            "fields":[{"name":"text","ty":"str"}]}],
          "functions":[{"name":"parse","params":[{"name":"r","ty":"Raw"}],"ret":"b::Parsed"}]}"#,
    );
    write(
        dir.join("universes/b.json"),
        r#"{"name":"b","types":[{"k":"struct","name":"Parsed",
            "fields":[{"name":"words","ty":"[str]"}]}]}"#,
    );
    write(
        dir.join("links/parse.json"),
        r#"{"links":[{"name":"raw_becomes_parsed","from":"a::Raw","to":"b::Parsed",
            "kind":{"k":"witness","sig":"a::parse"}}]}"#,
    );
    write(
        dir.join("impls/parse.json"),
        r#"{"environments":[
                {"id":"node","language":"typescript",
                 "reach":{"k":"subprocess","command":"node","args":["parse.js"]}}],
            "implementations":[
                {"id":"parse.js","proves":"a::parse","environment":"node",
                 "entry":"parse","cost":{"time":"o_n","space":"o_n"}}]}"#,
    );

    let output = Command::new(env!("CARGO_BIN_EXE_unitype"))
        .arg("--project")
        .arg(&dir)
        .args(["--json", "bridge", "--from", "a::Raw", "--to", "b::Parsed"])
        .output()
        .expect("runs");
    let route: serde_json::Value = serde_json::from_slice(&output.stdout).expect("valid JSON");

    let hop = &route["hops"][0];
    assert_eq!(hop["link"], "raw_becomes_parsed");
    assert!(
        hop["by"]
            .as_str()
            .is_some_and(|by| by.contains("typescript")),
        "the route should say which language carries this step: {hop}"
    );
    assert!(
        hop["by"].as_str().is_some_and(|by| by.contains("node")),
        "and where it runs: {hop}"
    );
}

#[test]
fn a_route_that_needs_nothing_run_says_so() {
    let shown = stdout(&["bridge", "--from", "geo::Point", "--to", "gfx::Vec2"]);
    assert!(
        shown.contains("nothing to run"),
        "the same data under two names is a cast: {shown}"
    );
}

#[test]
fn a_declaration_is_a_value_of_the_type_that_describes_declarations() {
    let reflected = json(&["reflect", "geo::Point"]);
    assert_eq!(reflected["is_a"], "unitype::TypeDef");
    assert_eq!(reflected["checks"], true, "{reflected}");
    assert_eq!(
        reflected["value"]["name"], "Point",
        "and what comes back is the declaration itself, as data: {reflected}"
    );
}

#[test]
fn the_whole_workspace_is_a_value_too() {
    let reflected = json(&["reflect"]);
    assert_eq!(reflected["is_a"], "unitype::Workspace");
    assert_eq!(reflected["checks"], true, "{reflected}");
    assert!(
        reflected["value"]["universes"]
            .as_array()
            .is_some_and(|u| u.len() >= 3),
        "{reflected}"
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

#[test]
fn one_call_has_one_name_and_every_platform_spells_it_differently() {
    // The thing this example exists to show: an HTTP request is one
    // proposition with one address, and `curl`, `fetch` and `requests` are
    // what that one proposition is called in the places that prove it.
    let output = in_project(&http(), &["--json", "words", "http::request"]);
    let found: serde_json::Value = serde_json::from_slice(&output.stdout).expect("valid JSON");

    assert_eq!(found["kind"], "proposition");
    assert!(
        found["address"]
            .as_str()
            .expect("an address")
            .starts_with("ut1:"),
        "{found}"
    );
    // Three words, said out loud, for a thing nobody agreed on a name for.
    let words = found["words"].as_str().expect("words");
    assert_eq!(words.split('-').count(), 3, "{words}");

    let called: Vec<(&str, &str)> = found["called"]
        .as_array()
        .expect("a list")
        .iter()
        .map(|c| {
            (
                c["language"].as_str().expect("a language"),
                c["called"].as_str().expect("a name"),
            )
        })
        .collect();
    for expected in [
        ("sh", "curl"),
        ("typescript", "fetch"),
        ("python", "requests.request"),
        ("rust", "reqwest::Client::execute"),
    ] {
        assert!(
            called.contains(&expected),
            "{expected:?} missing from {called:?}"
        );
    }
}

#[test]
fn asking_for_a_name_does_not_require_knowing_what_kind_of_thing_it_is() {
    // A declaration and a proposition both have addresses. `words` takes
    // either, because a person asking what something is called should not have
    // to know which of the two they are holding.
    let declaration = in_project(&http(), &["--json", "words", "http::Request"]);
    let declaration: serde_json::Value =
        serde_json::from_slice(&declaration.stdout).expect("valid JSON");
    assert_eq!(declaration["kind"], "declaration");

    let proposition = in_project(&http(), &["--json", "words", "http::request"]);
    let proposition: serde_json::Value =
        serde_json::from_slice(&proposition.stdout).expect("valid JSON");
    assert_eq!(proposition["kind"], "proposition");

    // Different things, so different addresses, even though the names differ
    // only by a capital letter.
    assert_ne!(declaration["address"], proposition["address"]);
    assert_ne!(declaration["words"], proposition["words"]);
}

#[test]
fn a_proposition_nobody_proves_says_so_rather_than_looking_broken() {
    let said = String::from_utf8_lossy(&in_project(&demo(), &["words", "demo::mergesort"]).stdout)
        .to_string();
    assert!(said.contains("goes by no name yet"), "{said}");
}
