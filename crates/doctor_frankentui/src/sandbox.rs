//! Sandbox enforcement for untrusted project analysis.
//!
//! Policy types and a cooperative enforcer meant to keep source analysis and
//! dynamic probes of untrusted repositories inside constrained boundaries.
//!
//! **Where it runs: the import snapshot's filesystem bounds, nothing else.**
//! `doctor_frankentui import` builds an enforcer from `--sandbox-profile`
//! (default `standard`). Once the untrusted source is materialized, and before
//! anything hashes or parses it, one walk of the snapshot applies
//! [`FsPolicy`]'s `max_depth`, `max_file_count`, `max_file_size` and
//! `max_read_bytes`. A violation fails the import with the violation's exit
//! code, and `sandbox_report.json` in the run directory records the verdict.
//!
//! The other policies have no consumer:
//! - `read_allow`/`read_deny` would reject ordinary source files (`**/*token*`
//!   matches `tokenizer.ts`).
//! - The network and process policies describe analysis subprocesses, and
//!   doctor spawns none. Import's `git`/`tar` fetch and `capture`/`suite`'s
//!   tmux, vhs and ffmpeg run trusted tools and the app under test, not the
//!   imported code.
//! - `ResourcePolicy`'s CPU-time, memory and open-fd fields have no check at
//!   all (bd-ignl4).
//!
//! Read those as a specification.
//!
//! # Design Principles
//!
//! 1. **Fail closed**: any violation immediately aborts with diagnostics.
//! 2. **Profile-configurable**: `SandboxProfile` presets with safe defaults.
//! 3. **Audit-logged**: every policy decision is recorded as structured JSONL.
//! 4. **Reproducible**: violation reports include full reproduction metadata.

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use crate::error::DoctorError;

/// Result type for sandbox checks. Uses `Box` to keep `Result` small.
type SandboxResult = std::result::Result<(), Box<SandboxViolation>>;

// ── Exit Codes ───────────────────────────────────────────────────────────

/// Dedicated exit code range for sandbox violations (50-59).
const EXIT_SANDBOX_FS_VIOLATION: i32 = 50;
const EXIT_SANDBOX_NETWORK_VIOLATION: i32 = 51;
const EXIT_SANDBOX_PROCESS_VIOLATION: i32 = 52;
const EXIT_SANDBOX_RESOURCE_VIOLATION: i32 = 53;
const EXIT_SANDBOX_POLICY_LOAD_ERROR: i32 = 54;

// ── Sandbox Profile ──────────────────────────────────────────────────────

/// Pre-configured sandbox policy profiles.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, clap::ValueEnum)]
#[serde(rename_all = "snake_case")]
pub enum SandboxProfile {
    /// Maximum restrictions: read-only FS within snapshot, no network, no subprocesses.
    Strict,
    /// Standard analysis: read-only FS, no network, limited subprocesses for
    /// toolchain detection (node --version, tsc --version, etc.).
    Standard,
    /// Relaxed constraints for trusted internal projects. Still enforces
    /// resource limits but allows broader FS access and network for package
    /// registry checks.
    Permissive,
}

impl SandboxProfile {
    /// Build the full `SandboxPolicy` from this profile.
    #[must_use]
    pub fn to_policy(self) -> SandboxPolicy {
        match self {
            Self::Strict => SandboxPolicy::strict(),
            Self::Standard => SandboxPolicy::standard(),
            Self::Permissive => SandboxPolicy::permissive(),
        }
    }

    /// Profile name for structured logging.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Strict => "strict",
            Self::Standard => "standard",
            Self::Permissive => "permissive",
        }
    }
}

// ── Sandbox Policy ───────────────────────────────────────────────────────

/// Complete sandbox policy specification.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SandboxPolicy {
    /// Which profile generated this policy (or "custom").
    pub profile_name: String,

    /// Filesystem constraints.
    pub fs: FsPolicy,

    /// Network constraints.
    pub network: NetworkPolicy,

    /// Process/subprocess constraints.
    pub process: ProcessPolicy,

    /// Resource (CPU/memory/time) constraints.
    pub resource: ResourcePolicy,
}

/// Filesystem access policy.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FsPolicy {
    /// Allowed read paths (glob patterns). Empty = deny all reads.
    pub read_allow: Vec<String>,
    /// Explicitly denied read paths (checked before allow).
    pub read_deny: Vec<String>,
    /// Allowed write paths (glob patterns). Empty = deny all writes.
    pub write_allow: Vec<String>,
    /// Maximum total bytes readable in a single analysis run.
    pub max_read_bytes: u64,
    /// Maximum single file size in bytes.
    pub max_file_size: u64,
    /// Maximum directory depth for traversal.
    pub max_depth: u32,
    /// Maximum number of files to enumerate.
    pub max_file_count: u64,
}

/// Network access policy.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NetworkPolicy {
    /// Whether any network access is permitted.
    pub allow_network: bool,
    /// Allowed destination hosts (exact match). Empty = all allowed if `allow_network`.
    pub allowed_hosts: Vec<String>,
    /// Blocked destination hosts (checked before allowed).
    pub blocked_hosts: Vec<String>,
    /// Maximum number of outbound connections.
    pub max_connections: u32,
    /// Connection timeout.
    pub connect_timeout_secs: u32,
}

/// Subprocess spawning policy.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProcessPolicy {
    /// Whether subprocess spawning is permitted.
    pub allow_subprocess: bool,
    /// Allowed executable names (basename only, e.g. "node", "git").
    pub allowed_executables: Vec<String>,
    /// Maximum concurrent subprocesses.
    pub max_concurrent: u32,
    /// Maximum total subprocesses during a run.
    pub max_total: u32,
    /// Per-subprocess timeout.
    pub subprocess_timeout_secs: u32,
}

/// Resource limit policy.
///
/// [`SandboxEnforcer`] is cooperative: a limit binds only where the enforcer
/// has a method for it and the caller calls that method. Two of these are
/// enforced that way — [`check_wall_time`] and [`record_output_bytes`]. The
/// other three are declared and carried in the policy, and nothing reads
/// them: applying them would need `setrlimit` before `exec`, which is
/// `unsafe`, and this crate is `#![forbid(unsafe_code)]`. Each is marked
/// below. Do not read a profile's number as a guarantee that a run stays
/// under it (bd-ignl4).
///
/// [`check_wall_time`]: SandboxEnforcer::check_wall_time
/// [`record_output_bytes`]: SandboxEnforcer::record_output_bytes
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResourcePolicy {
    /// Maximum wall-clock time for the entire analysis run. Enforced by
    /// [`SandboxEnforcer::check_wall_time`].
    pub max_wall_time_secs: u64,
    /// Maximum CPU time (user + system) in seconds. **Declarative only.**
    pub max_cpu_time_secs: u64,
    /// Maximum resident memory in bytes. **Declarative only.**
    pub max_memory_bytes: u64,
    /// Maximum number of open file descriptors. **Declarative only.**
    pub max_open_fds: u32,
    /// Maximum output size (stdout + stderr) from subprocesses. Enforced by
    /// [`SandboxEnforcer::record_output_bytes`].
    pub max_output_bytes: u64,
}

impl SandboxPolicy {
    /// Strict profile: maximum isolation.
    fn strict() -> Self {
        Self {
            profile_name: "strict".into(),
            fs: FsPolicy {
                read_allow: vec![], // Caller must add snapshot dir.
                read_deny: vec![
                    "**/.git/**".into(),
                    "**/node_modules/**".into(),
                    "**/.env*".into(),
                    "**/*secret*".into(),
                    "**/*credential*".into(),
                    "**/*token*".into(),
                ],
                write_allow: vec![],               // No writes at all.
                max_read_bytes: 256 * 1024 * 1024, // 256 MiB
                max_file_size: 16 * 1024 * 1024,   // 16 MiB
                max_depth: 20,
                max_file_count: 50_000,
            },
            network: NetworkPolicy {
                allow_network: false,
                allowed_hosts: vec![],
                blocked_hosts: vec![],
                max_connections: 0,
                connect_timeout_secs: 0,
            },
            process: ProcessPolicy {
                allow_subprocess: false,
                allowed_executables: vec![],
                max_concurrent: 0,
                max_total: 0,
                subprocess_timeout_secs: 0,
            },
            resource: ResourcePolicy {
                max_wall_time_secs: 120,
                max_cpu_time_secs: 60,
                max_memory_bytes: 512 * 1024 * 1024, // 512 MiB
                max_open_fds: 256,
                max_output_bytes: 8 * 1024 * 1024, // 8 MiB
            },
        }
    }

