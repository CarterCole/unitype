//! Starting a child process, one at a time.
//!
//! # Why this exists
//!
//! Starting a process means creating pipes for its streams and then forking.
//! Both halves have to happen, and on some systems — macOS among them — they
//! cannot happen atomically: there is no way to create a pipe already marked
//! close-on-exec, so the flag is set in a second step. Between those two steps
//! the pipe is an ordinary file descriptor, and anything else forking at that
//! moment inherits it.
//!
//! An inherited write end is a stream that never ends. The parent closes its
//! own copy to say "that is the whole request", but a stranger is still holding
//! one open, so the child waits for an end of input that will not come. Nothing
//! is corrupted and nothing crashes; a call simply hangs until its timeout, and
//! only when two calls happen to overlap in exactly the wrong microsecond,
//! which is the worst way for a bug to behave.
//!
//! Serializing the whole of starting a process closes the window: while one
//! thread is between creating its pipes and forking, no other thread is
//! forking. It costs nothing worth measuring — a spawn is around a millisecond
//! of work that a process start dwarfs — and it only orders the beginnings.
//! Once started, children run concurrently as before.
//!
//! This cannot help with processes started by anything else in the same program
//! that does not take part. It is the reason everything here starts its children
//! through this one door.

use std::io;
use std::sync::Mutex;

use tokio::process::{Child, Command};

/// Held for the length of a spawn and nothing else.
static STARTING: Mutex<()> = Mutex::new(());

/// Starts a child, with no other start overlapping it.
///
/// The lock is not held across an await — `spawn` returns as soon as the child
/// exists — so this orders process starts without serializing the work they go
/// on to do.
pub(crate) fn spawn(process: &mut Command) -> io::Result<Child> {
    let _one_at_a_time = STARTING
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    process.spawn()
}
