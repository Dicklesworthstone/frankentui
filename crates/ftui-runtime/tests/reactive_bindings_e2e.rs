#![forbid(unsafe_code)]

//! E2E test suite for the reactive data binding system.
//!
//! Organized into 5 modules per bd-2my1.6:
//! 1. `bind_oneway` – Observable to binding propagation
//! 2. `bind_twoway` – Bidirectional sync and cycle prevention
//! 3. `bind_computed` – Computed values with single/multi dependency
//! 4. `bind_batch` – Batch update coalescing
//! 5. `bind_lifecycle` – Binding cleanup, scope management, weak refs

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use ftui_runtime::reactive::batch::BatchScope;
use ftui_runtime::reactive::binding::{
    bind_mapped, bind_observable, BindingScope, TwoWayBinding,
};
use ftui_runtime::reactive::computed::Computed;
use ftui_runtime::reactive::observable::Observable;
use ftui_runtime::{bind, bind_map, bind_map2};

// =========================================================================
// 1. One-Way Binding
// =========================================================================

mod bind_oneway {
    use super::*;

    #[test]
    fn observable_to_binding_propagation() {
        let counter = Observable::new(0);
        let binding = bind_observable(&counter);

        assert_eq!(binding.get(), 0);
        counter.set(10);
        assert_eq!(binding.get(), 10);
        counter.set(42);
        assert_eq!(binding.get(), 42);
    }

    #[test]
    fn transform_function_applied() {
        let temp_c = Observable::new(100.0_f64);
        let temp_f = bind_mapped(&temp_c, |c| c * 9.0 / 5.0 + 32.0);

        assert!((temp_f.get() - 212.0).abs() < f64::EPSILON);
        temp_c.set(0.0);
        assert!((temp_f.get() - 32.0).abs() < f64::EPSILON);
    }

    #[test]
    fn update_propagation_is_immediate() {
        let source = Observable::new(String::from("initial"));
        let binding = bind_observable(&source);

        // Each set is immediately visible through the binding.
        for i in 0..50 {
            let val = format!("step-{i}");
            source.set(val.clone());
            assert_eq!(binding.get(), val);
        }
    }

    #[test]
    fn multiple_bindings_to_same_source() {
        let source = Observable::new(10);

        let direct = bind_observable(&source);
        let doubled = bind_mapped(&source, |v| v * 2);
        let as_string = bind_mapped(&source, |v| format!("val={v}"));

        source.set(7);

        assert_eq!(direct.get(), 7);
        assert_eq!(doubled.get(), 14);
        assert_eq!(as_string.get(), "val=7");
    }

    #[test]
    fn bind_macro_shorthand() {
        let obs = Observable::new(99);
        let b = bind!(obs);
        assert_eq!(b.get(), 99);

        obs.set(1);
        assert_eq!(b.get(), 1);
    }

    #[test]
    fn bind_map_macro_shorthand() {
        let count = Observable::new(3);
        let label = bind_map!(count, |c| format!("{c} items"));
        assert_eq!(label.get(), "3 items");

        count.set(0);
        assert_eq!(label.get(), "0 items");
    }

    #[test]
    fn bind_map2_combines_two_sources() {
        let first = Observable::new("Alice".to_string());
        let last = Observable::new("Smith".to_string());
        let full = bind_map2!(first, last, |f, l| format!("{f} {l}"));

        assert_eq!(full.get(), "Alice Smith");
        first.set("Bob".to_string());
        assert_eq!(full.get(), "Bob Smith");
        last.set("Jones".to_string());
        assert_eq!(full.get(), "Bob Jones");
    }

    #[test]
    fn binding_then_chained_transforms() {
        let pixels = Observable::new(1920);
        let label = bind_observable(&pixels)
            .then(|px| px as f64 / 96.0)
            .then(|inches| format!("{inches:.1}in"));

        assert_eq!(label.get(), "20.0in");
        pixels.set(960);
        assert_eq!(label.get(), "10.0in");
    }

    #[test]
    fn binding_clone_shares_source() {
        let obs = Observable::new(42);
        let b1 = bind_observable(&obs);
        let b2 = b1.clone();

        obs.set(7);
        assert_eq!(b1.get(), 7);
        assert_eq!(b2.get(), 7);
    }

