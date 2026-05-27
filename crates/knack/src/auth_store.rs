//! Credential storage for the knack CLI.
//!
//! Three backends, used in this resolution order by [`crate::api::ApiClient`]:
//!
//!   1. [`FileStore`] — primary, post-0.5. Plaintext PAT in
//!      `~/.knack/auth.json` (`%USERPROFILE%\.knack\auth.json` on Windows).
//!      Atomic write via tempfile+rename. `0600` perms on Unix. Readable
//!      from sandboxed agents (Codex et al.) that mount the real user's
//!      HOME read-only — which is the whole point of this redesign.
//!   2. [`KeyringStore`] — legacy fallback. Pre-0.5 versions wrote JWT
//!      pairs here. We still READ from it so existing users don't break
//!      on upgrade; we never WRITE to it from a fresh `knack auth login`.
//!      Once the user re-runs login they're on the file store forever.
//!   3. [`MemoryStore`] — tests only. Lets the wiremock-driven integration
//!      tests roundtrip credentials without touching the real keyring or
//!      filesystem.
//!
//! The single [`TokenStore`] trait covers all three so [`crate::api::ApiClient`]
//! doesn't care which backend it's talking to. [`StoredCredential`] models
//! both PATs (new) and JWT pairs (legacy) with the JWT-specific fields
//! optional.

use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use keyring::Entry;
use serde::{Deserialize, Serialize};

use crate::errors::CliError;

const DEFAULT_ACCOUNT: &str = "default";
const FILE_SCHEMA_VERSION: u32 = 1;

/// A stored credential — either a long-lived Personal Access Token (PAT,
/// new default since 0.5) or a JWT pair from a legacy keyring entry.
///
/// PAT shape: `token = "knack_pat_..."`, `token_id = Some(_)`,
/// `refresh_token = None`, `expires_at = None` (unless the user opted into
/// expiry via `--expires-in-days`).
///
/// JWT shape: `token = "<jwt access>"`, `refresh_token = Some(_)`,
/// `expires_at = Some(_)`, `token_id = None`. Only seen when reading from
/// [`KeyringStore`] for backwards compat.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct StoredCredential {
    /// The bearer token used in `Authorization: Bearer <token>`. PAT or
    /// JWT access token depending on shape.
    ///
    /// Serde alias: pre-0.5 keyring entries serialize this as
    /// `access_token`; we accept both shapes on read so an upgrade-in-
    /// place doesn't lose existing logins.
    #[serde(alias = "access_token")]
    pub token: String,

    /// Server-side identifier for the PAT row in `cli_tokens`. Set when
    /// the credential is a PAT minted via `POST /me/cli-tokens`. Used by
    /// `knack auth logout` to revoke server-side via
    /// `DELETE /me/cli-tokens/{id}`. `None` for legacy JWT entries.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token_id: Option<String>,

    /// First ~16 chars of the token (e.g. `knack_pat_aBcDeF`). Display-
    /// only; never used for auth. Lets `knack auth status` show the
    /// active token without leaking the full secret.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prefix: Option<String>,

    /// JWT refresh token. Only set for legacy keyring entries. `None`
    /// for PATs (they don't need refreshing).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refresh_token: Option<String>,

    /// Unix timestamp (seconds) when the token expires. JWT access
    /// tokens always have this; PATs only have it when the user opted
    /// into expiry. `None` = never expires.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<i64>,

    /// User-assigned label for the PAT, shown in `getknack.ai/app/settings#cli-tokens`.
    /// Defaults to `knack-cli@<hostname>` at mint time.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,

    /// Cached user id (from `/auth/me`). Lets `status` show identity
    /// without a server roundtrip when the network is down.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user_id: Option<String>,

    /// Cached email. Same purpose as `user_id`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub email: Option<String>,
}

impl StoredCredential {
    /// Quick check for whether the stored credential is a PAT (vs a legacy
    /// JWT access token). Used by retry logic in `ApiClient` to decide
    /// whether a 401 is recoverable via `/auth/refresh` (JWT) or terminal
    /// (PAT — user must re-login).
    pub fn is_pat(&self) -> bool {
        self.token.starts_with("knack_pat_")
    }
}

