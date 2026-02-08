//! Property-based invariant tests for the reactive Observable module.
//!
//! These tests verify structural invariants of Observable<T> that must hold
//! for any valid inputs:
//!
//! 1. Version starts at 0.
//! 2. Version increments by exactly 1 on each value-changing set.
//! 3. Set with same value is a no-op (no version bump).
//! 4. Version is monotonically non-decreasing.
//! 5. get() after set(v) returns v.
//! 6. Clone shares state (version, value).
//! 7. Multiple distinct sets accumulate correct version.
//! 8. update() with no-op closure is a no-op.
//! 9. Subscriber count is non-negative.
//! 10. No panics on arbitrary set/get sequences.

use ftui_runtime::reactive::Observable;
use proptest::prelude::*;

// ═════════════════════════════════════════════════════════════════════════
// 1. Version starts at 0
// ═════════════════════════════════════════════════════════════════════════

proptest! {
    #[test]
    fn version_starts_at_zero(val in any::<i64>()) {
        let obs = Observable::new(val);
        prop_assert_eq!(obs.version(), 0);
    }
}

// ═════════════════════════════════════════════════════════════════════════
// 2. Version increments by 1 on value-changing set
// ═════════════════════════════════════════════════════════════════════════

proptest! {
    #[test]
    fn version_increments_on_change(
        init in any::<i32>(),
        new_val in any::<i32>(),
    ) {
        let obs = Observable::new(init);
        let before = obs.version();
        obs.set(new_val);
        let after = obs.version();

        if init != new_val {
            prop_assert_eq!(
                after, before + 1,
                "Version should increment by 1 on change: {} -> {}, version {} -> {}",
                init, new_val, before, after
            );
        } else {
            prop_assert_eq!(
                after, before,
                "Version should not change for same value"
            );
        }
    }
}

// ═════════════════════════════════════════════════════════════════════════
// 3. Set with same value is a no-op
// ═════════════════════════════════════════════════════════════════════════

proptest! {
    #[test]
    fn same_value_set_is_noop(val in any::<i32>()) {
        let obs = Observable::new(val);
        let v0 = obs.version();
        obs.set(val);
        prop_assert_eq!(obs.version(), v0, "Setting same value should not bump version");
        // Setting it multiple times should still be a no-op
        obs.set(val);
        obs.set(val);
        prop_assert_eq!(obs.version(), v0);
    }
}

// ═════════════════════════════════════════════════════════════════════════
// 4. Version is monotonically non-decreasing
// ═════════════════════════════════════════════════════════════════════════

proptest! {
    #[test]
    fn version_monotone(
        init in any::<i32>(),
        values in proptest::collection::vec(any::<i32>(), 1..=100),
    ) {
        let obs = Observable::new(init);
        let mut prev_version = obs.version();
        for &v in &values {
            obs.set(v);
            let cur_version = obs.version();
            prop_assert!(
                cur_version >= prev_version,
                "Version should never decrease: {} -> {}",
                prev_version, cur_version
            );
            prev_version = cur_version;
        }
    }
}

// ═════════════════════════════════════════════════════════════════════════
// 5. get() after set(v) returns v
// ═════════════════════════════════════════════════════════════════════════

proptest! {
    #[test]
    fn get_after_set_returns_value(
        init in any::<i64>(),
        new_val in any::<i64>(),
    ) {
        let obs = Observable::new(init);
        obs.set(new_val);
        prop_assert_eq!(
            obs.get(), new_val,
            "get() should return {} after set({})",
            new_val, new_val
        );
    }
}

// ═════════════════════════════════════════════════════════════════════════
// 6. Clone shares state
// ═════════════════════════════════════════════════════════════════════════

proptest! {
    #[test]
    fn clone_shares_state(
        init in any::<i32>(),
        new_val in any::<i32>(),
    ) {
        let obs1 = Observable::new(init);
        let obs2 = obs1.clone();

        obs1.set(new_val);
        prop_assert_eq!(
            obs2.get(), new_val,
            "Clone should see value set on original"
        );
        prop_assert_eq!(
            obs1.version(), obs2.version(),
            "Clone should share version"
        );
    }
}

// ═════════════════════════════════════════════════════════════════════════
// 7. Multiple distinct sets accumulate correct version
// ═════════════════════════════════════════════════════════════════════════

proptest! {
    #[test]
    fn distinct_sets_accumulate_version(
        init in any::<i32>(),
        values in proptest::collection::vec(any::<i32>(), 1..=50),
    ) {
        let obs = Observable::new(init);
        let mut prev = init;
        let mut expected_version = 0u64;
        for &v in &values {
            obs.set(v);
            if v != prev {
                expected_version += 1;
            }
            prev = v;
        }
        prop_assert_eq!(
            obs.version(), expected_version,
            "Version should be {} after {} sets starting from {}",
            expected_version, values.len(), init
        );
    }
}

// ═════════════════════════════════════════════════════════════════════════
// 8. update() with identity closure is a no-op
// ═════════════════════════════════════════════════════════════════════════

proptest! {
    #[test]
    fn identity_update_is_noop(val in any::<i32>()) {
        let obs = Observable::new(val);
        let v0 = obs.version();
        obs.update(|_| {}); // doesn't change value
        prop_assert_eq!(obs.version(), v0, "Identity update should not bump version");
        prop_assert_eq!(obs.get(), val, "Identity update should preserve value");
    }
}

// ═════════════════════════════════════════════════════════════════════════
// 9. Subscriber count is non-negative
// ═════════════════════════════════════════════════════════════════════════

proptest! {
    #[test]
    fn subscriber_count_non_negative(
        init in any::<i32>(),
        num_subs in 0usize..=10,
    ) {
        let obs = Observable::new(init);
        let mut subs = Vec::new();
        for _ in 0..num_subs {
            subs.push(obs.subscribe(|_| {}));
        }
        prop_assert_eq!(obs.subscriber_count(), num_subs);
        // Drop half
        let keep = num_subs / 2;
        subs.truncate(keep);
        // Trigger notify to prune dead subscribers
        obs.set(init.wrapping_add(1));
        prop_assert_eq!(
            obs.subscriber_count(), keep,
            "After dropping {} subs and notifying, count should be {}",
            num_subs - keep, keep
        );
    }
}

// ═════════════════════════════════════════════════════════════════════════
// 10. No panics on arbitrary set/get sequences
// ═════════════════════════════════════════════════════════════════════════

proptest! {
    #[test]
    fn no_panic_operations(
        init in any::<i64>(),
        values in proptest::collection::vec(any::<i64>(), 0..=100),
    ) {
        let obs = Observable::new(init);
        for &v in &values {
            obs.set(v);
            let _ = obs.get();
            let _ = obs.version();
            let _ = obs.subscriber_count();
        }
        obs.with(|v| {
            let _ = v;
        });
    }
}
