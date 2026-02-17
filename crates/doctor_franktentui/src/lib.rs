#![forbid(unsafe_code)]

pub mod capture;
pub mod cli;
pub mod doctor;
pub mod error;
pub mod keyseq;
pub mod profile;
pub mod report;
pub mod runmeta;
pub mod seed;
pub mod suite;
pub mod tape;
pub mod util;

pub use cli::run_from_env;
pub use error::{DoctorError, Result};
