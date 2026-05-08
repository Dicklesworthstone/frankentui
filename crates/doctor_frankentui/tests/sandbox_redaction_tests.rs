//! Cross-cutting unit tests for sandbox policy enforcement and redaction
//! correctness across the ingestion pipeline.
//!
//! These tests validate:
//! 1. Allow/deny matrix and boundary resource scenarios.
//! 2. No secret leakage across manifest, log, and artifact paths.
//! 3. Structured logs include policy rule IDs and redaction token fingerprints.

use doctor_frankentui::redact::{
    ScanReport, builtin_patterns, redact_content, scan_content, unredact_content,
};
use doctor_frankentui::sandbox::{
    SandboxEnforcer, SandboxPolicy, SandboxProfile, SandboxViolation, ViolationKind,
};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::{env, fs};

// ══════════════════════════════════════════════════════════════════════════
// 1. Sandbox allow/deny matrix tests
// ══════════════════════════════════════════════════════════════════════════

#[test]
fn strict_profile_allow_deny_matrix() {
    let mut policy = SandboxProfile::Strict.to_policy();
    policy.allow_read_path("/snapshot/**");
    let mut e = SandboxEnforcer::new(policy, "matrix-test");

    // Allowed: paths inside snapshot.
    assert!(e.check_read(Path::new("/snapshot/src/main.rs")).is_ok());
    assert!(e.check_read(Path::new("/snapshot/package.json")).is_ok());
    assert!(e.check_read(Path::new("/snapshot/src/a/b/c.ts")).is_ok());

    // Denied: sensitive files inside snapshot.
    assert!(e.check_read(Path::new("/snapshot/.env")).is_err());
    assert!(e.check_read(Path::new("/snapshot/.env.local")).is_err());
    assert!(e.check_read(Path::new("/snapshot/.git/config")).is_err());
    assert!(
        e.check_read(Path::new("/snapshot/config/secret.json"))
            .is_err()
    );
    assert!(
        e.check_read(Path::new("/snapshot/credential_store.json"))
            .is_err()
    );

    // Denied: paths outside snapshot entirely.
    assert!(e.check_read(Path::new("/etc/passwd")).is_err());
    assert!(e.check_read(Path::new("/home/user/.ssh/id_rsa")).is_err());

    // Denied: all writes (strict has empty write_allow).
    assert!(e.check_write(Path::new("/snapshot/output.json")).is_err());
    assert!(e.check_write(Path::new("/tmp/result.json")).is_err());

    // Denied: all network and subprocess.
    assert!(e.check_network("example.com").is_err());
    assert!(e.check_subprocess("node").is_err());
}

#[test]
fn standard_profile_allow_deny_matrix() {
    let mut policy = SandboxProfile::Standard.to_policy();
    policy.allow_read_path("/project/**");
    let mut e = SandboxEnforcer::new(policy, "matrix-test");

    // Allowed reads.
    assert!(e.check_read(Path::new("/project/src/app.tsx")).is_ok());

    // Denied reads (sensitive files).
    assert!(e.check_read(Path::new("/project/.env")).is_err());
    assert!(
        e.check_read(Path::new("/project/config/secret.json"))
            .is_err()
    );

    // Denied: all network.
    assert!(e.check_network("npmjs.org").is_err());

    // Allowed subprocesses: toolchain only.
    assert!(e.check_subprocess("node").is_ok());
    e.record_subprocess_exit();
    assert!(e.check_subprocess("git").is_ok());
    e.record_subprocess_exit();
    assert!(e.check_subprocess("tsc").is_ok());
    e.record_subprocess_exit();

    // Denied subprocesses: non-toolchain.
    assert!(e.check_subprocess("curl").is_err());
    assert!(e.check_subprocess("wget").is_err());
    assert!(e.check_subprocess("bash").is_err());
    assert!(e.check_subprocess("python").is_err());
}

