//! Pack / unpack skill folders to and from the deterministic gzip-tar wire
//! format used by Knack's V2a multi-file storage.
//!
//! Mirrors `apps/api/knack_api/skill_format/pack.py` semantically. We do not
//! aim for byte-exact parity with the Python tarball — `flate2` and CPython's
//! `gzip` make slightly different Huffman choices. What we DO guarantee is:
//!
//!   - The **unpacked tree** matches the Python unpacker's output exactly.
//!   - The **per-file sha256s** in `.knack/manifest.json` match.
//!   - The manifest itself is byte-deterministic (sorted keys, fixed indent).
//!
//! That's enough for the server's "derive text fields from bundle" path on
//! `POST /skills/{id}/versions` to read what we sent.

use std::collections::BTreeMap;
use std::fs::File;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use flate2::read::GzDecoder;
use flate2::write::GzEncoder;
use flate2::Compression;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use walkdir::WalkDir;

use crate::errors::CliError;

const MANIFEST_PATH: &str = ".knack/manifest.json";
const MANIFEST_VERSION: u32 = 1;

// Keep these lists in lockstep with apps/api/knack_api/skill_format/pack.py.
const REQUIRED_FILES: &[&str] = &["SKILL.md", "meta.knack.yaml"];
const OPTIONAL_FILES: &[&str] = &["intuition.md"];
const OPTIONAL_DIRS: &[&str] = &["tests", "examples", "scripts", "assets", "references"];

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Manifest {
    pub version: u32,
    /// Map of `arcname` (POSIX-style relative path) -> hex sha256.
    pub files: BTreeMap<String, String>,
}

impl Manifest {
    pub fn to_json(&self) -> String {
        // Match pack.py's wire format: 2-space indent, sorted keys via BTreeMap.
        serde_json::to_string_pretty(self).expect("serialize manifest")
    }

    pub fn from_json(raw: &str) -> Result<Self, CliError> {
        serde_json::from_str(raw).map_err(CliError::from)
    }
}

#[derive(Debug, Clone)]
pub struct PackedSkill {
    pub bytes: Vec<u8>,
    pub manifest: Manifest,
    pub sha256: String,
}

// ─── pack ──────────────────────────────────────────────────────────────────

/// Build a deterministic gzip-tar from `skill_dir`. Validates required files
/// are present; computes per-file sha256s; embeds `.knack/manifest.json`.
pub fn pack_skill(skill_dir: &Path) -> Result<PackedSkill, CliError> {
    if !skill_dir.is_dir() {
        return Err(CliError::User {
            code: "PACK_NOT_DIRECTORY".into(),
            message: format!("not a directory: {}", skill_dir.display()),
            hint: None,
        });
    }

    let entries = collect_entries(skill_dir)?;
    let entry_set: std::collections::HashSet<&str> =
        entries.iter().map(|(arc, _)| arc.as_str()).collect();
    for required in REQUIRED_FILES {
        if !entry_set.contains(*required) {
            return Err(CliError::User {
                code: "PACK_MISSING_REQUIRED".into(),
                message: format!("missing required file: {required}"),
                hint: Some("a skill folder needs SKILL.md and meta.knack.yaml".into()),
            });
        }
    }

    // Compute per-file sha256 in canonical order.
    let mut files: BTreeMap<String, String> = BTreeMap::new();
    for (arcname, abspath) in &entries {
        files.insert(arcname.clone(), sha256_file(abspath)?);
    }
    let manifest = Manifest {
        version: MANIFEST_VERSION,
        files,
    };
    let manifest_bytes = manifest.to_json().into_bytes();

    // Build the tarball into an in-memory buffer.
    let mut gz = GzEncoder::new(Vec::new(), Compression::new(6));
    {
        let mut builder = tar::Builder::new(&mut gz);
        // Deterministic mode: writing in canonical order; mtime=0 via headers.
        for (arcname, abspath) in &entries {
            let data = std::fs::read(abspath).map_err(CliError::from)?;
            append_file(&mut builder, arcname, &data)?;
        }
        append_file(&mut builder, MANIFEST_PATH, &manifest_bytes)?;
        builder.finish().map_err(CliError::from)?;
    }
    let raw = gz.finish().map_err(CliError::from)?;
    let sha256 = sha256_bytes(&raw);

    Ok(PackedSkill {
        bytes: raw,
        manifest,
        sha256,
    })
}

