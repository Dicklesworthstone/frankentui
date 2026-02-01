#![forbid(unsafe_code)]

//! Optional optimization crate.
//!
//! Note: This project currently forbids unsafe code. This crate exists to host
//! safe optimizations (autovec-friendly loops, potential portable SIMD) behind
//! feature flags without impacting the rest of the workspace.
