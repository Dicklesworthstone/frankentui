#![forbid(unsafe_code)]

//! Input parser state machine.
//!
//! This will eventually decode bytes from the terminal into [`crate::event::Event`].

#[derive(Debug, Default)]
pub struct InputParser;