#[test]
fn permissive_profile_allow_deny_matrix() {
    let mut e = SandboxEnforcer::from_profile(SandboxProfile::Permissive, "matrix-test");

    // Allowed: broad FS reads.
    assert!(e.check_read(Path::new("/project/src/main.rs")).is_ok());
    assert!(e.check_read(Path::new("/usr/local/lib/node.js")).is_ok());

    // Denied: still blocks .env files.
    assert!(e.check_read(Path::new("/project/.env")).is_err());

    // Allowed: registry hosts.
    assert!(e.check_network("registry.npmjs.org").is_ok());
    assert!(e.check_network("crates.io").is_ok());
    assert!(e.check_network("pypi.org").is_ok());

    // Denied: non-registry hosts.
    assert!(e.check_network("evil.com").is_err());
    assert!(e.check_network("malware.xyz").is_err());

    // Allowed: all executables (empty allowlist = all allowed).
    assert!(e.check_subprocess("node").is_ok());
    e.record_subprocess_exit();
    assert!(e.check_subprocess("python").is_ok());
    e.record_subprocess_exit();
    assert!(e.check_subprocess("custom-tool").is_ok());
    e.record_subprocess_exit();
}

// ══════════════════════════════════════════════════════════════════════════
// 2. Boundary resource scenarios
// ══════════════════════════════════════════════════════════════════════════

#[test]
fn fs_bytes_at_exact_limit_is_allowed() {
    let mut policy = SandboxProfile::Strict.to_policy();
    policy.fs.max_read_bytes = 1000;
    let mut e = SandboxEnforcer::new(policy, "boundary-test");

    // Right at limit should be OK.
    assert!(e.record_bytes_read(1000).is_ok());
    // One more byte should fail.
    assert!(e.record_bytes_read(1).is_err());
}

#[test]
fn fs_file_count_at_exact_limit_is_allowed() {
    let mut policy = SandboxProfile::Strict.to_policy();
    policy.fs.max_file_count = 5;
    let mut e = SandboxEnforcer::new(policy, "boundary-test");

    for _ in 0..5 {
        assert!(e.record_file_enumerated().is_ok());
    }
    assert!(e.record_file_enumerated().is_err());
}

#[test]
fn fs_depth_at_exact_limit_is_allowed() {
    let mut policy = SandboxProfile::Strict.to_policy();
    policy.fs.max_depth = 10;
    let mut e = SandboxEnforcer::new(policy, "boundary-test");

    assert!(e.check_depth(10).is_ok());
    assert!(e.check_depth(11).is_err());
}

#[test]
fn fs_file_size_at_exact_limit_is_allowed() {
    let mut policy = SandboxProfile::Strict.to_policy();
    policy.fs.max_file_size = 1024;
    let mut e = SandboxEnforcer::new(policy, "boundary-test");

    assert!(e.check_file_size(Path::new("ok.bin"), 1024).is_ok());
    assert!(e.check_file_size(Path::new("big.bin"), 1025).is_err());
}

#[test]
fn process_total_at_exact_limit() {
    let mut policy = SandboxProfile::Standard.to_policy();
    policy.process.max_total = 3;
    let mut e = SandboxEnforcer::new(policy, "boundary-test");

    for _ in 0..3 {
        assert!(e.check_subprocess("node").is_ok());
        e.record_subprocess_exit();
    }
    // 4th should fail (total, not concurrent).
    assert!(e.check_subprocess("node").is_err());
}

#[test]
fn network_connections_at_exact_limit() {
    let mut policy = SandboxProfile::Permissive.to_policy();
    policy.network.max_connections = 2;
    let mut e = SandboxEnforcer::new(policy, "boundary-test");

    assert!(e.check_network("registry.npmjs.org").is_ok());
    assert!(e.check_network("crates.io").is_ok());
    assert!(e.check_network("pypi.org").is_err());
}

