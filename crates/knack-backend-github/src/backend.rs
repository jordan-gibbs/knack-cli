use async_trait::async_trait;
use knack_types::{Backend, BackendError, BackendResult, PublishReceipt, RunLog, SkillPackage, SkillSummary};
use std::path::PathBuf;

use crate::runs::append_run;

/// GitHub-backed Backend. Reads and writes a user-owned repo on disk that
/// mirrors a remote GitHub repository.
#[derive(Debug, Clone)]
pub struct GithubBackend {
    pub owner: String,
    pub repo: String,
    pub local_path: PathBuf,
}

impl GithubBackend {
    pub fn new(owner: impl Into<String>, repo: impl Into<String>, local_path: PathBuf) -> Self {
        Self {
            owner: owner.into(),
            repo: repo.into(),
            local_path,
        }
    }
}

#[async_trait]
impl Backend for GithubBackend {
    async fn pull(&self, _slug: &str, _version: Option<&str>) -> BackendResult<SkillPackage> {
        Err(BackendError::Other(
            "github pull: not implemented (Phase 2 stub)".into(),
        ))
    }

    async fn publish(&self, _package: SkillPackage) -> BackendResult<PublishReceipt> {
        Err(BackendError::Other(
            "github publish: not implemented (Phase 2 stub)".into(),
        ))
    }

    async fn list(&self) -> BackendResult<Vec<SkillSummary>> {
        Ok(Vec::new())
    }

    async fn search(&self, _query: &str) -> BackendResult<Vec<SkillSummary>> {
        Ok(Vec::new())
    }

    async fn record_run(&self, log: RunLog) -> BackendResult<()> {
        append_run(&self.local_path, &log)
            .map_err(|e| BackendError::Other(format!("write run log: {e}")))
    }
}
