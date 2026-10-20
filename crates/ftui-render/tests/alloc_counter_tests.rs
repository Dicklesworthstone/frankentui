#![forbid(unsafe_code)]

//! Unit tests for the G25 benchmark allocation counter (`bd-g00-root-epic-ewths.31.5`).

#[path = "../benches/support/alloc_counter.rs"]
mod alloc_counter;

#[test]
fn counter_counts_box_alloc_and_free() {
    alloc_counter::assert_counter_counts_box_alloc_and_free();
}

#[test]
fn counter_is_zero_for_stack_only_work() {
    alloc_counter::assert_counter_is_zero_for_stack_only_work();
}
