//! Shared helpers for integration tests.
//!
//! Each test in `tests/*.rs` becomes its own crate; they all `mod common;`
//! to pull this module in.

#![allow(dead_code)] // not every test exercises every helper

use std::sync::Arc;

use knack_cli::api::ApiClient;
use knack_cli::auth_store::{MemoryStore, StoredCredential, TokenStore};
use knack_cli::config::Config;
use wiremock::MockServer;

pub const FAKE_ACCESS_TOKEN: &str = "test-access-token";
pub const FAKE_REFRESH_TOKEN: &str = "test-refresh-token";
pub const FAKE_PAT: &str = "knack_pat_aBcDeF1234567890ghijklmnopqrstuvwxyz_abc";

fn jwt_credential() -> StoredCredential {
    StoredCredential {
        token: FAKE_ACCESS_TOKEN.into(),
        token_id: None,
        prefix: None,
        refresh_token: Some(FAKE_REFRESH_TOKEN.into()),
        expires_at: Some(chrono::Utc::now().timestamp() + 3600),
        label: None,
        user_id: None,
        email: None,
    }
}

fn pat_credential() -> StoredCredential {
    StoredCredential {
        token: FAKE_PAT.into(),
        token_id: Some("tok_test".into()),
        prefix: Some("knack_pat_aBcDeF".into()),
        refresh_token: None,
        expires_at: None,
        label: Some("knack-cli@test".into()),
        user_id: Some("u_test".into()),
        email: Some("test@example.com".into()),
    }
}

/// Spin up a wiremock server + an ApiClient pointed at it, with a memory
/// token store pre-seeded with a JWT credential so the refresh-on-401
/// retry path stays exercised by tests that drive that code.
pub async fn fixture() -> (MockServer, ApiClient, Arc<MemoryStore>) {
    let server = MockServer::start().await;
    let store = Arc::new(MemoryStore::new());
    store
        .save("default", &jwt_credential())
        .expect("seed memory store");

    let mut config = Config::load();
    config.api_base = server.uri();

    let client = ApiClient::new(
        config,
        store.clone() as Arc<dyn TokenStore + Send + Sync>,
        "default",
    );
    (server, client, store)
}

/// Like [`fixture`] but pre-seeded with a PAT-shaped credential — for
/// tests that need to assert PAT-specific behavior (no refresh retry on
/// 401, status displays the prefix, etc.).
pub async fn fixture_pat() -> (MockServer, ApiClient, Arc<MemoryStore>) {
    let server = MockServer::start().await;
    let store = Arc::new(MemoryStore::new());
    store
        .save("default", &pat_credential())
        .expect("seed memory store");

    let mut config = Config::load();
    config.api_base = server.uri();

    let client = ApiClient::new(
        config,
        store.clone() as Arc<dyn TokenStore + Send + Sync>,
        "default",
    );
    (server, client, store)
}

/// Same as [`fixture`] but with no credentials preloaded — used by tests
/// that want to drive the device-flow login from scratch.
pub async fn fixture_unauth() -> (MockServer, ApiClient) {
    let server = MockServer::start().await;
    let store = Arc::new(MemoryStore::new()) as Arc<dyn TokenStore + Send + Sync>;
    let mut config = Config::load();
    config.api_base = server.uri();
    let client = ApiClient::new(config, store, "default");
    (server, client)
}
