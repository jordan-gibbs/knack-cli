//! Integration tests for `api::skills` against a wiremock server.

use serde_json::json;
use wiremock::matchers::{header, method, path, query_param};
use wiremock::{Mock, ResponseTemplate};

use knack_cli::api::skills as api_skills;

mod common;

#[tokio::test]
async fn list_returns_paged_skills() {
    let (server, client, _store) = common::fixture().await;

    Mock::given(method("GET"))
        .and(path("/skills"))
        .and(query_param("limit", "50"))
        .and(header(
            "authorization",
            format!("Bearer {}", common::FAKE_ACCESS_TOKEN).as_str(),
        ))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "items": [
                {
                    "id": "00000000-0000-0000-0000-000000000001",
                    "slug": "monthly-close",
                    "name": "Monthly close",
                    "scope": "personal",
                    "owner_user_id": "u1",
                    "owner_team_id": null,
                    "current_version_id": "v1",
                    "current_version_semver": "1.0.0",
                    "created_at": "2026-05-08T00:00:00Z",
                }
            ],
            "next_cursor": null,
        })))
        .mount(&server)
        .await;

    let page = api_skills::list(&client, None, None, 50).await.unwrap();
    assert_eq!(page.items.len(), 1);
    assert_eq!(page.items[0].slug, "monthly-close");
    assert_eq!(
        page.items[0].current_version_semver.as_deref(),
        Some("1.0.0")
    );
    assert!(page.next_cursor.is_none());
}

#[tokio::test]
async fn list_filters_by_scope() {
    let (server, client, _store) = common::fixture().await;

    Mock::given(method("GET"))
        .and(path("/skills"))
        .and(query_param("scope", "public"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "items": [],
            "next_cursor": null,
        })))
        .expect(1)
        .mount(&server)
        .await;

    let page = api_skills::list(&client, Some("public"), None, 50)
        .await
        .unwrap();
    assert!(page.items.is_empty());
}

#[tokio::test]
async fn find_by_slug_paginates_and_matches() {
    // Two pages — first has one unrelated skill, second has the target.
    let (server, client, _store) = common::fixture().await;

    Mock::given(method("GET"))
        .and(path("/skills"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "items": [
                {
                    "id": "id-1",
                    "slug": "intake-cleanup",
                    "name": "Intake cleanup",
                    "scope": "personal",
                    "owner_user_id": "u1",
                    "owner_team_id": null,
                    "current_version_id": null,
                    "current_version_semver": null,
                    "created_at": "2026-05-08T00:00:00Z",
                }
            ],
            "next_cursor": null,
        })))
        .mount(&server)
        .await;

    // Should match on the only item (slug == "intake-cleanup"), not the
    // unrelated query slug.
    let hit = api_skills::find_by_slug(&client, "intake-cleanup")
        .await
        .unwrap();
    assert!(hit.is_some());
    assert_eq!(hit.unwrap().id, "id-1");

    let miss = api_skills::find_by_slug(&client, "does-not-exist")
        .await
        .unwrap();
    assert!(miss.is_none());
}

#[tokio::test]
async fn create_posts_full_body_and_returns_skill() {
    use wiremock::matchers::body_partial_json;

    let (server, client, _store) = common::fixture().await;

    Mock::given(method("POST"))
        .and(path("/skills"))
        .and(body_partial_json(json!({
            "slug": "month-end-close",
            "name": "Month-end close",
            "scope": "personal",
        })))
        .respond_with(ResponseTemplate::new(201).set_body_json(json!({
            "id": "00000000-0000-0000-0000-0000000000aa",
            "slug": "month-end-close",
            "name": "Month-end close",
            "scope": "personal",
            "owner_user_id": "u1",
            "owner_team_id": null,
            "current_version_id": null,
            "current_version_semver": null,
            "created_at": "2026-05-10T00:00:00Z",
        })))
        .mount(&server)
        .await;

    let skill = api_skills::create(
        &client,
        &api_skills::SkillCreate {
            slug: "month-end-close".into(),
            name: "Month-end close".into(),
            scope: Some("personal".into()),
            owner_team_id: None,
        },
    )
    .await
    .unwrap();
    assert_eq!(skill.slug, "month-end-close");
    assert!(skill.current_version_id.is_none());
}

