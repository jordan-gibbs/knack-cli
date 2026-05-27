//! Delimited-block read / splice / write for AGENTS.md and friends.
//!
//! Goal: re-running `knack install` updates a single managed block in place
//! without ever touching surrounding user content. The sentinels are HTML
//! comments so they render invisibly in any markdown viewer.

use std::fs;
use std::io;
use std::path::Path;

pub const START_MARKER: &str = "<!-- knack:start (managed by `knack install` — do not edit between markers) -->";
pub const END_MARKER: &str = "<!-- knack:end -->";

/// Splice `body` into the managed block at `path`. Creates the file (and any
/// missing parents) if needed. Returns `true` when the file's bytes changed.
pub fn upsert(path: &Path, body: &str) -> io::Result<bool> {
    let existing = read_to_string_or_empty(path)?;
    let cleaned = strip_block(&existing);
    let new_block = format!("{START_MARKER}\n{}\n{END_MARKER}\n", body.trim_end());
    let next = compose(&cleaned, &new_block);
    if next == existing {
        return Ok(false);
    }
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(path, next)?;
    Ok(true)
}

/// Remove the managed block from `path`. Returns `true` when the file
/// changed. If removing the block leaves the file empty, the file itself is
/// deleted so we don't litter empty CLAUDE.md / AGENTS.md files behind.
pub fn remove(path: &Path) -> io::Result<bool> {
    let existing = match fs::read_to_string(path) {
        Ok(s) => s,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(false),
        Err(e) => return Err(e),
    };
    let cleaned = strip_block(&existing);
    if cleaned == existing {
        return Ok(false);
    }
    if cleaned.trim().is_empty() {
        let _ = fs::remove_file(path);
    } else {
        fs::write(path, cleaned)?;
    }
    Ok(true)
}

fn read_to_string_or_empty(path: &Path) -> io::Result<String> {
    match fs::read_to_string(path) {
        Ok(s) => Ok(s),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(String::new()),
        Err(e) => Err(e),
    }
}

/// Combine the file's pre-existing content (already stripped of our block)
/// with the freshly rendered block. Ensures one blank line of separation.
fn compose(head: &str, block: &str) -> String {
    if head.trim().is_empty() {
        return block.to_string();
    }
    let trimmed = head.trim_end_matches('\n');
    format!("{trimmed}\n\n{block}")
}

/// Strip exactly one managed block from `input`. Tolerates a missing end
/// marker by returning the input unchanged (better than nuking the file).
pub fn strip_block(input: &str) -> String {
    let Some(start) = input.find(START_MARKER) else {
        return input.to_string();
    };
    let after_start = start + START_MARKER.len();
    let Some(rel_end) = input[after_start..].find(END_MARKER) else {
        return input.to_string();
    };
    let end = after_start + rel_end + END_MARKER.len();
    let head = input[..start].trim_end_matches('\n');
    let tail = input[end..].trim_start_matches('\n');
    match (head.is_empty(), tail.is_empty()) {
        (true, true) => String::new(),
        (true, false) => tail.to_string(),
        (false, true) => format!("{head}\n"),
        (false, false) => format!("{head}\n\n{tail}\n"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn make_body() -> &'static str {
        "Hello from knack."
    }

    #[test]
    fn upsert_creates_missing_file() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("nested").join("CLAUDE.md");
        let wrote = upsert(&path, make_body()).unwrap();
        assert!(wrote);
        let content = fs::read_to_string(&path).unwrap();
        assert!(content.contains(START_MARKER));
        assert!(content.contains(END_MARKER));
        assert!(content.contains("Hello from knack."));
    }

    #[test]
    fn upsert_is_idempotent() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("CLAUDE.md");
        upsert(&path, make_body()).unwrap();
        let after_first = fs::read_to_string(&path).unwrap();
        let wrote_second = upsert(&path, make_body()).unwrap();
        assert!(!wrote_second, "second identical upsert should be a no-op");
        let after_second = fs::read_to_string(&path).unwrap();
        assert_eq!(after_first, after_second);
    }

    #[test]
    fn upsert_preserves_existing_content() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("CLAUDE.md");
        fs::write(&path, "# My rules\n\n- Be terse.\n").unwrap();
        upsert(&path, make_body()).unwrap();
        let content = fs::read_to_string(&path).unwrap();
        assert!(content.starts_with("# My rules\n\n- Be terse.\n"));
        assert!(content.contains("Hello from knack."));
    }

    #[test]
    fn upsert_replaces_old_block() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("CLAUDE.md");
        upsert(&path, "old body").unwrap();
        upsert(&path, "new body").unwrap();
        let content = fs::read_to_string(&path).unwrap();
        assert!(content.contains("new body"));
        assert!(!content.contains("old body"));
        // exactly one start marker
        assert_eq!(content.matches(START_MARKER).count(), 1);
    }

    #[test]
    fn remove_strips_block_only() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("CLAUDE.md");
        fs::write(&path, "# Pre\n").unwrap();
        upsert(&path, make_body()).unwrap();
        let removed = remove(&path).unwrap();
        assert!(removed);
        let content = fs::read_to_string(&path).unwrap();
        assert_eq!(content.trim(), "# Pre");
    }

    #[test]
    fn remove_deletes_file_when_empty_after() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("CLAUDE.md");
        upsert(&path, make_body()).unwrap();
        let removed = remove(&path).unwrap();
        assert!(removed);
        assert!(!path.exists(), "empty file should be deleted, not left as empty");
    }

    #[test]
    fn remove_on_missing_file_is_noop() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("nope.md");
        assert!(!remove(&path).unwrap());
    }
}
