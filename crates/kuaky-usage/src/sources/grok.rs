//! Grok CLI: `~/.grok/sessions/<proyecto>/<sesión>/`.
//!
//! Formato documentado a partir de splitrail (MIT): `updates.jsonl` trae una
//! línea por turno con `sessionUpdate: "turn_completed"` y `usage {
//! inputTokens, outputTokens, cachedReadTokens, cacheCreationTokens,
//! reasoningTokens }`; el modelo está en `chat_history.jsonl` (`model_id`) y la
//! fecha en `summary.json` (`created_at`) o, si falta, en la fecha del archivo.

use super::{Interner, Source};
use crate::{Agent, UsageEvent};
use serde::Deserialize;
use serde_json::Value;
use std::path::{Path, PathBuf};

pub struct Grok {
    dir: Option<PathBuf>,
}

impl Grok {
    pub fn from_env() -> Self {
        Self { dir: dirs::home_dir().map(|h| h.join(".grok").join("sessions")) }
    }

    pub fn with_dir(dir: PathBuf) -> Self {
        Self { dir: Some(dir) }
    }
}

#[derive(Deserialize, Default)]
#[serde(default, rename_all = "camelCase")]
struct Usage {
    input_tokens: u64,
    output_tokens: u64,
    cached_read_tokens: u64,
    cache_creation_tokens: u64,
    reasoning_tokens: u64,
}

impl Source for Grok {
    fn agent(&self) -> Agent {
        Agent::Grok
    }

    fn load(&self) -> anyhow::Result<Vec<UsageEvent>> {
        let Some(dir) = &self.dir else { return Ok(Vec::new()) };
        let mut out = Vec::new();
        let mut interner = Interner::default();
        let Ok(projects) = std::fs::read_dir(dir) else { return Ok(out) };
        for project in projects.filter_map(Result::ok) {
            let Ok(sessions) = std::fs::read_dir(project.path()) else { continue };
            for session in sessions.filter_map(Result::ok) {
                out.extend(read_session(&session.path(), &mut interner));
            }
        }
        Ok(out)
    }
}

fn read_session(dir: &Path, interner: &mut Interner) -> Vec<UsageEvent> {
    let Ok(updates) = std::fs::read_to_string(dir.join("updates.jsonl")) else { return Vec::new() };
    let model = std::fs::read_to_string(dir.join("chat_history.jsonl"))
        .ok()
        .and_then(|text| {
            text.lines()
                .filter_map(|l| serde_json::from_str::<Value>(l).ok())
                .filter_map(|v| v.get("model_id").and_then(Value::as_str).map(str::to_string))
                .last()
        })
        .unwrap_or_else(|| "grok".into());
    let ts = std::fs::read_to_string(dir.join("summary.json"))
        .ok()
        .and_then(|t| serde_json::from_str::<Value>(&t).ok())
        .and_then(|v| v.get("created_at").and_then(Value::as_str).and_then(|s| s.parse::<jiff::Timestamp>().ok()))
        .or_else(|| {
            let modified = std::fs::metadata(dir.join("updates.jsonl")).and_then(|m| m.modified()).ok()?;
            jiff::Timestamp::try_from(modified).ok()
        });
    let Some(ts) = ts else { return Vec::new() };
    let session = interner.get(&dir.file_name().unwrap_or_default().to_string_lossy());
    let model = interner.get(&model);
    updates
        .lines()
        .filter_map(|l| serde_json::from_str::<Value>(l).ok())
        .filter_map(|v| {
            let update = v.get("params")?.get("update")?;
            if update.get("sessionUpdate")?.as_str()? != "turn_completed" {
                return None;
            }
            let u: Usage = serde_json::from_value(update.get("usage")?.clone()).ok()?;
            Some(UsageEvent {
                agent: Agent::Grok,
                ts,
                model: model.clone(),
                session: session.clone(),
                project: None,
                input: u.input_tokens.saturating_sub(u.cached_read_tokens),
                output: u.output_tokens + u.reasoning_tokens,
                cache_read: u.cached_read_tokens,
                cache_write: u.cache_creation_tokens,
                cache_write_1h: 0,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_turns_from_updates() {
        let dir = std::env::temp_dir().join(format!("kuaky-grok-{}", std::process::id()));
        let session = dir.join("proyecto/ses-1");
        std::fs::create_dir_all(&session).unwrap();
        std::fs::write(session.join("chat_history.jsonl"), "{\"type\":\"user\",\"content\":\"hola\"}\n{\"type\":\"assistant\",\"model_id\":\"grok-4.7\",\"content\":\"ok\"}\n").unwrap();
        std::fs::write(session.join("summary.json"), r#"{"created_at":"2026-11-20T10:00:00Z","generated_title":"x"}"#).unwrap();
        std::fs::write(
            session.join("updates.jsonl"),
            "{\"params\":{\"update\":{\"sessionUpdate\":\"agent_message_chunk\"}}}\n{\"params\":{\"update\":{\"sessionUpdate\":\"turn_completed\",\"usage\":{\"inputTokens\":1000,\"outputTokens\":50,\"cachedReadTokens\":800,\"cacheCreationTokens\":10,\"reasoningTokens\":5}}}}\n",
        )
        .unwrap();
        let events = Grok::with_dir(dir.clone()).load().unwrap();
        std::fs::remove_dir_all(&dir).ok();
        assert_eq!(events.len(), 1);
        assert_eq!(&*events[0].model, "grok-4.7");
        assert_eq!(events[0].input, 200);
        assert_eq!(events[0].output, 55);
        assert_eq!(&*events[0].session, "ses-1");
    }
}
