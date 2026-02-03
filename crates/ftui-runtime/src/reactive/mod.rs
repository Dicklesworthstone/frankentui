#![forbid(unsafe_code)]

//! Reactive data bindings for FrankenTUI.
//!
//! This module provides change-tracking primitives for reactive UI updates:
//!
//! - [`Observable`]: A shared, version-tracked value wrapper with change
//!   notification via subscriber callbacks.
//! - [`Subscription`]: RAII guard that automatically unsubscribes on drop.
//! - [`Computed`]: A lazily-evaluated, memoized value derived from one or
//!   more `Observable` dependencies.
//! - [`BatchScope`]: RAII guard that defers all `Observable` notifications
//!   until the scope exits, preventing intermediate renders.
//!
//! # Architecture
//!
//! `Observable<T>` uses `Rc<RefCell<..>>` for single-threaded shared ownership.
//! Subscribers are stored as `Weak` function pointers and cleaned up lazily
//! during notification.
//!
//! `Computed<T>` subscribes to its sources via `Observable::subscribe()`,
//! marking itself dirty on change. Recomputation is deferred until `get()`.
//!
//! `BatchScope` uses a thread-local context to defer notifications. Nested
//! scopes are supported; only the outermost scope triggers flush.
//!
//! # Invariants
//!
//! 1. Version increments exactly once per mutation that changes the value.
//! 2. Subscribers are notified in registration order.
//! 3. Setting a value equal to the current value is a no-op (no version bump,
//!    no notifications).
//! 4. Dropping a [`Subscription`] removes the callback before the next
//!    notification cycle.
//! 5. `Computed::get()` never returns a stale value.
//! 6. Within a `BatchScope`, values are updated immediately but notifications
//!    are deferred until the outermost scope exits.

pub mod batch;
pub mod binding;
pub mod computed;
pub mod observable;

pub use batch::BatchScope;
pub use binding::{Binding, TwoWayBinding, bind_mapped, bind_mapped2, bind_observable};
pub use computed::Computed;
pub use observable::{Observable, Subscription};
