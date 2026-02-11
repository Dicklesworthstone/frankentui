#![forbid(unsafe_code)]

//! Large static assets for the demo showcase.
//!
//! Native builds embed large text blobs directly in the binary for convenience.
//! WASM builds must avoid embedding multi-megabyte strings in the module (they
//! bloat download size and dramatically slow instantiation due to data segment
//! memcpy). For WASM, the host is expected to provide these blobs once at
//! startup via `set_*` functions (implemented as a one-time leak to obtain a
//! `'static` string slice).

#[cfg(target_arch = "wasm32")]
use std::sync::OnceLock;

// -------------------------------------------------------------------------------------
// Shakespeare (Gutenberg text)
// -------------------------------------------------------------------------------------

#[cfg(not(target_arch = "wasm32"))]
pub fn shakespeare_text() -> Option<&'static str> {
    Some(include_str!("../data/shakespeare.txt"))
}

#[cfg(target_arch = "wasm32")]
static SHAKESPEARE_TEXT: OnceLock<&'static str> = OnceLock::new();

#[cfg(target_arch = "wasm32")]
pub fn shakespeare_text() -> Option<&'static str> {
    SHAKESPEARE_TEXT.get().copied()
}

#[cfg(target_arch = "wasm32")]
pub fn set_shakespeare_text(text: String) -> bool {
    SHAKESPEARE_TEXT
        .set(Box::leak(text.into_boxed_str()))
        .is_ok()
}

// -------------------------------------------------------------------------------------
// SQLite amalgamation (sqlite3.c)
// -------------------------------------------------------------------------------------

#[cfg(not(target_arch = "wasm32"))]
pub fn sqlite_source() -> Option<&'static str> {
    Some(include_str!("../data/sqlite3.c"))
}

#[cfg(target_arch = "wasm32")]
static SQLITE_SOURCE: OnceLock<&'static str> = OnceLock::new();

#[cfg(target_arch = "wasm32")]
pub fn sqlite_source() -> Option<&'static str> {
    SQLITE_SOURCE.get().copied()
}

#[cfg(target_arch = "wasm32")]
pub fn set_sqlite_source(text: String) -> bool {
    SQLITE_SOURCE.set(Box::leak(text.into_boxed_str())).is_ok()
}
