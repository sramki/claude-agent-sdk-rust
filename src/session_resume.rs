//! Materialize a [`SessionStore`]-backed resume into a temp `CLAUDE_CONFIG_DIR`.
//!
//! Faithful port of `_internal/session_resume.py`. When `options.resume` (or
//! `continue_conversation`) is paired with `options.session_store`, the session
//! usually lives only in the external store. The CLI can only resume from a
//! local file, so this loads the session (+ subagents) from the store, writes it
//! to a temp directory laid out like `~/.claude/`, and repoints the subprocess
//! at it via `CLAUDE_CONFIG_DIR`.

use std::collections::HashMap;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde_json::{Map, Value};
use tempfile::TempDir;

use crate::error::{Error, Result};
use crate::paths::validate_uuid;
use crate::types::{
    ClaudeAgentOptions, SessionKey, SessionListSubkeysKey, SessionStore, SessionStoreEntry,
};

/// Result of [`materialize_resume_session`]. The temp directory is removed when
/// this is dropped, so keep it alive until the subprocess exits.
pub struct MaterializedResume {
    _tmp: TempDir,
    /// Temp directory laid out like `~/.claude/` (`CLAUDE_CONFIG_DIR`).
    pub config_dir: PathBuf,
    /// Session id to pass as `--resume`.
    pub resume_session_id: String,
}

/// Repoints `options` at a materialized temp config dir: sets
/// `CLAUDE_CONFIG_DIR` in `env`, `resume` to the materialized session id, and
/// clears `continue_conversation`. Mirrors `apply_materialized_options`.
pub fn apply_materialized_options(
    mut options: ClaudeAgentOptions,
    materialized: &MaterializedResume,
) -> ClaudeAgentOptions {
    options.env.insert(
        "CLAUDE_CONFIG_DIR".into(),
        materialized.config_dir.to_string_lossy().into_owned(),
    );
    options.resume = Some(materialized.resume_session_id.clone());
    options.continue_conversation = false;
    options
}

/// Loads a session from `options.session_store` and writes it to a temp dir.
/// Returns `None` when no materialization is needed (no store, no
/// resume/continue, empty store, or a non-UUID resume id). Mirrors
/// `materialize_resume_session`.
pub async fn materialize_resume_session(
    options: &ClaudeAgentOptions,
) -> Result<Option<MaterializedResume>> {
    let store = match &options.session_store {
        Some(s) => s.as_ref(),
        None => return Ok(None),
    };
    if options.resume.is_none() && !options.continue_conversation {
        return Ok(None);
    }

    let timeout = Duration::from_millis(options.load_timeout_ms.max(0) as u64);
    let project_key = crate::project_key_for_directory(options.cwd.as_deref());

    let resolved = if let Some(resume) = &options.resume {
        if !validate_uuid(resume) {
            return Ok(None);
        }
        load_candidate(store, &project_key, resume, timeout).await?
    } else {
        resolve_continue_candidate(store, &project_key, timeout).await?
    };
    let (session_id, entries) = match resolved {
        Some(r) => r,
        None => return Ok(None),
    };

    let tmp = tempfile::Builder::new()
        .prefix("claude-resume-")
        .tempdir()
        .map_err(Error::Io)?;
    let tmp_base = tmp.path().to_path_buf();

    // Any failure below leaves the temp dir; `tmp` (TempDir) removes it on drop,
    // including on the early `?` returns here.
    let project_dir = tmp_base.join("projects").join(&project_key);
    std::fs::create_dir_all(&project_dir).map_err(Error::Io)?;
    write_jsonl(&project_dir.join(format!("{session_id}.jsonl")), &entries)?;

    copy_auth_files(&tmp_base, &options.env);

    materialize_subkeys(store, &project_dir, &project_key, &session_id, timeout).await?;

    Ok(Some(MaterializedResume {
        _tmp: tmp,
        config_dir: tmp_base,
        resume_session_id: session_id,
    }))
}

// ---------------------------------------------------------------------------
// Candidate resolution
// ---------------------------------------------------------------------------

