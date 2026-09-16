#![forbid(unsafe_code)]

//! Large static assets for the demo showcase.
//!
//! Native builds embed large text blobs directly in the binary for convenience.
//! WASM builds must avoid embedding multi-megabyte strings in the module (they
//! bloat download size and dramatically slow instantiation due to data segment
//! memcpy). For WASM, the host is expected to provide these blobs once at
//! startup via `set_*` functions. Static text cells own their accepted string
//! for the life of the module. Evidence logs can be replaced during a session.

#[cfg(target_arch = "wasm32")]
use std::sync::OnceLock;
#[cfg(any(target_arch = "wasm32", test))]
use std::sync::{Arc, Mutex};

// -------------------------------------------------------------------------------------
// Shakespeare (Gutenberg text)
// -------------------------------------------------------------------------------------

#[cfg(not(target_arch = "wasm32"))]
pub fn shakespeare_text() -> Option<&'static str> {
    Some(include_str!("../data/shakespeare.txt"))
}

#[cfg(target_arch = "wasm32")]
static SHAKESPEARE_TEXT: OnceLock<String> = OnceLock::new();

#[cfg(target_arch = "wasm32")]
pub fn shakespeare_text() -> Option<&'static str> {
    SHAKESPEARE_TEXT.get().map(String::as_str)
}

#[cfg(target_arch = "wasm32")]
pub fn set_shakespeare_text(text: String) -> bool {
    SHAKESPEARE_TEXT.set(text).is_ok()
}

// -------------------------------------------------------------------------------------
// SQLite amalgamation (sqlite3.c)
// -------------------------------------------------------------------------------------

#[cfg(not(target_arch = "wasm32"))]
pub fn sqlite_source() -> Option<&'static str> {
    Some(include_str!("../data/sqlite3.c"))
}

#[cfg(target_arch = "wasm32")]
static SQLITE_SOURCE: OnceLock<String> = OnceLock::new();

#[cfg(target_arch = "wasm32")]
pub fn sqlite_source() -> Option<&'static str> {
    SQLITE_SOURCE.get().map(String::as_str)
}

#[cfg(target_arch = "wasm32")]
pub fn set_sqlite_source(text: String) -> bool {
    SQLITE_SOURCE.set(text).is_ok()
}

// -------------------------------------------------------------------------------------
// Evidence JSONL (explainability cockpit)
// -------------------------------------------------------------------------------------
//
// Native builds read a live log from a path chosen at runtime and fall back to
// this captured sample when no path is set, so the cockpit always has something
// real to show. A browser has no filesystem at all, so its host supplies the
// same JSONL, and can replace it as new evidence becomes available.

#[cfg(not(target_arch = "wasm32"))]
pub fn evidence_jsonl() -> Option<&'static str> {
    Some(include_str!("../data/evidence.jsonl"))
}

#[cfg(target_arch = "wasm32")]
static EVIDENCE_JSONL: EvidenceLog = EvidenceLog::new();

#[cfg(any(target_arch = "wasm32", test))]
struct EvidenceLog(Mutex<Option<Arc<str>>>);

#[cfg(any(target_arch = "wasm32", test))]
impl EvidenceLog {
    const fn new() -> Self {
        Self(Mutex::new(None))
    }

    fn snapshot(&self) -> Option<Arc<str>> {
        self.0.lock().unwrap_or_else(|err| err.into_inner()).clone()
    }

    fn replace(&self, text: String) -> bool {
        let mut current = self.0.lock().unwrap_or_else(|err| err.into_inner());
        if current.as_deref() == Some(text.as_str()) {
            return false;
        }
        *current = Some(Arc::from(text));
        true
    }
}

#[cfg(target_arch = "wasm32")]
pub fn evidence_jsonl() -> Option<Arc<str>> {
    EVIDENCE_JSONL.snapshot()
}

#[cfg(target_arch = "wasm32")]
pub fn set_evidence_jsonl(text: String) -> bool {
    EVIDENCE_JSONL.replace(text)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn evidence_replacement_preserves_snapshots_and_reclaims_old_logs() {
        let log = EvidenceLog::new();
        assert!(log.snapshot().is_none());
        assert!(log.replace("old".into()));
        let old = log.snapshot().unwrap();
        let weak = Arc::downgrade(&old);
        assert!(!log.replace("old".into()));
        assert!(Arc::ptr_eq(&old, &log.snapshot().unwrap()));
        assert!(log.replace("new".into()));
        assert_eq!(&*old, "old");
        assert_eq!(&*log.snapshot().unwrap(), "new");
        drop(old);
        assert!(weak.upgrade().is_none());
        assert!(log.replace(String::new()));
        assert_eq!(&*log.snapshot().unwrap(), "");
    }
}