/// Backend trait so tests can swap in an in-memory store.
pub trait TokenStore {
    fn load(&self, account: &str) -> Result<Option<StoredCredential>, CliError>;
    fn save(&self, account: &str, cred: &StoredCredential) -> Result<(), CliError>;
    fn clear(&self, account: &str) -> Result<(), CliError>;
}

// ─── FileStore ────────────────────────────────────────────────────────────
//
// Plaintext JSON at `~/.knack/auth.json`, multi-account, atomic writes,
// `0600` perms on Unix. The schema is `{ version, accounts: { <name>:
// <StoredCredential> }, updated_at }`.

#[derive(Debug, Clone, Serialize, Deserialize)]
struct AuthFile {
    version: u32,
    #[serde(default)]
    accounts: BTreeMap<String, StoredCredential>,
    #[serde(default)]
    updated_at: Option<DateTime<Utc>>,
}

impl Default for AuthFile {
    fn default() -> Self {
        Self {
            version: FILE_SCHEMA_VERSION,
            accounts: BTreeMap::new(),
            updated_at: None,
        }
    }
}

/// File-backed credential store. Path resolved by [`auth_file_path`].
#[derive(Debug, Clone)]
pub struct FileStore {
    pub path: PathBuf,
}

impl FileStore {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    /// Construct from the default resolution (env override, then
    /// `~/.knack/auth.json`). Returns `None` only on systems where neither
    /// home nor config dir is resolvable — very rare in practice.
    pub fn from_default_path() -> Option<Self> {
        auth_file_path().map(Self::new)
    }

    fn read_file(&self) -> Result<AuthFile, CliError> {
        match fs::read_to_string(&self.path) {
            Ok(s) if s.trim().is_empty() => Ok(AuthFile::default()),
            Ok(s) => {
                let file: AuthFile = serde_json::from_str(&s).map_err(|e| {
                    CliError::Internal(format!("could not parse {}: {e}", self.path.display()))
                })?;
                if file.version != FILE_SCHEMA_VERSION {
                    return Err(CliError::Internal(format!(
                        "{}: unsupported auth file version {} (expected {})",
                        self.path.display(),
                        file.version,
                        FILE_SCHEMA_VERSION
                    )));
                }
                Ok(file)
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(AuthFile::default()),
            Err(e) => Err(CliError::Internal(format!(
                "could not read {}: {e}",
                self.path.display()
            ))),
        }
    }

    fn write_file(&self, file: &AuthFile) -> Result<(), CliError> {
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent).map_err(|e| {
                CliError::Internal(format!("could not create {}: {e}", parent.display()))
            })?;
        }
        let body = serde_json::to_string_pretty(file)
            .map_err(|e| CliError::Internal(format!("serialize auth file: {e}")))?;
        let tmp = self
            .path
            .with_extension(format!("json.{}.tmp", std::process::id()));
        write_with_mode(&tmp, &body)?;
        fs::rename(&tmp, &self.path).map_err(|e| {
            CliError::Internal(format!(
                "could not rename {} → {}: {e}",
                tmp.display(),
                self.path.display()
            ))
        })?;
        Ok(())
    }
}

impl TokenStore for FileStore {
    fn load(&self, account: &str) -> Result<Option<StoredCredential>, CliError> {
        let account = account_or_default(account);
        let file = self.read_file()?;
        Ok(file.accounts.get(account).cloned())
    }

    fn save(&self, account: &str, cred: &StoredCredential) -> Result<(), CliError> {
        let account = account_or_default(account);
        let mut file = self.read_file()?;
        file.accounts.insert(account.to_string(), cred.clone());
        file.updated_at = Some(Utc::now());
        file.version = FILE_SCHEMA_VERSION;
        self.write_file(&file)
    }

