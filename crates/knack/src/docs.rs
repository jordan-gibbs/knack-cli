//! Embedded CLI docs.
//!
//! The canonical markdown lives at `apps/api/knack_api/cli_docs/` so the same
//! source serves the API's `GET /docs/cli` endpoint *and* the binary's
//! `knack docs` command. We bake them in at compile time via `include_str!` —
//! no network needed offline, and a stale binary can't drift from a stale
//! server: the build would have to repackage them.

pub struct Topic {
    pub slug: &'static str,
    pub title: &'static str,
    pub body: &'static str,
}

pub const TOPICS: &[Topic] = &[
    Topic {
        slug: "getting-started",
        title: "Getting started",
        body: include_str!("../embedded/cli-docs/getting-started.md"),
    },
    Topic {
        slug: "auth",
        title: "Auth",
        body: include_str!("../embedded/cli-docs/auth.md"),
    },
    Topic {
        slug: "commands",
        title: "Commands",
        body: include_str!("../embedded/cli-docs/commands.md"),
    },
    Topic {
        slug: "exit-codes",
        title: "Exit codes",
        body: include_str!("../embedded/cli-docs/exit-codes.md"),
    },
    Topic {
        slug: "json-schema",
        title: "JSON output schema",
        body: include_str!("../embedded/cli-docs/json-schema.md"),
    },
    Topic {
        slug: "agent-integration",
        title: "Agent integration",
        body: include_str!("../embedded/cli-docs/agent-integration.md"),
    },
    Topic {
        slug: "troubleshooting",
        title: "Troubleshooting",
        body: include_str!("../embedded/cli-docs/troubleshooting.md"),
    },
];

pub fn find(slug: &str) -> Option<&'static Topic> {
    TOPICS.iter().find(|t| t.slug == slug)
}

pub fn toc() -> String {
    let mut out = String::from("knack docs — available topics:\n\n");
    for t in TOPICS {
        out.push_str(&format!("  {:<20} {}\n", t.slug, t.title));
    }
    out.push_str("\nrun `knack docs <topic>` for one, or `knack docs all` for everything.\n");
    out
}

pub fn all() -> String {
    let mut out = String::new();
    for t in TOPICS {
        out.push_str(&format!("# {}\n\n", t.title));
        out.push_str(t.body.trim());
        out.push_str("\n\n---\n\n");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_topic_loaded_and_nonempty() {
        for t in TOPICS {
            assert!(!t.body.is_empty(), "topic {} has empty body", t.slug);
        }
    }

    #[test]
    fn find_known_topic() {
        assert!(find("getting-started").is_some());
        assert!(find("auth").is_some());
    }

    #[test]
    fn find_unknown_topic_is_none() {
        assert!(find("nope").is_none());
    }

    #[test]
    fn toc_lists_every_topic() {
        let out = toc();
        for t in TOPICS {
            assert!(out.contains(t.slug), "TOC missing {}", t.slug);
        }
    }
}