#[tokio::test]
async fn create_409_maps_to_conflict() {
    use knack_cli::errors::CliError;

    let (server, client, _store) = common::fixture().await;

    Mock::given(method("POST"))
        .and(path("/skills"))
        .respond_with(ResponseTemplate::new(409).set_body_json(json!({
            "ok": false,
            "error": { "code": "SLUG_TAKEN", "message": "slug already exists" }
        })))
        .mount(&server)
        .await;

    let err = api_skills::create(
        &client,
        &api_skills::SkillCreate {
            slug: "taken".into(),
            name: "Taken".into(),
            scope: Some("personal".into()),
            owner_team_id: None,
        },
    )
    .await
    .unwrap_err();
    matches!(err, CliError::Conflict { .. });
}

#[tokio::test]
async fn create_version_posts_full_body() {
    use wiremock::matchers::body_partial_json;

    let (server, client, _store) = common::fixture().await;

    Mock::given(method("POST"))
        .and(path("/skills/sk1/versions"))
        // Only match the always-present fields — `intuition_md` / `meta_yaml`
        // are skipped on the wire when empty (see SkillVersionCreate).
        .and(body_partial_json(json!({
            "version": "0.2.0",
            "skill_md": "# v2",
        })))
        .respond_with(ResponseTemplate::new(201).set_body_json(json!({
            "id": "ver-2",
            "skill_id": "sk1",
            "version": "0.2.0",
            "skill_md": "# v2",
            "intuition_md": "",
            "meta_yaml": "",
            "parent_version_id": null,
            "created_by": "u1",
            "created_at": "2026-05-08T00:00:00Z",
            "artifact_ids": [],
        })))
        .mount(&server)
        .await;

    let v = api_skills::create_version(
        &client,
        "sk1",
        &api_skills::SkillVersionCreate {
            version: "0.2.0".into(),
            skill_md: "# v2".into(),
            intuition_md: String::new(),
            meta_yaml: String::new(),
            parent_version_id: None,
            artifact_ids: vec![],
            packed_s3_key: None,
        },
    )
    .await
    .unwrap();
    assert_eq!(v.id, "ver-2");
    assert_eq!(v.version, "0.2.0");
}

#[tokio::test]
async fn get_version_404_maps_to_not_found() {
    use knack_cli::errors::CliError;

    let (server, client, _store) = common::fixture().await;

    Mock::given(method("GET"))
        .and(path("/skills/sk1/versions/9.9.9"))
        .respond_with(ResponseTemplate::new(404).set_body_json(json!({
            "ok": false,
            "error": { "code": "NOT_FOUND", "message": "version 9.9.9 not found" }
        })))
        .mount(&server)
        .await;

    let err = api_skills::get_version(&client, "sk1", "9.9.9")
        .await
        .unwrap_err();
    matches!(err, CliError::NotFound(_));
}

#[tokio::test]
async fn server_5xx_maps_to_server_error() {
    use knack_cli::errors::CliError;

    let (server, client, _store) = common::fixture().await;

    Mock::given(method("GET"))
        .and(path("/skills"))
        .respond_with(ResponseTemplate::new(503).set_body_json(json!({
            "ok": false,
            "error": { "code": "UPSTREAM", "message": "db overloaded" }
        })))
        .mount(&server)
        .await;

    let err = api_skills::list(&client, None, None, 50).await.unwrap_err();
    if let CliError::Server { status, .. } = err {
        assert_eq!(status, 503);
    } else {
        panic!("expected Server, got {err:?}");
    }
}