    fn clear(&self, account: &str) -> Result<(), CliError> {
        let account = account_or_default(account);
        let mut file = match self.read_file() {
            Ok(f) => f,
            Err(_) => return Ok(()),
        };
        if file.accounts.remove(account).is_none() {
            return Ok(());
        }
        if file.accounts.is_empty() {
            match fs::remove_file(&self.path) {
                Ok(()) => Ok(()),
                Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
                Err(e) => Err(CliError::Internal(format!(
                    "could not remove {}: {e}",
                    self.path.display()
                ))),
            }
        } else {
            file.updated_at = Some(Utc::now());
            self.write_file(&file)
        }
    }
}

/// Resolve the default path for `auth.json`. Order:
///   1. `$KNACK_AUTH_FILE` (env override — tests + advanced users)
///   2. **Codex sandbox bridge**: if `$CODEX_HOME` is set and ends in
///      `.codex`, derive the real user's HOME from its parent and use
///      `<real_home>/.knack/auth.json`. This is the load-bearing case
///      for Codex on Windows, where the sandbox runs as a separate
///      Windows user (`CodexSandboxOffline`) and `%USERPROFILE%` inside
///      the sandbox resolves to the wrong profile. Codex's ACL setup
///      grants the sandbox user READ access to the real user's profile,
///      and `$CODEX_HOME` is the breadcrumb that points at it.
///   3. `$HOME/.knack/auth.json` (or `%USERPROFILE%\.knack\auth.json`)
///   4. `$XDG_CONFIG_HOME/knack/auth.json` (Linux last-resort)
pub fn auth_file_path() -> Option<PathBuf> {
    if let Ok(custom) = std::env::var("KNACK_AUTH_FILE") {
        return Some(PathBuf::from(custom));
    }
    if let Some(p) = codex_sandbox_real_home_auth_path() {
        return Some(p);
    }
    if let Some(home) = dirs::home_dir() {
        return Some(home.join(".knack").join("auth.json"));
    }
    dirs::config_dir().map(|c| c.join("knack").join("auth.json"))
}

/// Derive the real user's auth.json path from `$CODEX_HOME` when it looks
/// like Codex's default layout (`<real_home>/.codex`). Returns `None`
/// when `CODEX_HOME` is unset, doesn't end in `.codex`, or has no
/// resolvable parent.
///
/// Only kicks in when the Codex layout is recognizable — a customized
/// `CODEX_HOME=D:\custom\codex` would have us writing to `D:\custom\
/// .knack\auth.json`, which is probably NOT what the user wants. The
/// `.codex` basename check keeps the heuristic conservative.
fn codex_sandbox_real_home_auth_path() -> Option<PathBuf> {
    let codex_home = std::env::var("CODEX_HOME").ok()?;
    let codex_path = PathBuf::from(&codex_home);
    if codex_path.file_name() != Some(std::ffi::OsStr::new(".codex")) {
        return None;
    }
    let real_home = codex_path.parent()?;
    // Bail if parent is empty (e.g. CODEX_HOME=".codex") — that's
    // certainly not a HOME directory.
    if real_home.as_os_str().is_empty() {
        return None;
    }
    Some(real_home.join(".knack").join("auth.json"))
}

/// Write `body` to `path` with `0600` perms on Unix; default perms on
/// Windows. Caller is responsible for renaming into place atomically.
fn write_with_mode(path: &Path, body: &str) -> Result<(), CliError> {
    #[cfg(unix)]
    {
        use std::io::Write as _;
        use std::os::unix::fs::OpenOptionsExt;
        let mut f = fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(path)
            .map_err(|e| CliError::Internal(format!("open {}: {e}", path.display())))?;
        f.write_all(body.as_bytes())
            .map_err(|e| CliError::Internal(format!("write {}: {e}", path.display())))?;
    }
    #[cfg(not(unix))]
    {
        fs::write(path, body)
            .map_err(|e| CliError::Internal(format!("write {}: {e}", path.display())))?;
    }
    Ok(())
}