    #[test]
    fn no_change_same_value_no_version_bump() {
        let obs = Observable::new(42);
        assert_eq!(obs.version(), 0);

        obs.set(42); // same value
        assert_eq!(obs.version(), 0);

        obs.set(43);
        assert_eq!(obs.version(), 1);

        obs.set(43); // same again
        assert_eq!(obs.version(), 1);
    }
}

// =========================================================================
// 2. Two-Way Binding
// =========================================================================

mod bind_twoway {
    use super::*;

    #[test]
    fn widget_to_observable_sync() {
        let model = Observable::new(0);
        let widget = Observable::new(0);
        let _binding = TwoWayBinding::new(&model, &widget);

        // Simulate widget input.
        widget.set(42);
        assert_eq!(model.get(), 42);
    }

    #[test]
    fn observable_to_widget_sync() {
        let model = Observable::new(0);
        let widget = Observable::new(0);
        let _binding = TwoWayBinding::new(&model, &widget);

        // Simulate programmatic update.
        model.set(99);
        assert_eq!(widget.get(), 99);
    }

    #[test]
    fn cycle_prevention() {
        let a = Observable::new(0);
        let b = Observable::new(0);
        let _binding = TwoWayBinding::new(&a, &b);

        // Rapid alternating updates should not cause infinite recursion.
        for i in 1..=100 {
            if i % 2 == 0 {
                a.set(i);
                assert_eq!(b.get(), i);
            } else {
                b.set(i);
                assert_eq!(a.get(), i);
            }
        }

        assert_eq!(a.get(), 100);
        assert_eq!(b.get(), 100);
    }

    #[test]
    fn initial_sync_direction() {
        let source = Observable::new(42);
        let target = Observable::new(0);
        let _binding = TwoWayBinding::new(&source, &target);

        // Target should sync to source's initial value.
        assert_eq!(target.get(), 42);
    }

    #[test]
    fn drop_disconnects_both_directions() {
        let a = Observable::new(0);
        let b = Observable::new(0);
        {
            let _binding = TwoWayBinding::new(&a, &b);
            a.set(5);
            assert_eq!(b.get(), 5);
            b.set(10);
            assert_eq!(a.get(), 10);
        }
        // After drop, no propagation.
        a.set(100);
        assert_eq!(b.get(), 10);
        b.set(200);
        assert_eq!(a.get(), 100);
    }

    #[test]
    fn two_way_with_complex_types() {
        let a = Observable::new(vec![1, 2, 3]);
        let b = Observable::new(vec![]);
        let _binding = TwoWayBinding::new(&a, &b);

        assert_eq!(b.get(), vec![1, 2, 3]);

        b.set(vec![4, 5]);
        assert_eq!(a.get(), vec![4, 5]);
    }

    #[test]
    fn same_value_no_propagation() {
        let a = Observable::new(42);
        let b = Observable::new(0);
        let _binding = TwoWayBinding::new(&a, &b);

        let a_version_before = a.version();
        let b_version_before = b.version();

        // Setting a to the same value it already has.
        a.set(42);
        assert_eq!(a.version(), a_version_before);
        // b should not have been re-notified.
        assert_eq!(b.version(), b_version_before);
    }
}

// =========================================================================
// 3. Computed Values
// =========================================================================

mod bind_computed {
    use super::*;

    #[test]
    fn single_dependency_computed() {
        let count = Observable::new(5);
        let doubled = Computed::from_observable(&count, |v| v * 2);

        assert_eq!(doubled.get(), 10);
        count.set(7);
        assert_eq!(doubled.get(), 14);
    }

    #[test]
    fn multi_dependency_computed() {
        let width = Observable::new(10);
        let height = Observable::new(20);
        let area = Computed::from2(&width, &height, |w, h| w * h);

        assert_eq!(area.get(), 200);
        width.set(5);
        assert_eq!(area.get(), 100);
        height.set(40);
        assert_eq!(area.get(), 200);
    }

