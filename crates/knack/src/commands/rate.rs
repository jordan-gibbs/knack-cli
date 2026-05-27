//! `knack rate <slug> <stars> [--review "..."]` — rate a public skill 1-5.
//!
//! `knack rate <slug> --clear` removes the caller's rating.

use clap::Args;
use serde_json::json;

use crate::api::{marketplace as api_market, skills as api_skills, ApiClient};
use crate::errors::{CliError, CliResult};
use crate::output::{emit_err, emit_ok, OutputMode};

#[derive(Debug, Args)]
pub struct RateArgs {
    /// Slug or `@author/slug` of the public skill.
    pub slug: String,

    /// 1-5 stars. Omit when passing --clear.
    #[arg(value_parser = clap::value_parser!(u8).range(1..=5))]
    pub stars: Option<u8>,

    /// Optional review text. Max 2000 chars. Ignored when --clear is set.
    #[arg(long)]
    pub review: Option<String>,

    /// Remove your existing rating instead of upserting.
    #[arg(long, conflicts_with_all = ["stars", "review"])]
    pub clear: bool,
}

pub async fn run(args: RateArgs, client: ApiClient, mode: OutputMode) -> CliResult<()> {
    if !args.clear && args.stars.is_none() {
        let err = CliError::User {
            code: "RATE_NO_STARS".into(),
            message: "pass a 1-5 star value, or --clear to remove your rating".into(),
            hint: None,
        };
        emit_err(mode, &err);
        return Err(err);
    }

    let skill = match api_skills::find_by_slug(&client, &args.slug).await? {
        Some(s) => s,
        None => {
            let err = CliError::NotFound(format!("skill `{}` not found", args.slug));
            emit_err(mode, &err);
            return Err(err);
        }
    };

    if args.clear {
        match api_market::clear_rating(&client, &skill.id).await {
            Ok(()) => {
                emit_ok(mode, json!({ "slug": args.slug, "cleared": true }), || {
                    println!("✓ cleared your rating for {}", args.slug)
                });
                return Ok(());
            }
            Err(e) => {
                emit_err(mode, &e);
                return Err(e);
            }
        }
    }

    let stars = args.stars.expect("checked above");
    match api_market::rate(&client, &skill.id, stars, args.review.as_deref()).await {
        Ok(resp) => {
            emit_ok(
                mode,
                json!({
                    "slug": args.slug,
                    "stars": resp.rating.stars,
                    "review": resp.rating.review,
                    "summary": {
                        "avg_stars": resp.summary.avg_stars,
                        "count": resp.summary.count,
                    },
                }),
                || {
                    let avg = resp
                        .summary
                        .avg_stars
                        .map(|s| format!("{s:.2}"))
                        .unwrap_or_else(|| "—".to_string());
                    println!(
                        "✓ {} rated {}★ — community avg now {} over {} rating(s)",
                        args.slug, resp.rating.stars, avg, resp.summary.count,
                    );
                },
            );
            Ok(())
        }
        Err(e) => {
            emit_err(mode, &e);
            Err(e)
        }
    }
}