async fn timed<T, F>(fut: F, timeout: Duration, what: &str) -> Result<T>
where
    F: Future<Output = Result<T>>,
{
    match tokio::time::timeout(timeout, fut).await {
        Ok(inner) => inner,
        Err(_) => Err(Error::connection(format!(
            "{what} timed out after {}ms during resume materialization",
            timeout.as_millis()
        ))),
    }
}

async fn load_candidate(
    store: &dyn SessionStore,
    project_key: &str,
    session_id: &str,
    timeout: Duration,
) -> Result<Option<(String, Vec<SessionStoreEntry>)>> {
    let key = SessionKey {
        project_key: project_key.to_string(),
        session_id: session_id.to_string(),
        subpath: None,
    };
    let entries = timed(
        store.load(&key),
        timeout,
        &format!("SessionStore.load() for session {session_id}"),
    )
    .await?;
    Ok(match entries {
        Some(e) if !e.is_empty() => Some((session_id.to_string(), e)),
        _ => None,
    })
}

async fn resolve_continue_candidate(
    store: &dyn SessionStore,
    project_key: &str,
    timeout: Duration,
) -> Result<Option<(String, Vec<SessionStoreEntry>)>> {
    let mut sessions = match timed(
        store.list_sessions(project_key),
        timeout,
        "SessionStore.list_sessions()",
    )
    .await
    {
        Ok(s) => s,
        Err(Error::Unsupported(_)) => {
            return Err(Error::Invalid(
                "continue_conversation with session_store requires the store to implement list_sessions()".into(),
            ))
        }
        Err(e) => return Err(e),
    };
    if sessions.is_empty() {
        return Ok(None);
    }
    sessions.sort_by(|a, b| b.mtime.cmp(&a.mtime));
    for cand in sessions {
        if !validate_uuid(&cand.session_id) {
            continue;
        }
        if let Some((sid, entries)) =
            load_candidate(store, project_key, &cand.session_id, timeout).await?
        {
            let is_sidechain = entries
                .first()
                .and_then(|e| e.get("isSidechain"))
                == Some(&Value::Bool(true));
            if is_sidechain {
                continue;
            }
            return Ok(Some((sid, entries)));
        }
    }
    Ok(None)
}

// ---------------------------------------------------------------------------
// Filesystem materialization
// ---------------------------------------------------------------------------

fn write_jsonl(path: &Path, entries: &[SessionStoreEntry]) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(Error::Io)?;
    }
    let mut out = String::new();
    for e in entries {
        out.push_str(&Value::Object(e.clone()).to_string());
        out.push('\n');
    }
    std::fs::write(path, out).map_err(Error::Io)?;
    set_mode_600(path);
    Ok(())
}

#[cfg(unix)]
fn set_mode_600(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
}
#[cfg(not(unix))]
fn set_mode_600(_path: &Path) {}

async fn materialize_subkeys(
    store: &dyn SessionStore,
    project_dir: &Path,
    project_key: &str,
    session_id: &str,
    timeout: Duration,
) -> Result<()> {
    let session_dir = project_dir.join(session_id);
    let subkeys = match timed(
        store.list_subkeys(&SessionListSubkeysKey {
            project_key: project_key.to_string(),
            session_id: session_id.to_string(),
        }),
        timeout,
        &format!("SessionStore.list_subkeys() for session {session_id}"),
    )
    .await
    {
        Ok(sk) => sk,
        Err(Error::Unsupported(_)) => return Ok(()),
        Err(e) => return Err(e),
    };

    for subpath in subkeys {
        if !is_safe_subpath(&subpath) {
            continue;
        }
        let sub_entries = timed(
            store.load(&SessionKey {
                project_key: project_key.to_string(),
                session_id: session_id.to_string(),
                subpath: Some(subpath.clone()),
            }),
            timeout,
            &format!("SessionStore.load() for session {session_id} subpath {subpath}"),
        )
        .await?;
        let sub_entries = match sub_entries {
            Some(e) if !e.is_empty() => e,
            _ => continue,
        };

        let mut metadata: Vec<SessionStoreEntry> = Vec::new();
        let mut transcript: Vec<SessionStoreEntry> = Vec::new();
        for e in sub_entries {
            if e.get("type").and_then(Value::as_str) == Some("agent_metadata") {
                metadata.push(e);
            } else {
                transcript.push(e);
            }
        }

        let sub_file = append_ext(&session_dir.join(&subpath), "jsonl");
        if !transcript.is_empty() {
            write_jsonl(&sub_file, &transcript)?;
        }
        if let Some(last) = metadata.last() {
            let mut meta: Map<String, Value> = last.clone();
            meta.remove("type");
            let meta_file = sub_file.with_extension("meta.json");
            if let Some(parent) = meta_file.parent() {
                std::fs::create_dir_all(parent).map_err(Error::Io)?;
            }
            std::fs::write(&meta_file, Value::Object(meta).to_string()).map_err(Error::Io)?;
            set_mode_600(&meta_file);
        }
    }
    Ok(())
}

