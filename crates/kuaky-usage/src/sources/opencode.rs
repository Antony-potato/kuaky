//! OpenCode: `~/.local/share/opencode/opencode*.db` (SQLite) y el formato
//! anterior `storage/message/<sesión>/<mensaje>.json`.
//!
//! Formato documentado a partir de splitrail (MIT). Cada mensaje del asistente
//! trae `modelID`, `time.created` (ms) y `tokens { input, output, reasoning,
//! cache { read, write } }`. En SQLite la tabla `message` tiene
//! `id, session_id, time_created, data` (JSON) y el proyecto sale de
//! `session.project_id → project.worktree`.

use super::{Interner, Source};
use crate::{Agent, UsageEvent};
use rusqlite::{Connection, OpenFlags};
use serde::Deserialize;
use std::{
    collections::{HashMap, HashSet},
    path::{Path, PathBuf},
    sync::Arc,
};

pub struct OpenCode {
    dir: Option<PathBuf>,
}

impl OpenCode {
    pub fn from_env() -> Self {
        let base = std::env::var_os("XDG_DATA_HOME")
            .map(PathBuf::from)
            .or_else(|| dirs::home_dir().map(|h| h.join(".local").join("share")));
        Self { dir: base.map(|b| b.join("opencode")) }
    }

    pub fn with_dir(dir: PathBuf) -> Self {
        Self { dir: Some(dir) }
    }
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct Message {
    id: Option<String>,
    #[serde(rename = "sessionID")]
    session_id: Option<String>,
    role: Option<String>,
    #[serde(rename = "modelID")]
    model_id: Option<String>,
    time: Time,
    tokens: Tokens,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct Time {
    created: i64,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct Tokens {
    input: u64,
    output: u64,
    reasoning: u64,
    cache: Cache,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct Cache {
    read: u64,
    write: u64,
}

impl Source for OpenCode {
    fn agent(&self) -> Agent {
        Agent::OpenCode
    }

    fn load(&self) -> anyhow::Result<Vec<UsageEvent>> {
        let Some(dir) = &self.dir else { return Ok(Vec::new()) };
        if !dir.is_dir() {
            return Ok(Vec::new());
        }
        let mut interner = Interner::default();
        let mut seen = HashSet::new();
        let mut out = Vec::new();
        // SQLite primero: si un mensaje también está en JSON, gana la base.
        let mut dbs: Vec<PathBuf> = std::fs::read_dir(dir)?
            .filter_map(Result::ok)
            .map(|e| e.path())
            .filter(|p| {
                let name = p.file_name().and_then(|n| n.to_str()).unwrap_or("");
                name.ends_with(".db") && (name == "opencode.db" || name.starts_with("opencode-"))
            })
            .collect();
        dbs.sort();
        for db in dbs {
            match read_db(&db, &mut interner) {
                Ok(events) => {
                    for (key, e) in events {
                        if seen.insert(key) {
                            out.push(e);
                        }
                    }
                }
                Err(err) => tracing::warn!(db = %db.display(), %err, "no se pudo leer OpenCode"),
            }
        }
        for (key, e) in read_json_storage(&dir.join("storage").join("message"), &mut interner) {
            if seen.insert(key) {
                out.push(e);
            }
        }
        Ok(out)
    }
}

fn to_event(m: Message, session: &str, fallback_ms: i64, project: Option<Arc<str>>, interner: &mut Interner) -> Option<UsageEvent> {
    if m.role.as_deref() != Some("assistant") {
        return None;
    }
    let t = &m.tokens;
    if t.input + t.output + t.reasoning + t.cache.read + t.cache.write == 0 {
        return None;
    }
    let ms = if m.time.created > 0 { m.time.created } else { fallback_ms };
    let ts = jiff::Timestamp::from_millisecond(ms).ok()?;
    Some(UsageEvent {
        agent: Agent::OpenCode,
        ts,
        model: interner.get(m.model_id.as_deref().unwrap_or("opencode")),
        session: interner.get(session),
        project,
        input: t.input,
        output: t.output + t.reasoning,
        cache_read: t.cache.read,
        cache_write: t.cache.write,
        cache_write_1h: 0,
    })
}

fn read_db(path: &Path, interner: &mut Interner) -> anyhow::Result<Vec<(String, UsageEvent)>> {
    let conn = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX)?;
    conn.busy_timeout(std::time::Duration::from_secs(5))?;
    // Sesión → carpeta del proyecto (si las tablas existen).
    let mut projects: HashMap<String, Arc<str>> = HashMap::new();
    if let Ok(mut stmt) = conn.prepare("SELECT s.id, p.worktree FROM session s JOIN project p ON p.id = s.project_id") {
        let rows = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, Option<String>>(1)?)))?;
        for (session, worktree) in rows.flatten() {
            if let Some(w) = worktree.filter(|w| !w.is_empty() && w != "/") {
                projects.insert(session, Arc::from(w.as_str()));
            }
        }
    }
    let mut stmt = conn.prepare("SELECT id, session_id, time_created, data FROM message")?;
    let rows = stmt.query_map([], |r| {
        Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, Option<i64>>(2)?, r.get::<_, String>(3)?))
    })?;
    let mut out = Vec::new();
    for (id, session, created, data) in rows.flatten() {
        let Ok(m) = serde_json::from_str::<Message>(&data) else { continue };
        let project = projects.get(&session).cloned();
        if let Some(e) = to_event(m, &session, created.unwrap_or(0), project, interner) {
            out.push((format!("{session}:{id}"), e));
        }
    }
    Ok(out)
}