fn append_file<W: Write>(
    builder: &mut tar::Builder<W>,
    arcname: &str,
    data: &[u8],
) -> Result<(), CliError> {
    let mut header = tar::Header::new_ustar();
    header.set_path(arcname).map_err(CliError::from)?;
    header.set_size(data.len() as u64);
    header.set_mode(0o644);
    header.set_mtime(0);
    header.set_uid(0);
    header.set_gid(0);
    header.set_username("").ok();
    header.set_groupname("").ok();
    header.set_entry_type(tar::EntryType::Regular);
    header.set_cksum();
    builder.append(&header, data).map_err(CliError::from)?;
    Ok(())
}

fn collect_entries(root: &Path) -> Result<Vec<(String, PathBuf)>, CliError> {
    let mut out: Vec<(String, PathBuf)> = Vec::new();

    // Top-level required + optional files (in that order; final list is sorted
    // below anyway, so order here is just for explicitness).
    for name in REQUIRED_FILES.iter().chain(OPTIONAL_FILES.iter()) {
        let p = root.join(name);
        if p.is_file() {
            out.push(((*name).to_string(), p));
        }
    }

    // Optional directories — recurse, file-only, posix arcnames.
    for dirname in OPTIONAL_DIRS {
        let d = root.join(dirname);
        if !d.is_dir() {
            continue;
        }
        for entry in WalkDir::new(&d).sort_by_file_name() {
            let entry = entry.map_err(|e| CliError::User {
                code: "PACK_WALK_FAILED".into(),
                message: format!("walking {dirname}: {e}"),
                hint: None,
            })?;
            if !entry.file_type().is_file() {
                continue;
            }
            let rel = entry
                .path()
                .strip_prefix(root)
                .expect("walked path is under root");
            let arcname = rel
                .components()
                .map(|c| c.as_os_str().to_string_lossy().into_owned())
                .collect::<Vec<_>>()
                .join("/");
            out.push((arcname, entry.path().to_path_buf()));
        }
    }

    out.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(out)
}

