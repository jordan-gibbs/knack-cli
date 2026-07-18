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
    assert!(matches!(err, CliError::NotFound(_)), "got {err:?}");
}

// === Phase B/D additions: list_for_skill + get_stats =======================

use wiremock::matchers::query_param;

#[tokio::test]
async fn list_for_skill_threads_filters_into_query() {
    let (server, client, _store) = common::fixture().await;

    Mock::given(method("GET"))
        .and(path("/runs/by-skill/skill-1"))
        .and(query_param("status", "failed"))
        .and(query_param("limit", "10"))
        .and(query_param("cursor", "cur-abc"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "items": [
                run_response("run-1", "succeeded"),
                run_response("run-2", "failed"),
            ],
            "next_cursor": "cur-next"
        })))
        .mount(&server)
        .await;

    let page = api_runs::list_for_skill(
        &client,
        "skill-1",
        &api_runs::RunsListQuery {
            status: Some("failed".into()),
            limit: Some(10),
            cursor: Some("cur-abc".into()),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    assert_eq!(page.items.len(), 2);
    assert_eq!(page.next_cursor.as_deref(), Some("cur-next"));
}

#[tokio::test]
async fn get_stats_default_returns_flat_shape() {
    use knack_cli::api::skills as api_skills;

    let (server, client, _store) = common::fixture().await;

    Mock::given(method("GET"))
        .and(path("/skills/skill-1/stats"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "skill_id": "skill-1",
            "runs_total": 10,
            "runs_succeeded": 8,
            "runs_failed": 1,
            "runs_unmarked": 1,
            "success_rate": 0.888,
            "last_run_at": "2026-05-27T00:00:00Z"
        })))
        .mount(&server)
        .await;

    let resp = api_skills::get_stats(&client, "skill-1", &api_skills::StatsQuery::default())
        .await
        .unwrap();
    match resp {
        api_skills::SkillStatsResponse::Flat(s) => {
            assert_eq!(s.runs_total, 10);
            assert!((s.success_rate.unwrap() - 0.888).abs() < 1e-6);
        }
        api_skills::SkillStatsResponse::ByVersion(_) => panic!("expected flat shape"),
    }
}

#[tokio::test]
async fn get_stats_group_by_version_returns_buckets_with_p50() {
    use knack_cli::api::skills as api_skills;

    let (server, client, _store) = common::fixture().await;

    Mock::given(method("GET"))
        .and(path("/skills/skill-1/stats"))
        .and(query_param("group_by", "version"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "skill_id": "skill-1",
            "dimensions": ["version"],
            "buckets": [
                {
                    "key": {"version": "0.1.3"},
                    "runs_total": 4, "runs_succeeded": 2, "runs_failed": 2,
                    "runs_unmarked": 0, "success_rate": 0.5,
                    "p50_ms": 180, "p95_ms": 900,
                    "last_run_at": "2026-05-27T00:00:00Z",
                    "top_notes": [{"note": "timeout", "count": 2}]
                },
                {
                    "key": {"version": "0.1.4"},
                    "runs_total": 6, "runs_succeeded": 6, "runs_failed": 0,
                    "runs_unmarked": 0, "success_rate": 1.0,
                    "p50_ms": 120, "p95_ms": 300,
                    "last_run_at": "2026-05-28T00:00:00Z",
                    "top_notes": []
                }
            ]
        })))
        .mount(&server)
        .await;

    let resp = api_skills::get_stats(
        &client,
        "skill-1",
        &api_skills::StatsQuery {
            group_by: Some("version".into()),
            include: vec!["p50".into(), "p95".into()],
            ..Default::default()
        },
    )
    .await
    .unwrap();

    match resp {
        api_skills::SkillStatsResponse::ByVersion(b) => {
            assert_eq!(b.dimensions, vec!["version".to_string()]);
            assert_eq!(b.buckets.len(), 2);
            let v014 = &b.buckets[1];
            assert_eq!(
                v014.key.get("version").unwrap().as_deref(),
                Some("0.1.4")
            );
            assert_eq!(v014.runs_succeeded, 6);
            assert_eq!(v014.p50_ms, Some(120));
        }
        api_skills::SkillStatsResponse::Flat(_) => panic!("expected by-version shape"),
    }
}