    /// Standard profile: read-only FS with toolchain detection subprocesses.
    fn standard() -> Self {
        Self {
            profile_name: "standard".into(),
            fs: FsPolicy {
                read_allow: vec![], // Caller must add snapshot dir.
                read_deny: vec![
                    "**/.git/**".into(),
                    "**/.env*".into(),
                    "**/*secret*".into(),
                    "**/*credential*".into(),
                    "**/*token*".into(),
                ],
                write_allow: vec![],                // No writes.
                max_read_bytes: 1024 * 1024 * 1024, // 1 GiB
                max_file_size: 64 * 1024 * 1024,    // 64 MiB
                max_depth: 30,
                max_file_count: 200_000,
            },
            network: NetworkPolicy {
                allow_network: false,
                allowed_hosts: vec![],
                blocked_hosts: vec![],
                max_connections: 0,
                connect_timeout_secs: 0,
            },
            process: ProcessPolicy {
                allow_subprocess: true,
                allowed_executables: vec![
                    "node".into(),
                    "npm".into(),
                    "npx".into(),
                    "pnpm".into(),
                    "yarn".into(),
                    "bun".into(),
                    "tsc".into(),
                    "git".into(),
                    "rustc".into(),
                    "cargo".into(),
                ],
                max_concurrent: 4,
                max_total: 50,
                subprocess_timeout_secs: 30,
            },
            resource: ResourcePolicy {
                max_wall_time_secs: 300,
                max_cpu_time_secs: 120,
                max_memory_bytes: 2 * 1024 * 1024 * 1024, // 2 GiB
                max_open_fds: 1024,
                max_output_bytes: 32 * 1024 * 1024, // 32 MiB
            },
        }
    }

    /// Permissive profile: relaxed for trusted internal projects.
    fn permissive() -> Self {
        Self {
            profile_name: "permissive".into(),
            fs: FsPolicy {
                read_allow: vec!["**".into()],
                read_deny: vec!["**/.env*".into(), "**/*secret*".into()],
                write_allow: vec![], // Still no writes during analysis.
                max_read_bytes: 4 * 1024 * 1024 * 1024, // 4 GiB
                max_file_size: 256 * 1024 * 1024, // 256 MiB
                max_depth: 50,
                max_file_count: 1_000_000,
            },
            network: NetworkPolicy {
                allow_network: true,
                allowed_hosts: vec![
                    "registry.npmjs.org".into(),
                    "crates.io".into(),
                    "pypi.org".into(),
                ],
                blocked_hosts: vec![],
                max_connections: 16,
                connect_timeout_secs: 10,
            },
            process: ProcessPolicy {
                allow_subprocess: true,
                allowed_executables: vec![], // Empty = all allowed.
                max_concurrent: 8,
                max_total: 200,
                subprocess_timeout_secs: 60,
            },
            resource: ResourcePolicy {
                max_wall_time_secs: 600,
                max_cpu_time_secs: 300,
                max_memory_bytes: 4 * 1024 * 1024 * 1024, // 4 GiB
                max_open_fds: 4096,
                max_output_bytes: 128 * 1024 * 1024, // 128 MiB
            },
        }
    }

    /// Load policy from JSON, returning a descriptive error on failure.
    pub fn from_json(json: &str) -> std::result::Result<Self, DoctorError> {
        serde_json::from_str(json).map_err(|e| {
            DoctorError::exit(
                EXIT_SANDBOX_POLICY_LOAD_ERROR,
                format!("sandbox policy parse error: {e}"),
            )
        })
    }

    /// Add a read-allow path (typically the snapshot directory).
    pub fn allow_read_path(&mut self, path: impl Into<String>) {
        self.fs.read_allow.push(path.into());
    }

    /// Add a write-allow path (typically the run output directory).
    pub fn allow_write_path(&mut self, path: impl Into<String>) {
        self.fs.write_allow.push(path.into());
    }
}

// ── Violation Types ──────────────────────────────────────────────────────

/// Category of sandbox violation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ViolationKind {
    FsReadDenied,
    FsWriteDenied,
    FsReadBytesExceeded,
    FsFileSizeExceeded,
    FsDepthExceeded,
    FsFileCountExceeded,
    NetworkBlocked,
    NetworkHostDenied,
    NetworkConnectionLimit,
    ProcessBlocked,
    ProcessExecutableDenied,
    ProcessConcurrentLimit,
    ProcessTotalLimit,
    ProcessTimeout,
    ResourceWallTimeExceeded,
    ResourceCpuTimeExceeded,
    ResourceMemoryExceeded,
    ResourceFdLimitExceeded,
    ResourceOutputExceeded,
}

impl ViolationKind {
    /// Exit code for this violation category.
    #[must_use]
    pub fn exit_code(self) -> i32 {
        match self {
            Self::FsReadDenied
            | Self::FsWriteDenied
            | Self::FsReadBytesExceeded
            | Self::FsFileSizeExceeded
            | Self::FsDepthExceeded
            | Self::FsFileCountExceeded => EXIT_SANDBOX_FS_VIOLATION,

            Self::NetworkBlocked | Self::NetworkHostDenied | Self::NetworkConnectionLimit => {
                EXIT_SANDBOX_NETWORK_VIOLATION
            }

            Self::ProcessBlocked
            | Self::ProcessExecutableDenied
            | Self::ProcessConcurrentLimit
            | Self::ProcessTotalLimit
            | Self::ProcessTimeout => EXIT_SANDBOX_PROCESS_VIOLATION,

            Self::ResourceWallTimeExceeded
            | Self::ResourceCpuTimeExceeded
            | Self::ResourceMemoryExceeded
            | Self::ResourceFdLimitExceeded
            | Self::ResourceOutputExceeded => EXIT_SANDBOX_RESOURCE_VIOLATION,
        }
    }
}

/// A recorded sandbox violation with reproduction metadata.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SandboxViolation {
    pub kind: ViolationKind,
    pub message: String,
    pub path: Option<String>,
    pub limit: String,
    pub actual: String,
    pub timestamp: String,
}

impl SandboxViolation {
    fn new(kind: ViolationKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
            path: None,
            limit: String::new(),
            actual: String::new(),
            timestamp: chrono::Utc::now().to_rfc3339(),
        }
    }

    fn with_path(mut self, path: impl Into<String>) -> Self {
        self.path = Some(path.into());
        self
    }

    fn with_limits(mut self, limit: impl Into<String>, actual: impl Into<String>) -> Self {
        self.limit = limit.into();
        self.actual = actual.into();
        self
    }

    /// Convert to a fail-closed `DoctorError`.
    #[must_use]
    pub fn into_error(self) -> DoctorError {
        DoctorError::exit(self.kind.exit_code(), self.message.clone())
    }
}

// ── Audit Log ────────────────────────────────────────────────────────────

/// Structured audit log entry for sandbox decisions.
#[derive(Debug, Clone, Serialize)]
pub struct AuditEntry {
    pub event: String,
    pub run_id: String,
    pub timestamp: String,
    pub category: String,
    pub action: String,
    pub detail: String,
    pub violation: Option<SandboxViolation>,
}

/// Audit log collecting all sandbox decisions during a run.
#[derive(Debug)]
pub struct AuditLog {
    run_id: String,
    entries: Vec<AuditEntry>,
}

