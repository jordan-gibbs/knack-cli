//! Cloud-backed [`Backend`] implementation.
//!
//! Wraps the existing [`ApiClient`] so cloud-mode callers can talk to the
//! Knack cloud through the trait. The full method bodies will be filled in
//! as we migrate commands off direct `ApiClient` calls; for now the trait
//! impl exists so [`crate::config::BackendMode::Cloud`] is usable.

use async_trait::async_trait;
use knack_types::{
    Backend, BackendError, BackendResult, PublishReceipt, RunLog, SkillPackage, SkillSummary,
};
use std::sync::Arc;

use crate::api::ApiClient;

#[derive(Clone)]
pub struct CloudBackend {
    pub client: Arc<ApiClient>,
}

impl CloudBackend {
    pub fn new(client: Arc<ApiClient>) -> Self {
        Self { client }
    }
}

#[async_trait]
impl Backend for CloudBackend {
    async fn pull(&self, _slug: &str, _version: Option<&str>) -> BackendResult<SkillPackage> {
        Err(BackendError::Other(
            "cloud pull through Backend trait: not wired yet (Phase 2 stub)".into(),
        ))
    }

    async fn publish(&self, _package: SkillPackage) -> BackendResult<PublishReceipt> {
        Err(BackendError::Other(
            "cloud publish through Backend trait: not wired yet (Phase 2 stub)".into(),
        ))
    }

    async fn list(&self) -> BackendResult<Vec<SkillSummary>> {
        Err(BackendError::Other(
            "cloud list through Backend trait: not wired yet (Phase 2 stub)".into(),
        ))
    }

    async fn search(&self, _query: &str) -> BackendResult<Vec<SkillSummary>> {
        Err(BackendError::Other(
            "cloud search through Backend trait: not wired yet (Phase 2 stub)".into(),
        ))
    }

    async fn record_run(&self, _log: RunLog) -> BackendResult<()> {
        Err(BackendError::Other(
            "cloud record_run through Backend trait: not wired yet (Phase 2 stub)".into(),
        ))
    }
}