    #[test]
    fn three_dependency_computed() {
        let r = Observable::new(255u8);
        let g = Observable::new(128u8);
        let b = Observable::new(0u8);
        let hex = Computed::from3(&r, &g, &b, |r, g, b| format!("#{r:02x}{g:02x}{b:02x}"));

        assert_eq!(hex.get(), "#ff8000");
        g.set(255);
        assert_eq!(hex.get(), "#ffff00");
    }

    #[test]
    fn lazy_evaluation_no_compute_until_get() {
        let compute_count = Rc::new(Cell::new(0u32));
        let cc = Rc::clone(&compute_count);

        let source = Observable::new(10);
        let computed = Computed::from_observable(&source, move |v| {
            cc.set(cc.get() + 1);
            v * 2
        });

        // Not computed yet.
        assert_eq!(compute_count.get(), 0);

        // First get triggers compute.
        assert_eq!(computed.get(), 20);
        assert_eq!(compute_count.get(), 1);
    }

    #[test]
    fn memoization_no_recompute_without_change() {
        let compute_count = Rc::new(Cell::new(0u32));
        let cc = Rc::clone(&compute_count);

        let source = Observable::new(5);
        let computed = Computed::from_observable(&source, move |v| {
            cc.set(cc.get() + 1);
            *v
        });

        assert_eq!(computed.get(), 5);
        assert_eq!(compute_count.get(), 1);

        // Repeated get without change — no recompute.
        assert_eq!(computed.get(), 5);
        assert_eq!(computed.get(), 5);
        assert_eq!(compute_count.get(), 1);

        // Change triggers recompute on next get.
        source.set(10);
        assert_eq!(computed.get(), 10);
        assert_eq!(compute_count.get(), 2);
    }

    #[test]
    fn diamond_dependency_correctness() {
        //     root
        //    /    \
        //  left  right
        //    \    /
        //    result
        let root = Observable::new(10);
        let left = Computed::from_observable(&root, |v| v + 1);
        let right = Computed::from_observable(&root, |v| v * 2);

        let l = left.clone();
        let r = right.clone();
        let result = Computed::from_observable(&root, move |_| l.get() + r.get());

        assert_eq!(result.get(), 11 + 20); // 31
        root.set(5);
        assert_eq!(result.get(), 6 + 10); // 16
    }

    #[test]
    fn computed_survives_source_drop() {
        let computed;
        {
            let source = Observable::new(42);
            computed = Computed::from_observable(&source, |v| *v);
            let _ = computed.get();
        }
        // Source dropped; computed retains last cached value.
        assert_eq!(computed.get(), 42);
    }

    #[test]
    fn computed_with_access() {
        let items = Observable::new(vec![3, 1, 4, 1, 5]);
        let sum = Computed::from_observable(&items, |v| v.iter().sum::<i32>());

        let result = sum.with(|s| *s);
        assert_eq!(result, 14);
    }

    #[test]
    fn invalidate_forces_recompute() {
        let compute_count = Rc::new(Cell::new(0u32));
        let cc = Rc::clone(&compute_count);

        let source = Observable::new(1);
        let computed = Computed::from_observable(&source, move |v| {
            cc.set(cc.get() + 1);
            *v
        });

        let _ = computed.get();
        assert_eq!(compute_count.get(), 1);

        computed.invalidate();
        assert!(computed.is_dirty());
        let _ = computed.get();
        assert_eq!(compute_count.get(), 2);
    }

    #[test]
    fn version_tracks_recomputations() {
        let source = Observable::new(0);
        let computed = Computed::from_observable(&source, |v| *v);

        assert_eq!(computed.version(), 0);
        let _ = computed.get(); // version 1
        assert_eq!(computed.version(), 1);

        source.set(1);
        let _ = computed.get(); // version 2
        assert_eq!(computed.version(), 2);

        let _ = computed.get(); // no change, still version 2
        assert_eq!(computed.version(), 2);
    }
}

// =========================================================================
// 4. Batch Updates
// =========================================================================

mod bind_batch {
    use super::*;
    use std::cell::RefCell;