impl AuditLog {
    fn new(run_id: &str) -> Self {
        Self {
            run_id: run_id.to_string(),
            entries: Vec::new(),
        }
    }

    fn record(&mut self, category: &str, action: &str, detail: &str) {
        self.entries.push(AuditEntry {
            event: "sandbox_audit".into(),
            run_id: self.run_id.clone(),
            timestamp: chrono::Utc::now().to_rfc3339(),
            category: category.into(),
            action: action.into(),
            detail: detail.into(),
            violation: None,
        });
    }

    fn record_violation(&mut self, category: &str, violation: &SandboxViolation) {
        self.entries.push(AuditEntry {
            event: "sandbox_violation".into(),
            run_id: self.run_id.clone(),
            timestamp: violation.timestamp.clone(),
            category: category.into(),
            action: "deny".into(),
            detail: violation.message.clone(),
            violation: Some(violation.clone()),
        });
    }

    /// Serialize all entries as JSONL.
    #[must_use]
    pub fn to_jsonl(&self) -> String {
        self.entries
            .iter()
            .filter_map(|e| serde_json::to_string(e).ok())
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// Number of recorded entries.
    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether the audit log is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Read-only access to entries.
    #[must_use]
    pub fn entries(&self) -> &[AuditEntry] {
        &self.entries
    }
}

// ── Sandbox Enforcer ─────────────────────────────────────────────────────

/// Runtime sandbox enforcer that validates operations against a policy.
///
/// The enforcer tracks cumulative resource usage and fails closed on any
/// policy violation it can observe. It is cooperative: it sees only what a
/// caller reports through the `check_*` and `record_*` methods, so a caller
/// that skips one skips that limit. Three fields of [`ResourcePolicy`] have
/// no method here at all and bind nothing — see that type for which.
#[derive(Debug)]
pub struct SandboxEnforcer {
    policy: SandboxPolicy,
    audit: AuditLog,
    start_time: Instant,

    // Cumulative counters.
    bytes_read: u64,
    files_enumerated: u64,
    subprocesses_spawned: u32,
    active_subprocesses: u32,
    connections_opened: u32,
    output_bytes: u64,
}

/// Summary report produced when the sandbox session ends.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SandboxReport {
    pub run_id: String,
    pub profile: String,
    pub verdict: String,
    pub violations: Vec<SandboxViolation>,
    pub counters: BTreeMap<String, u64>,
    pub wall_time_ms: u64,
}

impl SandboxEnforcer {
    /// Create a new enforcer with the given policy and run ID.
    pub fn new(policy: SandboxPolicy, run_id: &str) -> Self {
        let mut audit = AuditLog::new(run_id);
        audit.record(
            "init",
            "allow",
            &format!("sandbox initialized with profile '{}'", policy.profile_name),
        );
        Self {
            policy,
            audit,
            start_time: Instant::now(),
            bytes_read: 0,
            files_enumerated: 0,
            subprocesses_spawned: 0,
            active_subprocesses: 0,
            connections_opened: 0,
            output_bytes: 0,
        }
    }

    /// Create an enforcer from a profile preset.
    pub fn from_profile(profile: SandboxProfile, run_id: &str) -> Self {
        Self::new(profile.to_policy(), run_id)
    }

    /// Get a reference to the current policy.
    #[must_use]
    pub fn policy(&self) -> &SandboxPolicy {
        &self.policy
    }

    /// Get a reference to the audit log.
    #[must_use]
    pub fn audit_log(&self) -> &AuditLog {
        &self.audit
    }

