//! Conversaciones que los propios agentes guardan en disco, para poder
//! retomarlas desde kuaky aunque no se hayan abierto aquí.
//!
//! Antigravity (`agy`) guarda en `~/.gemini/antigravity-cli/cache/`:
//! - `last_conversations.json`: carpeta → ID de su conversación más reciente;
//! - `conversation_metadata.json`: título, fecha y carpetas de cada conversación.

use jiff::Timestamp;
use serde_json::Value;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq)]
pub struct PastConversation {
    pub id: String,
    pub title: String,
    pub updated: Option<Timestamp>,
}

/// Los IDs van dentro de un comando de shell: solo letras, números, `-` y `_`.
pub fn valid_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= 128 && id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

pub fn agy_dir() -> Option<PathBuf> {
    Some(dirs::home_dir()?.join(".gemini").join("antigravity-cli"))
}

fn read_json(path: &Path) -> Option<Value> {
    serde_json::from_str(&std::fs::read_to_string(path).ok()?).ok()
}

fn same_dir(a: &Path, b: &Path) -> bool {
    let norm = |p: &Path| p.to_string_lossy().trim_end_matches(['/', '\\']).to_string();
    norm(a) == norm(b)
}

/// ID de la conversación más reciente de `agy` en `cwd`.
fn agy_latest_id(dir: &Path, cwd: &Path) -> Option<String> {
    let map = read_json(&dir.join("cache").join("last_conversations.json"))?;
    map.as_object()?
        .iter()
        .find(|(k, _)| same_dir(Path::new(k), cwd))
        .and_then(|(_, v)| v.as_str())
        .filter(|id| valid_id(id))
        .map(str::to_string)
}

fn db_mtime(dir: &Path, id: &str) -> Option<Timestamp> {
    let modified = std::fs::metadata(dir.join("conversations").join(format!("{id}.db"))).ok()?.modified().ok()?;
    Timestamp::try_from(modified).ok()
}

/// Conversaciones de `agy` cuya carpeta de trabajo es `cwd`, la más reciente primero.
pub fn agy_conversations_in(dir: &Path, cwd: &Path) -> Vec<PastConversation> {
    let uri = kuaky_lsp::path_to_uri(cwd);
    let uri = uri.trim_end_matches('/');
    let mut out: Vec<PastConversation> = Vec::new();
    if let Some(meta) = read_json(&dir.join("cache").join("conversation_metadata.json")) {
        if let Some(convs) = meta.get("conversations").and_then(Value::as_object) {
            for (id, entry) in convs {
                let summary = entry.get("summary").unwrap_or(entry);
                let in_dir = summary
                    .get("WorkspaceURIs")
                    .and_then(Value::as_array)
                    .is_some_and(|uris| uris.iter().filter_map(Value::as_str).any(|u| u.trim_end_matches('/') == uri));
                if !in_dir || !valid_id(id) {
                    continue;
                }
                let title = summary
                    .get("Title")
                    .and_then(Value::as_str)
                    .filter(|t| !t.trim().is_empty())
                    .or_else(|| summary.get("Preview").and_then(Value::as_str))
                    .unwrap_or("Antigravity conversation")
                    .trim()
                    .to_string();
                let updated = summary
                    .get("UpdatedAt")
                    .and_then(Value::as_str)
                    .and_then(|s| s.parse::<Timestamp>().ok())
                    .or_else(|| db_mtime(dir, id));
                out.push(PastConversation { id: id.clone(), title, updated });
            }
        }
    }
    // La más reciente puede no tener metadatos todavía.
    if let Some(id) = agy_latest_id(dir, cwd) {
        if !out.iter().any(|c| c.id == id) {
            let updated = db_mtime(dir, &id);
            out.push(PastConversation { id, title: "Latest Antigravity conversation".into(), updated });
        }
    }
    out.sort_by(|a, b| b.updated.cmp(&a.updated));
    out
}

pub fn agy_conversations(cwd: &Path) -> Vec<PastConversation> {
    agy_dir().map(|d| agy_conversations_in(&d, cwd)).unwrap_or_default()
}

/// La conversación de `agy` que se está usando en `cwd` desde `since`: la más
/// reciente de la carpeta, si tuvo actividad después de que arrancó la sesión.
pub fn agy_current_in(dir: &Path, cwd: &Path, since: Timestamp) -> Option<String> {
    let id = agy_latest_id(dir, cwd)?;
    let updated = agy_conversations_in(dir, cwd).into_iter().find(|c| c.id == id).and_then(|c| c.updated);
    (updated? >= since).then_some(id)
}

pub fn agy_current(cwd: &Path, since: Timestamp) -> Option<String> {
    agy_current_in(&agy_dir()?, cwd, since)
}

#[cfg(test)]
mod tests {
    use super::{agy_conversations_in, agy_current_in, valid_id};
    use std::path::Path;

    fn fixture() -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("kuaky-agy-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("cache")).unwrap();
        std::fs::create_dir_all(dir.join("conversations")).unwrap();
        std::fs::write(
            dir.join("cache/last_conversations.json"),
            r#"{"/w/kuaky": "cccc-new", "/w/otro": "zzzz"}"#,
        )
        .unwrap();
        std::fs::write(
            dir.join("cache/conversation_metadata.json"),
            r#"{"conversations": {
                "aaaa-old": {"summary": {"Title": "Debugging LSP", "UpdatedAt": "2026-11-24T04:51:21.691932Z", "WorkspaceURIs": ["file:///w/kuaky"]}},
                "bbbb": {"summary": {"Title": "Otro proyecto", "UpdatedAt": "2026-11-24T05:00:00Z", "WorkspaceURIs": ["file:///w/otro"]}},
                "bad id!": {"summary": {"Title": "x", "WorkspaceURIs": ["file:///w/kuaky"]}}
            }}"#,
        )
        .unwrap();
        std::fs::write(dir.join("conversations/cccc-new.db"), b"").unwrap();
        dir
    }

    #[test]
    fn lists_conversations_of_the_folder() {
        let dir = fixture();
        let list = agy_conversations_in(&dir, Path::new("/w/kuaky/"));
        let ids: Vec<&str> = list.iter().map(|c| c.id.as_str()).collect();
        // La más reciente (sin metadatos, fecha del .db de hoy) va primero.
        assert_eq!(ids, ["cccc-new", "aaaa-old"]);
        assert_eq!(list[1].title, "Debugging LSP");
    }

    #[test]
    fn current_needs_activity_after_start() {
        let dir = fixture();
        let cwd = Path::new("/w/kuaky");
        let long_ago: jiff::Timestamp = "2020-01-01T00:00:00Z".parse().unwrap();
        assert_eq!(agy_current_in(&dir, cwd, long_ago).as_deref(), Some("cccc-new"));
        let future = jiff::Timestamp::now() + jiff::SignedDuration::from_hours(1);
        assert_eq!(agy_current_in(&dir, cwd, future), None);
    }

    #[test]
    fn rejects_unsafe_ids() {
        assert!(valid_id("008a12f2-03ea-49d3-86f5-23cc8f2a0e3f"));
        assert!(!valid_id("x; rm -rf ~"));
        assert!(!valid_id(""));
    }
}
