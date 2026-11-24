//! Claude Code: `~/.claude/projects/<proyecto>/<sesión>.jsonl`.
//!
//! Cada respuesta del modelo puede aparecer en varias líneas (streaming y
//! sesiones reanudadas que copian el historial), así que se deduplica por
//! `message.id` + `requestId` y se conserva la copia con más tokens de salida.

use super::{Interner, Source, files_with_ext, roots};
use crate::{Agent, UsageEvent};
use rayon::prelude::*;
use serde::Deserialize;
use std::{
    collections::HashMap,
    io::{BufRead, BufReader},
    path::{Path, PathBuf},
    sync::Arc,
};

pub struct ClaudeCode {
    roots: Vec<PathBuf>,
    cache_path: Option<PathBuf>,
}

impl ClaudeCode {
    pub fn from_env() -> Self {
        Self {
            roots: roots("CLAUDE_CONFIG_DIR", &[".claude", ".config/claude"]),
            cache_path: dirs::cache_dir().map(|d| d.join("kuaky").join("claude-usage-v1.json")),
        }
    }

    pub fn with_roots(roots: Vec<PathBuf>) -> Self {
        Self { roots, cache_path: None }
    }

    pub fn with_cache(mut self, path: PathBuf) -> Self {
        self.cache_path = Some(path);
        self
    }
}

/// Caché en disco de los eventos ya leídos de cada archivo.
mod cache {
    use super::Keyed;
    use crate::{Agent, UsageEvent};
    use serde::{Deserialize, Serialize};
    use std::{
        collections::HashMap,
        path::{Path, PathBuf},
        sync::Arc,
    };

    /// Tamaño y fecha de modificación (ns) del archivo.
    pub type Stamp = (u64, u128);

    #[derive(Serialize, Deserialize, Clone)]
    pub struct Entry {
        k: String,
        t: i64,
        m: String,
        s: String,
        p: Option<String>,
        i: u64,
        o: u64,
        r: u64,
        w: u64,
        w1: u64,
    }

    #[derive(Serialize, Deserialize, Default)]
    struct File {
        files: HashMap<PathBuf, (Stamp, Vec<Entry>)>,
    }

    pub fn stamp(path: &Path) -> Option<Stamp> {
        let meta = std::fs::metadata(path).ok()?;
        let modified = meta.modified().ok()?.duration_since(std::time::UNIX_EPOCH).ok()?.as_nanos();
        Some((meta.len(), modified))
    }

    pub fn load(path: &Path) -> HashMap<PathBuf, (Stamp, Vec<Entry>)> {
        std::fs::read(path)
            .ok()
            .and_then(|b| serde_json::from_slice::<File>(&b).ok())
            .map(|f| f.files)
            .unwrap_or_default()
    }

    pub fn decode(entries: &[Entry]) -> Vec<Keyed> {
        let mut models: HashMap<&str, Arc<str>> = HashMap::new();
        let mut sessions: HashMap<&str, Arc<str>> = HashMap::new();
        entries
            .iter()
            .filter_map(|e| {
                let model = models.entry(&e.m).or_insert_with(|| Arc::from(e.m.as_str())).clone();
                let session = sessions.entry(&e.s).or_insert_with(|| Arc::from(e.s.as_str())).clone();
                Some((
                    Box::from(e.k.as_str()),
                    UsageEvent {
                        agent: Agent::ClaudeCode,
                        ts: jiff::Timestamp::from_millisecond(e.t).ok()?,
                        model,
                        session,
                        project: e.p.as_deref().map(Arc::from),
                        input: e.i,
                        output: e.o,
                        cache_read: e.r,
                        cache_write: e.w,
                        cache_write_1h: e.w1,
                    },
                ))
            })
            .collect()
    }