    /// Consume the enforcer and produce a final report.
    pub fn into_report(self) -> SandboxReport {
        let elapsed = self.start_time.elapsed();
        let mut counters = BTreeMap::new();
        counters.insert("bytes_read".into(), self.bytes_read);
        counters.insert("files_enumerated".into(), self.files_enumerated);
        counters.insert(
            "subprocesses_spawned".into(),
            u64::from(self.subprocesses_spawned),
        );
        counters.insert(
            "connections_opened".into(),
            u64::from(self.connections_opened),
        );
        counters.insert("output_bytes".into(), self.output_bytes);
        let violations: Vec<SandboxViolation> = self
            .audit
            .entries
            .iter()
            .filter_map(|entry| entry.violation.clone())
            .collect();
        let verdict = if violations.is_empty() {
            "pass"
        } else {
            "fail"
        };

        SandboxReport {
            run_id: self.audit.run_id.clone(),
            profile: self.policy.profile_name.clone(),
            verdict: verdict.into(),
            violations,
            counters,
            wall_time_ms: u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX),
        }
    }

    fn read_deny_pattern(&self, path: &str) -> Option<&str> {
        self.policy
            .fs
            .read_deny
            .iter()
            .find(|pattern| glob_match(pattern, path))
            .map(String::as_str)
    }

    fn read_allowed(&self, path: &str) -> bool {
        !self.policy.fs.read_allow.is_empty()
            && self
                .policy
                .fs
                .read_allow
                .iter()
                .any(|pattern| glob_match(pattern, path))
    }

    fn write_allowed(&self, path: &str) -> bool {
        !self.policy.fs.write_allow.is_empty()
            && self
                .policy
                .fs
                .write_allow
                .iter()
                .any(|pattern| glob_match(pattern, path))
    }

    fn resolved_read_path(path: &Path) -> Option<String> {
        let resolved = fs::canonicalize(path).ok()?;
        let resolved_str = resolved.to_string_lossy().to_string();
        let path_str = path.to_string_lossy();
        if resolved_str == path_str {
            None
        } else {
            Some(resolved_str)
        }
    }

    fn resolved_write_path(path: &Path) -> Option<String> {
        if let Ok(resolved) = fs::canonicalize(path) {
            return Some(resolved.to_string_lossy().to_string());
        }

        let mut existing = path;
        let mut suffix: Vec<OsString> = Vec::new();
        while !existing.exists() {
            let name = existing.file_name()?.to_owned();
            suffix.push(name);
            existing = existing.parent()?;
        }

        let mut resolved: PathBuf = fs::canonicalize(existing).ok()?;
        for component in suffix.iter().rev() {
            resolved.push(component);
        }
        Some(resolved.to_string_lossy().to_string())
    }

    // ── FS Checks ────────────────────────────────────────────────────────

    /// Check whether a file read is allowed. Fails closed on violation.
    pub fn check_read(&mut self, path: &Path) -> SandboxResult {
        let path_str = path.to_string_lossy().to_string();

        if let Some(pattern) = self.read_deny_pattern(&path_str) {
            let v = SandboxViolation::new(
                ViolationKind::FsReadDenied,
                format!("read denied by pattern '{pattern}': {path_str}"),
            )
            .with_path(path_str.clone());
            self.audit.record_violation("fs", &v);
            return Err(Box::new(v));
        }

        if !self.read_allowed(&path_str) {
            let v = SandboxViolation::new(
                ViolationKind::FsReadDenied,
                format!("read not in allow list: {path_str}"),
            )
            .with_path(path_str.clone());
            self.audit.record_violation("fs", &v);
            return Err(Box::new(v));
        }

        if let Some(resolved_path) = Self::resolved_read_path(path) {
            if let Some(pattern) = self.read_deny_pattern(&resolved_path) {
                let v = SandboxViolation::new(
                    ViolationKind::FsReadDenied,
                    format!(
                        "read resolves to denied path by pattern '{pattern}': {path_str} -> {resolved_path}"
                    ),
                )
                .with_path(path_str.clone());
                self.audit.record_violation("fs", &v);
                return Err(Box::new(v));
            }

            if !self.read_allowed(&resolved_path) {
                let v = SandboxViolation::new(
                    ViolationKind::FsReadDenied,
                    format!("read resolves outside allow list: {path_str} -> {resolved_path}"),
                )
                .with_path(path_str.clone());
                self.audit.record_violation("fs", &v);
                return Err(Box::new(v));
            }

            self.audit.record(
                "fs",
                "allow",
                &format!("read: {path_str} -> {resolved_path}"),
            );
            return Ok(());
        }

        self.audit
            .record("fs", "allow", &format!("read: {path_str}"));
        Ok(())
    }

    /// Check whether a file write is allowed. Fails closed on violation.
    pub fn check_write(&mut self, path: &Path) -> SandboxResult {
        let path_str = path.to_string_lossy().to_string();

        if self.policy.fs.write_allow.is_empty() {
            let v = SandboxViolation::new(
                ViolationKind::FsWriteDenied,
                format!("all writes denied: {path_str}"),
            )
            .with_path(path_str.clone());
            self.audit.record_violation("fs", &v);
            return Err(Box::new(v));
        }

        if !self.write_allowed(&path_str) {
            let v = SandboxViolation::new(
                ViolationKind::FsWriteDenied,
                format!("write not in allow list: {path_str}"),
            )
            .with_path(path_str.clone());
            self.audit.record_violation("fs", &v);
            return Err(Box::new(v));
        }

        if let Some(resolved_path) = Self::resolved_write_path(path)
            && resolved_path != path_str
            && !self.write_allowed(&resolved_path)
        {
            let v = SandboxViolation::new(
                ViolationKind::FsWriteDenied,
                format!("write resolves outside allow list: {path_str} -> {resolved_path}"),
            )
            .with_path(path_str.clone());
            self.audit.record_violation("fs", &v);
            return Err(Box::new(v));
        }

        self.audit
            .record("fs", "allow", &format!("write: {path_str}"));
        Ok(())
    }

    /// Record bytes read and check cumulative limit.
    pub fn record_bytes_read(&mut self, bytes: u64) -> SandboxResult {
        self.bytes_read = self.bytes_read.saturating_add(bytes);
        if self.bytes_read > self.policy.fs.max_read_bytes {
            let v = SandboxViolation::new(
                ViolationKind::FsReadBytesExceeded,
                format!(
                    "cumulative read bytes exceeded: {} > {}",
                    self.bytes_read, self.policy.fs.max_read_bytes
                ),
            )
            .with_limits(
                self.policy.fs.max_read_bytes.to_string(),
                self.bytes_read.to_string(),
            );
            self.audit.record_violation("fs", &v);
            return Err(Box::new(v));
        }
        Ok(())
    }

    /// Check a single file's size against the limit.
    pub fn check_file_size(&mut self, path: &Path, size: u64) -> SandboxResult {
        if size > self.policy.fs.max_file_size {
            let v = SandboxViolation::new(
                ViolationKind::FsFileSizeExceeded,
                format!(
                    "file too large: {} ({} > {})",
                    path.display(),
                    size,
                    self.policy.fs.max_file_size
                ),
            )
            .with_path(path.to_string_lossy().to_string())
            .with_limits(self.policy.fs.max_file_size.to_string(), size.to_string());
            self.audit.record_violation("fs", &v);
            return Err(Box::new(v));
        }
        Ok(())
    }

    /// Check directory depth against the limit.
    pub fn check_depth(&mut self, depth: u32) -> SandboxResult {
        if depth > self.policy.fs.max_depth {
            let v = SandboxViolation::new(
                ViolationKind::FsDepthExceeded,
                format!(
                    "directory depth exceeded: {} > {}",
                    depth, self.policy.fs.max_depth
                ),
            )
            .with_limits(self.policy.fs.max_depth.to_string(), depth.to_string());
            self.audit.record_violation("fs", &v);
            return Err(Box::new(v));
        }
        Ok(())
    }

    /// Record a file enumeration and check count limit.
    pub fn record_file_enumerated(&mut self) -> SandboxResult {
        let Some(next_count) = self.files_enumerated.checked_add(1) else {
            let v = SandboxViolation::new(
                ViolationKind::FsFileCountExceeded,
                "file count counter overflowed",
            )
            .with_limits(self.policy.fs.max_file_count.to_string(), "overflow");
            self.audit.record_violation("fs", &v);
            return Err(Box::new(v));
        };

        if next_count > self.policy.fs.max_file_count {
            let v = SandboxViolation::new(
                ViolationKind::FsFileCountExceeded,
                format!(
                    "file count exceeded: {} > {}",
                    next_count, self.policy.fs.max_file_count
                ),
            )
            .with_limits(
                self.policy.fs.max_file_count.to_string(),
                next_count.to_string(),
            );
            self.audit.record_violation("fs", &v);
            return Err(Box::new(v));
        }
        self.files_enumerated = next_count;
        Ok(())
    }

    // ── Network Checks ───────────────────────────────────────────────────

    /// Check whether a network connection to the given host is allowed.
    pub fn check_network(&mut self, host: &str) -> SandboxResult {
        let normalized_host = normalize_host(host);
        if normalized_host.is_empty() {
            let v = SandboxViolation::new(ViolationKind::NetworkHostDenied, "empty host denied");
            self.audit.record_violation("network", &v);
            return Err(Box::new(v));
        }

        if !self.policy.network.allow_network {
            let v = SandboxViolation::new(
                ViolationKind::NetworkBlocked,
                format!("network access denied (policy: no network): host={normalized_host}"),
            );
            self.audit.record_violation("network", &v);
            return Err(Box::new(v));
        }

        // Check blocked hosts.
        if self
            .policy
            .network
            .blocked_hosts
            .iter()
            .any(|h| normalize_host(h) == normalized_host)
        {
            let v = SandboxViolation::new(
                ViolationKind::NetworkHostDenied,
                format!("host explicitly blocked: {normalized_host}"),
            );
            self.audit.record_violation("network", &v);
            return Err(Box::new(v));
        }

        // Check allowed hosts (if non-empty, must match).
        if !self.policy.network.allowed_hosts.is_empty()
            && !self
                .policy
                .network
                .allowed_hosts
                .iter()
                .any(|h| normalize_host(h) == normalized_host)
        {
            let v = SandboxViolation::new(
                ViolationKind::NetworkHostDenied,
                format!("host not in allow list: {normalized_host}"),
            );
            self.audit.record_violation("network", &v);
            return Err(Box::new(v));
        }

        // Check connection count.
        if self.connections_opened >= self.policy.network.max_connections {
            let v = SandboxViolation::new(
                ViolationKind::NetworkConnectionLimit,
                format!(
                    "connection limit reached: {} >= {}",
                    self.connections_opened, self.policy.network.max_connections
                ),
            )
            .with_limits(
                self.policy.network.max_connections.to_string(),
                self.connections_opened.to_string(),
            );
            self.audit.record_violation("network", &v);
            return Err(Box::new(v));
        }
        self.connections_opened += 1;

        self.audit
            .record("network", "allow", &format!("connect: {normalized_host}"));
        Ok(())
    }

    // ── Process Checks ───────────────────────────────────────────────────

    /// Check whether spawning a subprocess with the given executable is allowed.
    pub fn check_subprocess(&mut self, executable: &str) -> SandboxResult {
        if !self.policy.process.allow_subprocess {
            let v = SandboxViolation::new(
                ViolationKind::ProcessBlocked,
                format!("subprocess spawning denied: {executable}"),
            );
            self.audit.record_violation("process", &v);
            return Err(Box::new(v));
        }

        // Check allowed executables (if non-empty, must match).
        if !self.policy.process.allowed_executables.is_empty() {
            if executable.contains('/') || executable.contains('\\') {
                let v = SandboxViolation::new(
                    ViolationKind::ProcessExecutableDenied,
                    format!("executable path not allowed by basename policy: {executable}"),
                );
                self.audit.record_violation("process", &v);
                return Err(Box::new(v));
            }

            let basename = Path::new(executable)
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_else(|| executable.to_string());
            if !self
                .policy
                .process
                .allowed_executables
                .iter()
                .any(|e| e == &basename)
            {
                let v = SandboxViolation::new(
                    ViolationKind::ProcessExecutableDenied,
                    format!("executable not in allow list: {executable}"),
                );
                self.audit.record_violation("process", &v);
                return Err(Box::new(v));
            }
        }

        // Check total subprocess limit.
        if self.subprocesses_spawned >= self.policy.process.max_total {
            let v = SandboxViolation::new(
                ViolationKind::ProcessTotalLimit,
                format!(
                    "total subprocess limit reached: {} >= {}",
                    self.subprocesses_spawned, self.policy.process.max_total
                ),
            )
            .with_limits(
                self.policy.process.max_total.to_string(),
                self.subprocesses_spawned.to_string(),
            );
            self.audit.record_violation("process", &v);
            return Err(Box::new(v));
        }

        // Check concurrent subprocess limit.
        if self.active_subprocesses >= self.policy.process.max_concurrent {
            let v = SandboxViolation::new(
                ViolationKind::ProcessConcurrentLimit,
                format!(
                    "concurrent subprocess limit reached: {} >= {}",
                    self.active_subprocesses, self.policy.process.max_concurrent
                ),
            )
            .with_limits(
                self.policy.process.max_concurrent.to_string(),
                self.active_subprocesses.to_string(),
            );
            self.audit.record_violation("process", &v);
            return Err(Box::new(v));
        }

        self.subprocesses_spawned += 1;
        self.active_subprocesses += 1;
        self.audit
            .record("process", "allow", &format!("spawn: {executable}"));
        Ok(())
    }

    /// Record that a subprocess has exited.
    pub fn record_subprocess_exit(&mut self) {
        self.active_subprocesses = self.active_subprocesses.saturating_sub(1);
    }

    /// Record subprocess output bytes and check limit.
    pub fn record_output_bytes(&mut self, bytes: u64) -> SandboxResult {
        self.output_bytes = self.output_bytes.saturating_add(bytes);
        if self.output_bytes > self.policy.resource.max_output_bytes {
            let v = SandboxViolation::new(
                ViolationKind::ResourceOutputExceeded,
                format!(
                    "subprocess output exceeded: {} > {}",
                    self.output_bytes, self.policy.resource.max_output_bytes
                ),
            )
            .with_limits(
                self.policy.resource.max_output_bytes.to_string(),
                self.output_bytes.to_string(),
            );
            self.audit.record_violation("resource", &v);
            return Err(Box::new(v));
        }
        Ok(())
    }

    /// Get the subprocess timeout duration from policy.
    #[must_use]
    pub fn subprocess_timeout(&self) -> Duration {
        Duration::from_secs(u64::from(self.policy.process.subprocess_timeout_secs))
    }

    // ── Resource Checks ──────────────────────────────────────────────────

    /// Check wall-clock time against the limit.
    pub fn check_wall_time(&mut self) -> SandboxResult {
        let elapsed = self.start_time.elapsed();
        let limit = Duration::from_secs(self.policy.resource.max_wall_time_secs);
        if elapsed > limit {
            let v = SandboxViolation::new(
                ViolationKind::ResourceWallTimeExceeded,
                format!(
                    "wall time exceeded: {:.1}s > {}s",
                    elapsed.as_secs_f64(),
                    self.policy.resource.max_wall_time_secs
                ),
            )
            .with_limits(
                format!("{}s", self.policy.resource.max_wall_time_secs),
                format!("{:.1}s", elapsed.as_secs_f64()),
            );
            self.audit.record_violation("resource", &v);
            return Err(Box::new(v));
        }
        Ok(())
    }

    /// Elapsed wall time since sandbox creation.
    #[must_use]
    pub fn elapsed(&self) -> Duration {
        self.start_time.elapsed()
    }
}