#[tokio::test]
async fn get_stats_cross_tab_version_and_agent() {
    use knack_cli::api::skills as api_skills;
    let (server, client, _store) = common::fixture().await;
    Mock::given(method("GET"))
        .and(path("/skills/skill-1/stats"))
        .and(query_param("group_by", "version,agent"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "skill_id": "skill-1",
            "dimensions": ["version", "agent"],
            "buckets": [
                {
                    "key": {"version": "0.1.4", "agent": "cursor"},
                    "runs_total": 3, "runs_succeeded": 1, "runs_failed": 2,
                    "runs_unmarked": 0, "success_rate": 0.333,
                    "p50_ms": 250, "p95_ms": 800,
                    "last_run_at": "2026-05-28T00:00:00Z",
                    "top_notes": [{"note": "schema mismatch", "count": 2}]
                }
            ]
        })))
        .mount(&server)
        .await;

    let resp = api_skills::get_stats(
        &client,
        "skill-1",
        &api_skills::StatsQuery {
            group_by: Some("version,agent".into()),
            include: vec!["p50".into(), "p95".into()],
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let by = match resp {
        api_skills::SkillStatsResponse::ByVersion(b) => b,
        api_skills::SkillStatsResponse::Flat(_) => panic!("expected by-version shape"),
    };
    assert_eq!(by.dimensions, vec!["version".to_string(), "agent".to_string()]);
    let cursor = &by.buckets[0];
    assert_eq!(cursor.runs_failed, 2);
    assert_eq!(cursor.top_notes[0].note, "schema mismatch");
}

#[tokio::test]
async fn get_trend_daily_returns_series() {
    use knack_cli::api::skills as api_skills;
    let (server, client, _store) = common::fixture().await;
    Mock::given(method("GET"))
        .and(path("/skills/skill-1/stats/trend"))
        .and(query_param("interval", "day"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "skill_id": "skill-1",
            "interval": "day",
            "dimensions": [],
            "series": [
                {
                    "bucket_start": "2026-05-27",
                    "bucket_end": "2026-05-27",
                    "buckets": [
                        {"key": {}, "runs_total": 2, "runs_succeeded": 1, "runs_failed": 1,
                         "runs_unmarked": 0, "success_rate": 0.5,
                         "p50_ms": 200, "p95_ms": 1000,
                         "last_run_at": "2026-05-27T11:00:00Z", "top_notes": []}
                    ]
                },
                {
                    "bucket_start": "2026-05-28",
                    "bucket_end": "2026-05-28",
                    "buckets": [
                        {"key": {}, "runs_total": 1, "runs_succeeded": 1, "runs_failed": 0,
                         "runs_unmarked": 0, "success_rate": 1.0,
                         "p50_ms": 120, "p95_ms": 120,
                         "last_run_at": "2026-05-28T10:00:00Z", "top_notes": []}
                    ]
                }
            ]
        })))
        .mount(&server)
        .await;

    let resp = api_skills::get_trend(
        &client,
        "skill-1",
        &api_skills::TrendQuery {
            interval: "day".into(),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    assert_eq!(resp.series.len(), 2);
    assert_eq!(resp.series[1].buckets[0].runs_succeeded, 1);
}

#[tokio::test]
async fn get_overview_returns_summary_with_regressions() {
    use knack_cli::api::overview as api_overview;
    let (server, client, _store) = common::fixture().await;
    Mock::given(method("GET"))
        .and(path("/runs/overview"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "skills": [
                {
                    "slug": "email-triage",
                    "current_version": "0.1.4",
                    "runs_total": 22, "succeeded": 20, "failed": 2,
                    "success_rate": 0.909,
                    "p50_ms": 165, "p95_ms": 340,
                    "last_run_at": "2026-05-28T11:00:00Z",
                    "regression": {
                        "current_version": "0.1.4",
                        "prior_version": "0.1.3",
                        "delta_success_rate": -0.083,
                        "current_success_rate": 0.909,
                        "prior_success_rate": 0.992
                    },
                    "stale": false
                },
                {
                    "slug": "unused",
                    "current_version": null,
                    "runs_total": 0, "succeeded": 0, "failed": 0,
                    "success_rate": null,
                    "p50_ms": null, "p95_ms": null,
                    "last_run_at": null,
                    "regression": null,
                    "stale": true
                }
            ]
        })))
        .mount(&server)
        .await;

    let resp = api_overview::get_overview(&client, &api_overview::OverviewQuery::default())
        .await
        .unwrap();
    assert_eq!(resp.skills.len(), 2);
    let triage = resp.skills.iter().find(|s| s.slug == "email-triage").unwrap();
    assert!(triage.regression.is_some());
    let stale = resp.skills.iter().find(|s| s.slug == "unused").unwrap();
    assert!(stale.stale);
}

#[test]
fn skill_stats_untagged_enum_round_trips() {
    // Belt-and-suspenders: both variants have `skill_id`, so an
    // additive field on either could silently flip serde's chosen arm.
    use knack_cli::api::skills as api_skills;
    let flat = json!({
        "skill_id": "s1",
        "runs_total": 0,
        "runs_succeeded": 0,
        "runs_failed": 0,
        "runs_unmarked": 0,
        "success_rate": null,
        "last_run_at": null
    });
    let parsed: api_skills::SkillStatsResponse = serde_json::from_value(flat).unwrap();
    assert!(matches!(parsed, api_skills::SkillStatsResponse::Flat(_)));

    let by_ver = json!({
        "skill_id": "s1",
        "dimensions": ["version"],
        "buckets": []
    });
    let parsed: api_skills::SkillStatsResponse = serde_json::from_value(by_ver).unwrap();
    assert!(matches!(parsed, api_skills::SkillStatsResponse::ByVersion(_)));
}

