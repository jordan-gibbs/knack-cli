//! Integration tests for `api::runs` against a wiremock server.

use serde_json::json;
use wiremock::matchers::{body_partial_json, method, path};
use wiremock::{Mock, ResponseTemplate};

use knack_cli::api::runs as api_runs;

mod common;

fn run_response(id: &str, status: &str) -> serde_json::Value {
    json!({
        "id": id,
        "skill_version_id": "ver-1",
        "agent_id": "a1",
        "runtime": "claude-code",
        "started_at": "2026-05-08T00:00:00Z",
        "finished_at": null,
        "status": status,
        "inputs_summary": null,
        "outputs_summary": null,
        "files_touched": null,
        "marks": [],
    })
}

#[tokio::test]
async fn start_run_posts_skill_version_id() {
    let (server, client, _store) = common::fixture().await;

    Mock::given(method("POST"))
        .and(path("/runs"))
        .and(body_partial_json(json!({
            "skill_version_id": "ver-1",
            "agent_id": "a1",
            "runtime": "claude-code",
        })))
        .respond_with(ResponseTemplate::new(201).set_body_json(run_response("run-1", "running")))
        .mount(&server)
        .await;

    let r = api_runs::start(
        &client,
        &api_runs::RunCreate {
            skill_version_id: "ver-1".into(),
            agent_id: Some("a1".into()),
            runtime: Some("claude-code".into()),
            inputs_summary: None,
        },
    )
    .await
    .unwrap();
    assert_eq!(r.id, "run-1");
    assert_eq!(r.status, "running");
}

#[tokio::test]
async fn finish_run_includes_status_and_outputs() {
    let (server, client, _store) = common::fixture().await;

    Mock::given(method("POST"))
        .and(path("/runs/run-1/finish"))
        .and(body_partial_json(json!({
            "status": "succeeded",
            "outputs_summary": { "duration_s": 4.2 },
        })))
        .respond_with(ResponseTemplate::new(200).set_body_json({
            let mut j = run_response("run-1", "succeeded");
            j["finished_at"] = json!("2026-05-08T00:01:00Z");
            j
        }))
        .mount(&server)
        .await;

    let r = api_runs::finish(
        &client,
        "run-1",
        &api_runs::RunFinish {
            status: "succeeded".into(),
            outputs_summary: Some(json!({ "duration_s": 4.2 })),
            files_touched: None,
        },
    )
    .await
    .unwrap();
    assert_eq!(r.status, "succeeded");
    assert!(r.finished_at.is_some());
}

#[tokio::test]
async fn mark_run_posts_status_and_note() {
    let (server, client, _store) = common::fixture().await;

    Mock::given(method("POST"))
        .and(path("/runs/run-1/mark"))
        .and(body_partial_json(json!({
            "status": "failed",
            "note": "missed two receipts on page 3",
        })))
        .respond_with(ResponseTemplate::new(200).set_body_json({
            let mut j = run_response("run-1", "running");
            j["marks"] = json!([{
                "id": "mark-1",
                "user_id": "u1",
                "status": "failed",
                "note": "missed two receipts on page 3",
                "created_at": "2026-05-08T00:02:00Z",
            }]);
            j
        }))
        .mount(&server)
        .await;

    let r = api_runs::mark(
        &client,
        "run-1",
        &api_runs::RunMarkBody {
            status: "failed".into(),
            note: Some("missed two receipts on page 3".into()),
        },
    )
    .await
    .unwrap();
    assert_eq!(r.marks.len(), 1);
}

#[tokio::test]
async fn mark_404_when_run_unknown() {
    use knack_cli::errors::CliError;

    let (server, client, _store) = common::fixture().await;

    Mock::given(method("POST"))
        .and(path("/runs/nope/mark"))
        .respond_with(ResponseTemplate::new(404).set_body_json(json!({
            "ok": false,
            "error": { "code": "NOT_FOUND", "message": "run not found" }
        })))
        .mount(&server)
        .await;

    let err = api_runs::mark(
        &client,
        "nope",
        &api_runs::RunMarkBody {
            status: "succeeded".into(),
            note: None,
        },
    )
    .await
    .unwrap_err();
    matches!(err, CliError::NotFound(_));
}