fn account_or_default(account: &str) -> &str {
    if account.is_empty() {
        DEFAULT_ACCOUNT
    } else {
        account
    }
}

// ─── KeyringStore (legacy) ────────────────────────────────────────────────

/// OS-keyring-backed store. Pre-0.5 versions used this as the primary
/// credential store; 0.5+ uses it as a read-only fallback so existing
/// users don't break on upgrade. Once they re-run `knack auth login`,
/// the file store takes over.
#[derive(Debug, Clone)]
pub struct KeyringStore {
    pub service: String,
}

impl KeyringStore {
    pub fn new(service: impl Into<String>) -> Self {
        Self {
            service: service.into(),
        }
    }

    fn entry(&self, account: &str) -> Result<Entry, CliError> {
        Entry::new(&self.service, account).map_err(CliError::from)
    }
}

impl TokenStore for KeyringStore {
    fn load(&self, account: &str) -> Result<Option<StoredCredential>, CliError> {
        let account = account_or_default(account);
        let entry = self.entry(account)?;
        match entry.get_password() {
            Ok(blob) => {
                let cred: StoredCredential = serde_json::from_str(&blob)?;
                Ok(Some(cred))
            }
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(e) => Err(CliError::from(e)),
        }
    }

    fn save(&self, account: &str, cred: &StoredCredential) -> Result<(), CliError> {
        let account = account_or_default(account);
        let entry = self.entry(account)?;
        let blob = serde_json::to_string(cred)?;
        entry.set_password(&blob)?;
        Ok(())
    }

    fn clear(&self, account: &str) -> Result<(), CliError> {
        let account = account_or_default(account);
        let entry = self.entry(account)?;
        match entry.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(e) => Err(CliError::from(e)),
        }
    }
}

// ─── MemoryStore (tests) ──────────────────────────────────────────────────

/// In-memory store for tests and `--auth-token` overrides where we don't
/// want to touch the real keyring or filesystem.
#[derive(Debug, Default)]
pub struct MemoryStore {
    inner: std::sync::Mutex<std::collections::HashMap<String, StoredCredential>>,
}

impl MemoryStore {
    pub fn new() -> Self {
        Self::default()
    }
}

impl TokenStore for MemoryStore {
    fn load(&self, account: &str) -> Result<Option<StoredCredential>, CliError> {
        let g = self.inner.lock().unwrap();
        Ok(g.get(account).cloned())
    }

    fn save(&self, account: &str, cred: &StoredCredential) -> Result<(), CliError> {
        let mut g = self.inner.lock().unwrap();
        g.insert(account.to_string(), cred.clone());
        Ok(())
    }