#[test]
fn output_bytes_at_exact_limit() {
    let mut policy = SandboxProfile::Strict.to_policy();
    policy.resource.max_output_bytes = 100;
    let mut e = SandboxEnforcer::new(policy, "boundary-test");

    assert!(e.record_output_bytes(100).is_ok());
    assert!(e.record_output_bytes(1).is_err());
}

// ══════════════════════════════════════════════════════════════════════════
// 3. Redaction no-leakage tests
// ══════════════════════════════════════════════════════════════════════════

const SECRET_AWS: &str = "AKIAIOSFODNN7EXAMPLE";
const SECRET_GITHUB: &str = "ghp_ABCDEFGHIJKLMNOPQRSTUVWXYZabcdef0123456789";
const SECRET_DB: &str = "postgres://admin:supersecret@db.example.com:5432/mydb";

#[test]
fn no_secrets_in_redacted_manifest_output() {
    let manifest_content = format!(
        r#"{{
  "source_hash": "sha256:abc",
  "github_token": "{SECRET_GITHUB}",
  "aws_key": "{SECRET_AWS}",
  "db_url": "{SECRET_DB}"
}}"#
    );

    let result = redact_content(
        &manifest_content,
        "intake_meta.json",
        &builtin_patterns(),
        "leak-test",
    );

    // None of the secrets should appear in the output.
    assert!(!result.content.contains(SECRET_GITHUB));
    assert!(!result.content.contains(SECRET_AWS));
    assert!(!result.content.contains("supersecret"));

    // Placeholders should be present.
    assert!(
        result.content.contains("[REDACTED:"),
        "redacted output should contain placeholders"
    );
}

#[test]
fn no_secrets_in_redacted_log_output() {
    let log = format!(
        "2026-02-25T12:00:00Z INFO Connecting to {SECRET_DB}\n\
         2026-02-25T12:00:01Z INFO Using token {SECRET_GITHUB}\n\
         2026-02-25T12:00:02Z INFO AWS access key: {SECRET_AWS}"
    );

    let result = redact_content(&log, "pipeline.log", &builtin_patterns(), "leak-test");

    assert!(!result.content.contains("supersecret"));
    assert!(!result.content.contains("ghp_"));
    assert!(!result.content.contains(SECRET_AWS));
}

#[test]
fn no_secrets_in_redacted_artifact_source() {
    let source = format!(
        r#"const config = {{
  apiKey: '{SECRET_AWS}',
  token: '{SECRET_GITHUB}',
  dbUrl: '{SECRET_DB}',
}};"#
    );

    let result = redact_content(&source, "config.ts", &builtin_patterns(), "leak-test");

    assert!(!result.content.contains(SECRET_AWS));
    assert!(!result.content.contains("ghp_"));
    assert!(!result.content.contains("supersecret"));
}

#[test]
fn redaction_preserves_non_secret_structure() {
    let content = format!(
        r#"{{
  "name": "my-project",
  "version": "1.0.0",
  "api_key": "{SECRET_AWS}",
  "description": "A safe project"
}}"#
    );

    let result = redact_content(&content, "config.json", &builtin_patterns(), "struct-test");

    // Non-secret fields should be preserved.
    assert!(result.content.contains("my-project"));
    assert!(result.content.contains("1.0.0"));
    assert!(result.content.contains("A safe project"));

    // Secret field should be redacted.
    assert!(!result.content.contains(SECRET_AWS));
}

#[test]
fn redaction_records_contain_no_raw_secrets() {
    let content = format!("token = {SECRET_GITHUB}");
    let result = redact_content(&content, ".env", &builtin_patterns(), "record-test");

    for record in &result.records {
        // Placeholder should not contain raw secret.
        assert!(
            !record.placeholder.contains("ghp_"),
            "placeholder should not contain raw secret"
        );
        // Value hash should be a hex string, not the raw value.
        assert!(
            record.value_hash.len() == 64,
            "value_hash should be 64-char SHA-256 hex"
        );
        assert!(
            record.value_hash.chars().all(|c| c.is_ascii_hexdigit()),
            "value_hash should be hex only"
        );
    }
}

