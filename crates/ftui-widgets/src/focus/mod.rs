#![forbid(unsafe_code)]

//! Focus management: navigation graph, manager, and spatial navigation.

pub mod graph;
pub mod manager;
pub mod spatial;

pub use graph::{FocusGraph, FocusId, FocusNode, NavDirection};
pub use manager::{FocusEvent, FocusGroup, FocusManager, FocusTrap};
pub use spatial::{build_spatial_edges, spatial_navigate};
