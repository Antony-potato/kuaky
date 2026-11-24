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

/// Settings file with kuaky hooks + statusline capture script (written once per run).
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
        let mut settings = json!({ "hooks": hooks });
        if let Some(script) = write_claude_statusline_script(&dir) {
            settings["statusLine"] = json!({
                "type": "command",
                "command": script.to_string_lossy(),
            });
        }
        std::fs::write(&path, serde_json::to_string_pretty(&settings).ok()?).ok()?;
        Some(path)
    })
    .as_ref()
}

/// Writes a wrapper script for Claude Code's `statusLine` hook: saves the incoming
/// JSON payload (including `rate_limits`) to `<config>/kuaky/claude-rate-limits.json`
/// (or `$KUAKY_CLAUDE_LIMITS_FILE`, as `kuaky_usage::limits` reads it), then forwards
/// stdin to the user's original `~/.claude/settings.json` statusLine command if one is configured.
fn write_claude_statusline_script(dir: &std::path::Path) -> Option<PathBuf> {
    let script_path = dir.join("claude-statusline.sh");
    let limits_path = dir.join("claude-rate-limits.json");
    let script = format!(
        r#"#!/bin/sh
OUT='{limits}'
[ -n "$KUAKY_CLAUDE_LIMITS_FILE" ] && OUT="$KUAKY_CLAUDE_LIMITS_FILE"
TMP="$OUT.tmp.$$"
cat > "$TMP"
if [ -s "$TMP" ]; then
    cp "$TMP" "$OUT.new.$$" 2>/dev/null && mv "$OUT.new.$$" "$OUT" 2>/dev/null
fi
ORIG=""
if [ -f "$HOME/.claude/settings.json" ] && command -v python3 >/dev/null 2>&1; then
    ORIG=$(python3 -c 'import json, os, sys
try:
    d = json.load(open(os.path.expanduser("~/.claude/settings.json")))
    sl = d.get("statusLine") or {{}}
    cmd = sl.get("command") if isinstance(sl, dict) else (sl if isinstance(sl, str) else "")
    if cmd and "claude-statusline.sh" not in cmd:
        sys.stdout.write(cmd)
except Exception:
    pass
' 2>/dev/null)
fi
if [ -n "$ORIG" ]; then
    sh -c "$ORIG" < "$TMP"
fi
rm -f "$TMP"
exit 0
"#,
        limits = limits_path.to_string_lossy().replace('\'', "'\\''"),
    );
    std::fs::write(&script_path, script).ok()?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&script_path, std::fs::Permissions::from_mode(0o755));
    }
    Some(script_path)
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

/// Claude Code's session ID from any hook line (`session_id` in the payload).
pub fn parse_session_id(line: &str) -> Option<String> {
    let (_, payload) = line.split_once('\t')?;
    let payload: Value = serde_json::from_str(payload).ok()?;
    let id = payload.get("session_id")?.as_str()?;
    crate::agent_history::valid_id(id).then(|| id.to_string())
}

/// Full prompt text (whitespace collapsed) from a `UserPromptSubmit` hook line.
pub fn parse_prompt_text(line: &str) -> Option<String> {
    let (event, payload) = line.split_once('\t')?;
    if event.trim() != "UserPromptSubmit" {
        return None;
    }
    let payload: Value = serde_json::from_str(payload).ok()?;
    let raw = payload.get("prompt").or_else(|| payload.get("user_prompt")).and_then(Value::as_str)?;
    let text = raw.trim();
    (!text.is_empty()).then(|| text.chars().take(2000).collect())
}

/// Extracts a concise title from a `UserPromptSubmit` hook line, if present.
pub fn parse_prompt(line: &str) -> Option<String> {
    let (event, payload) = line.split_once('\t')?;
    if event.trim() != "UserPromptSubmit" {
        return None;
    }
    let payload: Value = serde_json::from_str(payload).ok()?;
    let raw = payload
        .get("prompt")
        .or_else(|| payload.get("user_prompt"))
        .or_else(|| payload.get("message"))
        .and_then(Value::as_str)?;
    let collapsed: String = raw.split_whitespace().collect::<Vec<_>>().join(" ");
    if collapsed.is_empty() {
        return None;
    }
    let mut chars = collapsed.chars();
    let short: String = chars.by_ref().take(56).collect();
    if chars.next().is_some() {
        Some(format!("{short}…"))
    } else {
        Some(short)
    }
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
        assert_eq!(parse_prompt("UserPromptSubmit\t{\"prompt\":\"  Fix   sidebar   overflow  \"}"), Some("Fix sidebar overflow".into()));
        assert_eq!(parse_prompt("Stop\t{\"prompt\":\"ignored\"}"), None);
        let long = format!("UserPromptSubmit\t{{\"prompt\":\"{}\"}}", "a".repeat(80));
        let parsed = parse_prompt(&long).unwrap();
        assert!(parsed.ends_with('…'));
        assert!(parsed.chars().count() <= 57);
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

    #[test]
    fn statusline_script_captures_rate_limits() {
        let settings_path = claude_settings().unwrap();
        let settings: Value = serde_json::from_str(&std::fs::read_to_string(settings_path).unwrap()).unwrap();
        let script = settings["statusLine"]["command"].as_str().unwrap();
        // A file of its own: the real one holds the user's limits.
        let saved_path = std::env::temp_dir().join(format!("kuaky-limits-test-{}.json", std::process::id()));
        let mut child = std::process::Command::new("sh")
            .arg(script)
            .env("KUAKY_CLAUDE_LIMITS_FILE", &saved_path)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        use std::io::Write;
        let payload = r#"{"rate_limits":{"five_hour":{"used_percentage":42,"resets_at":1795500000},"seven_day":{"used_percentage":18}}}"#;
        child.stdin.take().unwrap().write_all(payload.as_bytes()).unwrap();
        assert!(child.wait().unwrap().success());
        let saved = std::fs::read_to_string(&saved_path).unwrap();
        std::fs::remove_file(&saved_path).ok();
        let limits = kuaky_usage::limits::parse_claude_statusline(&saved, None).unwrap();
        assert_eq!(limits.primary.unwrap().used_pct, 42.0);
        assert_eq!(limits.secondary.unwrap().used_pct, 18.0);
    }
}
