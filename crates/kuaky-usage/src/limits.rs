//! Remaining rate-limit quotas for supported agents.
//!
//! - **Claude Code:** `<config>/kuaky/claude-rate-limits.json`, written by kuaky's statusline
//!   wrapper (`rate_limits.five_hour` / `seven_day`) and by the chat, which also saves the
//!   Fable weekly window it reads from each reply ([`parse_claude_rate_info`]).
//! - **Codex:** read from the latest `token_count` event in `~/.codex/sessions/**/rollout-*.jsonl`
//!   (`rate_limits.primary` / `secondary`).

use crate::Agent;
use jiff::{Timestamp, tz::TimeZone};
use serde_json::{Value, json};
use std::{
    io::{BufRead, BufReader},
    path::{Path, PathBuf},
};

#[derive(Debug, Clone, PartialEq)]
pub struct RateWindow {
    /// Short label such as `"5h"` or `"week"`.
    pub label: String,
    /// Percentage of the window already used (`0.0..=100.0`).
    pub used_pct: f64,
    /// Percentage remaining (`0.0..=100.0`).
    pub remaining_pct: f64,
    /// When this window resets, if reported.
    pub resets_at: Option<Timestamp>,
}

impl RateWindow {
    pub fn from_used(label: impl Into<String>, used_pct: f64, resets_at: Option<Timestamp>) -> Self {
        let used = used_pct.clamp(0.0, 100.0);
        Self {
            label: label.into(),
            used_pct: used,
            remaining_pct: (100.0 - used).clamp(0.0, 100.0),
            resets_at,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct AgentLimits {
    pub agent: Agent,
    pub primary: Option<RateWindow>,
    pub secondary: Option<RateWindow>,
    /// Weekly window of a model with its own quota (Claude: Fable).
    pub model_week: Option<RateWindow>,
    pub updated_at: Option<Timestamp>,
}

impl AgentLimits {
    pub fn windows(&self) -> impl Iterator<Item = &RateWindow> {
        self.primary.iter().chain(self.secondary.iter()).chain(self.model_week.iter())
    }

    pub fn has_any(&self) -> bool {
        self.windows().next().is_some()
    }
}

/// Path where kuaky's Claude Code statusline wrapper writes the latest JSON payload.
pub fn claude_limits_path() -> Option<PathBuf> {
    if let Ok(custom) = std::env::var("KUAKY_CLAUDE_LIMITS_FILE") {
        if !custom.trim().is_empty() {
            return Some(PathBuf::from(custom));
        }
    }
    Some(dirs::config_dir()?.join("kuaky").join("claude-rate-limits.json"))
}

/// Loads remaining rate limits for all supported agents that have local data.
pub fn load_all_limits() -> Vec<AgentLimits> {
    let mut out = Vec::new();
    if let Some(claude) = load_claude_limits() {
        out.push(claude);
    }
    if let Some(codex) = load_codex_limits() {
        out.push(codex);
    }
    out
}

pub fn load_claude_limits() -> Option<AgentLimits> {
    let path = claude_limits_path()?;
    let text = std::fs::read_to_string(&path).ok()?;
    let updated_at = file_mtime(&path);
    parse_claude_statusline(&text, updated_at)
}

/// Claude's per-model weekly window. Claude Code calls it the "Fable limit".
const FABLE_KEY: &str = "seven_day_overage_included";

/// Parses Claude Code's statusline JSON (`rate_limits.five_hour` / `seven_day`, plus the
/// Fable window when the chat saved it).
pub fn parse_claude_statusline(json_text: &str, updated_at: Option<Timestamp>) -> Option<AgentLimits> {
    let root: Value = serde_json::from_str(json_text).ok()?;
    let rl = root.get("rate_limits")?;
    let window = |key: &str, label: &str| rl.get(key).and_then(|w| parse_claude_window(label, w));
    let limits = AgentLimits {
        agent: Agent::ClaudeCode,
        primary: window("five_hour", "5h"),
        secondary: window("seven_day", "week"),
        model_week: window(FABLE_KEY, "Fable"),
        updated_at,
    };
    limits.has_any().then_some(limits)
}

/// Parses the `rate_limit_info` Claude Code sends after each reply (in ACP, the
/// `_meta["_claude/rateLimit"]` of `usage_update`). Its `unifiedWindows` hold every
/// window, with `utilization` as a fraction of the limit.
pub fn parse_claude_rate_info(info: &Value, updated_at: Option<Timestamp>) -> Option<AgentLimits> {
    let windows = info.get("unifiedWindows")?;
    let window = |key: &str, label: &str| {
        let w = windows.get(key)?;
        let used = w.get("utilization")?.as_f64()? * 100.0;
        Some(RateWindow::from_used(label, used, w.get("resetsAt").and_then(parse_timestamp)))
    };
    let limits = AgentLimits {
        agent: Agent::ClaudeCode,
        primary: window("five_hour", "5h"),
        secondary: window("seven_day", "week"),
        model_week: window(FABLE_KEY, "Fable"),
        updated_at,
    };
    limits.has_any().then_some(limits)
}

/// Claude limits in the statusline file format (what [`parse_claude_statusline`] reads).
pub fn claude_limits_json(limits: &AgentLimits) -> String {
    let mut rl = serde_json::Map::new();
    for (key, w) in [("five_hour", &limits.primary), ("seven_day", &limits.secondary), (FABLE_KEY, &limits.model_week)] {
        if let Some(w) = w {
            rl.insert(key.into(), json!({ "used_percentage": w.used_pct, "resets_at": w.resets_at.map(|t| t.as_second()) }));
        }
    }
    json!({ "rate_limits": rl }).to_string()
}

/// Saves Claude limits where every view reads them (replacing the file in one step).
pub fn save_claude_limits(limits: &AgentLimits) -> std::io::Result<()> {
    let path = claude_limits_path().ok_or_else(|| std::io::Error::other("no config folder"))?;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension(format!("json.{}", std::process::id()));
    std::fs::write(&tmp, claude_limits_json(limits))?;
    std::fs::rename(&tmp, &path)
}

fn parse_claude_window(label: &str, v: &Value) -> Option<RateWindow> {
    let used = v
        .get("used_percentage")
        .or_else(|| v.get("used_percent"))
        .or_else(|| v.get("usedPercentage"))
        .and_then(Value::as_f64)?;
    let resets_at = v.get("resets_at").or_else(|| v.get("resetsAt")).and_then(parse_timestamp);
    Some(RateWindow::from_used(label, used, resets_at))
}

pub fn load_codex_limits() -> Option<AgentLimits> {
    let roots = codex_roots();
    load_codex_limits_from(&roots)
}

fn codex_roots() -> Vec<PathBuf> {
    if let Ok(env) = std::env::var("CODEX_HOME") {
        if !env.is_empty() {
            return vec![PathBuf::from(env)];
        }
    }
    dirs::home_dir().map(|h| vec![h.join(".codex")]).unwrap_or_default()
}

/// Scans recent Codex session `.jsonl` files for the newest `token_count` event carrying `rate_limits`.
pub fn load_codex_limits_from(roots: &[PathBuf]) -> Option<AgentLimits> {
    let mut files: Vec<(std::time::SystemTime, PathBuf)> = Vec::new();
    for root in roots {
        for sub in ["sessions", "archived_sessions"] {
            let dir = root.join(sub);
            if !dir.is_dir() {
                continue;
            }
            for entry in walkdir::WalkDir::new(dir).into_iter().filter_map(Result::ok) {
                if !entry.file_type().is_file() {
                    continue;
                }
                if entry.path().extension().and_then(|e| e.to_str()) != Some("jsonl") {
                    continue;
                }
                let mtime = entry.metadata().ok().and_then(|m| m.modified().ok()).unwrap_or(std::time::UNIX_EPOCH);
                files.push((mtime, entry.into_path()));
            }
        }
    }
    files.sort_by_key(|(m, _)| std::cmp::Reverse(*m));
    for (_, path) in files.into_iter().take(12) {
        if let Some(limits) = parse_codex_session_file(&path) {
            return Some(limits);
        }
    }
    None
}

fn parse_codex_session_file(path: &Path) -> Option<AgentLimits> {
    let file = std::fs::File::open(path).ok()?;
    let mut latest: Option<AgentLimits> = None;
    for line in BufReader::new(file).lines() {
        let Ok(line) = line else { break };
        if !line.contains("rate_limits") {
            continue;
        }
        if let Some(found) = parse_codex_line(&line) {
            latest = Some(found);
        }
    }
    latest
}

/// Parses a single Codex JSONL line containing a `token_count` event with `rate_limits`.
pub fn parse_codex_line(line: &str) -> Option<AgentLimits> {
    let root: Value = serde_json::from_str(line).ok()?;
    if root.get("type").and_then(Value::as_str) != Some("event_msg") {
        return None;
    }
    let payload = root.get("payload")?;
    if payload.get("type").and_then(Value::as_str) != Some("token_count") {
        return None;
    }
    let rl = payload
        .get("rate_limits")
        .or_else(|| payload.get("info").and_then(|i| i.get("rate_limits")))?;
    let event_ts = root.get("timestamp").and_then(parse_timestamp);
    let primary = rl.get("primary").and_then(|w| parse_codex_window(w, event_ts, "5h"));
    let secondary = rl.get("secondary").and_then(|w| parse_codex_window(w, event_ts, "week"));
    let limits = AgentLimits { agent: Agent::Codex, primary, secondary, model_week: None, updated_at: event_ts };
    limits.has_any().then_some(limits)
}

fn parse_codex_window(v: &Value, event_ts: Option<Timestamp>, fallback_label: &str) -> Option<RateWindow> {
    let used = v
        .get("used_percent")
        .or_else(|| v.get("used_percentage"))
        .and_then(Value::as_f64)?;
    let label = v
        .get("window_minutes")
        .and_then(Value::as_u64)
        .map(format_window_minutes)
        .unwrap_or_else(|| fallback_label.to_string());
    let resets_at = v
        .get("resets_at")
        .and_then(parse_timestamp)
        .or_else(|| {
            let delta = v.get("resets_in_seconds").and_then(Value::as_i64)?;
            let base = event_ts?;
            Timestamp::from_second(base.as_second().saturating_add(delta)).ok()
        });
    Some(RateWindow::from_used(label, used, resets_at))
}

fn format_window_minutes(mins: u64) -> String {
    if mins == 10_080 {
        "week".into()
    } else if mins % 1440 == 0 && mins >= 1440 {
        format!("{}d", mins / 1440)
    } else if mins % 60 == 0 && mins >= 60 {
        format!("{}h", mins / 60)
    } else {
        format!("{mins}m")
    }
}

fn parse_timestamp(v: &Value) -> Option<Timestamp> {
    if let Some(s) = v.as_str() {
        return s.parse().ok();
    }
    let raw = v.as_i64().or_else(|| v.as_f64().map(|f| f.round() as i64))?;
    if raw > 1_000_000_000_000 {
        Timestamp::from_millisecond(raw).ok()
    } else if raw > 0 {
        Timestamp::from_second(raw).ok()
    } else {
        None
    }
}

fn file_mtime(path: &Path) -> Option<Timestamp> {
    let mtime = std::fs::metadata(path).ok()?.modified().ok()?;
    let dur = mtime.duration_since(std::time::UNIX_EPOCH).ok()?;
    Timestamp::from_second(dur.as_secs() as i64).ok()
}

/// Formats a reset timestamp in local time: `"3:40 pm"` if today, or `"Thu 3:40 pm"` otherwise.
pub fn format_reset(ts: Timestamp, now: Timestamp, tz: &TimeZone) -> Option<String> {
    if ts <= now {
        return None;
    }
    let z = ts.to_zoned(tz.clone());
    let today = now.to_zoned(tz.clone()).date();
    let hour24 = z.hour();
    let (hour12, suffix) = match hour24 {
        0 => (12, "am"),
        1..=11 => (hour24, "am"),
        12 => (12, "pm"),
        _ => (hour24 - 12, "pm"),
    };
    let time = format!("{}:{:02} {}", hour12, z.minute(), suffix);
    if z.date() == today {
        Some(time)
    } else {
        let day = z.strftime("%a");
        Some(format!("{day} {time}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_claude_statusline_rate_limits() {
        let json = r#"{
            "model": { "id": "claude-opus-5-5" },
            "rate_limits": {
                "five_hour": { "used_percentage": 42.4, "resets_at": 1795500000 },
                "seven_day": { "used_percentage": 18.0, "resets_at": 1796000000 }
            }
        }"#;
        let limits = parse_claude_statusline(json, None).unwrap();
        assert_eq!(limits.agent, Agent::ClaudeCode);
        let p = limits.primary.unwrap();
        assert_eq!(p.label, "5h");
        assert!((p.used_pct - 42.4).abs() < 0.01);
        assert!((p.remaining_pct - 57.6).abs() < 0.01);
        assert_eq!(p.resets_at.unwrap().as_second(), 1795500000);
        let s = limits.secondary.unwrap();
        assert_eq!(s.label, "week");
        assert_eq!(s.used_pct, 18.0);
    }

    #[test]
    fn parses_claude_rate_info_with_fable_window() {
        let info = serde_json::json!({
            "status": "allowed",
            "rateLimitType": "five_hour",
            "utilization": 0.42,
            "unifiedWindows": {
                "five_hour": { "utilization": 0.42, "resetsAt": 1795500000, "status": "allowed" },
                "seven_day": { "utilization": 0.18, "resetsAt": 1796000000 },
                "seven_day_overage_included": { "utilization": 0.065, "resetsAt": 1796000000 }
            }
        });
        let limits = parse_claude_rate_info(&info, None).unwrap();
        assert!((limits.primary.as_ref().unwrap().used_pct - 42.0).abs() < 0.01);
        assert!((limits.secondary.as_ref().unwrap().used_pct - 18.0).abs() < 0.01);
        let fable = limits.model_week.as_ref().unwrap();
        assert_eq!(fable.label, "Fable");
        assert!((fable.used_pct - 6.5).abs() < 0.01);
        assert_eq!(limits.windows().count(), 3);
        // Without the per-window detail there's nothing to show.
        assert_eq!(parse_claude_rate_info(&serde_json::json!({ "status": "allowed" }), None), None);
    }

    #[test]
    fn saved_claude_limits_read_back_the_same() {
        let info = serde_json::json!({ "unifiedWindows": {
            "five_hour": { "utilization": 0.3, "resetsAt": 1795500000 },
            "seven_day_overage_included": { "utilization": 0.5, "resetsAt": 1796000000 }
        } });
        let limits = parse_claude_rate_info(&info, None).unwrap();
        let back = parse_claude_statusline(&claude_limits_json(&limits), None).unwrap();
        assert_eq!(back, limits);
        assert!(back.secondary.is_none());
    }

    #[test]
    fn parses_codex_token_count_rate_limits() {
        let line = r#"{"timestamp":"2026-11-23T15:00:00Z","type":"event_msg","payload":{"type":"token_count","rate_limits":{"primary":{"used_percent":35.0,"window_minutes":300,"resets_in_seconds":3600},"secondary":{"used_percent":12.5,"window_minutes":10080,"resets_at":1796000000}}}}"#;
        let limits = parse_codex_line(line).unwrap();
        assert_eq!(limits.agent, Agent::Codex);
        let p = limits.primary.unwrap();
        assert_eq!(p.label, "5h");
        assert_eq!(p.used_pct, 35.0);
        assert_eq!(p.remaining_pct, 65.0);
        assert_eq!(p.resets_at.unwrap().as_second(), limits.updated_at.unwrap().as_second() + 3600);
        let s = limits.secondary.unwrap();
        assert_eq!(s.label, "week");
        assert_eq!(s.used_pct, 12.5);
    }

    #[test]
    fn formats_reset_time_in_local_tz() {
        let now: Timestamp = "2026-11-23T14:00:00Z".parse().unwrap();
        let later: Timestamp = "2026-11-23T15:40:00Z".parse().unwrap();
        let past: Timestamp = "2026-11-23T13:00:00Z".parse().unwrap();
        assert_eq!(format_reset(later, now, &TimeZone::UTC).as_deref(), Some("3:40 pm"));
        assert_eq!(format_reset(past, now, &TimeZone::UTC), None);
    }
}
