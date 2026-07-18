//! GitHub-backed Backend implementation.
//!
//! Stores skills, versions, and run logs in a user-owned GitHub repository.
//! Versions are per-skill git tags (e.g. `email-triage/v0.3.0`). Run logs are
//! appended to monthly JSONL files and batch-pushed.

pub mod aggregate;
mod auth;
mod backend;
mod bootstrap;
mod external;
pub mod runs;

pub use aggregate::{
    build_bucket, build_overview, detect_regression, group_buckets, group_time_buckets,
    scan_snapshots, NoteCount, RegressionInfo, SkillOverview, StatsBucket, TrendInterval,
    TrendPoint,
};
pub use auth::{resolve_token, GithubAuth};
pub use backend::{latest_published_version, GithubBackend};
pub use bootstrap::{bootstrap_repo, BootstrapOpts, BootstrapResult, Visibility};
pub use external::{parse_spec, pull_external, ExternalSpec};
pub use git::{resolve_remote, RemoteTarget};
pub use runs::{
    find_run, mark_run, pending_events, push_pending, start_run, telemetry_batch_threshold,
    RunSnapshot, DEFAULT_LOOKBACK_DAYS,
};
pub use workspace::{read_workspace_auto_push, PushPolicy};

mod git;
mod workspace;
