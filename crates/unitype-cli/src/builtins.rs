//! Proofs compiled into this program.
//!
//! A native implementation has to be linked into whatever binary runs it —
//! there is no loading a Rust function out of a file — so a command-line tool
//! can only offer the ones it was built with. These are here so that the
//! example project is self-contained: without a compiled-in proof there would
//! be nothing for a Python one to be chosen *over*, and the whole point of
//! having a policy would be invisible.
//!
//! A real program embedding unitype registers its own instead, and does it the
//! same way: this is a module registering itself as an execution source, and
//! being linked in is the only thing that makes it different.

use unitype_core::value::Value;
use unitype_runtime::{ExecError, Registry};

/// Registers what this build can prove for itself.
pub fn register(registry: &mut Registry) {
    // Sorting, the fast way: needs room for what it merges.
    registry.register_native_sync("builtin::merge_sort", |args| {
        let mut items = integers(args.first())?;
        items.sort_unstable();
        Ok(Value::List(items.into_iter().map(Value::Int).collect()))
    });

    // Reversing, which needs nothing beyond the list.
    registry.register_native_sync("builtin::reverse", |args| {
        let Some(Value::List(items)) = args.into_iter().next() else {
            return Err(ExecError::Untyped("expected a list".into()));
        };
        Ok(Value::List(items.into_iter().rev().collect()))
    });

    // The identity, which is occasionally exactly what a link needs.
    registry.register_native_sync("builtin::identity", |mut args| {
        if args.len() == 1 {
            Ok(args.remove(0))
        } else {
            Err(ExecError::Untyped(format!(
                "identity takes one thing, not {}",
                args.len()
            )))
        }
    });
}

/// The integers in a list argument, or a clear complaint.
fn integers(value: Option<&Value>) -> Result<Vec<i64>, ExecError> {
    let Some(Value::List(items)) = value else {
        return Err(ExecError::Untyped(format!(
            "expected a list of integers, got {}",
            value.map(Value::kind_name).unwrap_or("nothing")
        )));
    };
    items
        .iter()
        .map(|item| {
            item.as_int().ok_or_else(|| {
                ExecError::Untyped(format!(
                    "expected a list of integers, but one is a {}",
                    item.kind_name()
                ))
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn what_is_compiled_in_is_reachable_by_name() {
        let mut registry = Registry::new();
        register(&mut registry);

        let sort = registry.native("builtin::merge_sort").expect("registered");
        let sorted = sort(vec![Value::List(vec![Value::Int(3), Value::Int(1)])])
            .await
            .expect("runs");
        assert_eq!(sorted, Value::List(vec![Value::Int(1), Value::Int(3)]));
    }

    #[tokio::test]
    async fn being_handed_the_wrong_thing_says_what_was_wanted() {
        let mut registry = Registry::new();
        register(&mut registry);

        let sort = registry.native("builtin::merge_sort").expect("registered");
        let err = sort(vec![Value::Str("not a list".into())])
            .await
            .unwrap_err();
        assert!(err.to_string().contains("list of integers"), "{err}");
    }
}