#[test]
fn scan_report_contains_no_raw_secrets() {
    let content = format!("token = {SECRET_GITHUB}\nkey = {SECRET_AWS}");
    let findings = scan_content(&content, ".env", &builtin_patterns());
    let report = ScanReport::from_findings("report-test", &findings);

    let json = serde_json::to_string_pretty(&report).unwrap();

    // Report JSON should never contain raw secrets.
    assert!(!json.contains(SECRET_GITHUB));
    assert!(!json.contains(SECRET_AWS));
}

// ══════════════════════════════════════════════════════════════════════════
// 4. Redaction round-trip correctness
// ══════════════════════════════════════════════════════════════════════════

#[test]
fn round_trip_restores_all_secrets() {
    let content = format!(
        "GITHUB_TOKEN={SECRET_GITHUB}\n\
         AWS_ACCESS_KEY_ID={SECRET_AWS}\n\
         DATABASE_URL={SECRET_DB}\n\
         SAFE_VAR=hello_world"
    );

    let result = redact_content(&content, ".env", &builtin_patterns(), "round-trip");
    let restored = unredact_content(&result.content, &result.map);
    assert_eq!(
        restored, content,
        "round-trip must restore original content exactly"
    );
}

#[test]
fn round_trip_with_env_file_containing_mixed_content() {
    let content = "\
# Database configuration
NODE_ENV=production
PORT=3000
SECRET_KEY=my-super-secret-key-value-12345678
API_KEY=sk_test_4eC39HqLyjWDarjtT1zdp7dc
DEBUG=false";

    let result = redact_content(content, ".env", &builtin_patterns(), "mixed-test");

    // Non-secret lines should be unchanged.
    assert!(result.content.contains("NODE_ENV=production"));
    assert!(result.content.contains("PORT=3000"));
    assert!(result.content.contains("DEBUG=false"));

    // Secret lines should be redacted.
    assert!(
        !result
            .content
            .contains("my-super-secret-key-value-12345678")
    );
    assert!(!result.content.contains("sk_test_4eC39HqLyjWDarjtT1zdp7dc"));

    // Round-trip should restore.
    let restored = unredact_content(&result.content, &result.map);
    assert_eq!(restored, content);
}

// ══════════════════════════════════════════════════════════════════════════
// 5. Structured audit log quality
// ══════════════════════════════════════════════════════════════════════════

#[test]
fn audit_log_contains_category_and_action() {
    let mut policy = SandboxProfile::Standard.to_policy();
    policy.allow_read_path("/project/**");
    let mut e = SandboxEnforcer::new(policy, "audit-test");

    // Generate some decisions.
    let _ = e.check_read(Path::new("/project/src/main.rs")); // allow
    let _ = e.check_read(Path::new("/project/.env")); // deny
    let _ = e.check_subprocess("node"); // allow
    e.record_subprocess_exit();
    let _ = e.check_subprocess("curl"); // deny
    let _ = e.check_network("example.com"); // deny

    let jsonl = e.audit_log().to_jsonl();
    for line in jsonl.lines() {
        let v: serde_json::Value = serde_json::from_str(line).unwrap();

        // Every entry must have category and action.
        assert!(
            v["category"].as_str().is_some(),
            "audit entry must have category"
        );
        assert!(
            v["action"].as_str().is_some(),
            "audit entry must have action"
        );
        assert!(
            v["run_id"].as_str() == Some("audit-test"),
            "run_id must match"
        );
    }
}

