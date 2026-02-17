#![forbid(unsafe_code)]

fn main() {
    if let Err(error) = doctor_franktentui::run_from_env() {
        eprintln!("{error}");
        std::process::exit(error.exit_code());
    }
}