    fn clear(&self, account: &str) -> Result<(), CliError> {
        let mut g = self.inner.lock().unwrap();
        g.remove(account);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn sample_pat() -> StoredCredential {
        StoredCredential {
            token: "knack_pat_aBcDeF1234567890ghijklmnopqrstuvwxyz_abc".into(),
            token_id: Some("tok_123".into()),
            prefix: Some("knack_pat_aBcDeF".into()),
            refresh_token: None,
            expires_at: None,
            label: Some("knack-cli@hostname".into()),
            user_id: Some("u_42".into()),
            email: Some("jordan@example.com".into()),
        }
    }

    fn sample_legacy_jwt() -> StoredCredential {
        StoredCredential {
            token: "header.payload.sig".into(),
            token_id: None,
            prefix: None,
            refresh_token: Some("rrr".into()),
            expires_at: Some(1_900_000_000),
            label: None,
            user_id: None,
            email: None,
        }
    }

    /// Point ``auth_file_path`` at a fresh tempdir per test so we never
    /// touch the developer's real `~/.knack/auth.json`.
    fn isolate_file_store() -> (TempDir, PathBuf) {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("auth.json");
        unsafe {
            std::env::set_var("KNACK_AUTH_FILE", &path);
        }
        (dir, path)
    }

    #[test]
    fn memory_store_round_trips() {
        let store = MemoryStore::new();
        assert!(store.load("default").unwrap().is_none());
        store.save("default", &sample_pat()).unwrap();
        assert_eq!(store.load("default").unwrap().unwrap(), sample_pat());
        store.clear("default").unwrap();
        assert!(store.load("default").unwrap().is_none());
    }

    #[test]
    fn memory_store_isolates_accounts() {
        let store = MemoryStore::new();
        let mut t = sample_pat();
        store.save("work", &t).unwrap();
        t.token = "knack_pat_personal_xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx".into();
        store.save("personal", &t).unwrap();
        assert_eq!(
            store.load("work").unwrap().unwrap().token,
            "knack_pat_aBcDeF1234567890ghijklmnopqrstuvwxyz_abc"
        );
        assert_eq!(
            store.load("personal").unwrap().unwrap().token,
            "knack_pat_personal_xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx"
        );
    }

    #[test]
    fn file_store_round_trips() {
        let (_dir, path) = isolate_file_store();
        let store = FileStore::new(&path);
        assert!(store.load("default").unwrap().is_none());
        store.save("default", &sample_pat()).unwrap();
        assert!(path.exists());
        assert_eq!(store.load("default").unwrap().unwrap(), sample_pat());
        store.clear("default").unwrap();
        // Clearing the last account deletes the file entirely.
        assert!(!path.exists());
        assert!(store.load("default").unwrap().is_none());
    }

    #[test]
    fn file_store_isolates_accounts() {
        let (_dir, path) = isolate_file_store();
        let store = FileStore::new(&path);
        let mut work = sample_pat();
        work.label = Some("work".into());
        let mut personal = sample_pat();
        personal.token = "knack_pat_xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx".into();
        personal.label = Some("personal".into());
        store.save("work", &work).unwrap();
        store.save("personal", &personal).unwrap();
        assert_eq!(
            store.load("work").unwrap().unwrap().label.as_deref(),
            Some("work")
        );
        assert_eq!(
            store.load("personal").unwrap().unwrap().label.as_deref(),
            Some("personal")
        );
        // Clearing one keeps the other.
        store.clear("work").unwrap();
        assert!(store.load("work").unwrap().is_none());
        assert!(store.load("personal").unwrap().is_some());
        assert!(path.exists());
    }

    #[test]
    fn file_store_atomic_write_leaves_no_tempfiles() {
        let (_dir, path) = isolate_file_store();
        let store = FileStore::new(&path);
        store.save("default", &sample_pat()).unwrap();
        let mut second = sample_pat();
        second.token_id = Some("tok_999".into());
        store.save("default", &second).unwrap();
        // Tempfile must not leak in the parent dir.
        let leftovers: Vec<_> = fs::read_dir(path.parent().unwrap())
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_string_lossy().contains(".tmp"))
            .collect();
        assert!(leftovers.is_empty(), "tempfile leaked: {leftovers:?}");
    }

    #[test]
    fn file_store_missing_file_returns_none() {
        let (_dir, path) = isolate_file_store();
        let store = FileStore::new(&path);
        assert!(!path.exists());
        assert!(store.load("default").unwrap().is_none());
    }

    #[test]
    fn file_store_rejects_unknown_schema_version() {
        let (_dir, path) = isolate_file_store();
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, r#"{"version": 999, "accounts": {}}"#).unwrap();
        let store = FileStore::new(&path);
        let err = store.load("default").unwrap_err();
        // Surface as Internal so the user sees a clear message rather than
        // silently truncating their auth file.
        assert!(format!("{err:?}").contains("unsupported auth file version"));
    }

    #[test]
    fn file_store_accepts_legacy_access_token_field() {
        // Pre-0.5 keyring entries had the field named `access_token`. The
        // serde alias on StoredCredential::token means a hand-migrated
        // file from those entries still parses correctly.
        let (_dir, path) = isolate_file_store();
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(
            &path,
            r#"{"version": 1, "accounts": {"default": {"access_token": "header.payload.sig", "refresh_token": "rrr", "expires_at": 1900000000}}}"#,
        )
        .unwrap();
        let store = FileStore::new(&path);
        let loaded = store.load("default").unwrap().unwrap();
        assert_eq!(loaded.token, "header.payload.sig");
        assert_eq!(loaded.refresh_token.as_deref(), Some("rrr"));
    }