    pub fn save(path: &Path, results: &[(PathBuf, Stamp, Vec<Keyed>, bool)]) {
        let files = results
            .iter()
            .map(|(p, stamp, entries, _)| {
                let entries = entries
                    .iter()
                    .map(|(k, e)| Entry {
                        k: k.to_string(),
                        t: e.ts.as_millisecond(),
                        m: e.model.to_string(),
                        s: e.session.to_string(),
                        p: e.project.as_deref().map(str::to_string),
                        i: e.input,
                        o: e.output,
                        r: e.cache_read,
                        w: e.cache_write,
                        w1: e.cache_write_1h,
                    })
                    .collect();
                (p.clone(), (*stamp, entries))
            })
            .collect();
        let Ok(bytes) = serde_json::to_vec(&File { files }) else { return };
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let tmp = path.with_extension("json.tmp");
        if std::fs::write(&tmp, bytes).is_ok() {
            let _ = std::fs::rename(&tmp, path);
        }
    }
}

impl Source for ClaudeCode {
    fn agent(&self) -> Agent {
        Agent::ClaudeCode
    }

    fn load(&self) -> anyhow::Result<Vec<UsageEvent>> {
        let files: Vec<PathBuf> = self
            .roots
            .iter()
            .flat_map(|r| files_with_ext(&r.join("projects"), "jsonl"))
            .collect();

        // Caché por archivo: solo se releen los que cambiaron (tamaño o fecha).
        let cache_path = self.cache_path.clone();
        let cache = cache_path.as_deref().map(cache::load).unwrap_or_default();
        let results: Vec<(PathBuf, cache::Stamp, Vec<Keyed>, bool)> = files
            .par_iter()
            .filter_map(|f| {
                let stamp = cache::stamp(f)?;
                match cache.get(f) {
                    Some((s, entries)) if *s == stamp => Some((f.clone(), stamp, cache::decode(entries), false)),
                    _ => Some((f.clone(), stamp, parse_file(f), true)),
                }
            })
            .collect();
        let changed = results.iter().any(|r| r.3) || results.len() != cache.len();
        if changed {
            if let Some(path) = &cache_path {
                cache::save(path, &results);
            }
        }
        let per_file: Vec<Vec<Keyed>> = results.into_iter().map(|r| r.2).collect();

        // Deduplicación global: una sesión reanudada repite mensajes de otra.
        let mut best: HashMap<Box<str>, UsageEvent> = HashMap::new();
        for (key, event) in per_file.into_iter().flatten() {
            match best.get_mut(&key) {
                Some(prev) if prev.output >= event.output => {}
                Some(prev) => *prev = event,
                None => {
                    best.insert(key, event);
                }
            }
        }
        Ok(best.into_values().collect())
    }
}

type Keyed = (Box<str>, UsageEvent);

#[derive(Deserialize)]
struct Line {
    #[serde(rename = "type")]
    kind: Option<String>,
    timestamp: Option<String>,
    #[serde(rename = "sessionId")]
    session_id: Option<String>,
    cwd: Option<String>,
    #[serde(rename = "requestId")]
    request_id: Option<String>,
    message: Option<Message>,
}

