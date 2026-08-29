//! A proof of `demo::sort`, compiled to WebAssembly.
//!
//! Build it with:
//!
//! ```text
//! cargo build --release --target wasm32-unknown-unknown
//! ```
//!
//! and point a manifest at the `.wasm` that lands in `target/`.
//!
//! What a guest has to do is small: hand the host somewhere to put a request,
//! read it, answer, and say where the answer is. Everything else about the
//! exchange — the shape of the message, what an error looks like — is the same
//! as it is over a pipe, so this file is the same program as the Python one
//! next door, minus the pipe.

use std::alloc::{Layout, alloc};

/// Gives the host somewhere to put a request.
///
/// # Safety
///
/// Called by the host with a length it is about to write that many bytes into.
/// The block is deliberately leaked: the host writes to it immediately after
/// this returns, and the whole instance is discarded after one call, so there
/// is nothing to free and nobody to free it.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn unitype_alloc(len: i32) -> i32 {
    let Ok(size) = usize::try_from(len) else {
        return 0;
    };
    let Ok(layout) = Layout::from_size_align(size.max(1), 1) else {
        return 0;
    };
    // SAFETY: the layout has a non-zero size and an alignment of one.
    let block = unsafe { alloc(layout) };
    block as i32
}

/// Reads one request and answers it.
///
/// # Safety
///
/// `ptr` and `len` describe a block the host allocated through
/// [`unitype_alloc`] and filled with UTF-8. The returned number packs where the
/// answer is: its pointer in the high half and its length in the low half.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn unitype_invoke(ptr: i32, len: i32) -> i64 {
    // SAFETY: the host wrote exactly this many bytes here before calling.
    let request = unsafe { std::slice::from_raw_parts(ptr as *const u8, len as usize) };
    let answer = answer(request);
    hand_back(answer)
}

/// What this module proves, and the answer to anything else.
fn answer(request: &[u8]) -> String {
    let Ok(request) = serde_json::from_slice::<serde_json::Value>(request) else {
        return failed("bad_request", "could not read the request");
    };

    if request["unitype"] != "1" {
        return failed("bad_version", "this module speaks version 1");
    }
    if request["kind"] != "invoke" {
        return failed("bad_kind", "this module only knows how to invoke");
    }
    if request["entry"] != "sort" {
        return failed("no_such_entry", "this module proves sort, and nothing else");
    }

    let Some(items) = request["args"][0].as_array() else {
        return failed("bad_args", "sort takes a list");
    };
    let mut numbers: Vec<i64> = Vec::with_capacity(items.len());
    for item in items {
        match item.as_i64() {
            Some(n) => numbers.push(n),
            None => return failed("bad_args", "sort takes a list of integers"),
        }
    }
    numbers.sort_unstable();

    serde_json::json!({ "ok": true, "value": numbers }).to_string()
}

/// A failure of the proof, said the way the protocol says it.
fn failed(code: &str, message: &str) -> String {
    serde_json::json!({ "ok": false, "error": { "code": code, "message": message } }).to_string()
}

/// Leaks the answer and says where it is.
///
/// Leaking is right here: the host reads these bytes after this function
/// returns, so freeing them first would hand back memory that is already gone,
/// and the instance is discarded straight afterwards regardless.
fn hand_back(answer: String) -> i64 {
    let bytes = answer.into_bytes().leak();
    ((bytes.as_ptr() as i64) << 32) | (bytes.len() as i64)
}