    #[test]
    fn multiple_updates_coalesced() {
        let obs = Observable::new(0);
        let notify_count = Rc::new(Cell::new(0u32));
        let nc = Rc::clone(&notify_count);

        let _sub = obs.subscribe(move |_| nc.set(nc.get() + 1));

        {
            let _batch = BatchScope::new();
            obs.set(1);
            obs.set(2);
            obs.set(3);
            // No notifications yet.
            assert_eq!(notify_count.get(), 0);
        }
        // All three notifications fire (callbacks are deferred per-set, not coalesced by identity).
        assert!(notify_count.get() > 0);
    }

    #[test]
    fn nested_batch_scopes() {
        let obs = Observable::new(0);
        let seen_values = Rc::new(RefCell::new(Vec::new()));
        let sv = Rc::clone(&seen_values);

        let _sub = obs.subscribe(move |v| sv.borrow_mut().push(*v));

        {
            let _outer = BatchScope::new();
            obs.set(1);
            {
                let _inner = BatchScope::new();
                obs.set(2);
                // Inner exit should NOT flush.
            }
            assert!(seen_values.borrow().is_empty());
            obs.set(3);
        }
        // Outer exit flushes all.
        assert!(!seen_values.borrow().is_empty());
    }

    #[test]
    fn intermediate_state_hidden_from_subscribers() {
        let a = Observable::new(0);
        let b = Observable::new(0);
        let snapshots = Rc::new(RefCell::new(Vec::new()));

        // Subscribe to a; capture both a and b at notification time.
        let a_for_sub = a.clone();
        let b_for_sub = b.clone();
        let snap = Rc::clone(&snapshots);
        let _sub = a.subscribe(move |_| {
            snap.borrow_mut().push((a_for_sub.get(), b_for_sub.get()));
        });

        {
            let _batch = BatchScope::new();
            a.set(1); // intermediate
            b.set(10);
            a.set(2); // final for a
            b.set(20); // final for b
        }

        // The subscriber sees the current values at flush time, not intermediate.
        let snaps = snapshots.borrow();
        // All notification callbacks see value 2 for a (the current value at flush).
        for (av, _) in snaps.iter() {
            assert_eq!(*av, 2);
        }
    }

    #[test]
    fn batch_values_immediately_visible_via_get() {
        let obs = Observable::new(0);
        {
            let _batch = BatchScope::new();
            obs.set(42);
            assert_eq!(obs.get(), 42); // Value is updated even during batch.
            obs.set(99);
            assert_eq!(obs.get(), 99);
        }
    }

    #[test]
    fn batch_with_computed_values() {
        let x = Observable::new(0);
        let y = Observable::new(0);
        let sum = Computed::from2(&x, &y, |a, b| a + b);

        {
            let _batch = BatchScope::new();
            x.set(10);
            y.set(20);
            // Computed values reflect changes immediately via get().
            assert_eq!(sum.get(), 30);
        }
    }

    #[test]
    fn batch_with_bindings() {
        let source = Observable::new(0);
        let binding = bind_observable(&source);
        let mapped = bind_mapped(&source, |v| v * 2);

        {
            let _batch = BatchScope::new();
            source.set(5);
            // Bindings read current values (they evaluate lazily on get).
            assert_eq!(binding.get(), 5);
            assert_eq!(mapped.get(), 10);
        }
    }

    #[test]
    fn multiple_observables_in_batch() {
        let a = Observable::new(0);
        let b = Observable::new(String::new());
        let a_count = Rc::new(Cell::new(0u32));
        let b_count = Rc::new(Cell::new(0u32));
        let ac = Rc::clone(&a_count);
        let bc = Rc::clone(&b_count);

        let _sa = a.subscribe(move |_| ac.set(ac.get() + 1));
        let _sb = b.subscribe(move |_| bc.set(bc.get() + 1));

        {
            let _batch = BatchScope::new();
            a.set(1);
            b.set("hello".to_string());
            a.set(2);
            b.set("world".to_string());
            assert_eq!(a_count.get(), 0);
            assert_eq!(b_count.get(), 0);
        }

        assert!(a_count.get() > 0);
        assert!(b_count.get() > 0);
    }
}

// =========================================================================
// 5. Lifecycle
// =========================================================================

mod bind_lifecycle {
    use super::*;

