use async_trait::async_trait;
use thiserror::Error;

use crate::{PublishReceipt, RunLog, SkillPackage, SkillSummary};

#[derive(Debug, Error)]
pub enum BackendError {
    #[error("not found: {0}")]
    NotFound(String),
    #[error("auth: {0}")]
    Auth(String),
    #[error("network: {0}")]
    Network(String),
    #[error("conflict: {0}")]
    Conflict(String),
    #[error("invalid: {0}")]
    Invalid(String),
    #[error("backend: {0}")]
    Other(String),
}

pub type BackendResult<T> = Result<T, BackendError>;

#[async_trait]
pub trait Backend: Send + Sync {
    async fn pull(&self, slug: &str, version: Option<&str>) -> BackendResult<SkillPackage>;

    async fn publish(&self, package: SkillPackage) -> BackendResult<PublishReceipt>;

    async fn list(&self) -> BackendResult<Vec<SkillSummary>>;

    async fn search(&self, query: &str) -> BackendResult<Vec<SkillSummary>>;

    async fn record_run(&self, log: RunLog) -> BackendResult<()>;
}
