//! Marketplace + ratings API wrappers (Track E).
//!
//! `GET /marketplace/skills?q=...` for full-text search (backed by the
//! server's tsvector + GIN index — scales fine), and the rating endpoints
//! living on the skills router.

use reqwest::Method;
use serde::{Deserialize, Serialize};

use crate::api::{ApiClient, Page};
use crate::errors::CliError;

#[derive(Debug, Clone, Deserialize)]
pub struct MarketplaceAuthor {
    pub username: String,
    #[serde(default)]
    pub display_name: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct MarketplaceCard {
    pub id: String,
    pub slug: String,
    pub name: String,
    pub description: String,
    pub author: MarketplaceAuthor,
    #[serde(default)]
    pub current_version_semver: Option<String>,
    #[serde(default)]
    pub avg_stars: Option<f64>,
    #[serde(default)]
    pub ratings_count: u64,
    #[serde(default)]
    pub runs_count: u64,
    #[serde(default)]
    pub downloads_count: u64,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RatingsSummary {
    #[serde(default)]
    pub avg_stars: Option<f64>,
    #[serde(default)]
    pub count: u64,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RatingRead {
    pub stars: u8,
    #[serde(default)]
    pub review: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RatingUpsertResponse {
    pub rating: RatingRead,
    pub summary: RatingsSummary,
}

#[derive(Debug, Serialize)]
struct RatingCreateBody<'a> {
    stars: u8,
    #[serde(skip_serializing_if = "Option::is_none")]
    review: Option<&'a str>,
}

pub async fn search(
    client: &ApiClient,
    q: Option<&str>,
    sort: &str,
    limit: u32,
) -> Result<Page<MarketplaceCard>, CliError> {
    let q = q.map(str::to_string);
    let sort = sort.to_string();
    client
        .send_json::<Page<MarketplaceCard>>(|c| {
            let mut rb = c.request(Method::GET, "/marketplace/skills")?;
            rb = rb.query(&[("sort", sort.as_str()), ("limit", &limit.to_string())]);
            if let Some(q) = &q {
                rb = rb.query(&[("q", q.as_str())]);
            }
            Ok(rb)
        })
        .await
}

pub async fn rate(
    client: &ApiClient,
    skill_id: &str,
    stars: u8,
    review: Option<&str>,
) -> Result<RatingUpsertResponse, CliError> {
    let path = format!("/skills/{skill_id}/ratings");
    let body = serde_json::to_value(RatingCreateBody { stars, review })?;
    client
        .send_json::<RatingUpsertResponse>(|c| Ok(c.request(Method::POST, &path)?.json(&body)))
        .await
}

pub async fn clear_rating(client: &ApiClient, skill_id: &str) -> Result<(), CliError> {
    let path = format!("/skills/{skill_id}/ratings/mine");
    client
        .send_empty(|c| c.request(Method::DELETE, &path))
        .await
}
