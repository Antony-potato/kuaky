//! Estado exacto de los agentes mediante hooks.
//!
//! Para Claude Code, kuaky genera un archivo de ajustes con hooks y lo pasa con
//! `claude --settings <archivo>`: no se toca tu `~/.claude/settings.json`. Cada
//! hook agrega una línea `Evento<TAB>json` al archivo de estado de esa terminal
//! (variable `KUAKY_STATUS_FILE`), que la vista lee para saber si el agente
//! está trabajando, esperando tu permiso o terminó.

use serde_json::{Value, json};
use std::{path::PathBuf, sync::OnceLock};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AgentState {
    Working,
    /// Pide permiso o una respuesta; trae el mensaje del agente.
    NeedsYou(String),
    /// Terminó su turno.
    Done,
    Ended,
}

impl AgentState {
    pub fn label(&self) -> &'static str {
        match self {
            AgentState::Working => "working",
            AgentState::NeedsYou(_) => "needs permission",
            AgentState::Done => "done · your turn",
            AgentState::Ended => "session closed",
        }
    }
}

const EVENTS: &[(&str, bool)] = &[
    ("UserPromptSubmit", false),
    ("PreToolUse", true),
    ("PostToolUse", true),
    ("PermissionRequest", true),
    ("Notification", false),
    ("Stop", false),
    ("SessionEnd", false),
];

/// Archivo de ajustes con los hooks de kuaky (se escribe una vez por ejecución).
pub fn claude_settings() -> Option<&'static PathBuf> {
    static PATH: OnceLock<Option<PathBuf>> = OnceLock::new();
    PATH.get_or_init(|| {
        let dir = dirs::config_dir()?.join("kuaky");
        std::fs::create_dir_all(&dir).ok()?;
        let path = dir.join("claude-hooks.json");
        let mut hooks = serde_json::Map::new();
        for (event, tool) in EVENTS {
            let command = format!(
                "f=\"$KUAKY_STATUS_FILE\"; [ -n \"$f\" ] && {{ printf '{event}\\t'; tr -d '\\n'; echo; }} >> \"$f\"; exit 0"
            );
            let mut entry = json!({ "hooks": [{ "type": "command", "command": command, "timeout": 5 }] });
            if *tool {
                entry["matcher"] = json!("*");
            }
            hooks.insert((*event).to_string(), json!([entry]));
        }
        let settings = json!({ "hooks": hooks });
        std::fs::write(&path, serde_json::to_string_pretty(&settings).ok()?).ok()?;
        Some(path)
    })
    .as_ref()
}

/// Archivo nuevo (vacío) para los eventos de una terminal.
pub fn new_status_file() -> Option<PathBuf> {
    let dir = std::env::temp_dir().join("kuaky-status");
    std::fs::create_dir_all(&dir).ok()?;
    let name = format!("{}-{}.log", std::process::id(), jiff::Timestamp::now().as_nanosecond());
    let path = dir.join(name);
    std::fs::write(&path, b"").ok()?;
    Some(path)
}

/// Interpreta una línea `Evento<TAB>json`.
pub fn parse_line(line: &str) -> Option<AgentState> {
    let (event, payload) = line.split_once('\t').unwrap_or((line, ""));
    let payload: Value = serde_json::from_str(payload).unwrap_or(Value::Null);
    let message = payload.get("message").and_then(Value::as_str).unwrap_or("").to_string();
    let kind = payload.get("notification_type").and_then(Value::as_str).unwrap_or("");
    match event.trim() {
        "UserPromptSubmit" | "PreToolUse" | "PostToolUse" => Some(AgentState::Working),
        "PermissionRequest" => {
            let tool = payload.get("tool_name").and_then(Value::as_str).unwrap_or("a tool");
            Some(AgentState::NeedsYou(format!("Wants to use {tool}")))
        }
        "Notification" if kind == "idle_prompt" => Some(AgentState::Done),
        "Notification" => Some(AgentState::NeedsYou(if message.is_empty() { "Needs your attention".into() } else { message })),
        "Stop" => Some(AgentState::Done),
        "SessionEnd" => Some(AgentState::Ended),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_hook_lines() {
        assert_eq!(parse_line("UserPromptSubmit\t{\"prompt\":\"hola\"}"), Some(AgentState::Working));
        assert_eq!(parse_line("Stop\t{}"), Some(AgentState::Done));
        assert_eq!(
            parse_line("Notification\t{\"message\":\"Claude needs your permission to use Bash\",\"notification_type\":\"permission_prompt\"}"),
            Some(AgentState::NeedsYou("Claude needs your permission to use Bash".into()))
        );
        assert_eq!(parse_line("Notification\t{\"notification_type\":\"idle_prompt\"}"), Some(AgentState::Done));
        assert_eq!(
            parse_line("PermissionRequest\t{\"tool_name\":\"Bash\"}"),
            Some(AgentState::NeedsYou("Wants to use Bash".into()))
        );
        assert_eq!(parse_line("Otro\t{}"), None);
    }

    #[test]
    fn hook_command_appends_event_line() {
        let settings: Value =
            serde_json::from_str(&std::fs::read_to_string(claude_settings().unwrap()).unwrap()).unwrap();
        let cmd = settings["hooks"]["Stop"][0]["hooks"][0]["command"].as_str().unwrap().to_string();
        let file = new_status_file().unwrap();
        let mut child = std::process::Command::new("sh")
            .args(["-c", &cmd])
            .env("KUAKY_STATUS_FILE", &file)
            .stdin(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        use std::io::Write;
        child.stdin.take().unwrap().write_all(b"{\"a\":\n1}").unwrap();
        assert!(child.wait().unwrap().success());
        let text = std::fs::read_to_string(&file).unwrap();
        std::fs::remove_file(&file).ok();
        assert_eq!(text, "Stop\t{\"a\":1}\n");
        assert_eq!(parse_line(text.trim_end()), Some(AgentState::Done));
    }
}
