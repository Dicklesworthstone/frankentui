use std::path::PathBuf;

use thiserror::Error;

pub type Result<T> = std::result::Result<T, DoctorError>;

#[derive(Debug, Error)]
pub enum DoctorError {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),

    #[error("HTTP error: {0}")]
    Http(#[from] reqwest::Error),

    #[error("missing dependency command: {command}")]
    MissingCommand { command: String },

    #[error("profile not found: {name}")]
    ProfileNotFound { name: String },

    #[error("invalid argument: {message}")]
    InvalidArgument { message: String },

    #[error("required path does not exist: {path}")]
    MissingPath { path: PathBuf },

    #[error("path is not executable: {path}")]
    NotExecutable { path: PathBuf },

    #[error("external command failed: {command} (exit={exit_code})")]
    ExternalCommandFailed { command: String, exit_code: i32 },

    #[error("external command timed out: {command} ({seconds}s)")]
    ExternalCommandTimedOut { command: String, seconds: u64 },

    #[error("{message}")]
    Exit { code: i32, message: String },
}

impl DoctorError {
    #[must_use]
    pub fn exit_code(&self) -> i32 {
        match self {
            Self::Exit { code, .. } => *code,
            Self::ExternalCommandFailed { exit_code, .. } => *exit_code,
            _ => 1,
        }
    }

    #[must_use]
    pub fn exit(code: i32, message: impl Into<String>) -> Self {
        Self::Exit {
            code,
            message: message.into(),
        }
    }

    #[must_use]
    pub fn invalid(message: impl Into<String>) -> Self {
        Self::InvalidArgument {
            message: message.into(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::DoctorError;

    #[test]
    fn exit_constructor_preserves_code_and_message() {
        let error = DoctorError::exit(42, "boom");
        assert_eq!(error.exit_code(), 42);
        assert_eq!(error.to_string(), "boom");
    }

    #[test]
    fn external_command_failed_exit_code_is_propagated() {
        let error = DoctorError::ExternalCommandFailed {
            command: "vhs".to_string(),
            exit_code: 17,
        };
        assert_eq!(error.exit_code(), 17);
    }
}