#[test]
fn audit_log_violation_entries_have_violation_details() {
    let mut policy = SandboxProfile::Strict.to_policy();
    policy.allow_read_path("/snapshot/**");
    let mut e = SandboxEnforcer::new(policy, "violation-detail-test");

    // Trigger a violation.
    let _ = e.check_read(Path::new("/etc/passwd"));

    let jsonl = e.audit_log().to_jsonl();
    let violation_lines: Vec<&str> = jsonl
        .lines()
        .filter(|l| l.contains("sandbox_violation"))
        .collect();

    assert!(
        !violation_lines.is_empty(),
        "should have at least one violation entry"
    );

    for line in violation_lines {
        let v: serde_json::Value = serde_json::from_str(line).unwrap();
        assert!(
            v["violation"].is_object(),
            "violation entry must have violation object"
        );
        let violation = &v["violation"];
        assert!(
            violation["kind"].as_str().is_some(),
            "violation must have kind"
        );
        assert!(
            violation["message"].as_str().is_some(),
            "violation must have message"
        );
    }
}

#[test]
fn audit_log_all_entries_are_valid_jsonl() {
    let mut e = SandboxEnforcer::from_profile(SandboxProfile::Standard, "jsonl-test");

    // Generate a mix of decisions.
    let _ = e.check_subprocess("node");
    e.record_subprocess_exit();
    let _ = e.check_subprocess("curl"); // deny
    let _ = e.check_network("evil.com"); // deny
    let _ = e.record_bytes_read(100);
    let _ = e.record_file_enumerated();

    let jsonl = e.audit_log().to_jsonl();
    let line_count = jsonl.lines().count();
    assert!(
        line_count >= 4,
        "should have at least 4 log entries, got {line_count}"
    );

    for (i, line) in jsonl.lines().enumerate() {
        let _: serde_json::Value = serde_json::from_str(line)
            .unwrap_or_else(|e| panic!("line {i} is not valid JSON: {e}"));
    }
}

// ══════════════════════════════════════════════════════════════════════════
// 6. Sandbox report integrity
// ══════════════════════════════════════════════════════════════════════════

#[test]
fn sandbox_report_captures_all_counters() {
    let mut policy = SandboxProfile::Standard.to_policy();
    policy.allow_read_path("/project/**");
    let mut e = SandboxEnforcer::new(policy, "report-test");

    let _ = e.record_bytes_read(2048);
    let _ = e.record_file_enumerated();
    let _ = e.record_file_enumerated();
    let _ = e.check_subprocess("node");
    e.record_subprocess_exit();
    let _ = e.record_output_bytes(512);

    let report = e.into_report();
    assert_eq!(report.run_id, "report-test");
    assert_eq!(report.profile, "standard");
    assert_eq!(report.counters["bytes_read"], 2048);
    assert_eq!(report.counters["files_enumerated"], 2);
    assert_eq!(report.counters["subprocesses_spawned"], 1);
    assert_eq!(report.counters["output_bytes"], 512);
}

#[test]
fn sandbox_report_is_json_serializable() {
    let e = SandboxEnforcer::from_profile(SandboxProfile::Strict, "json-test");
    let report = e.into_report();
    let json = serde_json::to_string_pretty(&report).unwrap();
    let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();

    assert_eq!(parsed["run_id"], "json-test");
    assert_eq!(parsed["profile"], "strict");
    assert_eq!(parsed["verdict"], "pass");
    assert!(parsed["counters"].is_object());
    assert!(parsed["wall_time_ms"].is_number());
}

// ══════════════════════════════════════════════════════════════════════════
// 7. Custom policy from JSON
// ══════════════════════════════════════════════════════════════════════════

#[test]
fn custom_policy_from_json_enforces_correctly() {
    let json = serde_json::to_string(&SandboxProfile::Standard.to_policy()).unwrap();
    let mut policy = SandboxPolicy::from_json(&json).unwrap();
    policy.allow_read_path("/custom/**");
    let mut e = SandboxEnforcer::new(policy, "custom-test");

    assert!(e.check_read(Path::new("/custom/file.rs")).is_ok());
    assert!(e.check_read(Path::new("/other/file.rs")).is_err());
}

