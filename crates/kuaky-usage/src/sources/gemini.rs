//! Gemini CLI: `~/.gemini/tmp/<hash>/chats/*.json`, un JSON por sesión.
//!
//! Los mensajes `type: "gemini"` traen `tokens { input, output, cached,
//! thoughts, tool, total }`. `input` incluye los tokens en caché.

use super::{Interner, Source, roots};
use crate::{Agent, UsageEvent};
use rayon::prelude::*;
use serde::Deserialize;
use std::path::{Path, PathBuf};

pub struct GeminiCli {
    roots: Vec<PathBuf>,
}

impl GeminiCli {
    pub fn from_env() -> Self {
        Self {
            roots: roots("GEMINI_CLI_HOME", &[".gemini"]),
        }
    }
}

impl Source for GeminiCli {
    fn agent(&self) -> Agent {
        Agent::GeminiCli
    }

    fn load(&self) -> anyhow::Result<Vec<UsageEvent>> {
        let mut files = Vec::new();
        for root in &self.roots {
            let Ok(projects) = std::fs::read_dir(root.join("tmp")) else {
                continue;
            };
            for project in projects.filter_map(Result::ok) {
                let Ok(chats) = std::fs::read_dir(project.path().join("chats")) else {
                    continue;
                };
                files.extend(
                    chats
                        .filter_map(Result::ok)
                        .map(|e| e.path())
                        .filter(|p| p.extension().is_some_and(|x| x == "json")),
                );
            }
        }
        Ok(files.par_iter().flat_map_iter(|f| parse_file(f)).collect())
    }
}

#[derive(Deserialize)]
struct Chat {
    #[serde(rename = "sessionId")]
    session_id: Option<String>,
    #[serde(default)]
    messages: Vec<Msg>,
}

#[derive(Deserialize)]
struct Msg {
    #[serde(rename = "type")]
    kind: Option<String>,
    timestamp: Option<String>,
    model: Option<String>,
    tokens: Option<Tokens>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct Tokens {
    input: u64,
    output: u64,
    cached: u64,
    thoughts: u64,
}

fn parse_file(path: &Path) -> Vec<UsageEvent> {
    let Ok(text) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    parse_chat(&text, path)
}

fn parse_chat(text: &str, path: &Path) -> Vec<UsageEvent> {
    let Ok(chat) = serde_json::from_str::<Chat>(text) else {
        return Vec::new();
    };
    let mut interner = Interner::default();
    let session = chat
        .session_id
        .unwrap_or_else(|| path.file_stem().unwrap_or_default().to_string_lossy().into_owned());
    let session = interner.get(&session);
    chat.messages
        .into_iter()
        .filter(|m| m.kind.as_deref() == Some("gemini"))
        .filter_map(|m| {
            let t = m.tokens?;
            let ts = m.timestamp?.parse().ok()?;
            Some(UsageEvent {
                agent: Agent::GeminiCli,
                ts,
                model: interner.get(m.model.as_deref().unwrap_or("gemini")),
                session: session.clone(),
                project: None,
                input: t.input.saturating_sub(t.cached),
                output: t.output + t.thoughts,
                cache_read: t.cached,
                cache_write: 0,
                cache_write_1h: 0,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_gemini_messages() {
        let text = r#"{"sessionId":"g1","messages":[
            {"type":"user","timestamp":"2026-11-20T10:00:00Z","content":"hola"},
            {"type":"gemini","timestamp":"2026-11-20T10:00:03Z","model":"gemini-4-argon-high",
             "tokens":{"input":1000,"output":20,"cached":600,"thoughts":30,"tool":0,"total":1050}}]}"#;
        let events = parse_chat(text, Path::new("x.json"));
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].input, 400);
        assert_eq!(events[0].cache_read, 600);
        assert_eq!(events[0].output, 50);
    }
}