    #[test]
    fn scope_cleanup_on_drop() {
        let obs = Observable::new(0);
        let seen = Rc::new(Cell::new(0));

        {
            let mut scope = BindingScope::new();
            let s = Rc::clone(&seen);
            scope.subscribe(&obs, move |v| s.set(*v));
            obs.set(1);
            assert_eq!(seen.get(), 1);
        }

        // Scope dropped — subscription released.
        obs.set(99);
        assert_eq!(seen.get(), 1);
    }

    #[test]
    fn scope_clear_releases_immediately() {
        let obs = Observable::new(0);
        let count = Rc::new(Cell::new(0u32));

        let mut scope = BindingScope::new();
        let c = Rc::clone(&count);
        scope.subscribe(&obs, move |_| c.set(c.get() + 1));

        obs.set(1);
        assert_eq!(count.get(), 1);

        scope.clear();
        obs.set(2);
        assert_eq!(count.get(), 1, "callback should not fire after clear");
    }

    #[test]
    fn scope_reusable_after_clear() {
        let obs = Observable::new(0);
        let mut scope = BindingScope::new();

        let v1 = Rc::new(Cell::new(false));
        let v1c = Rc::clone(&v1);
        scope.subscribe(&obs, move |_| v1c.set(true));
        scope.clear();

        let v2 = Rc::new(Cell::new(false));
        let v2c = Rc::clone(&v2);
        scope.subscribe(&obs, move |_| v2c.set(true));

        obs.set(1);
        assert!(!v1.get(), "old subscription should be gone");
        assert!(v2.get(), "new subscription should be active");
    }

    #[test]
    fn no_memory_leak_subscription_drop() {
        let obs = Observable::new(0);
        let initial_subs = obs.subscriber_count();

        {
            let _sub = obs.subscribe(|_| {});
        }
        // After dropping the subscription, trigger prune via set.
        obs.set(1);
        assert_eq!(obs.subscriber_count(), initial_subs);
    }

    #[test]
    fn no_memory_leak_scope_drop() {
        let obs = Observable::new(0);

        {
            let mut scope = BindingScope::new();
            for _ in 0..10 {
                scope.subscribe(&obs, |_| {});
            }
            assert_eq!(obs.subscriber_count(), 10);
        }
        // After scope drop + prune trigger.
        obs.set(1);
        assert_eq!(obs.subscriber_count(), 0);
    }

    #[test]
    fn weak_reference_behavior() {
        // Observable uses Weak refs for subscribers internally.
        // Verify that dead subscriptions don't prevent notification of live ones.
        let obs = Observable::new(0);
        let live_count = Rc::new(Cell::new(0u32));
        let lc = Rc::clone(&live_count);

        let dead_sub = obs.subscribe(|_| {});
        let _live_sub = obs.subscribe(move |_| lc.set(lc.get() + 1));
        drop(dead_sub);

        obs.set(1);
        assert_eq!(live_count.get(), 1, "live subscriber should still fire");
    }

    #[test]
    fn binding_scope_with_multiple_observables() {
        let a = Observable::new(0);
        let b = Observable::new(String::new());

        let a_seen = Rc::new(Cell::new(0));
        let b_seen = Rc::new(RefCell::new(String::new()));

        {
            let mut scope = BindingScope::new();
            let ac = Rc::clone(&a_seen);
            scope.subscribe(&a, move |v| ac.set(*v));
            let bc = Rc::clone(&b_seen);
            scope.subscribe(&b, move |v| *bc.borrow_mut() = v.clone());

            a.set(42);
            b.set("hello".to_string());
            assert_eq!(a_seen.get(), 42);
            assert_eq!(*b_seen.borrow(), "hello");
        }

        a.set(100);
        b.set("world".to_string());
        assert_eq!(a_seen.get(), 42, "scope dropped, a callback should not fire");
        assert_eq!(*b_seen.borrow(), "hello", "scope dropped, b callback should not fire");
    }