/// Appends an extension to a path (`foo` + `jsonl` -> `foo.jsonl`), without
/// replacing an existing extension (unlike `with_extension`).
fn append_ext(path: &Path, ext: &str) -> PathBuf {
    let mut s = path.as_os_str().to_os_string();
    s.push(".");
    s.push(ext);
    PathBuf::from(s)
}

/// Rejects subpaths that are empty, absolute, drive-prefixed, or contain `..`
/// / NUL — anything that could escape the session dir. Mirrors `_is_safe_subpath`.
fn is_safe_subpath(subpath: &str) -> bool {
    if subpath.is_empty() || subpath.contains('\0') {
        return false;
    }
    if subpath.starts_with('/') || subpath.starts_with('\\') {
        return false;
    }
    // Windows drive prefix (e.g. `C:foo`).
    let bytes = subpath.as_bytes();
    if bytes.len() >= 2 && bytes[1] == b':' && bytes[0].is_ascii_alphabetic() {
        return false;
    }
    subpath
        .split(['/', '\\'])
        .all(|p| p != "." && p != "..")
}

// ---------------------------------------------------------------------------
// Auth file copying
// ---------------------------------------------------------------------------

const KEYCHAIN_SERVICE_NAME: &str = "Claude Code-credentials";

fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("USERPROFILE").filter(|v| !v.is_empty()).map(PathBuf::from))
}

/// Copies `.credentials.json` (refreshToken redacted) and `.claude.json` from
/// the caller's effective config into the temp dir. Mirrors `_copy_auth_files`.
fn copy_auth_files(tmp_base: &Path, opt_env: &HashMap<String, String>) {
    let caller_config_dir = opt_env
        .get("CLAUDE_CONFIG_DIR")
        .cloned()
        .or_else(|| std::env::var("CLAUDE_CONFIG_DIR").ok());
    let source_config_dir = caller_config_dir
        .as_ref()
        .map(PathBuf::from)
        .or_else(|| home_dir().map(|h| h.join(".claude")))
        .unwrap_or_default();

    let mut creds_json = std::fs::read_to_string(source_config_dir.join(".credentials.json")).ok();

    // macOS Keychain fallback when using default config + OAuth (no env auth).
    let has_env_auth = opt_env.contains_key("ANTHROPIC_API_KEY")
        || std::env::var_os("ANTHROPIC_API_KEY").is_some()
        || opt_env.contains_key("CLAUDE_CODE_OAUTH_TOKEN")
        || std::env::var_os("CLAUDE_CODE_OAUTH_TOKEN").is_some();
    if caller_config_dir.is_none() && !has_env_auth {
        if let Some(keychain) = read_keychain_credentials() {
            creds_json = Some(keychain);
        }
    }

    write_redacted_credentials(creds_json.as_deref(), &tmp_base.join(".credentials.json"));

    let claude_json_src = match &caller_config_dir {
        Some(dir) => PathBuf::from(dir).join(".claude.json"),
        None => home_dir().map(|h| h.join(".claude.json")).unwrap_or_default(),
    };
    if let Ok(bytes) = std::fs::read(&claude_json_src) {
        let _ = std::fs::write(tmp_base.join(".claude.json"), bytes);
    }
}

