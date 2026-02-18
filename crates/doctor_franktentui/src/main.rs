#![forbid(unsafe_code)]

fn main() {
    let integration = doctor_franktentui::util::OutputIntegration::detect();
    if let Err(error) = doctor_franktentui::run_from_env() {
        if integration.should_emit_json() {
            eprintln!(
                "{}",
                serde_json::json!({
                    "status": "error",
                    "error": error.to_string(),
                    "exit_code": error.exit_code(),
                    "integration": integration,
                })
            );
        } else {
            eprintln!("{error}");
        }
        std::process::exit(error.exit_code());
    }
}