#[derive(Deserialize)]
struct Message {
    id: Option<String>,
    model: Option<String>,
    usage: Option<Usage>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct Usage {
    input_tokens: u64,
    output_tokens: u64,
    cache_creation_input_tokens: u64,
    cache_read_input_tokens: u64,
    cache_creation: CacheCreation,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct CacheCreation {
    ephemeral_1h_input_tokens: u64,
}

fn parse_file(path: &Path) -> Vec<Keyed> {
    let Ok(file) = std::fs::File::open(path) else {
        return Vec::new();
    };
    let fallback_session = path
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    let mut interner = Interner::default();
    let mut out = Vec::new();
    for line in BufReader::new(file).lines() {
        let Ok(line) = line else { break };
        if let Some(keyed) = parse_line(&line, &fallback_session, &mut interner) {
            out.push(keyed);
        }
    }
    out
}

fn parse_line(line: &str, fallback_session: &str, interner: &mut Interner) -> Option<Keyed> {
    // Filtro barato antes de deserializar: la mayoría de líneas no traen consumo.
    if !line.contains("\"usage\"") || !line.contains("\"assistant\"") {
        return None;
    }
    let parsed: Line = serde_json::from_str(line).ok()?;
    if parsed.kind.as_deref() != Some("assistant") {
        return None;
    }
    let message = parsed.message?;
    let usage = message.usage?;
    let model = message.model.unwrap_or_default();
    if model.is_empty() || model == "<synthetic>" {
        return None;
    }
    let raw_ts = parsed.timestamp?;
    let ts = raw_ts.parse().ok()?;
    let session = parsed.session_id.unwrap_or_else(|| fallback_session.to_owned());
    let msg_id = message.id.unwrap_or_default();
    let req_id = parsed.request_id.unwrap_or_default();
    let key: Box<str> = if msg_id.is_empty() && req_id.is_empty() {
        // Sin identificadores: sesión + hora es lo más cercano a una identidad.
        format!("anon:{session}:{raw_ts}").into()
    } else {
        format!("{msg_id}:{req_id}").into()
    };
    Some((
        key,
        UsageEvent {
            agent: Agent::ClaudeCode,
            ts,
            model: interner.get(&model),
            session: interner.get(&session),
            project: parsed.cwd.map(|c| Arc::from(c.as_str())),
            input: usage.input_tokens,
            output: usage.output_tokens,
            cache_read: usage.cache_read_input_tokens,
            cache_write: usage.cache_creation_input_tokens,
            cache_write_1h: usage.cache_creation.ephemeral_1h_input_tokens.min(usage.cache_creation_input_tokens),
        },
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    const LINE: &str = r#"{"type":"assistant","timestamp":"2026-11-22T01:40:35.245Z","sessionId":"s1","cwd":"/tmp/app","requestId":"req_1","message":{"id":"msg_1","model":"claude-opus-5-5","usage":{"input_tokens":2,"cache_creation_input_tokens":100,"cache_read_input_tokens":400,"output_tokens":50,"cache_creation":{"ephemeral_1h_input_tokens":100,"ephemeral_5m_input_tokens":0}}}}"#;

    #[test]
    fn parses_assistant_usage() {
        let mut i = Interner::default();
        let (key, e) = parse_line(LINE, "x", &mut i).unwrap();
        assert_eq!(&*key, "msg_1:req_1");
        assert_eq!(e.input, 2);
        assert_eq!(e.cache_read, 400);
        assert_eq!(e.cache_write, 100);
        assert_eq!(e.cache_write_1h, 100);
        assert_eq!(e.output, 50);
        assert_eq!(e.input_total(), 502);
        assert_eq!(&*e.model, "claude-opus-5-5");
        assert_eq!(e.project.as_deref(), Some("/tmp/app"));
    }

    #[test]
    fn skips_user_and_synthetic_lines() {
        let mut i = Interner::default();
        assert!(parse_line(r#"{"type":"user","message":{"usage":{}}}"#, "x", &mut i).is_none());
        let synthetic = LINE.replace("claude-opus-5-5", "<synthetic>");
        assert!(parse_line(&synthetic, "x", &mut i).is_none());
    }

    #[test]
    fn deduplicates_across_files_keeping_largest_output() {
        let dir = std::env::temp_dir().join(format!("kuaky-claude-{}", std::process::id()));
        let projects = dir.join("projects/p");
        std::fs::create_dir_all(&projects).unwrap();
        let partial = LINE.replace("\"output_tokens\":50", "\"output_tokens\":10");
        let mut a = std::fs::File::create(projects.join("a.jsonl")).unwrap();
        writeln!(a, "{partial}\n{LINE}").unwrap();
        let mut b = std::fs::File::create(projects.join("b.jsonl")).unwrap();
        writeln!(b, "{LINE}").unwrap();

        let cache = dir.join("cache.json");
        let source = ClaudeCode::with_roots(vec![dir.clone()]).with_cache(cache.clone());
        let events = source.load().unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].output, 50);
        // Segunda lectura desde la caché: mismo resultado.
        assert!(cache.is_file());
        let again = source.load().unwrap();
        std::fs::remove_dir_all(&dir).ok();
        assert_eq!(again.len(), 1);
        assert_eq!(again[0].output, 50);
        assert_eq!(again[0].cache_write_1h, 100);
    }
}