fn write_redacted_credentials(creds_json: Option<&str>, dst: &Path) {
    let Some(creds) = creds_json else { return };
    let out = match serde_json::from_str::<Value>(creds) {
        Ok(mut data) => {
            if let Some(oauth) = data
                .get_mut("claudeAiOauth")
                .and_then(Value::as_object_mut)
            {
                oauth.remove("refreshToken");
            }
            data.to_string()
        }
        Err(_) => creds.to_string(),
    };
    if std::fs::write(dst, out).is_ok() {
        set_mode_600(dst);
    }
}

#[cfg(target_os = "macos")]
fn read_keychain_credentials() -> Option<String> {
    let user = std::env::var("USER").unwrap_or_else(|_| "claude-code-user".into());
    let output = std::process::Command::new("security")
        .args(["find-generic-password", "-a", &user, "-w", "-s", KEYCHAIN_SERVICE_NAME])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let out = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if out.is_empty() {
        None
    } else {
        Some(out)
    }
}

#[cfg(not(target_os = "macos"))]
fn read_keychain_credentials() -> Option<String> {
    let _ = KEYCHAIN_SERVICE_NAME;
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn is_safe_subpath_cases() {
        assert!(is_safe_subpath("subagents/agent-abc"));
        assert!(is_safe_subpath("subagents/workflows/run-1/agent-x"));
        assert!(!is_safe_subpath(""));
        assert!(!is_safe_subpath("/etc/passwd"));
        assert!(!is_safe_subpath("../escape"));
        assert!(!is_safe_subpath("a/../b"));
        assert!(!is_safe_subpath("C:foo"));
        assert!(!is_safe_subpath("a\0b"));
    }

    #[test]
    fn append_ext_does_not_replace() {
        assert_eq!(append_ext(Path::new("a/b"), "jsonl"), PathBuf::from("a/b.jsonl"));
        assert_eq!(append_ext(Path::new("agent-abc"), "jsonl"), PathBuf::from("agent-abc.jsonl"));
    }

    #[test]
    fn redacts_refresh_token() {
        let dir = tempfile::tempdir().unwrap();
        let dst = dir.path().join(".credentials.json");
        write_redacted_credentials(
            Some(r#"{"claudeAiOauth":{"accessToken":"a","refreshToken":"r"}}"#),
            &dst,
        );
        let written = std::fs::read_to_string(&dst).unwrap();
        assert!(written.contains("accessToken"));
        assert!(!written.contains("refreshToken"));
    }

    // --- materialize_resume_session -----------------------------------------

    use crate::store::InMemorySessionStore;
    use crate::types::{SessionStore, SessionStoreFlushMode};
    use serde_json::json;
    use std::sync::Arc;

    const DIR: &str = "/workspace/resume-proj";
    const SID: &str = "aaaaaaaa-1111-4111-8111-aaaaaaaaaaaa";

    fn entry(v: Value) -> SessionStoreEntry {
        v.as_object().cloned().unwrap()
    }

    fn key_for(sid: &str, subpath: Option<&str>) -> SessionKey {
        SessionKey {
            project_key: crate::project_key_for_directory(Some(Path::new(DIR))),
            session_id: sid.to_string(),
            subpath: subpath.map(str::to_string),
        }
    }

    /// Options with a store + an empty CLAUDE_CONFIG_DIR (so auth-copy reads no
    /// real credentials).
    fn opts(store: Arc<dyn SessionStore>, empty_cfg: &Path) -> ClaudeAgentOptions {
        let mut o = ClaudeAgentOptions {
            session_store: Some(store),
            session_store_flush: SessionStoreFlushMode::Batched,
            cwd: Some(PathBuf::from(DIR)),
            load_timeout_ms: 60_000,
            ..Default::default()
        };
        o.env
            .insert("CLAUDE_CONFIG_DIR".into(), empty_cfg.to_string_lossy().into_owned());
        o
    }

    #[tokio::test]
    async fn materialize_writes_session_and_subagents() {
        let empty_cfg = tempfile::tempdir().unwrap();
        let store = Arc::new(InMemorySessionStore::new());
        store
            .append(
                &key_for(SID, None),
                &[
                    entry(json!({"type": "user", "uuid": "u1", "parentUuid": null, "sessionId": SID, "message": {"content": "hi"}})),
                    entry(json!({"type": "assistant", "uuid": "a1", "parentUuid": "u1", "sessionId": SID, "message": {"content": "yo"}})),
                ],
            )
            .await
            .unwrap();
        store
            .append(
                &key_for(SID, Some("subagents/agent-x")),
                &[
                    entry(json!({"type": "agent_metadata", "agentType": "gp"})),
                    entry(json!({"type": "user", "uuid": "s1", "parentUuid": null, "sessionId": SID, "message": {"content": "task"}})),
                ],
            )
            .await
            .unwrap();

        let store_dyn: Arc<dyn SessionStore> = store;
        let mut options = opts(store_dyn, empty_cfg.path());
        options.resume = Some(SID.into());
        let m = materialize_resume_session(&options).await.unwrap().unwrap();
        assert_eq!(m.resume_session_id, SID);

        let pk = crate::project_key_for_directory(Some(Path::new(DIR)));
        let main = m.config_dir.join("projects").join(&pk).join(format!("{SID}.jsonl"));
        let text = std::fs::read_to_string(&main).unwrap();
        assert_eq!(text.lines().count(), 2);

        let sub = m
            .config_dir
            .join("projects")
            .join(&pk)
            .join(SID)
            .join("subagents")
            .join("agent-x.jsonl");
        assert!(sub.exists(), "subagent transcript should be materialized");
        let meta = sub.with_extension("meta.json");
        assert!(meta.exists());
        let meta_text = std::fs::read_to_string(&meta).unwrap();
        assert!(meta_text.contains("agentType"));
        assert!(!meta_text.contains("agent_metadata")); // synthetic type stripped
    }

    #[tokio::test]
    async fn continue_picks_most_recent_non_sidechain() {
        let empty_cfg = tempfile::tempdir().unwrap();
        let store = Arc::new(InMemorySessionStore::new());
        let normal = "bbbbbbbb-1111-4111-8111-bbbbbbbbbbbb";
        let side = "cccccccc-1111-4111-8111-cccccccccccc";
        store
            .append(&key_for(normal, None), &[entry(json!({"type": "user", "uuid": "n1", "parentUuid": null, "sessionId": normal, "message": {"content": "real"}}))])
            .await
            .unwrap();
        // Sidechain appended LAST -> highest mtime, but must be skipped.
        store
            .append(&key_for(side, None), &[entry(json!({"type": "user", "uuid": "x1", "parentUuid": null, "sessionId": side, "isSidechain": true, "message": {"content": "side"}}))])
            .await
            .unwrap();

        let store_dyn: Arc<dyn SessionStore> = store;
        let mut options = opts(store_dyn, empty_cfg.path());
        options.continue_conversation = true;
        let m = materialize_resume_session(&options).await.unwrap().unwrap();
        assert_eq!(m.resume_session_id, normal);
    }

    #[tokio::test]
    async fn materialize_none_cases() {
        let empty_cfg = tempfile::tempdir().unwrap();
        let store = Arc::new(InMemorySessionStore::new());
        let store_dyn: Arc<dyn SessionStore> = store;

        // No resume/continue -> None.
        let o = opts(store_dyn.clone(), empty_cfg.path());
        assert!(materialize_resume_session(&o).await.unwrap().is_none());

        // No store -> None.
        let o2 = ClaudeAgentOptions {
            resume: Some(SID.into()),
            ..Default::default()
        };
        assert!(materialize_resume_session(&o2).await.unwrap().is_none());

        // Non-UUID resume -> None.
        let mut o3 = opts(store_dyn.clone(), empty_cfg.path());
        o3.resume = Some("not-a-uuid".into());
        assert!(materialize_resume_session(&o3).await.unwrap().is_none());

        // resume for an absent session -> None.
        let mut o4 = opts(store_dyn, empty_cfg.path());
        o4.resume = Some(SID.into());
        assert!(materialize_resume_session(&o4).await.unwrap().is_none());
    }

    #[test]
    fn apply_options_sets_env_resume_continue() {
        let tmp = tempfile::tempdir().unwrap();
        let m = MaterializedResume {
            _tmp: tempfile::tempdir().unwrap(),
            config_dir: tmp.path().to_path_buf(),
            resume_session_id: SID.into(),
        };
        let options = ClaudeAgentOptions {
            continue_conversation: true,
            ..Default::default()
        };
        let out = apply_materialized_options(options, &m);
        assert_eq!(out.resume.as_deref(), Some(SID));
        assert!(!out.continue_conversation);
        assert_eq!(
            out.env.get("CLAUDE_CONFIG_DIR").map(String::as_str),
            Some(tmp.path().to_string_lossy().as_ref())
        );
    }

    #[tokio::test]
    async fn copy_auth_files_redacts_credentials_and_copies_claude_json() {
        let src_cfg = tempfile::tempdir().unwrap();
        std::fs::write(
            src_cfg.path().join(".credentials.json"),
            r#"{"claudeAiOauth":{"accessToken":"a","refreshToken":"r"}}"#,
        )
        .unwrap();
        std::fs::write(src_cfg.path().join(".claude.json"), r#"{"ok":true}"#).unwrap();

        let store = Arc::new(InMemorySessionStore::new());
        store
            .append(
                &key_for(SID, None),
                &[entry(json!({"type": "user", "uuid": "u1", "parentUuid": null, "sessionId": SID, "message": {"content": "hi"}}))],
            )
            .await
            .unwrap();
        let store_dyn: Arc<dyn SessionStore> = store;
        let mut options = opts(store_dyn, src_cfg.path());
        options.resume = Some(SID.into());

        let m = materialize_resume_session(&options).await.unwrap().unwrap();
        let creds = std::fs::read_to_string(m.config_dir.join(".credentials.json")).unwrap();
        assert!(creds.contains("accessToken"));
        assert!(!creds.contains("refreshToken")); // redacted
        assert!(m.config_dir.join(".claude.json").exists());
    }

    #[tokio::test]
    async fn continue_without_list_sessions_errors() {
        #[derive(Default)]
        struct MiniStore;
        #[async_trait::async_trait]
        impl SessionStore for MiniStore {
            async fn append(&self, _: &SessionKey, _: &[SessionStoreEntry]) -> Result<()> {
                Ok(())
            }
            async fn load(&self, _: &SessionKey) -> Result<Option<Vec<SessionStoreEntry>>> {
                Ok(None)
            }
        }
        let empty = tempfile::tempdir().unwrap();
        let store: Arc<dyn SessionStore> = Arc::new(MiniStore);
        let mut options = opts(store, empty.path());
        options.continue_conversation = true;
        let res = materialize_resume_session(&options).await;
        assert!(matches!(res, Err(Error::Invalid(_))));
    }

    #[tokio::test]
    async fn materialize_skips_unsafe_subkeys() {
        struct UnsafeStore {
            inner: InMemorySessionStore,
        }
        #[async_trait::async_trait]
        impl SessionStore for UnsafeStore {
            async fn append(&self, k: &SessionKey, e: &[SessionStoreEntry]) -> Result<()> {
                self.inner.append(k, e).await
            }
            async fn load(&self, k: &SessionKey) -> Result<Option<Vec<SessionStoreEntry>>> {
                self.inner.load(k).await
            }
            async fn list_subkeys(&self, _: &SessionListSubkeysKey) -> Result<Vec<String>> {
                // Both are unsafe and must be skipped without writing/panicking.
                Ok(vec!["../evil".into(), String::new()])
            }
        }
        let empty = tempfile::tempdir().unwrap();
        let store = UnsafeStore { inner: InMemorySessionStore::new() };
        store
            .append(
                &key_for(SID, None),
                &[entry(json!({"type": "user", "uuid": "u1", "parentUuid": null, "sessionId": SID, "message": {"content": "hi"}}))],
            )
            .await
            .unwrap();
        let store_dyn: Arc<dyn SessionStore> = Arc::new(store);
        let mut options = opts(store_dyn, empty.path());
        options.resume = Some(SID.into());

        let m = materialize_resume_session(&options).await.unwrap().unwrap();
        let pk = crate::project_key_for_directory(Some(Path::new(DIR)));
        // Main transcript written; no subagent dir (both subkeys were unsafe).
        assert!(m.config_dir.join("projects").join(&pk).join(format!("{SID}.jsonl")).exists());
        assert!(!m.config_dir.join("projects").join(&pk).join(SID).exists());
    }
}