// ── Glob Matching ────────────────────────────────────────────────────────

fn normalize_host(host: &str) -> String {
    host.trim().trim_end_matches('.').to_ascii_lowercase()
}

/// Simple glob matching for sandbox path patterns.
///
/// Supports:
/// - `*` matches any sequence of non-separator characters
/// - `**` matches any sequence of characters (including separators)
/// - `?` matches a single non-separator character
fn glob_match(pattern: &str, path: &str) -> bool {
    // Normalize separators.
    let pattern = pattern.replace('\\', "/");
    let path = path.replace('\\', "/");
    glob_match_inner(&pattern, &path)
}

fn glob_match_inner(pattern: &str, path: &str) -> bool {
    if pattern.is_empty() {
        return path.is_empty();
    }

    // Handle ** (matches everything including /).
    if let Some(rest) = pattern.strip_prefix("**/") {
        // Match zero or more path segments.
        if glob_match_inner(rest, path) {
            return true;
        }
        for (i, _) in path.char_indices() {
            if path[i..].starts_with('/') && glob_match_inner(rest, &path[i + 1..]) {
                return true;
            }
        }
        return false;
    }

    if pattern == "**" {
        return true;
    }

    // Handle trailing **
    if let Some(prefix) = pattern.strip_suffix("/**") {
        if glob_match_inner(prefix, path) {
            return true;
        }
        for (i, ch) in path.char_indices() {
            if ch == '/' && glob_match_inner(prefix, &path[..i]) {
                return true;
            }
        }
        return false;
    }

    let mut pat_chars = pattern.chars().peekable();
    let mut path_chars = path.chars().peekable();

    while let Some(pc) = pat_chars.next() {
        match pc {
            '*' => {
                let rest_pattern: String = pat_chars.collect();
                // Try matching rest of pattern after consuming any number of
                // non-separator UTF-8 characters.
                let remaining: String = path_chars.collect();
                if glob_match_inner(&rest_pattern, &remaining) {
                    return true;
                }
                for (i, ch) in remaining.char_indices() {
                    if ch == '/' {
                        break; // * doesn't cross /
                    }
                    let next = i + ch.len_utf8();
                    if glob_match_inner(&rest_pattern, &remaining[next..]) {
                        return true;
                    }
                }
                return false;
            }
            '?' => match path_chars.next() {
                Some('/') | None => return false,
                _ => {}
            },
            c => {
                if path_chars.next() != Some(c) {
                    return false;
                }
            }
        }
    }

    path_chars.next().is_none()
}

