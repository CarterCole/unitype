//! `net::parse_v4`, `format_v4`, `parse_v6` and `format_v6`, proved in Rust.
//!
//! Python and TypeScript get a generated harness; Rust gets this, which is the
//! whole of what taking part costs: read one JSON message per line from standard
//! input, write one back on standard output, and flush. The parent decides
//! whether to send one message and close the stream or to keep it open, and
//! this loop serves both.
//!
//! The proofs themselves are `std::net`: `Ipv4Addr` and `Ipv6Addr` parse from
//! text, give their bytes and segments back, and print as RFC 5952 says.

use std::io::{self, BufRead, Write};
use std::net::{Ipv4Addr, Ipv6Addr};

use serde_json::{Value, json};

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
    let Some(entry) = request["entry"].as_str() else {
        return fail("bad_request", "no entry named".into());
    };
    let args = request["args"].as_array().cloned().unwrap_or_default();
    match prove(entry, &args) {
        Ok(value) => json!({ "ok": true, "value": value }),
        Err((code, why)) => fail(code, why),
    }
}

/// The four propositions, by the entry name the manifest gives each.
fn prove(entry: &str, args: &[Value]) -> Result<Value, (&'static str, String)> {
    match entry {
        "Ipv4Addr::from_str" => {
            let text = text(args)?;
            Ok(match text.parse::<Ipv4Addr>() {
                Ok(ip) => json!(ip.octets()),
                Err(_) => Value::Null,
            })
        }
        "Ipv4Addr::to_string" => {
            let octets: [u8; 4] = numbers(args)?;
            Ok(json!(Ipv4Addr::from(octets).to_string()))
        }
        "Ipv6Addr::from_str" => {
            let text = text(args)?;
            Ok(match text.parse::<Ipv6Addr>() {
                Ok(ip) => json!(ip.segments()),
                Err(_) => Value::Null,
            })
        }
        "Ipv6Addr::to_string" => {
            let segments: [u16; 8] = numbers(args)?;
            Ok(json!(Ipv6Addr::from(segments).to_string()))
        }
        other => Err(("no_such_entry", format!("nothing here is called {other}"))),
    }
}

/// The one argument, as text.
fn text(args: &[Value]) -> Result<&str, (&'static str, String)> {
    args.first()
        .and_then(Value::as_str)
        .ok_or(("bad_arguments", "expected one argument, a string".into()))
}

/// The one argument, as exactly as many numbers as the address has.
///
/// The parent has checked the bounds already — an octet over 255 never gets
/// here — so a number that does not fit is a protocol failure, not a case.
fn numbers<T, const N: usize>(args: &[Value]) -> Result<[T; N], (&'static str, String)>
where
    T: TryFrom<u64> + Copy + Default,
{
    let items = args
        .first()
        .and_then(Value::as_array)
        .ok_or(("bad_arguments", "expected one argument, a list".into()))?;
    let mut out = [T::default(); N];
    if items.len() != N {
        return Err((
            "bad_arguments",
            format!("expected {N} numbers, got {}", items.len()),
        ));
    }
    for (slot, item) in out.iter_mut().zip(items) {
        *slot = item
            .as_u64()
            .and_then(|n| T::try_from(n).ok())
            .ok_or(("bad_arguments", format!("{item} is not a number that fits")))?;
    }
    Ok(out)
}

fn fail(code: &str, message: String) -> Value {
    json!({ "ok": false, "error": { "code": code, "message": message } })
}