    #[cfg(unix)]
    #[test]
    fn file_store_writes_with_0600_perms() {
        use std::os::unix::fs::PermissionsExt;
        let (_dir, path) = isolate_file_store();
        let store = FileStore::new(&path);
        store.save("default", &sample_pat()).unwrap();
        let mode = fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "expected 0600, got {mode:o}");
    }

    #[test]
    fn stored_credential_is_pat_detection() {
        assert!(sample_pat().is_pat());
        assert!(!sample_legacy_jwt().is_pat());
    }

    /// Guard against shared `KNACK_AUTH_FILE` / `CODEX_HOME` env vars
    /// leaking between tests in this module. Each codex-resolution test
    /// sets `CODEX_HOME` then asserts; tearing down here keeps neighbors
    /// (and the rest of the test suite) from observing the leftover.
    fn clear_codex_env() {
        unsafe {
            std::env::remove_var("CODEX_HOME");
            std::env::remove_var("KNACK_AUTH_FILE");
        }
    }

    #[test]
    fn auth_file_path_uses_codex_sandbox_fallback_when_codex_home_ends_in_dot_codex() {
        clear_codex_env();
        // Simulate Codex sandbox: CODEX_HOME points at the real user's
        // .codex even though dirs::home_dir() inside the sandbox would
        // be the sandbox user's profile. The CLI should derive the real
        // HOME from CODEX_HOME's parent.
        let real_home = if cfg!(windows) {
            r"C:\Users\Jordan"
        } else {
            "/home/jordan"
        };
        let codex_home = if cfg!(windows) {
            r"C:\Users\Jordan\.codex"
        } else {
            "/home/jordan/.codex"
        };
        unsafe {
            std::env::set_var("CODEX_HOME", codex_home);
        }
        let resolved = auth_file_path().expect("path resolves");
        let expected = PathBuf::from(real_home).join(".knack").join("auth.json");
        assert_eq!(resolved, expected);
        clear_codex_env();
    }

    #[test]
    fn auth_file_path_ignores_codex_home_with_nonstandard_basename() {
        clear_codex_env();
        // Custom CODEX_HOME that doesn't end in `.codex` — could be a
        // user-customized layout. We should NOT assume `parent()` is a
        // HOME directory in that case; fall through to dirs::home_dir().
        let custom = if cfg!(windows) {
            r"D:\custom\codexstuff"
        } else {
            "/srv/codexstuff"
        };
        unsafe {
            std::env::set_var("CODEX_HOME", custom);
        }
        let resolved = auth_file_path().expect("path resolves");
        // The resolved path should NOT live under D:\custom or /srv —
        // it should be dirs::home_dir()-based.
        assert!(
            !resolved.starts_with(PathBuf::from(custom).parent().unwrap()),
            "expected dirs::home_dir fallback, got {resolved:?}"
        );
        clear_codex_env();
    }

    #[test]
    fn auth_file_path_env_override_beats_codex_fallback() {
        clear_codex_env();
        // Explicit KNACK_AUTH_FILE wins over both the Codex fallback
        // and the dirs::home_dir() default — used by tests and by
        // power users who want to keep the auth file in a non-default
        // location.
        let custom_path = "/tmp/custom/auth.json";
        let codex_home = if cfg!(windows) {
            r"C:\Users\Jordan\.codex"
        } else {
            "/home/jordan/.codex"
        };
        unsafe {
            std::env::set_var("KNACK_AUTH_FILE", custom_path);
            std::env::set_var("CODEX_HOME", codex_home);
        }
        let resolved = auth_file_path().expect("path resolves");
        assert_eq!(resolved, PathBuf::from(custom_path));
        clear_codex_env();
    }
}
