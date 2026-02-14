#![forbid(unsafe_code)]

//! Focus management: navigation graph, manager, spatial navigation, and styling.

pub mod graph;
pub mod indicator;
pub mod manager;
pub mod spatial;

pub use graph::{FocusGraph, FocusId, FocusNode, NavDirection};
pub use indicator::{FocusIndicator, FocusIndicatorKind};
pub use manager::{FocusEvent, FocusGroup, FocusManager, FocusTrap};
pub use spatial::{build_spatial_edges, spatial_navigate};
