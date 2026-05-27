//! `knack search <query>` — full-text search the public marketplace.

use clap::Args;
use serde_json::json;

use crate::api::{marketplace as api_market, ApiClient};
use crate::config::BackendMode;
use crate::errors::{CliError, CliResult};
use crate::output::{emit_err, emit_ok, OutputMode};

#[derive(Debug, Args)]
pub struct SearchArgs {
    /// Search terms. If omitted, lists the current top page.
    pub query: Vec<String>,

    /// Sort order. `recent` = newest published. `top` = highest-rated.
    /// `trending` = recent runs-per-hour weighted.
    #[arg(long, value_parser = ["recent", "top", "trending"], default_value = "trending")]
    pub sort: String,

    /// Max results. Server caps at 50; default 30.
    #[arg(long, default_value_t = 30)]
    pub limit: u32,
}

pub async fn run(args: SearchArgs, client: ApiClient, mode: OutputMode) -> CliResult<()> {
    let q = if args.query.is_empty() {
        None
    } else {
        Some(args.query.join(" "))
    };

    if let BackendMode::Github { local_path, .. } = &client.config.backend {
        return github_search(q.as_deref(), local_path, mode).await;
    }
    let page = match api_market::search(&client, q.as_deref(), &args.sort, args.limit).await {
        Ok(p) => p,
        Err(e) => {
            emit_err(mode, &e);
            return Err(e);
        }
    };

    emit_ok(
        mode,
        json!({
            "query": q,
            "sort": args.sort,
            "items": page.items.iter().map(|c| json!({
                "slug": format!("@{}/{}", c.author.username, c.slug),
                "id": c.id,
                "name": c.name,
                "description": c.description,
                "current_version_semver": c.current_version_semver,
                "avg_stars": c.avg_stars,
                "ratings_count": c.ratings_count,
                "runs_count": c.runs_count,
                "downloads_count": c.downloads_count,
            })).collect::<Vec<_>>(),
            "next_cursor": page.next_cursor,
        }),
        || {
            if page.items.is_empty() {
                println!("(no matches)");
                return;
            }
            for c in &page.items {
                let stars = c
                    .avg_stars
                    .map(|s| format!("{s:.1}★"))
                    .unwrap_or_else(|| "—".to_string());
                println!(
                    "  @{}/{:<20} {:<24} {}  {}",
                    c.author.username,
                    c.slug,
                    truncate(&c.name, 24),
                    stars,
                    truncate(&c.description, 60),
                );
            }
        },
    );
    Ok(())
}

fn truncate(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        s.to_string()
    } else {
        let mut out: String = s.chars().take(n.saturating_sub(1)).collect();
        out.push('…');
        out
    }
}

async fn github_search(
    query: Option<&str>,
    local_path: &std::path::Path,
    mode: OutputMode,
) -> CliResult<()> {
    use knack_backend_github::GithubBackend;
    use knack_types::Backend;

    let q = query.unwrap_or("");
    let backend = GithubBackend::new(String::new(), String::new(), local_path.to_path_buf());
    let hits = backend.search(q).await.map_err(|e| {
        let err = CliError::Internal(format!("github search: {e}"));
        emit_err(mode, &err);
        err
    })?;

    emit_ok(
        mode,
        json!({
            "items": hits.iter().map(|s| json!({
                "slug": s.slug,
                "version": s.version,
                "description": s.description,
            })).collect::<Vec<_>>(),
            "backend": "github",
            "scope": "local-clone",
            "note": "self-host search is limited to your local clone. cross-user discovery lives on the public marketplace at knack.ai.",
        }),
        || {
            if hits.is_empty() {
                println!("no matches in your local clone.");
                println!(
                    "(self-host search is local-only; for cross-user discovery, browse knack.ai)"
                );
                return;
            }
            for h in &hits {
                let desc = h.description.as_deref().unwrap_or("");
                println!("  {:<28} {:<10} {}", h.slug, h.version, desc);
            }
        },
    );
    Ok(())
}