// ══════════════════════════════════════════════════════════════════════════
// 8. Violation exit codes and error conversion
// ══════════════════════════════════════════════════════════════════════════

#[test]
fn all_violation_kinds_map_to_50_59_range() {
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
            "exit code {code} for {kind:?} not in 50-59"
        );
    }
}

#[test]
fn fs_violations_get_exit_code_50() {
    for kind in [
        ViolationKind::FsReadDenied,
        ViolationKind::FsWriteDenied,
        ViolationKind::FsReadBytesExceeded,
        ViolationKind::FsFileSizeExceeded,
        ViolationKind::FsDepthExceeded,
        ViolationKind::FsFileCountExceeded,
    ] {
        assert_eq!(kind.exit_code(), 50, "{kind:?} should be exit code 50");
    }
}

#[test]
fn network_violations_get_exit_code_51() {
    for kind in [
        ViolationKind::NetworkBlocked,
        ViolationKind::NetworkHostDenied,
        ViolationKind::NetworkConnectionLimit,
    ] {
        assert_eq!(kind.exit_code(), 51, "{kind:?} should be exit code 51");
    }
}

#[test]
fn process_violations_get_exit_code_52() {
    for kind in [
        ViolationKind::ProcessBlocked,
        ViolationKind::ProcessExecutableDenied,
        ViolationKind::ProcessConcurrentLimit,
        ViolationKind::ProcessTotalLimit,
        ViolationKind::ProcessTimeout,
    ] {
        assert_eq!(kind.exit_code(), 52, "{kind:?} should be exit code 52");
    }
}

#[test]
fn resource_violations_get_exit_code_53() {
    for kind in [
        ViolationKind::ResourceWallTimeExceeded,
        ViolationKind::ResourceCpuTimeExceeded,
        ViolationKind::ResourceMemoryExceeded,
        ViolationKind::ResourceFdLimitExceeded,
        ViolationKind::ResourceOutputExceeded,
    ] {
        assert_eq!(kind.exit_code(), 53, "{kind:?} should be exit code 53");
    }
}

// ══════════════════════════════════════════════════════════════════════════
// 9. Redaction determinism across runs
// ══════════════════════════════════════════════════════════════════════════

#[test]
fn redaction_is_deterministic_across_multiple_calls() {
    let content = format!("KEY={SECRET_AWS}\nTOKEN={SECRET_GITHUB}\nDB={SECRET_DB}");

    let results: Vec<_> = (0..5)
        .map(|_| redact_content(&content, ".env", &builtin_patterns(), "det-test"))
        .collect();

    for i in 1..results.len() {
        assert_eq!(
            results[0].content, results[i].content,
            "redacted content must be identical across runs"
        );
        assert_eq!(
            results[0].records.len(),
            results[i].records.len(),
            "record count must be identical"
        );
    }
}

// ══════════════════════════════════════════════════════════════════════════
// 10. False positive controls
// ══════════════════════════════════════════════════════════════════════════

#[test]
fn no_false_positive_on_normal_code() {
    let code = r#"
fn main() {
    let count = 42;
    let name = "hello";
    println!("Hello, {}! Count: {}", name, count);
    let result = compute_something(count);
    assert!(result > 0);
}

fn compute_something(n: i32) -> i32 {
    n * 2 + 1
}
"#;

    let findings = scan_content(code, "main.rs", &builtin_patterns());
    assert!(
        findings.is_empty(),
        "normal Rust code should not trigger secrets: {findings:?}"
    );
}

#[test]
fn no_false_positive_on_package_json() {
    let pkg = r#"{
  "name": "my-app",
  "version": "1.0.0",
  "description": "A web application",
  "main": "index.js",
  "scripts": {
    "start": "node index.js",
    "test": "jest"
  },
  "dependencies": {
    "express": "^4.18.0",
    "react": "^18.2.0"
  }
}"#;

    let findings = scan_content(pkg, "package.json", &builtin_patterns());
    assert!(
        findings.is_empty(),
        "package.json without secrets should not trigger: {findings:?}"
    );
}