fn sha256_file(path: &Path) -> Result<String, CliError> {
    let mut hasher = Sha256::new();
    let mut f = File::open(path).map_err(CliError::from)?;
    let mut buf = [0u8; 65536];
    loop {
        let n = f.read(&mut buf).map_err(CliError::from)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(hex::encode(hasher.finalize()))
}

fn sha256_bytes(data: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(data);
    hex::encode(hasher.finalize())
}

// ─── unpack ────────────────────────────────────────────────────────────────

/// Extract `tarball` into `target_dir`, verifying every sha256 against the
/// embedded manifest. Rejects path traversal and bundles whose manifest
/// version is newer than this build understands.
pub fn unpack_skill(tarball: &[u8], target_dir: &Path) -> Result<Manifest, CliError> {
    std::fs::create_dir_all(target_dir).map_err(CliError::from)?;

    let dec = GzDecoder::new(tarball);
    let mut archive = tar::Archive::new(dec);

    let mut manifest: Option<Manifest> = None;
    let mut members: BTreeMap<String, Vec<u8>> = BTreeMap::new();

    for entry in archive.entries().map_err(CliError::from)? {
        let mut entry = entry.map_err(CliError::from)?;
        if !entry.header().entry_type().is_file() {
            continue;
        }
        let path = entry
            .path()
            .map_err(CliError::from)?
            .into_owned();
        let arcname = posix_arcname(&path);
        check_safe_name(&arcname)?;

        let mut data = Vec::new();
        entry.read_to_end(&mut data).map_err(CliError::from)?;

        if arcname == MANIFEST_PATH {
            let parsed = Manifest::from_json(std::str::from_utf8(&data).map_err(|e| {
                CliError::User {
                    code: "UNPACK_BAD_MANIFEST".into(),
                    message: format!("manifest is not utf-8: {e}"),
                    hint: None,
                }
            })?)?;
            manifest = Some(parsed);
        } else {
            members.insert(arcname, data);
        }
    }

    let manifest = manifest.ok_or_else(|| CliError::User {
        code: "UNPACK_NO_MANIFEST".into(),
        message: format!("tarball is missing {MANIFEST_PATH}"),
        hint: None,
    })?;

    if manifest.version > MANIFEST_VERSION {
        return Err(CliError::User {
            code: "UNPACK_NEWER_MANIFEST".into(),
            message: format!(
                "manifest version {} is newer than this build understands (max {})",
                manifest.version, MANIFEST_VERSION
            ),
            hint: Some("upgrade your Knack CLI: irm https://getknack.ai/install.ps1 | iex".into()),
        });
    }

    // Verify the member set matches the manifest, then per-file sha256.
    let member_names: std::collections::BTreeSet<&String> = members.keys().collect();
    let manifest_names: std::collections::BTreeSet<&String> = manifest.files.keys().collect();
    if member_names != manifest_names {
        let extra: Vec<&String> = member_names.difference(&manifest_names).copied().collect();
        let missing: Vec<&String> = manifest_names.difference(&member_names).copied().collect();
        return Err(CliError::User {
            code: "UNPACK_MANIFEST_MISMATCH".into(),
            message: format!("manifest mismatch (extra={extra:?} missing={missing:?})"),
            hint: None,
        });
    }
    for (arcname, expected_hex) in &manifest.files {
        let actual = sha256_bytes(&members[arcname]);
        if &actual != expected_hex {
            return Err(CliError::User {
                code: "UNPACK_SHA256_MISMATCH".into(),
                message: format!("sha256 mismatch on {arcname}"),
                hint: None,
            });
        }
    }

    // Write to disk.
    for (arcname, data) in &members {
        let out = target_dir.join(arcname.replace('/', std::path::MAIN_SEPARATOR_STR));
        if let Some(parent) = out.parent() {
            std::fs::create_dir_all(parent).map_err(CliError::from)?;
        }
        std::fs::write(&out, data).map_err(CliError::from)?;
    }

    Ok(manifest)
}

fn posix_arcname(path: &Path) -> String {
    path.components()
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .collect::<Vec<_>>()
        .join("/")
}

fn check_safe_name(name: &str) -> Result<(), CliError> {
    if name.starts_with('/') || name.split('/').any(|c| c == "..") {
        return Err(CliError::User {
            code: "UNPACK_UNSAFE_NAME".into(),
            message: format!("unsafe tar entry: {name:?}"),
            hint: None,
        });
    }
    Ok(())
}

/// Canonical R2 key for a packed version. Mirrors `pack.py:packed_s3_key`.
pub fn packed_s3_key(skill_id: &str, version: &str) -> String {
    format!("skills/{skill_id}/{version}.tar.gz")
}

/// Minimal slice of a SKILL.md frontmatter — just the fields downstream
/// shim writers need. We don't model the whole Anthropic Skills schema
/// here; that's the server's job. If a SKILL.md author drops in extra
/// fields they get ignored, which matches the spec's tolerant-reader
/// posture.
#[derive(Debug, Clone, Deserialize, Serialize, Default, PartialEq, Eq)]
pub struct SkillFrontmatter {
    /// Slug. Required by the Anthropic spec; we tolerate missing values
    /// for backward compat (caller substitutes from the filesystem
    /// position when this is None).
    pub name: Option<String>,
    /// Description sentence — what gets read on agent session start and
    /// triggers progressive disclosure. Required for Claude Code shim
    /// writers to do anything useful.
    pub description: Option<String>,
}

/// Lift YAML frontmatter from the head of a SKILL.md body.
///
/// The shape we expect: file starts with `---\n`, has a YAML block,
/// closes with another `---\n` line, then continues with markdown.
/// Anything that doesn't open with `---` is treated as no-frontmatter
/// and returns `Ok(None)`. Malformed YAML returns `Err` so callers can
/// distinguish "no frontmatter to read" from "this skill is broken."
pub fn parse_skill_md_frontmatter(
    skill_md: &str,
) -> Result<Option<SkillFrontmatter>, CliError> {
    // Tolerate BOM + leading blank lines; Anthropic's reference writers
    // sometimes prepend a UTF-8 BOM and most editors round-trip it.
    let trimmed = skill_md.trim_start_matches('\u{feff}').trim_start();
    if !trimmed.starts_with("---") {
        return Ok(None);
    }
    // Find the closing `---` on its own line. We require a newline before
    // the closing fence to avoid greedy-matching `---` inside the body.
    let after_open = match trimmed.find('\n') {
        Some(i) => &trimmed[i + 1..],
        None => return Ok(None),
    };
    let mut end_offset: Option<usize> = None;
    for (i, line) in after_open.split_inclusive('\n').enumerate() {
        let s = line.trim_end_matches(['\r', '\n']);
        if s == "---" || s == "..." {
            // Compute byte offset of this line's start in after_open.
            let mut byte = 0usize;
            for (j, l) in after_open.split_inclusive('\n').enumerate() {
                if j == i {
                    break;
                }
                byte += l.len();
            }
            end_offset = Some(byte);
            break;
        }
    }
    let Some(end_offset) = end_offset else {
        // No closing fence — not a parseable frontmatter block.
        return Ok(None);
    };
    let yaml_body = &after_open[..end_offset];
    let parsed: SkillFrontmatter = serde_yaml::from_str(yaml_body)
        .map_err(|e| CliError::Internal(format!("SKILL.md frontmatter parse: {e}")))?;
    Ok(Some(parsed))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn write_minimal_skill(root: &Path) {
        std::fs::write(
            root.join("SKILL.md"),
            "---\nname: x\ndescription: y\n---\n\n# X\n",
        )
        .unwrap();
        std::fs::write(
            root.join("meta.knack.yaml"),
            "id: knack_x\nname: x\nslug: x\nauthor: a@b.c\n",
        )
        .unwrap();
    }

    #[test]
    fn pack_unpack_round_trip_minimal() {
        let dir = tempdir().unwrap();
        let src = dir.path().join("skill");
        std::fs::create_dir(&src).unwrap();
        write_minimal_skill(&src);

        let packed = pack_skill(&src).unwrap();
        assert!(packed.manifest.files.contains_key("SKILL.md"));
        assert!(packed.manifest.files.contains_key("meta.knack.yaml"));

        let dest = dir.path().join("out");
        let manifest = unpack_skill(&packed.bytes, &dest).unwrap();
        assert_eq!(manifest.version, 1);
        for arcname in manifest.files.keys() {
            let a = std::fs::read(src.join(arcname.replace('/', std::path::MAIN_SEPARATOR_STR)))
                .unwrap();
            let b = std::fs::read(dest.join(arcname.replace('/', std::path::MAIN_SEPARATOR_STR)))
                .unwrap();
            assert_eq!(a, b);
        }
    }

    #[test]
    fn pack_includes_full_anthropic_layout() {
        let dir = tempdir().unwrap();
        let src = dir.path().join("skill");
        std::fs::create_dir(&src).unwrap();
        write_minimal_skill(&src);
        std::fs::write(src.join("intuition.md"), "- be careful\n").unwrap();
        std::fs::create_dir(src.join("scripts")).unwrap();
        std::fs::write(src.join("scripts/fetch.py"), "print('hi')\n").unwrap();
        std::fs::create_dir(src.join("references")).unwrap();
        std::fs::write(src.join("references/policy.md"), "policy here\n").unwrap();
        std::fs::create_dir(src.join("examples")).unwrap();
        std::fs::write(src.join("examples/input.txt"), "hello\n").unwrap();

        let packed = pack_skill(&src).unwrap();
        for expected in [
            "SKILL.md",
            "meta.knack.yaml",
            "intuition.md",
            "scripts/fetch.py",
            "references/policy.md",
            "examples/input.txt",
        ] {
            assert!(
                packed.manifest.files.contains_key(expected),
                "missing {expected} from manifest"
            );
        }

        // Round-trip into a fresh dir and verify content equality.
        let out = dir.path().join("out");
        unpack_skill(&packed.bytes, &out).unwrap();
        for arcname in packed.manifest.files.keys() {
            let native = arcname.replace('/', std::path::MAIN_SEPARATOR_STR);
            assert_eq!(
                std::fs::read(src.join(&native)).unwrap(),
                std::fs::read(out.join(&native)).unwrap()
            );
        }
    }

    #[test]
    fn pack_rejects_missing_required() {
        let dir = tempdir().unwrap();
        let src = dir.path().join("skill");
        std::fs::create_dir(&src).unwrap();
        // Only SKILL.md, no meta.knack.yaml.
        std::fs::write(src.join("SKILL.md"), "# X\n").unwrap();
        let err = pack_skill(&src).unwrap_err();
        let code = format!("{:?}", err);
        assert!(code.contains("PACK_MISSING_REQUIRED"), "got {code}");
    }

    #[test]
    fn check_safe_name_rejects_traversal_and_absolute() {
        // We can't easily forge a malicious tarball through the rust `tar`
        // crate (it refuses to write paths containing `..` at pack time).
        // Instead, verify the unpack-time guard directly.
        for bad in ["../escape.txt", "/etc/passwd", "ok/../bad", "ok/../../oops"] {
            assert!(
                check_safe_name(bad).is_err(),
                "expected `{bad}` to be rejected"
            );
        }
        for good in ["SKILL.md", "scripts/fetch.py", "references/policy.md"] {
            assert!(check_safe_name(good).is_ok(), "expected `{good}` to pass");
        }
    }

    #[test]
    fn unpack_rejects_newer_manifest_version() {
        // Pack a real skill, then surgically rewrite the embedded manifest.
        let dir = tempdir().unwrap();
        let src = dir.path().join("skill");
        std::fs::create_dir(&src).unwrap();
        write_minimal_skill(&src);
        let packed = pack_skill(&src).unwrap();

        // Decode → tamper manifest version → re-encode.
        let mut tampered = Manifest {
            version: 99,
            files: packed.manifest.files.clone(),
        };
        tampered.files = packed.manifest.files.clone();

        let mut gz = GzEncoder::new(Vec::new(), Compression::new(6));
        {
            let mut b = tar::Builder::new(&mut gz);
            // Write the actual member files from the original tarball.
            let dec = GzDecoder::new(packed.bytes.as_slice());
            let mut a = tar::Archive::new(dec);
            for e in a.entries().unwrap() {
                let mut e = e.unwrap();
                let p = e.path().unwrap().into_owned();
                let arc = p.to_string_lossy().replace('\\', "/");
                if arc == MANIFEST_PATH {
                    continue;
                }
                let mut data = Vec::new();
                e.read_to_end(&mut data).unwrap();
                let mut h = tar::Header::new_ustar();
                h.set_path(&arc).unwrap();
                h.set_size(data.len() as u64);
                h.set_mode(0o644);
                h.set_mtime(0);
                h.set_cksum();
                b.append(&h, &data[..]).unwrap();
            }
            // Append tampered manifest.
            let mb = tampered.to_json().into_bytes();
            let mut h = tar::Header::new_ustar();
            h.set_path(MANIFEST_PATH).unwrap();
            h.set_size(mb.len() as u64);
            h.set_mode(0o644);
            h.set_mtime(0);
            h.set_cksum();
            b.append(&h, &mb[..]).unwrap();
            b.finish().unwrap();
        }
        let bytes = gz.finish().unwrap();

        let out = dir.path().join("out");
        let err = unpack_skill(&bytes, &out).unwrap_err();
        let code = format!("{:?}", err);
        assert!(code.contains("UNPACK_NEWER_MANIFEST"), "got {code}");
    }

    #[test]
    fn packed_s3_key_canonical() {
        assert_eq!(
            packed_s3_key("abc-123", "1.0.0"),
            "skills/abc-123/1.0.0.tar.gz"
        );
    }

    #[test]
    fn parse_frontmatter_returns_none_when_no_fence() {
        let body = "# Hello\n\nNo frontmatter here.\n";
        let parsed = parse_skill_md_frontmatter(body).unwrap();
        assert!(parsed.is_none());
    }

    #[test]
    fn parse_frontmatter_lifts_name_and_description() {
        let body =
            "---\nname: monthly-close\ndescription: Reconciles every Monday.\n---\n\n# Body\n";
        let parsed = parse_skill_md_frontmatter(body).unwrap().unwrap();
        assert_eq!(parsed.name.as_deref(), Some("monthly-close"));
        assert_eq!(
            parsed.description.as_deref(),
            Some("Reconciles every Monday.")
        );
    }

    #[test]
    fn parse_frontmatter_tolerates_bom_and_blank_lines() {
        let body = "\u{feff}\n---\nname: x\ndescription: y\n---\n\nBody\n";
        let parsed = parse_skill_md_frontmatter(body).unwrap().unwrap();
        assert_eq!(parsed.name.as_deref(), Some("x"));
        assert_eq!(parsed.description.as_deref(), Some("y"));
    }

    #[test]
    fn parse_frontmatter_ignores_extra_fields() {
        let body = "---\nname: x\ndescription: y\nversion: 1.2.3\nallowed-tools:\n  - Bash\n---\n\nBody\n";
        let parsed = parse_skill_md_frontmatter(body).unwrap().unwrap();
        assert_eq!(parsed.name.as_deref(), Some("x"));
    }

    #[test]
    fn parse_frontmatter_no_close_fence_returns_none() {
        let body = "---\nname: x\n(forgot the closing fence)\n";
        let parsed = parse_skill_md_frontmatter(body).unwrap();
        assert!(parsed.is_none());
    }
}
