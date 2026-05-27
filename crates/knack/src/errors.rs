//! Typed CLI errors with stable exit codes.
//!
//! Track E plan §E6 fixes the exit-code table; this module owns it. Anything
//! the CLI surfaces as a non-zero exit must map to one of these variants —
//! ad-hoc `anyhow!`s are converted to [`CliError::Internal`] (exit 70) at the
//! main boundary.

use thiserror::Error;

/// Stable exit codes — documented in `knack docs exit-codes`. Never re-number.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ExitCode(pub i32);

impl ExitCode {
    pub const SUCCESS: ExitCode = ExitCode(0);
    pub const USER: ExitCode = ExitCode(1);
    pub const AUTH: ExitCode = ExitCode(2);
    pub const NETWORK: ExitCode = ExitCode(3);
    pub const CONFLICT: ExitCode = ExitCode(4);
    pub const PLAN: ExitCode = ExitCode(5);
    pub const USAGE: ExitCode = ExitCode(64);
    pub const INTERNAL: ExitCode = ExitCode(70);
}

/// All structured errors the CLI surfaces.
#[derive(Debug, Error)]
pub enum CliError {
    #[error("{message}")]
    User {
        code: String,
        message: String,
        hint: Option<String>,
    },

    #[error("not signed in — run `knack auth login`")]
    AuthRequired,

    #[error("authentication failed: {0}")]
    AuthFailed(String),

    #[error("network error: {0}")]
    Network(String),

    #[error("conflict: {message}")]
    Conflict {
        message: String,
        hint: Option<String>,
    },

    #[error("plan limit hit: {message}")]
    PlanLimit {
        message: String,
        hint: Option<String>,
    },

    #[error("not found: {0}")]
    NotFound(String),

    #[error("server error ({status}): {message}")]
    Server {
        status: u16,
        code: String,
        message: String,
    },

    #[error("{0}")]
    Internal(String),
}

impl CliError {
    /// Stable error code surfaced in `--json` envelopes.
    pub fn code(&self) -> &'static str {
        match self {
            CliError::User { .. } => "USER_ERROR",
            CliError::AuthRequired => "AUTH_REQUIRED",
            CliError::AuthFailed(_) => "AUTH_FAILED",
            CliError::Network(_) => "NETWORK",
            CliError::Conflict { .. } => "CONFLICT",
            CliError::PlanLimit { .. } => "PLAN_LIMIT_EXCEEDED",
            CliError::NotFound(_) => "NOT_FOUND",
            CliError::Server { .. } => "SERVER",
            CliError::Internal(_) => "INTERNAL",
        }
    }

    pub fn exit_code(&self) -> ExitCode {
        match self {
            CliError::User { .. } | CliError::NotFound(_) => ExitCode::USER,
            CliError::AuthRequired | CliError::AuthFailed(_) => ExitCode::AUTH,
            CliError::Network(_) => ExitCode::NETWORK,
            CliError::Conflict { .. } => ExitCode::CONFLICT,
            CliError::PlanLimit { .. } => ExitCode::PLAN,
            CliError::Server { .. } | CliError::Internal(_) => ExitCode::INTERNAL,
        }
    }

    pub fn hint(&self) -> Option<&str> {
        match self {
            CliError::User { hint, .. }
            | CliError::Conflict { hint, .. }
            | CliError::PlanLimit { hint, .. } => hint.as_deref(),
            CliError::AuthRequired => Some(
                "your knack session expired. run `knack auth login` to sign in again \
                 (about 30 seconds in the browser)",
            ),
            _ => None,
        }
    }
}

impl From<reqwest::Error> for CliError {
    fn from(e: reqwest::Error) -> Self {
        CliError::Network(e.to_string())
    }
}

impl From<serde_json::Error> for CliError {
    fn from(e: serde_json::Error) -> Self {
        CliError::Internal(format!("json parse: {e}"))
    }
}

impl From<std::io::Error> for CliError {
    fn from(e: std::io::Error) -> Self {
        CliError::Internal(format!("io: {e}"))
    }
}

impl From<keyring::Error> for CliError {
    fn from(e: keyring::Error) -> Self {
        CliError::Internal(format!("keyring: {e}"))
    }
}

pub type CliResult<T> = Result<T, CliError>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exit_codes_are_stable() {
        // These numbers are documented in `knack docs exit-codes` — never change.
        assert_eq!(ExitCode::SUCCESS.0, 0);
        assert_eq!(ExitCode::USER.0, 1);
        assert_eq!(ExitCode::AUTH.0, 2);
        assert_eq!(ExitCode::NETWORK.0, 3);
        assert_eq!(ExitCode::CONFLICT.0, 4);
        assert_eq!(ExitCode::PLAN.0, 5);
        assert_eq!(ExitCode::USAGE.0, 64);
        assert_eq!(ExitCode::INTERNAL.0, 70);
    }

    #[test]
    fn error_kind_to_exit_code() {
        assert_eq!(CliError::AuthRequired.exit_code(), ExitCode::AUTH);
        assert_eq!(
            CliError::Conflict {
                message: "x".into(),
                hint: None
            }
            .exit_code(),
            ExitCode::CONFLICT
        );
        assert_eq!(
            CliError::PlanLimit {
                message: "x".into(),
                hint: None
            }
            .exit_code(),
            ExitCode::PLAN,
        );
        assert_eq!(CliError::Network("x".into()).exit_code(), ExitCode::NETWORK);
        assert_eq!(CliError::NotFound("x".into()).exit_code(), ExitCode::USER);
    }

    #[test]
    fn auth_required_has_hint() {
        assert!(CliError::AuthRequired.hint().is_some());
    }
}
