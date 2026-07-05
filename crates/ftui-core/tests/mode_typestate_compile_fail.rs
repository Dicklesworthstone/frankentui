//! Compile-fail tests for terminal mode typestate transitions (bd-3rrzt.3).
//!
//! Uses JSON diagnostics from `cargo check` to verify that invalid mode
//! transitions produce compile-time errors, enforcing the typestate safety
//! guarantees without depending on rustc's line-wrapping details.

use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::Value;

#[derive(Debug, Clone, Copy)]
struct CompileFailCase {
    file: &'static str,
    code: &'static str,
    message_substring: &'static str,
}

const CASES: &[CompileFailCase] = &[
    CompileFailCase {
        file: "alt_cannot_exit_raw.rs",
        code: "E0599",
        message_substring: "no method named `exit_raw` found",
    },
    CompileFailCase {
        file: "cooked_cannot_enable_mouse.rs",
        code: "E0599",
        message_substring: "no method named `enable_mouse` found",
    },
    CompileFailCase {
        file: "cooked_cannot_enable_paste.rs",
        code: "E0599",
        message_substring: "no method named `enable_bracketed_paste` found",
    },
    CompileFailCase {
        file: "cooked_cannot_enter_alt.rs",
        code: "E0599",
        message_substring: "no method named `enter_alt_screen` found",
    },
    CompileFailCase {
        file: "cooked_cannot_exit_raw.rs",
        code: "E0599",
        message_substring: "no method named `exit_raw` found",
    },
    CompileFailCase {
        file: "cooked_cannot_teardown.rs",
        code: "E0599",
        message_substring: "no method named `teardown` found",
    },
    CompileFailCase {
        file: "raw_cannot_enable_focus.rs",
        code: "E0599",
        message_substring: "no method named `enable_focus_events` found",
    },
    CompileFailCase {
        file: "raw_cannot_enable_mouse.rs",
        code: "E0599",
        message_substring: "no method named `enable_mouse` found",
    },
    CompileFailCase {
        file: "raw_cannot_enable_paste.rs",
        code: "E0599",
        message_substring: "no method named `enable_bracketed_paste` found",
    },
    CompileFailCase {
        file: "raw_cannot_enter_raw.rs",
        code: "E0599",
        message_substring: "no method named `enter_raw` found",
    },
];

#[test]
fn compile_fail_invalid_transitions() {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let cases_dir = manifest_dir.join("tests/compile_fail");
    let shared_target_dir = manifest_dir.join("target/compile_fail_json");

    for case in CASES {
        assert_compile_fail_contains(&manifest_dir, &cases_dir, &shared_target_dir, *case);
    }
}

fn assert_compile_fail_contains(
    manifest_dir: &Path,
    cases_dir: &Path,
    shared_target_dir: &Path,
    case: CompileFailCase,
) {
    let crate_root = tempfile::tempdir().expect("temporary compile-fail crate");
    fs::create_dir_all(crate_root.path().join("src")).expect("create temp crate src");

    let source = fs::read_to_string(cases_dir.join(case.file)).expect("read compile-fail case");
    fs::write(crate_root.path().join("src/main.rs"), source).expect("write compile-fail main");
    fs::write(
        crate_root.path().join("Cargo.toml"),
        cargo_toml(manifest_dir, case.file),
    )
    .expect("write temp Cargo.toml");

    let output = Command::new(cargo_bin())
        .current_dir(crate_root.path())
        .env("CARGO_TARGET_DIR", shared_target_dir)
        .arg("check")
        .arg("--quiet")
        .arg("--color=never")
        .arg("--message-format=json")
        .output()
        .expect("run cargo check for compile-fail case");

    assert!(
        !output.status.success(),
        "compile-fail case {file} unexpectedly compiled successfully",
        file = case.file
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    let diagnostics = compiler_errors(&stdout);
    let matched = diagnostics.iter().any(|diagnostic| {
        diagnostic
            .pointer("/message/code/code")
            .and_then(Value::as_str)
            == Some(case.code)
            && diagnostic
                .pointer("/message/message")
                .and_then(Value::as_str)
                .is_some_and(|message| message.contains(case.message_substring))
    });

    assert!(
        matched,
        "compile-fail case {file} did not emit {code} containing {needle:?}\nstdout:\n{stdout}\nstderr:\n{stderr}",
        file = case.file,
        code = case.code,
        needle = case.message_substring,
        stdout = stdout,
        stderr = String::from_utf8_lossy(&output.stderr),
    );
}

fn compiler_errors(stdout: &str) -> Vec<Value> {
    stdout
        .lines()
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .filter(|value| value.get("reason").and_then(Value::as_str) == Some("compiler-message"))
        .filter(|value| value.pointer("/message/level").and_then(Value::as_str) == Some("error"))
        .collect()
}

fn cargo_toml(manifest_dir: &Path, file_name: &str) -> String {
    let crate_name = format!(
        "ftui-core-compile-fail-{}",
        file_name.trim_end_matches(".rs").replace('_', "-")
    );
    // The empty `[workspace]` table keeps the synthesized crate standalone
    // even when the tempdir lands inside the frankentui workspace tree (e.g.
    // remote builders that point TMPDIR at a directory under the repo root) —
    // without it cargo refuses with "believes it's in a workspace".
    format!(
        "[package]\nname = \"{crate_name}\"\nversion = \"0.0.0\"\nedition = \"2024\"\n\n[workspace]\n\n[dependencies]\nftui-core = {{ path = \"{path}\" }}\n",
        crate_name = crate_name,
        path = toml_escape_path(manifest_dir),
    )
}

fn toml_escape_path(path: &Path) -> String {
    path.display()
        .to_string()
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
}

fn cargo_bin() -> OsString {
    std::env::var_os("CARGO").unwrap_or_else(|| OsString::from("cargo"))
}
