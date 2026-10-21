//! `fetch::fetch_text`, proved in Rust.
//!
//! Python and TypeScript get a generated harness; Rust gets this, which is the
//! whole of what taking part costs: read one JSON message per line from standard
//! input, write one back on standard output, and flush. The parent decides
//! whether to send one message and close the stream or to keep it open, and
//! this loop serves both.

use std::io::{self, BufRead, Write};

use encoding_rs::Encoding;
use serde_json::{Value, json};

/// The entry name the manifest points at.
const ENTRY: &str = "ureq::get";

fn main() {
    let mut out = io::stdout().lock();
    for line in io::stdin().lock().lines() {
        let Ok(line) = line else {
            break;
        };
        if line.trim().is_empty() {
            continue;
        }
        let answer = answer(&line);
        // A parent waiting on the next line would otherwise wait on a buffer.
        if writeln!(out, "{answer}")
            .and_then(|()| out.flush())
            .is_err()
        {
            break;
        }
    }
}

/// One request in, one response out.
///
/// A failure of the proof is reported as an answer, not by exiting: a process
/// that exits non-zero says the environment is broken, which it is not.
fn answer(line: &str) -> Value {
    let request: Value = match serde_json::from_str(line) {
        Ok(request) => request,
        Err(e) => return fail("bad_request", format!("could not read the request: {e}")),
    };
    if request["unitype"] != "1" {
        return fail(
            "bad_version",
            format!("this speaks version 1, not {}", request["unitype"]),
        );
    }
    if request["kind"] != "invoke" {
        return fail("bad_kind", format!("cannot do {}", request["kind"]));
    }
    if request["entry"] != ENTRY {
        return fail(
            "no_such_entry",
            format!("this proves {ENTRY}, not {}", request["entry"]),
        );
    }
    let Some(url) = request["args"].get(0).and_then(Value::as_str) else {
        return fail("bad_arguments", "expected one argument, a URL".into());
    };
    match fetch_text(url) {
        Ok(text) => json!({ "ok": true, "value": text }),
        Err(why) => fail("fetch_failed", why),
    }
}

/// Fetches a page and decodes it the way the server said it is encoded.
fn fetch_text(url: &str) -> Result<String, String> {
    // A status of 400 or more is an error here by default, which is what the
    // proposition asks for.
    let mut response = ureq::get(url).call().map_err(|e| e.to_string())?;
    let label = response
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .and_then(charset)
        .unwrap_or("utf-8")
        .to_string();
    let bytes = response
        .body_mut()
        .read_to_vec()
        .map_err(|e| e.to_string())?;

    let encoding = Encoding::for_label(label.as_bytes())
        .ok_or_else(|| format!("`{label}` is not a charset this knows"))?;
    // Strict, like the other proofs: a byte that does not belong is a failure,
    // not a replacement character.
    let (text, malformed) = encoding.decode_with_bom_removal(&bytes);
    if malformed {
        return Err(format!("the body is not valid {}", encoding.name()));
    }
    Ok(text.into_owned())
}

/// The `charset` parameter of a Content-Type, if it has one.
fn charset(content_type: &str) -> Option<&str> {
    content_type
        .split(';')
        .skip(1)
        .filter_map(|param| param.split_once('='))
        .find(|(key, _)| key.trim().eq_ignore_ascii_case("charset"))
        .map(|(_, value)| value.trim().trim_matches('"'))
}

fn fail(code: &str, message: String) -> Value {
    json!({ "ok": false, "error": { "code": code, "message": message } })
}
