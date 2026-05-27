use anyhow::{anyhow, Context, Result};
use std::process::Command;

/// Resolved GitHub credentials. The CLI never stores the raw token; it
/// re-resolves at use time so `gh auth refresh` stays in lockstep.
#[derive(Debug, Clone)]
pub struct GithubAuth {
    pub token: String,
    pub source: TokenSource,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TokenSource {
    Env,
    GhCli,
    DeviceFlow,
}

/// Resolution order:
/// 1. $GITHUB_TOKEN
/// 2. `gh auth token`
/// 3. Device-code OAuth flow against github.com/login/device
pub fn resolve_token() -> Result<GithubAuth> {
    if let Ok(token) = std::env::var("GITHUB_TOKEN") {
        if !token.is_empty() {
            return Ok(GithubAuth {
                token,
                source: TokenSource::Env,
            });
        }
    }

    if let Some(token) = try_gh_cli()? {
        return Ok(GithubAuth {
            token,
            source: TokenSource::GhCli,
        });
    }

    // Device-code OAuth fallback — runs against
    // https://github.com/login/device. Not yet wired; surface a clean
    // error pointing at `gh` as the supported path.
    //
    // To implement: POST to https://github.com/login/device/code with the
    // OAuth client id; poll the access_token endpoint; cache the result.
    // Stored as a GithubAuthRef variant in ~/.knack/auth.json.

    Err(anyhow!(
        "no GitHub credentials. install gh and run `gh auth login`, or set GITHUB_TOKEN"
    ))
}

fn try_gh_cli() -> Result<Option<String>> {
    let output = match Command::new("gh").arg("auth").arg("token").output() {
        Ok(o) => o,
        Err(_) => return Ok(None),
    };
    if !output.status.success() {
        return Ok(None);
    }
    let token = String::from_utf8(output.stdout)
        .context("gh auth token returned non-UTF-8")?
        .trim()
        .to_string();
    if token.is_empty() {
        Ok(None)
    } else {
        Ok(Some(token))
    }
}
