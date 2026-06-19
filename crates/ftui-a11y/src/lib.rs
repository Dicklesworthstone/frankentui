#![forbid(unsafe_code)]

//! Accessibility layer for FrankenTUI.
//!
//! # Role in FrankenTUI
//!
//! `ftui-a11y` defines the accessibility tree infrastructure that lets
//! widgets describe themselves to screen readers and other assistive
//! technology. It is **Phase 1** of the accessibility initiative: the
//! core types and trait contract.
//!
//! # Architecture
//!
//! ```text
//!  Widget ──impl Accessible──> Vec<A11yNodeInfo>
//!                                     │
//!                              A11yTreeBuilder::add_node()
//!                                     │
//!                              A11yTreeBuilder::build()
//!                                     │
//!                                A11yTree (immutable snapshot)
//!                                     │
//!                              A11yTree::diff(&prev)
//!                                     │
//!                               A11yTreeDiff
//!                                     │
//!              screen-reader mirror + live announcements
//!                                     │
//!                          (future: platform bridge)
//! ```
//!
//! # How it fits in the system
//!
//! - **`ftui-core`**: provides `Rect` for node bounding boxes.
//! - **`ftui-widgets`**: widgets will `impl Accessible` in a future phase.
//! - **`ftui-render`**: the render pass will collect nodes via the builder.
//! - **Platform bridges** (future Phase 3): consume `A11yTreeDiff`,
//!   screen-reader mirror text, and bounded live announcements to push
//!   updates to AccessKit / platform APIs.
//!
//! # Quick start
//!
//! ```rust
//! use ftui_a11y::node::{A11yNodeInfo, A11yRole};
//! use ftui_a11y::tree::A11yTreeBuilder;
//! use ftui_core::geometry::Rect;
//!
//! // Build a small tree.
//! let mut builder = A11yTreeBuilder::new();
//! let root = A11yNodeInfo::new(1, A11yRole::Window, Rect::new(0, 0, 80, 24))
//!     .with_name("My App")
//!     .with_children(vec![2]);
//! let button = A11yNodeInfo::new(2, A11yRole::Button, Rect::new(10, 5, 8, 1))
//!     .with_name("OK")
//!     .with_parent(1);
//! builder.add_node(root);
//! builder.add_node(button);
//! builder.set_root(1);
//! builder.set_focused(Some(2));
//! let tree = builder.build();
//!
//! assert_eq!(tree.node_count(), 2);
//! assert_eq!(tree.focused().unwrap().name.as_deref(), Some("OK"));
//! ```

pub mod node;
pub mod preferences;
pub mod tree;

pub use preferences::{
    AccessibilityPreferences, ContrastProfile, MotionFilteredAnnouncements, MotionProfile,
};

use ftui_core::geometry::Rect;
use node::A11yNodeInfo;

// ── Accessible trait ───────────────────────────────────────────────────

/// Trait for widgets that provide accessibility metadata.
///
/// Implementing this trait is **opt-in**. Widgets that do not implement
/// it are invisible to screen readers (treated as presentational /
/// decorative).
///
/// # Contract
///
/// - `accessibility_nodes` must return at least one node for the widget.
/// - The first node in the returned `Vec` is the widget's "primary"
///   node; additional nodes represent internal structure (e.g. a table
///   widget returns the table node plus row/cell nodes).
/// - Node IDs must be unique within a single render pass. Use a
///   deterministic scheme (e.g. widget identity hash) to keep IDs
///   stable across frames for efficient diffing.
/// - `area` is the bounding rectangle the widget was laid out into.
///
/// # Example
///
/// ```rust
/// use ftui_core::geometry::Rect;
/// use ftui_a11y::Accessible;
/// use ftui_a11y::node::{A11yNodeInfo, A11yRole};
///
/// struct MyButton {
///     label: String,
///     id: u64,
/// }
///
/// impl Accessible for MyButton {
///     fn accessibility_nodes(&self, area: Rect) -> Vec<A11yNodeInfo> {
///         vec![
///             A11yNodeInfo::new(self.id, A11yRole::Button, area)
///                 .with_name(&self.label)
///         ]
///     }
/// }
/// ```
pub trait Accessible {
    /// Return accessibility node(s) for this widget at the given bounds.
    ///
    /// Container widgets should return a parent node with children IDs,
    /// plus the child nodes themselves.
    fn accessibility_nodes(&self, area: Rect) -> Vec<A11yNodeInfo>;
}
