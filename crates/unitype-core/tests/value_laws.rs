//! Properties tying values, types, and inference together.
//!
//! The one that matters most: whatever inference says about a value, the value
//! has to actually satisfy. If that ever failed, `infer` would be producing
//! types that describe nothing, and every schema derived from sample data would
//! be quietly wrong.

use proptest::prelude::*;

use unitype_core::Workspace;
use unitype_core::check::check_workspace;
use unitype_core::value::{Value, infer, infer_many};

/// Values across the whole representation, including the cases JSON has no
/// shape for.
fn arb_value() -> impl Strategy<Value = Value> {
    let leaf = prop_oneof![
        Just(Value::Null),
        any::<bool>().prop_map(Value::Bool),
        any::<i64>().prop_map(Value::Int),
        // Non-finite floats are refused at the encoding boundary, so they are
        // not values this system ever holds.
        any::<f64>()
            .prop_filter("finite", |f| f.is_finite())
            .prop_map(Value::Float),
        "[a-z ]{0,6}".prop_map(Value::Str),
        prop::collection::vec(any::<u8>(), 0..8).prop_map(Value::Bytes),
    ];

    leaf.prop_recursive(3, 24, 4, |inner| {
        prop_oneof![
            prop::collection::vec(inner.clone(), 0..4).prop_map(Value::List),
            prop::collection::vec(inner.clone(), 2..4).prop_map(Value::Tuple),
            prop::collection::vec(("[a-e]{1,3}", inner.clone()), 0..4).prop_map(Value::record),
            prop::collection::vec(("[a-e]{1,3}", inner.clone()), 0..3).prop_map(|entries| {
                Value::Map(
                    entries
                        .into_iter()
                        .map(|(k, v)| (Value::Str(k), v))
                        .collect(),
                )
            }),
        ]
    })
}

fn empty_workspace() -> unitype_core::check::CheckedWorkspace {
    check_workspace(Workspace::default()).expect("an empty workspace checks")
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(400))]

    /// A value always satisfies the type inferred from it. Without this,
    /// inference would be making claims about data that the data does not meet.
    #[test]
    fn a_value_satisfies_what_was_inferred_from_it(v in arb_value()) {
        let ws = empty_workspace();
        let inferred = infer(&v);
        prop_assert!(
            ws.validate(&v, &inferred).is_ok(),
            "{v:?} does not satisfy the type inferred from it: {inferred:?}"
        );
    }

    /// And the same holds for a whole set of samples at once, which is the case
    /// that produces optional fields and unions.
    #[test]
    fn every_sample_satisfies_what_was_inferred_from_the_set(
        samples in prop::collection::vec(arb_value(), 1..5)
    ) {
        let ws = empty_workspace();
        let inferred = infer_many(&samples);
        for sample in &samples {
            prop_assert!(
                ws.validate(sample, &inferred).is_ok(),
                "{sample:?} does not satisfy {inferred:?}, inferred from a set including it"
            );
        }
    }

    /// A value written out and read back is the same value.
    #[test]
    fn values_round_trip_through_json(v in arb_value()) {
        let json = v.to_json().expect("encodes");
        let back = Value::from_json(&json).expect("decodes");
        prop_assert_eq!(&back, &v);
    }

    /// Encoding is deterministic, which is what lets the conformance harness
    /// compare two implementations' results.
    #[test]
    fn encoding_a_value_twice_gives_the_same_text(v in arb_value()) {
        prop_assert_eq!(v.to_json_string().unwrap(), v.to_json_string().unwrap());
    }

    /// Inference does not depend on how many times it ran.
    #[test]
    fn inferring_from_one_sample_twice_over_changes_nothing(v in arb_value()) {
        prop_assert_eq!(infer(&v), infer_many([&v, &v]));
    }

    /// Nor on the order the samples arrived in.
    #[test]
    fn the_order_of_samples_does_not_change_what_is_inferred(
        a in arb_value(), b in arb_value()
    ) {
        prop_assert_eq!(infer_many([&a, &b]), infer_many([&b, &a]));
    }
}