#[test]
fn no_false_positive_on_tsconfig() {
    let tsconfig = r#"{
  "compilerOptions": {
    "target": "ES2020",
    "module": "commonjs",
    "strict": true,
    "outDir": "./dist",
    "rootDir": "./src",
    "paths": {
      "@/*": ["./src/*"]
    }
  }
}"#;

    let findings = scan_content(tsconfig, "tsconfig.json", &builtin_patterns());
    assert!(
        findings.is_empty(),
        "tsconfig.json should not trigger: {findings:?}"
    );
}

// ══════════════════════════════════════════════════════════════════════════
// 11. Adversarial ingestion E2E security artifact export
// ══════════════════════════════════════════════════════════════════════════

#[test]
fn adversarial_ingestion_security_audit_export_is_fail_closed_and_redacted() {
    let run_id = env::var("DOCTOR_FRANKENTUI_ADVERSARIAL_INGESTION_E2E_RUN_ID")
        .unwrap_or_else(|_| "adversarial-ingestion-security-unit".to_string());
    let run_root = env::var_os("DOCTOR_FRANKENTUI_ADVERSARIAL_INGESTION_E2E_RUN_ROOT")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            env::temp_dir().join("doctor_frankentui/adversarial_ingestion_security_unit")
        });
    let meta_dir = run_root.join("meta");
    fs::create_dir_all(&meta_dir).expect("create adversarial ingestion meta dir");

    let security_audit_jsonl = meta_dir.join("security_audit.jsonl");
    let security_report_json = meta_dir.join("adversarial_security_report.json");
    let replay_command = format!(
        "DOCTOR_FRANKENTUI_ADVERSARIAL_INGESTION_E2E_RUN_ID={run_id} \
         cargo test -p doctor_frankentui --test sandbox_redaction_tests \
         adversarial_ingestion_security_audit_export_is_fail_closed_and_redacted -- --nocapture"
    );

    let mut policy = SandboxProfile::Strict.to_policy();
    policy.allow_read_path("/snapshot/**");
    let mut enforcer = SandboxEnforcer::new(policy, &run_id);
    let mut rows = Vec::new();

    push_blocked_decision(
        &mut rows,
        &run_id,
        BlockedDecisionCase {
            fixture_id: "path-traversal-env",
            fixture_kind: "path_traversal",
            operation: "fs_read",
            expected_exit_code: 50,
        },
        enforcer.check_read(Path::new("/snapshot/../.env")),
        &replay_command,
    );
    push_blocked_decision(
        &mut rows,
        &run_id,
        BlockedDecisionCase {
            fixture_id: "sensitive-token-file",
            fixture_kind: "secret_file",
            operation: "fs_read",
            expected_exit_code: 50,
        },
        enforcer.check_read(Path::new("/snapshot/config/token.json")),
        &replay_command,
    );
    push_blocked_decision(
        &mut rows,
        &run_id,
        BlockedDecisionCase {
            fixture_id: "malicious-subprocess",
            fixture_kind: "malicious_script",
            operation: "subprocess",
            expected_exit_code: 52,
        },
        enforcer.check_subprocess("node"),
        &replay_command,
    );
    push_blocked_decision(
        &mut rows,
        &run_id,
        BlockedDecisionCase {
            fixture_id: "blocked-network",
            fixture_kind: "network_exfiltration",
            operation: "network",
            expected_exit_code: 51,
        },
        enforcer.check_network("evil.example"),
        &replay_command,
    );
    push_blocked_decision(
        &mut rows,
        &run_id,
        BlockedDecisionCase {
            fixture_id: "oversized-payload",
            fixture_kind: "oversized_payload",
            operation: "fs_read_bytes",
            expected_exit_code: 50,
        },
        enforcer.record_bytes_read(256 * 1024 * 1024 + 1),
        &replay_command,
    );

    let secret_probe = format!(
        "GITHUB_TOKEN={SECRET_GITHUB}\nAWS_ACCESS_KEY_ID={SECRET_AWS}\nDATABASE_URL={SECRET_DB}"
    );
    let redacted = redact_content(
        &secret_probe,
        "fixtures/secret-leak.env",
        &builtin_patterns(),
        &run_id,
    );
    assert!(!redacted.content.contains(SECRET_GITHUB));
    assert!(!redacted.content.contains(SECRET_AWS));
    assert!(!redacted.content.contains("supersecret"));
    rows.push(json!({
        "schema_version": "doctor-adversarial-ingestion-security-e2e-v1",
        "run_id": run_id,
        "fixture_id": "secret-leak-probe",
        "fixture_kind": "secret_leak",
        "operation": "redaction",
        "status": "redacted",
        "policy_decision": "redact",
        "blocked": false,
        "exit_code": 0,
        "redaction_count": redacted.records.len(),
        "replay_id": "replay-secret-leak-probe",
        "replay_command": replay_command,
        "artifact_path": "fixtures/secret-leak.env",
        "redacted_preview": redacted.content,
    }));

    let audit_text = rows
        .iter()
        .map(|row| serde_json::to_string(row).expect("serialize audit row"))
        .collect::<Vec<_>>()
        .join("\n")
        + "\n";
    fs::write(&security_audit_jsonl, &audit_text).expect("write security audit jsonl");

    let blocked_operations = rows
        .iter()
        .filter(|row| row.get("status").and_then(Value::as_str) == Some("blocked"))
        .count();
    let report = json!({
        "schema_version": "doctor-adversarial-ingestion-security-report-v1",
        "status": "passed",
        "run_id": run_id,
        "blocked_operations": blocked_operations,
        "redacted_secret_probes": 1,
        "security_audit_jsonl": security_audit_jsonl.to_string_lossy(),
        "replay_ids": rows.iter().filter_map(|row| row.get("replay_id").and_then(Value::as_str)).collect::<Vec<_>>(),
    });
    let report_text =
        serde_json::to_string_pretty(&report).expect("serialize security report") + "\n";
    fs::write(&security_report_json, &report_text).expect("write security report");

    let combined = format!("{audit_text}\n{report_text}");
    assert!(!combined.contains(SECRET_GITHUB));
    assert!(!combined.contains(SECRET_AWS));
    assert!(!combined.contains("supersecret"));
    assert_eq!(blocked_operations, 5);
    assert_eq!(
        enforcer
            .audit_log()
            .entries()
            .iter()
            .filter(|entry| entry.action == "deny")
            .count(),
        5
    );
}

struct BlockedDecisionCase<'a> {
    fixture_id: &'a str,
    fixture_kind: &'a str,
    operation: &'a str,
    expected_exit_code: i32,
}

fn push_blocked_decision(
    rows: &mut Vec<Value>,
    run_id: &str,
    case: BlockedDecisionCase<'_>,
    decision: std::result::Result<(), Box<SandboxViolation>>,
    replay_command: &str,
) {
    let violation = decision.expect_err("adversarial fixture must fail closed");
    assert_eq!(violation.kind.exit_code(), case.expected_exit_code);
    rows.push(json!({
        "schema_version": "doctor-adversarial-ingestion-security-e2e-v1",
        "run_id": run_id,
        "fixture_id": case.fixture_id,
        "fixture_kind": case.fixture_kind,
        "operation": case.operation,
        "status": "blocked",
        "policy_decision": "deny",
        "blocked": true,
        "exit_code": violation.kind.exit_code(),
        "violation_kind": format!("{:?}", violation.kind),
        "violation_message": violation.message,
        "replay_id": format!("replay-{}", case.fixture_id),
        "replay_command": replay_command,
    }));
}
