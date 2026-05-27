//! Skills API — list, create, get, version CRUD.

use chrono::{DateTime, Utc};
use reqwest::Method;
use serde::{Deserialize, Serialize};

use crate::api::{ApiClient, Page};
use crate::errors::CliError;

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Skill {
    pub id: String,
    pub slug: String,
    pub name: String,
    #[serde(default)]
    pub description: String,
    pub scope: String,
    pub owner_user_id: Option<String>,
    pub owner_team_id: Option<String>,
    #[serde(default)]
    pub owner_username: Option<String>,
    pub current_version_id: Option<String>,
    pub current_version_semver: Option<String>,
    #[serde(default)]
    pub published_at: Option<DateTime<Utc>>,
    /// Per-owner organizational folder. ``None`` when the skill is
    /// unfiled (or when the server is an older deploy that doesn't yet
    /// emit the field — ``#[serde(default)]`` keeps the CLI forward-
    /// compatible).
    #[serde(default)]
    pub folder_id: Option<String>,
    #[serde(default)]
    pub folder_name: Option<String>,
    /// Internal fork lineage. Set on rows created via
    /// ``POST /skills/{id}/fork``. ``forked_from_skill_id`` is the raw
    /// FK; ``forked_from`` carries a hydrated ``(id, slug,
    /// author_username)`` triple populated by single-skill GETs and
    /// the fork response (list endpoints leave it ``None`` to avoid an
    /// N+1 JOIN).
    #[serde(default)]
    pub forked_from_skill_id: Option<String>,
    #[serde(default)]
    pub forked_from: Option<SkillForkedFrom>,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct SkillForkedFrom {
    pub id: String,
    pub slug: String,
    pub author_username: Option<String>,
}

/// Response shape of `GET /skills/resolve?author=<u>&slug=<s>`.
#[derive(Debug, Clone, Deserialize)]
pub struct SkillResolve {
    pub skill_id: String,
    pub owner_username: String,
    pub slug: String,
    pub current_version_id: Option<String>,
    pub current_version_semver: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct SkillVersion {
    pub id: String,
    pub skill_id: String,
    pub version: String,
    pub skill_md: String,
    pub intuition_md: String,
    pub meta_yaml: String,
    pub parent_version_id: Option<String>,
    pub created_by: Option<String>,
    pub created_at: DateTime<Utc>,
    #[serde(default)]
    pub artifact_ids: Vec<String>,
    /// V2a: when present, the canonical R2 key for this version's packed
    /// tarball. Use [`bundle_download`] to get a presigned GET URL.
    #[serde(default)]
    pub packed_s3_key: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct BundlePresignResponse {
    pub s3_key: String,
    pub upload_url: String,
    pub expires_in: u64,
}

#[derive(Debug, Clone, Deserialize)]
pub struct BundleDownloadResponse {
    pub url: String,
    pub expires_in: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct SkillCreate {
    pub slug: String,
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scope: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub owner_team_id: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct SkillVersionCreate {
    pub version: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub skill_md: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub intuition_md: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub meta_yaml: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent_version_id: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub artifact_ids: Vec<String>,
    /// V2a: draft S3 key returned by [`presign_bundle`]. When present the
    /// server downloads the tarball, derives skill_md / intuition_md /
    /// meta_yaml from the unpacked contents, and copies the object to its
    /// canonical key. Omit when publishing legacy text-only versions.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub packed_s3_key: Option<String>,
}

pub async fn list(
    client: &ApiClient,
    scope: Option<&str>,
    cursor: Option<&str>,
    limit: u32,
) -> Result<Page<Skill>, CliError> {
    let scope = scope.map(str::to_string);
    let cursor = cursor.map(str::to_string);
    client
        .send_json::<Page<Skill>>(|c| {
            let mut rb = c.request(Method::GET, "/skills")?;
            rb = rb.query(&[("limit", limit.to_string())]);
            if let Some(s) = &scope {
                rb = rb.query(&[("scope", s)]);
            }
            if let Some(cur) = &cursor {
                rb = rb.query(&[("cursor", cur)]);
            }
            Ok(rb)
        })
        .await
}

/// Like [`list`] but with folder filtering. Pass ``folder_id`` to filter
/// to one folder; pass ``unfiled = true`` to show only skills with no
/// folder. Mutually exclusive — when both are set, ``folder_id`` wins.
pub async fn list_with_folder(
    client: &ApiClient,
    scope: Option<&str>,
    folder_id: Option<&str>,
    unfiled: bool,
    limit: u32,
) -> Result<Page<Skill>, CliError> {
    let scope = scope.map(str::to_string);
    let folder_id = folder_id.map(str::to_string);
    client
        .send_json::<Page<Skill>>(|c| {
            let mut rb = c.request(Method::GET, "/skills")?;
            rb = rb.query(&[("limit", limit.to_string())]);
            if let Some(s) = &scope {
                rb = rb.query(&[("scope", s)]);
            }
            if let Some(fid) = &folder_id {
                rb = rb.query(&[("folder_id", fid)]);
            } else if unfiled {
                rb = rb.query(&[("folder", "unfiled")]);
            }
            Ok(rb)
        })
        .await
}

pub async fn get(client: &ApiClient, skill_id: &str) -> Result<Skill, CliError> {
    let path = format!("/skills/{skill_id}");
    client
        .send_json::<Skill>(|c| c.request(Method::GET, &path))
        .await
}

/// Find a skill by slug. Two paths:
///
///   * `@author/slug` — calls the anonymous marketplace detail endpoint.
///     Works for public skills regardless of whether the caller is signed
///     in.
///   * bare `slug` — fastpath via `GET /skills?slug=<s>&limit=1`. The server
///     filters by the `(owner_user_id, slug)` / `(owner_team_id, slug)`
///     unique indexes, so this is O(1) regardless of library size.
pub async fn find_by_slug(client: &ApiClient, slug: &str) -> Result<Option<Skill>, CliError> {
    if let Some((author, slug_only)) = parse_handle_slug(slug) {
        return match marketplace_detail(client, &author, &slug_only).await {
            Ok(detail) => Ok(Some(detail.into_skill())),
            Err(CliError::NotFound(_)) => Ok(None),
            Err(e) => Err(e),
        };
    }

    let slug_owned = slug.to_string();
    let page = client
        .send_json::<Page<Skill>>(|c| {
            Ok(c.request(Method::GET, "/skills")?
                .query(&[("slug", slug_owned.as_str()), ("limit", "1")]))
        })
        .await?;
    Ok(page.items.into_iter().next())
}

/// Subset of the marketplace detail shape needed for the CLI pull
/// path. Server-side fields like ratings and full markdown are
/// ignored — we only care about the skill_id + current version so
/// the existing `get_version` + bundle-download flow can run.
#[derive(Debug, Clone, Deserialize)]
struct MarketplaceDetail {
    id: String,
    slug: String,
    name: String,
    #[serde(default)]
    description: String,
    author: MarketplaceAuthor,
    current_version_id: Option<String>,
    current_version_semver: Option<String>,
    published_at: Option<DateTime<Utc>>,
    created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Deserialize)]
struct MarketplaceAuthor {
    username: String,
}

impl MarketplaceDetail {
    fn into_skill(self) -> Skill {
        Skill {
            id: self.id,
            slug: self.slug,
            name: self.name,
            description: self.description,
            scope: "public".to_string(),
            owner_user_id: None,
            owner_team_id: None,
            owner_username: Some(self.author.username),
            current_version_id: self.current_version_id,
            current_version_semver: self.current_version_semver,
            published_at: self.published_at,
            // Public skills are never foldered (CHECK ``ck_skills_public_no_folder``).
            folder_id: None,
            folder_name: None,
            // The marketplace detail view doesn't surface fork lineage
            // (the public surface is for browsing originals, not seeing
            // who forked from them). The workspace GET /skills/{id}
            // populates these; here they stay None.
            forked_from_skill_id: None,
            forked_from: None,
            created_at: self.created_at,
        }
    }
}

async fn marketplace_detail(
    client: &ApiClient,
    author: &str,
    slug: &str,
) -> Result<MarketplaceDetail, CliError> {
    let handle = author.trim_start_matches('@');
    let path = format!("/marketplace/@{}/{}", handle, slug);
    client
        .send_json::<MarketplaceDetail>(|c| c.request(Method::GET, &path))
        .await
}

/// Parse `@author/slug` (or `author/slug`) into its parts. Returns `None`
/// for bare-slug inputs so callers fall through to the legacy scan path.
pub fn parse_handle_slug(input: &str) -> Option<(String, String)> {
    let trimmed = input.trim().trim_start_matches('@');
    let (author, slug) = trimmed.split_once('/')?;
    if author.is_empty() || slug.is_empty() {
        return None;
    }
    Some((author.to_string(), slug.to_string()))
}

/// Resolve `@author/slug` to a public skill row via the marketplace
/// resolver endpoint. No auth required. Reserved for Phase 2
/// (``knack search``) — the pull path uses the richer
/// ``/marketplace/@user/slug`` detail endpoint instead.
#[allow(dead_code)]
pub async fn resolve(
    client: &ApiClient,
    author: &str,
    slug: &str,
) -> Result<SkillResolve, CliError> {
    let author = author.to_string();
    let slug = slug.to_string();
    client
        .send_json::<SkillResolve>(|c| {
            Ok(c.request(Method::GET, "/skills/resolve")?
                .query(&[("author", &author), ("slug", &slug)]))
        })
        .await
}

pub async fn get_version(
    client: &ApiClient,
    skill_id: &str,
    semver: &str,
) -> Result<SkillVersion, CliError> {
    let path = format!("/skills/{skill_id}/versions/{semver}");
    client
        .send_json::<SkillVersion>(|c| c.request(Method::GET, &path))
        .await
}

pub async fn list_versions(
    client: &ApiClient,
    skill_id: &str,
) -> Result<Page<SkillVersion>, CliError> {
    let path = format!("/skills/{skill_id}/versions");
    client
        .send_json::<Page<SkillVersion>>(|c| {
            Ok(c.request(Method::GET, &path)?.query(&[("limit", "200")]))
        })
        .await
}

pub async fn create(client: &ApiClient, body: &SkillCreate) -> Result<Skill, CliError> {
    let body = serde_json::to_value(body)?;
    client
        .send_json::<Skill>(|c| Ok(c.request(Method::POST, "/skills")?.json(&body)))
        .await
}

/// PATCH /skills/{skill_id} — update name/description and flip/transfer
/// scope. Slug is immutable server-side. Fields omitted from the body
/// are unchanged.
///
/// When `scope` is `"team"`, `owner_team_id` must be set: the server
/// transfers the skill into that team's library and clears
/// `owner_user_id`. Going from team to personal pulls the skill back
/// into the caller's personal library (caller becomes the new
/// `owner_user_id`).
/// Tiny double-Option serializer for ``folder_id`` on ``SkillUpdate``.
///
/// We need three states on the wire:
///   * field omitted   → leave folder_id alone server-side
///   * `"folder_id": "<uuid>"` → assign
///   * `"folder_id": null`    → unfile
///
/// Outer ``Option`` distinguishes "supplied or not"; inner ``Option``
/// is "null vs a value". ``skip_serializing_if`` skips the outer-None
/// case; this helper handles the remaining two by serializing the
/// inner Option directly.
mod double_option {
    use serde::{Serialize, Serializer};

    pub fn serialize<S, T>(value: &Option<Option<T>>, ser: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
        T: Serialize,
    {
        match value {
            Some(inner) => inner.serialize(ser),
            None => ser.serialize_none(),
        }
    }
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct SkillUpdate {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scope: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub owner_team_id: Option<String>,
    /// Folder assignment. Outer ``Option`` = "did the caller set this
    /// field at all?"; inner ``Option`` = ``null`` (unfile) vs a
    /// specific folder id.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        serialize_with = "double_option::serialize"
    )]
    pub folder_id: Option<Option<String>>,
}

/// Soft-delete a skill (server-side `DELETE /skills/{id}`). Owner-only.
/// We deliberately do NOT expose this as a CLI command — deletion is a
/// web-only surface so an agent can't accidentally nuke a published
/// skill. This wrapper exists for tests + future scripted use cases.
pub async fn delete(client: &ApiClient, skill_id: &str) -> Result<(), CliError> {
    let path = format!("/skills/{skill_id}");
    client
        .send_empty(|c| c.request(Method::DELETE, &path))
        .await
}

pub async fn update(
    client: &ApiClient,
    skill_id: &str,
    body: &SkillUpdate,
) -> Result<Skill, CliError> {
    let path = format!("/skills/{skill_id}");
    let body = serde_json::to_value(body)?;
    client
        .send_json::<Skill>(|c| Ok(c.request(Method::PATCH, &path)?.json(&body)))
        .await
}

pub async fn create_version(
    client: &ApiClient,
    skill_id: &str,
    body: &SkillVersionCreate,
) -> Result<SkillVersion, CliError> {
    let path = format!("/skills/{skill_id}/versions");
    let body = serde_json::to_value(body)?;
    client
        .send_json::<SkillVersion>(|c| Ok(c.request(Method::POST, &path)?.json(&body)))
        .await
}

/// POST /skills/{id}/fork — body fields are optional; the server defaults
/// `slug` to the original's slug and `name` to the original's name.
#[derive(Debug, Clone, Default, Serialize)]
pub struct SkillFork {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub slug: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

/// Fork a public skill into the caller's personal library. Returns the
/// new Skill row (scope=personal, version=0.1.0, with
/// `forked_from_skill_id` pointing back at the original).
pub async fn fork(
    client: &ApiClient,
    skill_id: &str,
    body: &SkillFork,
) -> Result<Skill, CliError> {
    let path = format!("/skills/{skill_id}/fork");
    let body = serde_json::to_value(body)?;
    client
        .send_json::<Skill>(|c| Ok(c.request(Method::POST, &path)?.json(&body)))
        .await
}

/// Resolve a public skill via the marketplace detail endpoint and return
/// `(skill_id, current_version_semver?)`. Used by `knack fork` so the
/// caller can pass `@author/slug` without first scanning their library.
pub async fn resolve_public(
    client: &ApiClient,
    author: &str,
    slug: &str,
) -> Result<(String, Option<String>), CliError> {
    let detail = marketplace_detail(client, author, slug).await?;
    Ok((detail.id, detail.current_version_semver))
}

/// V2a: request a presigned PUT URL for uploading a packed skill bundle.
/// The CLI uploads tarball bytes directly to the returned ``upload_url``,
/// then echoes ``s3_key`` back as ``packed_s3_key`` on the subsequent
/// ``create_version`` call.
pub async fn presign_bundle(
    client: &ApiClient,
    skill_id: &str,
) -> Result<BundlePresignResponse, CliError> {
    let path = format!("/skills/{skill_id}/versions/presign-bundle");
    client
        .send_json::<BundlePresignResponse>(|c| c.request(Method::POST, &path))
        .await
}

/// V2a: request a presigned GET URL for a version's packed tarball. Returns
/// 404 for versions that pre-date V2a (no packed_s3_key); callers fall back
/// to the three-text-field write path.
pub async fn bundle_download(
    client: &ApiClient,
    skill_id: &str,
    semver: &str,
) -> Result<BundleDownloadResponse, CliError> {
    let path = format!("/skills/{skill_id}/versions/{semver}/bundle");
    client
        .send_json::<BundleDownloadResponse>(|c| c.request(Method::GET, &path))
        .await
}