// ══════════════════════════════════════════════════════════════════════════
// Tests
// ══════════════════════════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;

    // ── Glob matching ────────────────────────────────────────────────────

    #[test]
    fn glob_exact_match() {
        assert!(glob_match("foo.txt", "foo.txt"));
        assert!(!glob_match("foo.txt", "bar.txt"));
    }

    #[test]
    fn glob_star_matches_filename() {
        assert!(glob_match("*.txt", "foo.txt"));
        assert!(glob_match("*.txt", "bar.txt"));
        assert!(!glob_match("*.txt", "foo.rs"));
    }

    #[test]
    fn glob_star_does_not_cross_separator() {
        assert!(!glob_match("*.txt", "dir/foo.txt"));
    }

    #[test]
    fn glob_star_handles_non_ascii_without_byte_boundary_panic() {
        assert!(glob_match("*.txt", "café.txt"));
        assert!(glob_match("caf*.txt", "café.txt"));
        assert!(!glob_match("caf*.rs", "café.txt"));
    }

    #[test]
    fn glob_doublestar_matches_any_depth() {
        assert!(glob_match("**/.env*", ".env"));
        assert!(glob_match("**/.env*", ".env.local"));
        assert!(glob_match("**/.env*", "src/.env.production"));
        assert!(glob_match("**/.env*", "a/b/c/.env"));
    }

    #[test]
    fn glob_doublestar_trailing() {
        assert!(glob_match("src/**", "src"));
        assert!(glob_match("src/**", "src/foo.rs"));
        assert!(glob_match("src/**", "src/a/b/c.rs"));
        assert!(!glob_match("src/**", "lib/foo.rs"));
        assert!(!glob_match("src/**", "src2/foo.rs"));
        assert!(glob_match("*/src/**", "project/src/main.rs"));
        assert!(!glob_match("*/src/**", "project/src2/main.rs"));
    }

    #[test]
    fn glob_doublestar_standalone() {
        assert!(glob_match("**", "anything"));
        assert!(glob_match("**", "a/b/c/d"));
    }

    #[test]
    fn glob_question_mark() {
        assert!(glob_match("f?o", "foo"));
        assert!(glob_match("f?o", "fao"));
        assert!(!glob_match("f?o", "fooo"));
        assert!(!glob_match("f?o", "f/o")); // ? doesn't match /
    }

    // ── Profile construction ─────────────────────────────────────────────

    #[test]
    fn strict_profile_denies_network_and_processes() {
        let policy = SandboxProfile::Strict.to_policy();
        assert!(!policy.network.allow_network);
        assert!(!policy.process.allow_subprocess);
        assert!(policy.fs.write_allow.is_empty());
    }

    #[test]
    fn standard_profile_allows_toolchain_subprocesses() {
        let policy = SandboxProfile::Standard.to_policy();
        assert!(!policy.network.allow_network);
        assert!(policy.process.allow_subprocess);
        assert!(
            policy
                .process
                .allowed_executables
                .contains(&"node".to_string())
        );
        assert!(
            policy
                .process
                .allowed_executables
                .contains(&"git".to_string())
        );
    }

    #[test]
    fn permissive_profile_allows_registry_hosts() {
        let policy = SandboxProfile::Permissive.to_policy();
        assert!(policy.network.allow_network);
        assert!(
            policy
                .network
                .allowed_hosts
                .contains(&"registry.npmjs.org".to_string())
        );
    }

    #[test]
    fn profile_roundtrip_names() {
        assert_eq!(SandboxProfile::Strict.as_str(), "strict");
        assert_eq!(SandboxProfile::Standard.as_str(), "standard");
        assert_eq!(SandboxProfile::Permissive.as_str(), "permissive");
    }

    // ── Policy JSON round-trip ───────────────────────────────────────────

    #[test]
    fn policy_json_round_trip() {
        let policy = SandboxProfile::Standard.to_policy();
        let json = serde_json::to_string_pretty(&policy).unwrap();
        let parsed = SandboxPolicy::from_json(&json).unwrap();
        assert_eq!(parsed.profile_name, "standard");
        assert_eq!(
            parsed.process.allowed_executables,
            policy.process.allowed_executables
        );
    }

    #[test]
    fn policy_from_invalid_json_returns_error() {
        let result = SandboxPolicy::from_json("not json");
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert_eq!(err.exit_code(), EXIT_SANDBOX_POLICY_LOAD_ERROR);
    }

    // ── FS enforcement ───────────────────────────────────────────────────

    #[test]
    fn enforcer_denies_read_in_deny_list() {
        let mut enforcer = SandboxEnforcer::from_profile(SandboxProfile::Standard, "test-run");
        let result = enforcer.check_read(Path::new("/project/.env.local"));
        assert!(result.is_err());
        let v = result.unwrap_err();
        assert_eq!(v.kind, ViolationKind::FsReadDenied);
    }

    #[test]
    fn enforcer_allows_read_not_in_deny_list_with_explicit_allow() {
        let mut policy = SandboxProfile::Standard.to_policy();
        policy.allow_read_path("/project/**");
        let mut enforcer = SandboxEnforcer::new(policy, "test-run");
        let result = enforcer.check_read(Path::new("/project/src/main.rs"));
        assert!(result.is_ok());
    }

    #[test]
    fn enforcer_denies_read_when_allow_list_empty() {
        for profile in [SandboxProfile::Strict, SandboxProfile::Standard] {
            let mut enforcer = SandboxEnforcer::from_profile(profile, "test-run");
            let result = enforcer.check_read(Path::new("/project/src/main.rs"));
            assert!(result.is_err(), "{profile:?} should fail closed");
            assert_eq!(result.unwrap_err().kind, ViolationKind::FsReadDenied);
        }
    }

    #[test]
    fn enforcer_denies_read_outside_allow_list() {
        let mut policy = SandboxProfile::Strict.to_policy();
        policy.allow_read_path("/snapshot/**");
        let mut enforcer = SandboxEnforcer::new(policy, "test-run");
        let result = enforcer.check_read(Path::new("/etc/passwd"));
        assert!(result.is_err());
        assert_eq!(result.unwrap_err().kind, ViolationKind::FsReadDenied);
    }

    #[test]
    fn enforcer_denies_all_writes_when_write_allow_empty() {
        let mut enforcer = SandboxEnforcer::from_profile(SandboxProfile::Strict, "test-run");
        let result = enforcer.check_write(Path::new("/any/path"));
        assert!(result.is_err());
        assert_eq!(result.unwrap_err().kind, ViolationKind::FsWriteDenied);
    }

    #[test]
    fn enforcer_allows_writes_within_allow_list() {
        let mut policy = SandboxProfile::Standard.to_policy();
        policy.allow_write_path("/output/**");
        let mut enforcer = SandboxEnforcer::new(policy, "test-run");
        let result = enforcer.check_write(Path::new("/output/report.json"));
        assert!(result.is_ok());
    }

    #[test]
    fn enforcer_tracks_cumulative_bytes_read() {
        let mut enforcer = SandboxEnforcer::from_profile(SandboxProfile::Strict, "test-run");
        // Strict limit: 256 MiB. Read in small chunks should work.
        assert!(enforcer.record_bytes_read(1024).is_ok());
        assert!(enforcer.record_bytes_read(1024).is_ok());
        // Exceed limit.
        let result = enforcer.record_bytes_read(256 * 1024 * 1024);
        assert!(result.is_err());
        assert_eq!(result.unwrap_err().kind, ViolationKind::FsReadBytesExceeded);
    }

    #[test]
    fn enforcer_rejects_oversized_file() {
        let mut enforcer = SandboxEnforcer::from_profile(SandboxProfile::Strict, "test-run");
        // Strict max_file_size: 16 MiB.
        let result = enforcer.check_file_size(Path::new("/big.bin"), 32 * 1024 * 1024);
        assert!(result.is_err());
        assert_eq!(result.unwrap_err().kind, ViolationKind::FsFileSizeExceeded);
    }

    #[test]
    fn enforcer_rejects_excessive_depth() {
        let mut enforcer = SandboxEnforcer::from_profile(SandboxProfile::Strict, "test-run");
        assert!(enforcer.check_depth(5).is_ok());
        let result = enforcer.check_depth(25);
        assert!(result.is_err());
        assert_eq!(result.unwrap_err().kind, ViolationKind::FsDepthExceeded);
    }

    #[test]
    fn enforcer_rejects_excessive_file_count() {
        let mut policy = SandboxProfile::Strict.to_policy();
        policy.fs.max_file_count = 3;
        let mut enforcer = SandboxEnforcer::new(policy, "test-run");
        assert!(enforcer.record_file_enumerated().is_ok());
        assert!(enforcer.record_file_enumerated().is_ok());
        assert!(enforcer.record_file_enumerated().is_ok());
        let result = enforcer.record_file_enumerated();
        assert!(result.is_err());
        assert_eq!(result.unwrap_err().kind, ViolationKind::FsFileCountExceeded);
    }

    // ── Network enforcement ──────────────────────────────────────────────

    #[test]
    fn strict_profile_blocks_all_network() {
        let mut enforcer = SandboxEnforcer::from_profile(SandboxProfile::Strict, "test-run");
        let result = enforcer.check_network("example.com");
        assert!(result.is_err());
        assert_eq!(result.unwrap_err().kind, ViolationKind::NetworkBlocked);
    }

    #[test]
    fn permissive_network_allows_registry_hosts() {
        let mut enforcer = SandboxEnforcer::from_profile(SandboxProfile::Permissive, "test-run");
        assert!(enforcer.check_network("registry.npmjs.org").is_ok());
        assert!(enforcer.check_network("crates.io").is_ok());
    }

    #[test]
    fn permissive_profile_denies_unlisted_hosts() {
        let mut enforcer = SandboxEnforcer::from_profile(SandboxProfile::Permissive, "test-run");
        let result = enforcer.check_network("evil.com");
        assert!(result.is_err());
        assert_eq!(result.unwrap_err().kind, ViolationKind::NetworkHostDenied);
    }

    #[test]
    fn network_connection_limit_enforced() {
        let mut policy = SandboxProfile::Permissive.to_policy();
        policy.network.max_connections = 2;
        let mut enforcer = SandboxEnforcer::new(policy, "test-run");
        assert!(enforcer.check_network("registry.npmjs.org").is_ok());
        assert!(enforcer.check_network("crates.io").is_ok());
        let result = enforcer.check_network("pypi.org");
        assert!(result.is_err());
        assert_eq!(
            result.unwrap_err().kind,
            ViolationKind::NetworkConnectionLimit
        );
    }

    #[test]
    fn network_host_matching_is_case_insensitive_and_trims_trailing_dot() {
        let mut enforcer = SandboxEnforcer::from_profile(SandboxProfile::Permissive, "test-run");
        assert!(enforcer.check_network("Registry.Npmjs.Org.").is_ok());

        let mut policy = SandboxProfile::Permissive.to_policy();
        policy.network.allowed_hosts.clear();
        policy.network.blocked_hosts = vec!["blocked.example".to_string()];
        let mut blocked = SandboxEnforcer::new(policy, "test-run");
        let result = blocked.check_network("BLOCKED.EXAMPLE.");
        assert!(result.is_err());
        assert_eq!(result.unwrap_err().kind, ViolationKind::NetworkHostDenied);
    }

    #[test]
    fn network_connection_limit_denies_before_incrementing_counter() {
        let mut policy = SandboxProfile::Permissive.to_policy();
        policy.network.max_connections = 1;
        let mut enforcer = SandboxEnforcer::new(policy, "test-run");

        assert!(enforcer.check_network("registry.npmjs.org").is_ok());
        let result = enforcer.check_network("crates.io");
        assert!(result.is_err());
        assert_eq!(
            result.unwrap_err().kind,
            ViolationKind::NetworkConnectionLimit
        );

        let report = enforcer.into_report();
        assert_eq!(report.counters["connections_opened"], 1);
    }

    // ── Process enforcement ──────────────────────────────────────────────

    #[test]
    fn strict_profile_blocks_all_subprocesses() {
        let mut enforcer = SandboxEnforcer::from_profile(SandboxProfile::Strict, "test-run");
        let result = enforcer.check_subprocess("node");
        assert!(result.is_err());
        assert_eq!(result.unwrap_err().kind, ViolationKind::ProcessBlocked);
    }

    #[test]
    fn standard_profile_allows_toolchain_executables() {
        let mut enforcer = SandboxEnforcer::from_profile(SandboxProfile::Standard, "test-run");
        assert!(enforcer.check_subprocess("node").is_ok());
        assert!(enforcer.check_subprocess("git").is_ok());
        assert!(enforcer.check_subprocess("tsc").is_ok());
    }

    #[test]
    fn standard_profile_denies_unknown_executables() {
        let mut enforcer = SandboxEnforcer::from_profile(SandboxProfile::Standard, "test-run");
        let result = enforcer.check_subprocess("curl");
        assert!(result.is_err());
        assert_eq!(
            result.unwrap_err().kind,
            ViolationKind::ProcessExecutableDenied
        );
    }

    #[test]
    fn standard_profile_denies_executable_paths_even_with_allowed_basename() {
        let mut enforcer = SandboxEnforcer::from_profile(SandboxProfile::Standard, "test-run");

        for executable in ["/tmp/node", "tools/git", r"tools\tsc"] {
            let result = enforcer.check_subprocess(executable);
            assert!(result.is_err(), "{executable} should be denied");
            assert_eq!(
                result.unwrap_err().kind,
                ViolationKind::ProcessExecutableDenied
            );
        }
    }

    #[test]
    fn process_total_limit_enforced() {
        let mut policy = SandboxProfile::Standard.to_policy();
        policy.process.max_total = 2;
        let mut enforcer = SandboxEnforcer::new(policy, "test-run");
        assert!(enforcer.check_subprocess("node").is_ok());
        enforcer.record_subprocess_exit();
        assert!(enforcer.check_subprocess("git").is_ok());
        enforcer.record_subprocess_exit();
        let result = enforcer.check_subprocess("tsc");
        assert!(result.is_err());
        assert_eq!(result.unwrap_err().kind, ViolationKind::ProcessTotalLimit);
    }

    #[test]
    fn process_concurrent_limit_enforced() {
        let mut policy = SandboxProfile::Standard.to_policy();
        policy.process.max_concurrent = 1;
        let mut enforcer = SandboxEnforcer::new(policy, "test-run");
        assert!(enforcer.check_subprocess("node").is_ok());
        // Second subprocess without exit should fail.
        let result = enforcer.check_subprocess("git");
        assert!(result.is_err());
        assert_eq!(
            result.unwrap_err().kind,
            ViolationKind::ProcessConcurrentLimit
        );
        // After exit, should work again.
        enforcer.record_subprocess_exit();
        assert!(enforcer.check_subprocess("git").is_ok());
    }

    #[test]
    fn subprocess_timeout_from_policy() {
        let enforcer = SandboxEnforcer::from_profile(SandboxProfile::Standard, "test-run");
        assert_eq!(enforcer.subprocess_timeout(), Duration::from_secs(30));
    }

    // ── Resource enforcement ─────────────────────────────────────────────

    #[test]
    fn output_bytes_limit_enforced() {
        let mut policy = SandboxProfile::Strict.to_policy();
        policy.resource.max_output_bytes = 100;
        let mut enforcer = SandboxEnforcer::new(policy, "test-run");
        assert!(enforcer.record_output_bytes(50).is_ok());
        assert!(enforcer.record_output_bytes(50).is_ok());
        let result = enforcer.record_output_bytes(1);
        assert!(result.is_err());
        assert_eq!(
            result.unwrap_err().kind,
            ViolationKind::ResourceOutputExceeded
        );
    }

    // ── Audit log ────────────────────────────────────────────────────────

    #[test]
    fn audit_log_records_decisions() {
        let mut enforcer = SandboxEnforcer::from_profile(SandboxProfile::Strict, "test-run");
        // Init entry.
        assert_eq!(enforcer.audit_log().len(), 1);

        // Trigger a violation.
        let _ = enforcer.check_network("example.com");
        assert_eq!(enforcer.audit_log().len(), 2);

        let jsonl = enforcer.audit_log().to_jsonl();
        assert_eq!(jsonl.lines().count(), 2);
        for line in jsonl.lines() {
            let v: serde_json::Value = serde_json::from_str(line).unwrap();
            assert_eq!(v["run_id"], "test-run");
        }
    }

    #[test]
    fn audit_log_entries_are_valid_json() {
        let mut policy = SandboxProfile::Standard.to_policy();
        policy.allow_read_path("/project/**");
        let mut enforcer = SandboxEnforcer::new(policy, "test-run");

        let _ = enforcer.check_read(Path::new("/project/src/main.rs"));
        let _ = enforcer.check_subprocess("node");
        let _ = enforcer.check_network("evil.com"); // Should fail.

        let jsonl = enforcer.audit_log().to_jsonl();
        for line in jsonl.lines() {
            let _: serde_json::Value =
                serde_json::from_str(line).expect("each audit entry must be valid JSON");
        }
    }

    // ── Violation metadata ───────────────────────────────────────────────

    #[test]
    fn violation_has_exit_code() {
        let v = SandboxViolation::new(ViolationKind::FsReadDenied, "test");
        assert_eq!(v.kind.exit_code(), EXIT_SANDBOX_FS_VIOLATION);
    }

    #[test]
    fn violation_converts_to_doctor_error() {
        let v = SandboxViolation::new(ViolationKind::NetworkBlocked, "net blocked");
        let err = v.into_error();
        assert_eq!(err.exit_code(), EXIT_SANDBOX_NETWORK_VIOLATION);
        assert_eq!(err.to_string(), "net blocked");
    }

    #[test]
    fn violation_with_path_and_limits() {
        let v = SandboxViolation::new(ViolationKind::FsFileSizeExceeded, "too big")
            .with_path("/big/file.bin")
            .with_limits("16777216", "33554432");
        assert_eq!(v.path.as_deref(), Some("/big/file.bin"));
        assert_eq!(v.limit, "16777216");
        assert_eq!(v.actual, "33554432");
    }

    // ── Report generation ────────────────────────────────────────────────

    #[test]
    fn report_captures_counters() {
        let mut enforcer = SandboxEnforcer::from_profile(SandboxProfile::Standard, "test-run");
        let _ = enforcer.record_bytes_read(1024);
        let _ = enforcer.check_subprocess("node");
        enforcer.record_subprocess_exit();

        let report = enforcer.into_report();
        assert_eq!(report.run_id, "test-run");
        assert_eq!(report.profile, "standard");
        assert_eq!(report.verdict, "pass");
        assert_eq!(report.counters["bytes_read"], 1024);
        assert_eq!(report.counters["subprocesses_spawned"], 1);
        assert!(report.wall_time_ms < 1000); // Should be fast.
    }

    #[test]
    fn report_captures_recorded_violations() {
        let mut enforcer = SandboxEnforcer::from_profile(SandboxProfile::Strict, "test-run");
        let error = enforcer
            .check_network("example.com")
            .expect_err("strict profile should block network");
        assert_eq!(error.kind, ViolationKind::NetworkBlocked);

        let report = enforcer.into_report();
        assert_eq!(report.verdict, "fail");
        assert_eq!(report.violations.len(), 1);
        assert_eq!(report.violations[0].kind, ViolationKind::NetworkBlocked);
    }

    #[test]
    fn report_serializes_to_json() {
        let enforcer = SandboxEnforcer::from_profile(SandboxProfile::Strict, "test-run");
        let report = enforcer.into_report();
        let json = serde_json::to_string(&report).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed["run_id"], "test-run");
        assert_eq!(parsed["profile"], "strict");
    }

    // ── Deny list patterns ───────────────────────────────────────────────

    #[test]
    fn deny_patterns_block_sensitive_files() {
        let mut policy = SandboxProfile::Standard.to_policy();
        policy.allow_read_path("/project/**");
        let mut enforcer = SandboxEnforcer::new(policy, "test-run");

        // These should be denied.
        assert!(enforcer.check_read(Path::new("/project/.env")).is_err());
        assert!(
            enforcer
                .check_read(Path::new("/project/.env.local"))
                .is_err()
        );
        assert!(
            enforcer
                .check_read(Path::new("/project/config/secret.json"))
                .is_err()
        );
        assert!(
            enforcer
                .check_read(Path::new("/project/.git/config"))
                .is_err()
        );

        // These should be allowed.
        assert!(
            enforcer
                .check_read(Path::new("/project/src/main.rs"))
                .is_ok()
        );
        assert!(
            enforcer
                .check_read(Path::new("/project/package.json"))
                .is_ok()
        );
    }

    // ── Edge cases ───────────────────────────────────────────────────────

    #[test]
    fn subprocess_exit_does_not_underflow() {
        let mut enforcer = SandboxEnforcer::from_profile(SandboxProfile::Standard, "test-run");
        // Call exit without prior spawn — should not panic.
        enforcer.record_subprocess_exit();
        enforcer.record_subprocess_exit();
        // active should be 0, not negative.
    }

    #[test]
    fn bytes_read_saturates_on_overflow() {
        let mut policy = SandboxProfile::Strict.to_policy();
        policy.fs.max_read_bytes = u64::MAX;
        let mut enforcer = SandboxEnforcer::new(policy, "test-run");
        assert!(enforcer.record_bytes_read(u64::MAX).is_ok());
        // Adding more should saturate, not overflow.
        assert!(enforcer.record_bytes_read(1).is_ok());
    }

    #[test]
    fn policy_serde_round_trip_all_profiles() {
        for profile in [
            SandboxProfile::Strict,
            SandboxProfile::Standard,
            SandboxProfile::Permissive,
        ] {
            let policy = profile.to_policy();
            let json = serde_json::to_string(&policy).unwrap();
            let parsed = SandboxPolicy::from_json(&json).unwrap();
            assert_eq!(parsed.profile_name, profile.as_str());
        }
    }

    #[test]
    fn enforcer_elapsed_increases() {
        let enforcer = SandboxEnforcer::from_profile(SandboxProfile::Strict, "test-run");
        let d1 = enforcer.elapsed();
        std::thread::sleep(Duration::from_millis(10));
        let d2 = enforcer.elapsed();
        assert!(d2 > d1);
    }

    #[cfg(unix)]
    #[test]
    fn check_read_denies_symlink_escape_from_allowed_prefix() {
        use std::os::unix::fs::symlink;

        let temp = tempfile::tempdir().unwrap();
        let snapshot_dir = temp.path().join("snapshot");
        let outside_dir = temp.path().join("outside");
        fs::create_dir_all(&snapshot_dir).unwrap();
        fs::create_dir_all(&outside_dir).unwrap();

        let outside_file = outside_dir.join("secret.txt");
        fs::write(&outside_file, "top-secret").unwrap();
        symlink(&outside_file, snapshot_dir.join("escape.txt")).unwrap();

        let mut policy = SandboxProfile::Strict.to_policy();
        policy.allow_read_path(format!("{}/**", snapshot_dir.display()));
        let mut enforcer = SandboxEnforcer::new(policy, "test-run");

        let error = enforcer
            .check_read(&snapshot_dir.join("escape.txt"))
            .expect_err("symlink escape should be denied");
        assert_eq!(error.kind, ViolationKind::FsReadDenied);
    }

    #[cfg(unix)]
    #[test]
    fn check_write_denies_symlink_parent_escape_from_allowed_prefix() {
        use std::os::unix::fs::symlink;

        let temp = tempfile::tempdir().unwrap();
        let output_dir = temp.path().join("output");
        let outside_dir = temp.path().join("outside");
        fs::create_dir_all(&output_dir).unwrap();
        fs::create_dir_all(&outside_dir).unwrap();
        symlink(&outside_dir, output_dir.join("escape")).unwrap();

        let mut policy = SandboxProfile::Standard.to_policy();
        policy.allow_write_path(format!("{}/**", output_dir.display()));
        let mut enforcer = SandboxEnforcer::new(policy, "test-run");

        let error = enforcer
            .check_write(&output_dir.join("escape/out.txt"))
            .expect_err("symlinked parent escape should be denied");
        assert_eq!(error.kind, ViolationKind::FsWriteDenied);
    }

    #[test]
    fn violation_kind_exit_codes_are_in_range() {
        let kinds = [
            ViolationKind::FsReadDenied,
            ViolationKind::FsWriteDenied,
            ViolationKind::FsReadBytesExceeded,
            ViolationKind::FsFileSizeExceeded,
            ViolationKind::FsDepthExceeded,
            ViolationKind::FsFileCountExceeded,
            ViolationKind::NetworkBlocked,
            ViolationKind::NetworkHostDenied,
            ViolationKind::NetworkConnectionLimit,
            ViolationKind::ProcessBlocked,
            ViolationKind::ProcessExecutableDenied,
            ViolationKind::ProcessConcurrentLimit,
            ViolationKind::ProcessTotalLimit,
            ViolationKind::ProcessTimeout,
            ViolationKind::ResourceWallTimeExceeded,
            ViolationKind::ResourceCpuTimeExceeded,
            ViolationKind::ResourceMemoryExceeded,
            ViolationKind::ResourceFdLimitExceeded,
            ViolationKind::ResourceOutputExceeded,
        ];
        for kind in kinds {
            let code = kind.exit_code();
            assert!(
                (50..=59).contains(&code),
                "exit code {code} for {kind:?} not in 50-59 range"
            );
        }
    }
}