fn read_json_storage(dir: &Path, interner: &mut Interner) -> Vec<(String, UsageEvent)> {
    let mut out = Vec::new();
    let Ok(sessions) = std::fs::read_dir(dir) else { return out };
    for session_dir in sessions.filter_map(Result::ok) {
        let Ok(files) = std::fs::read_dir(session_dir.path()) else { continue };
        let session = session_dir.file_name().to_string_lossy().into_owned();
        for file in files.filter_map(Result::ok).map(|f| f.path()).filter(|p| p.extension().is_some_and(|x| x == "json")) {
            let Ok(text) = std::fs::read_to_string(&file) else { continue };
            let Ok(m) = serde_json::from_str::<Message>(&text) else { continue };
            let id = m.id.clone().unwrap_or_else(|| file.file_stem().unwrap_or_default().to_string_lossy().into_owned());
            let session = m.session_id.clone().unwrap_or_else(|| session.clone());
            if let Some(e) = to_event(m, &session, 0, None, interner) {
                out.push((format!("{session}:{id}"), e));
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const ASSISTANT: &str = r#"{"role":"assistant","modelID":"claude-sonnet-5-5","time":{"created":1795000000000},"tokens":{"input":100,"output":20,"reasoning":5,"cache":{"read":300,"write":40}}}"#;

    #[test]
    fn reads_sqlite_and_json_without_duplicates() {
        let dir = std::env::temp_dir().join(format!("kuaky-opencode-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("storage/message/ses1")).unwrap();
        let conn = Connection::open(dir.join("opencode.db")).unwrap();
        conn.execute_batch(
            "CREATE TABLE project (id TEXT, worktree TEXT);
             CREATE TABLE session (id TEXT, project_id TEXT, title TEXT);
             CREATE TABLE message (id TEXT, session_id TEXT, time_created INTEGER, data TEXT);
             INSERT INTO project VALUES ('p1', '/tmp/app');
             INSERT INTO session VALUES ('ses1', 'p1', 'x');",
        )
        .unwrap();
        conn.execute("INSERT INTO message VALUES ('m1', 'ses1', 0, ?1)", [ASSISTANT]).unwrap();
        conn.execute("INSERT INTO message VALUES ('m0', 'ses1', 0, ?1)", [r#"{"role":"user"}"#]).unwrap();
        drop(conn);
        // El mismo mensaje en el formato anterior no se cuenta dos veces.
        let legacy = ASSISTANT.replacen('{', r#"{"id":"m1","sessionID":"ses1","#, 1);
        std::fs::write(dir.join("storage/message/ses1/m1.json"), legacy).unwrap();
        let other = ASSISTANT.replacen('{', r#"{"id":"m2","sessionID":"ses1","#, 1);
        std::fs::write(dir.join("storage/message/ses1/m2.json"), other).unwrap();

        let events = OpenCode::with_dir(dir.clone()).load().unwrap();
        std::fs::remove_dir_all(&dir).ok();
        assert_eq!(events.len(), 2);
        let e = &events[0];
        assert_eq!(&*e.model, "claude-sonnet-5-5");
        assert_eq!(e.output, 25);
        assert_eq!(e.cache_read, 300);
        assert_eq!(e.cache_write, 40);
        assert_eq!(e.project.as_deref(), Some("/tmp/app"));
    }
}