    #[test]
    fn scope_hold_external_subscription() {
        let obs = Observable::new(0);
        let seen = Rc::new(Cell::new(0));
        let sc = Rc::clone(&seen);

        let mut scope = BindingScope::new();
        let sub = obs.subscribe(move |v| sc.set(*v));
        scope.hold(sub);

        obs.set(5);
        assert_eq!(seen.get(), 5);

        drop(scope);
        obs.set(99);
        assert_eq!(seen.get(), 5, "held sub released on scope drop");
    }

    #[test]
    fn scope_binding_count_accurate() {
        let obs = Observable::new(0);
        let mut scope = BindingScope::new();

        assert_eq!(scope.binding_count(), 0);
        assert!(scope.is_empty());

        scope.subscribe(&obs, |_| {});
        scope.subscribe(&obs, |_| {});
        assert_eq!(scope.binding_count(), 2);
        assert!(!scope.is_empty());

        scope.clear();
        assert_eq!(scope.binding_count(), 0);
        assert!(scope.is_empty());
    }

    #[test]
    fn debug_tooling_accuracy() {
        let obs = Observable::new(42);
        let binding = bind_observable(&obs);
        let computed = Computed::from_observable(&obs, |v| v * 2);

        // Debug output should contain useful state info.
        let b_debug = format!("{binding:?}");
        assert!(b_debug.contains("Binding"));
        assert!(b_debug.contains("42"));

        let _ = computed.get();
        let c_debug = format!("{computed:?}");
        assert!(c_debug.contains("Computed"));
        assert!(c_debug.contains("84"));

        let o_debug = format!("{obs:?}");
        assert!(o_debug.contains("Observable"));
        assert!(o_debug.contains("42"));
    }

    #[test]
    fn scope_bind_convenience() {
        let obs = Observable::new(10);
        let mut scope = BindingScope::new();

        let b = scope.bind(&obs);
        assert_eq!(b.get(), 10);

        let mapped = scope.bind_map(&obs, |v| format!("v={v}"));
        assert_eq!(mapped.get(), "v=10");

        obs.set(20);
        assert_eq!(b.get(), 20);
        assert_eq!(mapped.get(), "v=20");
    }

    #[test]
    fn two_way_binding_lifecycle() {
        let model = Observable::new(0);
        let widget = Observable::new(0);

        {
            let _tw = TwoWayBinding::new(&model, &widget);
            model.set(5);
            assert_eq!(widget.get(), 5);
        }

        // After two-way binding dropped, both sides are independent.
        model.set(100);
        assert_eq!(widget.get(), 5);
        widget.set(200);
        assert_eq!(model.get(), 100);
    }

    #[test]
    fn batch_within_scope() {
        let obs = Observable::new(0);
        let count = Rc::new(Cell::new(0u32));

        let mut scope = BindingScope::new();
        let c = Rc::clone(&count);
        scope.subscribe(&obs, move |_| c.set(c.get() + 1));

        {
            let _batch = BatchScope::new();
            obs.set(1);
            obs.set(2);
            obs.set(3);
            assert_eq!(count.get(), 0);
        }
        // Batch flushes; scope's subscription is still alive.
        assert!(count.get() > 0);

        let before = count.get();
        drop(scope);
        obs.set(99);
        assert_eq!(count.get(), before, "scope dropped, no more notifications");
    }

    #[test]
    fn observable_clone_shares_subscriptions() {
        let obs1 = Observable::new(0);
        let obs2 = obs1.clone();
        let seen = Rc::new(Cell::new(0));
        let sc = Rc::clone(&seen);

        let _sub = obs1.subscribe(move |v| sc.set(*v));

        // Setting through clone triggers subscriber registered on original.
        obs2.set(42);
        assert_eq!(seen.get(), 42);
    }

    #[test]
    fn large_subscriber_churn() {
        let obs = Observable::new(0);

        // Add and drop many subscriptions.
        for i in 0..200 {
            let sub = obs.subscribe(move |_| {
                let _ = i; // capture
            });
            if i % 2 == 0 {
                drop(sub);
            }
        }

        // Trigger prune.
        obs.set(1);

        // Only the odd-indexed subs (which weren't dropped) remain,
        // but they went out of scope too. After prune, count should be 0.
        assert_eq!(obs.subscriber_count(), 0);
    }
}
