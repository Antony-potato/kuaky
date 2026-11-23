//! Codex CLI: `~/.codex/sessions/AAAA/MM/DD/rollout-*.jsonl` (y `archived_sessions`).
//!
//! Los eventos `token_count` traen `last_token_usage` (el turno) y
//! `total_token_usage` (acumulado). Codex repite el mismo evento a veces, así
//! que se ignoran los que no hacen avanzar el total. `input_tokens` incluye los
//! tokens en caché, por eso se restan.

use super::{Interner, Source, files_with_ext, roots};
use crate::{Agent, UsageEvent};
use rayon::prelude::*;
use serde::Deserialize;
use std::{
    io::{BufRead, BufReader},
    path::{Path, PathBuf},
    sync::Arc,
};

pub struct Codex {
    roots: Vec<PathBuf>,
}

impl Codex {
    pub fn from_env() -> Self {
        Self {
            roots: roots("CODEX_HOME", &[".codex"]),
        }
    }

    pub fn with_roots(roots: Vec<PathBuf>) -> Self {
        Self { roots }
    }
}

impl Source for Codex {
    fn agent(&self) -> Agent {
        Agent::Codex
    }

    fn load(&self) -> anyhow::Result<Vec<UsageEvent>> {
        let files: Vec<PathBuf> = self
            .roots
            .iter()
            .flat_map(|r| {
                let mut v = files_with_ext(&r.join("sessions"), "jsonl");
                v.extend(files_with_ext(&r.join("archived_sessions"), "jsonl"));
                v
            })
            .collect();
        Ok(files.par_iter().flat_map_iter(|f| parse_file(f)).collect())
    }
}

#[derive(Deserialize)]
struct Line {
    #[serde(rename = "type")]
    kind: Option<String>,
    timestamp: Option<String>,
    payload: Option<serde_json::Value>,
}

#[derive(Deserialize, Default, Clone, Copy, PartialEq)]
#[serde(default)]
struct Tokens {
    input_tokens: u64,
    cached_input_tokens: u64,
    output_tokens: u64,
    total_tokens: u64,
}

fn parse_file(path: &Path) -> Vec<UsageEvent> {
    let Ok(file) = std::fs::File::open(path) else {
        return Vec::new();
    };
    let mut state = State {
        session: path
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default(),
        ..State::default()
    };
    let mut out = Vec::new();
    for line in BufReader::new(file).lines() {
        let Ok(line) = line else { break };
        if let Some(e) = state.feed(&line) {
            out.push(e);
        }
    }
    out
}

#[derive(Default)]
struct State {
    session: String,
    cwd: Option<Arc<str>>,
    model: String,
    last_total: Option<Tokens>,
    interner: Interner,
}

impl State {
    fn feed(&mut self, line: &str) -> Option<UsageEvent> {
        let parsed: Line = serde_json::from_str(line).ok()?;
        let payload = parsed.payload?;
        match parsed.kind.as_deref() {
            Some("session_meta") => {
                if let Some(id) = payload.get("id").and_then(|v| v.as_str()) {
                    self.session = id.to_owned();
                }
                self.set_cwd(&payload);
                None
            }
            Some("turn_context") => {
                if let Some(m) = payload.get("model").and_then(|v| v.as_str()) {
                    self.model = m.to_owned();
                }
                self.set_cwd(&payload);
                None
            }
            Some("event_msg") if payload.get("type").and_then(|v| v.as_str()) == Some("token_count") => {
                let info = payload.get("info")?;
                let total: Option<Tokens> = info
                    .get("total_token_usage")
                    .and_then(|v| serde_json::from_value(v.clone()).ok());
                if total.is_some() && total == self.last_total {
                    return None; // evento repetido
                }
                let last: Option<Tokens> = info
                    .get("last_token_usage")
                    .and_then(|v| serde_json::from_value(v.clone()).ok());
                let turn = match (last, total, self.last_total) {
                    (Some(l), _, _) => l,
                    (None, Some(t), Some(prev)) => Tokens {
                        input_tokens: t.input_tokens.saturating_sub(prev.input_tokens),
                        cached_input_tokens: t.cached_input_tokens.saturating_sub(prev.cached_input_tokens),
                        output_tokens: t.output_tokens.saturating_sub(prev.output_tokens),
                        total_tokens: t.total_tokens.saturating_sub(prev.total_tokens),
                    },
                    (None, Some(t), None) => t,
                    (None, None, _) => return None,
                };
                if total.is_some() {
                    self.last_total = total;
                }
                let ts = parsed.timestamp?.parse().ok()?;
                let model = if self.model.is_empty() { "gpt (codex)" } else { &self.model };
                Some(UsageEvent {
                    agent: Agent::Codex,
                    ts,
                    model: self.interner.get(model),
                    session: self.interner.get(&self.session),
                    project: self.cwd.clone(),
                    input: turn.input_tokens.saturating_sub(turn.cached_input_tokens),
                    output: turn.output_tokens,
                    cache_read: turn.cached_input_tokens,
                    cache_write: 0,
                })
            }
            _ => None,
        }
    }

    fn set_cwd(&mut self, payload: &serde_json::Value) {
        if let Some(cwd) = payload.get("cwd").and_then(|v| v.as_str()) {
            self.cwd = Some(Arc::from(cwd));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_turns_and_skips_repeats() {
        let mut s = State::default();
        let lines = [
            r#"{"timestamp":"2026-11-20T10:00:00Z","type":"session_meta","payload":{"id":"sess-1","cwd":"/tmp/app"}}"#,
            r#"{"timestamp":"2026-11-20T10:00:01Z","type":"turn_context","payload":{"model":"gpt-6.2-astra","cwd":"/tmp/app"}}"#,
            r#"{"timestamp":"2026-11-20T10:00:05Z","type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":1000,"cached_input_tokens":800,"output_tokens":50,"total_tokens":1050},"last_token_usage":{"input_tokens":1000,"cached_input_tokens":800,"output_tokens":50,"total_tokens":1050}}}}"#,
            r#"{"timestamp":"2026-11-20T10:00:06Z","type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":1000,"cached_input_tokens":800,"output_tokens":50,"total_tokens":1050},"last_token_usage":{"input_tokens":1000,"cached_input_tokens":800,"output_tokens":50,"total_tokens":1050}}}}"#,
            r#"{"timestamp":"2026-11-20T10:01:00Z","type":"event_msg","payload":{"type":"token_count","info":null}}"#,
        ];
        let events: Vec<_> = lines.iter().filter_map(|l| s.feed(l)).collect();
        assert_eq!(events.len(), 1);
        let e = &events[0];
        assert_eq!(&*e.session, "sess-1");
        assert_eq!(&*e.model, "gpt-6.2-astra");
        assert_eq!(e.input, 200);
        assert_eq!(e.cache_read, 800);
        assert_eq!(e.output, 50);
        assert_eq!(e.project.as_deref(), Some("/tmp/app"));
    }
}
