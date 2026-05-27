//! GitHub-backed Backend implementation.
//!
//! Stores skills, versions, and run logs in a user-owned GitHub repository.
//! Versions are per-skill git tags (e.g. `email-triage/v0.3.0`). Run logs are
//! appended to monthly JSONL files and batch-pushed.

mod auth;
mod backend;
mod bootstrap;
mod external;
pub mod runs;

pub use auth::{resolve_token, GithubAuth};
pub use backend::GithubBackend;
pub use bootstrap::{bootstrap_repo, BootstrapOpts, BootstrapResult, Visibility};
pub use external::{parse_spec, pull_external, ExternalSpec};
pub use runs::{find_run, mark_run, start_run, RunSnapshot};