// === `knack mark last` / prefix shorthand (command layer + run_state) ===

use knack_cli::commands::mark::{self, MarkArgs};
use knack_cli::output::OutputMode;
use knack_cli::run_state;
use std::sync::Mutex;

/// KNACK_RECENT_RUNS_FILE is process-global; serialize the tests that
/// set it so parallel test threads can't cross paths.
static ENV_LOCK: Mutex<()> = Mutex::new(());

fn quiet() -> OutputMode {
    OutputMode {
        json: false,
        quiet: true,
        no_color: true,
    }
}

fn mark_args(run_id: &str) -> MarkArgs {
    MarkArgs {
        run_id: run_id.into(),
        outcome: Some("succeeded".into()),
        status_flag: None,
        note: None,
        reason: None,
        output: Vec::new(),
        no_push: false,
    }
}

// The guard is held across awaits on purpose: it serializes mutation of
// the process-global env var for the whole test body.
#[allow(clippy::await_holding_lock)]
#[tokio::test]
async fn mark_last_closes_newest_unmarked_run() {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let tmp = tempfile::tempdir().unwrap();
    unsafe {
        std::env::set_var("KNACK_RECENT_RUNS_FILE", tmp.path().join("recent_runs.json"));
    }

    const RUN_A: &str = "aaaa1111-cd2a-4749-b38a-ae885a1b417c";
    const RUN_B: &str = "bbbb2222-cd2a-4749-b38a-ae885a1b417c";
    run_state::push_run(RUN_A, "s1", "cloud").unwrap();
    run_state::push_run(RUN_B, "s2", "cloud").unwrap();

    let (server, client, _store) = common::fixture().await;
    Mock::given(method("POST"))
        .and(path(format!("/runs/{RUN_B}/mark")))
        .and(body_partial_json(json!({"status": "succeeded"})))
        .respond_with(ResponseTemplate::new(200).set_body_json(run_response(RUN_B, "running")))
        .mount(&server)
        .await;

    mark::run(mark_args("last"), client, quiet()).await.unwrap();

    // RUN_B (newest) was closed and flagged; `last` now points at RUN_A.
    assert_eq!(run_state::last_open("cloud").unwrap().run_id, RUN_A);

    unsafe {
        std::env::remove_var("KNACK_RECENT_RUNS_FILE");
    }
}

#[allow(clippy::await_holding_lock)]
#[tokio::test]
async fn mark_prefix_resolves_unique_recent_run() {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let tmp = tempfile::tempdir().unwrap();
    unsafe {
        std::env::set_var("KNACK_RECENT_RUNS_FILE", tmp.path().join("recent_runs.json"));
    }

    const RUN_A: &str = "aaaa1111-cd2a-4749-b38a-ae885a1b417c";
    const RUN_B: &str = "bbbb2222-cd2a-4749-b38a-ae885a1b417c";
    run_state::push_run(RUN_A, "s1", "cloud").unwrap();
    run_state::push_run(RUN_B, "s2", "cloud").unwrap();

    let (server, client, _store) = common::fixture().await;
    Mock::given(method("POST"))
        .and(path(format!("/runs/{RUN_A}/mark")))
        .respond_with(ResponseTemplate::new(200).set_body_json(run_response(RUN_A, "running")))
        .mount(&server)
        .await;

    mark::run(mark_args("aaaa"), client, quiet()).await.unwrap();

    // RUN_A closed via its prefix; RUN_B is still the open one.
    assert_eq!(run_state::last_open("cloud").unwrap().run_id, RUN_B);

    unsafe {
        std::env::remove_var("KNACK_RECENT_RUNS_FILE");
    }
}

#[allow(clippy::await_holding_lock)]
#[tokio::test]
async fn mark_ambiguous_prefix_errors_without_network() {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let tmp = tempfile::tempdir().unwrap();
    unsafe {
        std::env::set_var("KNACK_RECENT_RUNS_FILE", tmp.path().join("recent_runs.json"));
    }

    run_state::push_run("aaaa1111-cd2a-4749-b38a-ae885a1b417c", "s1", "cloud").unwrap();
    run_state::push_run("aaaa2222-cd2a-4749-b38a-ae885a1b417c", "s2", "cloud").unwrap();

    // No mock mounted: an ambiguous prefix must fail before any HTTP.
    let (_server, client, _store) = common::fixture().await;
    let err = mark::run(mark_args("aaaa"), client, quiet()).await.unwrap_err();
    assert_eq!(err.code(), "MARK_AMBIGUOUS_PREFIX");

    unsafe {
        std::env::remove_var("KNACK_RECENT_RUNS_FILE");
    }
}
