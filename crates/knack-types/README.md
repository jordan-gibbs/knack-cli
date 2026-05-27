# knack-types

Shared wire-format types and the `Backend` trait that both cloud and
GitHub-backed implementations of the [Knack CLI](https://github.com/jordan-gibbs/knack-cli)
satisfy.

This crate is the public contract between any Knack client and any
Knack-compatible registry. It is small on purpose: just the structs and
the trait, no business logic.

## Usage

```toml
[dependencies]
knack-types = "0.1"
```

```rust
use knack_types::{Backend, SkillSummary, RunLog, RunStatus};

async fn list_skills(backend: &dyn Backend) -> anyhow::Result<Vec<SkillSummary>> {
    Ok(backend.list().await?)
}
```

## Stability

Pre-1.0. Expect breaking changes between minor versions; pin to a
specific minor for now.

## License

MIT. See the [LICENSE](https://github.com/jordan-gibbs/knack-cli/blob/main/LICENSE)
file in the parent repository.
