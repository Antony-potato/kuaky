//! Persistent application state: recent projects, panel sizes, settings,
//! and per-project open files and agent sessions.
//!
//! Stored in `<config>/kuaky/state.json` (on macOS,
//! `~/Library/Application Support/kuaky/state.json`).

use gpui_kit::{App, Global};
use serde::{Deserialize, Serialize};
use std::{collections::HashMap, path::PathBuf};

const MAX_RECENT: usize = 20;

fn default_true() -> bool {
    true
}

#[derive(Serialize, Deserialize, Default, Clone)]
pub struct State {
    #[serde(default)]
    pub recent: Vec<RecentProject>,
    #[serde(default)]
    pub projects: HashMap<String, ProjectState>,
    #[serde(default)]
    pub left_width: Option<f32>,
    #[serde(default)]
    pub right_width: Option<f32>,
    #[serde(default)]
    pub right_bottom_height: Option<f32>,
    #[serde(default)]
    pub settings: Settings,
    /// Project open when kuaky was closed; reopened on launch (`None` = Home).
    #[serde(default)]
    pub last_open: Option<String>,
    /// Projects open as tabs when kuaky was closed (in tab order).
    #[serde(default)]
    pub open: Vec<String>,
    /// Left sidebar folded into a thin rail.
    #[serde(default)]
    pub left_collapsed: bool,
}

#[derive(Serialize, Deserialize, Default, Clone, PartialEq)]
pub struct Settings {
    /// Per-agent configuration keyed by harness key (`claude`, `codex`, `agy`, `gemini`, `opencode`).
    #[serde(default)]
    pub agents: HashMap<String, AgentConfig>,
    /// Custom base directory for git worktrees (defaults to `~/.kuaky/worktrees`).
    #[serde(default)]
    pub worktrees_dir: String,
    /// Right-panel tabs the user hid (`files`, `changes`, `branches`, `prs`, `graph`).
    #[serde(default)]
    pub hidden_panels: Vec<String>,
    /// How the single "Merge" button merges PRs: `merge`, `squash` or `rebase`.
    #[serde(default)]
    pub pr_merge_method: String,
    /// Last choices in the chat box per agent (model, mode, effort, toggles),
    /// applied to every new chat session: agent label → option id → value.
    #[serde(default)]
    pub chat_prefs: HashMap<String, HashMap<String, String>>,
}

impl Settings {
    pub fn agent(&self, key: &str) -> AgentConfig {
        self.agents.get(key).cloned().unwrap_or_default()
    }

    pub fn is_visible(&self, key: &str) -> bool {
        self.agents.get(key).map(|a| a.visible).unwrap_or(true)
    }

    pub fn resolved_worktrees_dir(&self) -> Option<PathBuf> {
        let raw = self.worktrees_dir.trim();
        if raw.is_empty() {
            return None;
        }
        if let Some(rest) = raw.strip_prefix("~/") {
            if let Some(home) = dirs::home_dir() {
                return Some(home.join(rest));
            }
        }
        Some(PathBuf::from(raw))
    }
}

#[derive(Serialize, Deserialize, Clone, PartialEq)]
pub struct AgentConfig {
    #[serde(default = "default_true")]
    pub visible: bool,
    #[serde(default)]
    pub binary_path: String,
    #[serde(default)]
    pub extra_args: String,
    /// How "New session" opens this agent: `chat` (default) or `terminal`.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub open_as: String,
    /// Where it works: this folder (default) or `worktree` (a new branch in its own copy).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub workspace: String,
}

impl Default for AgentConfig {
    fn default() -> Self {
        Self { visible: true, binary_path: String::new(), extra_args: String::new(), open_as: String::new(), workspace: String::new() }
    }
}

impl AgentConfig {
    pub fn as_chat(&self) -> bool {
        self.open_as != "terminal"
    }

    pub fn in_worktree(&self) -> bool {
        self.workspace == "worktree"
    }
}

#[derive(Serialize, Deserialize, Clone)]
pub struct RecentProject {
    pub path: String,
    /// Last opened (Unix seconds).
    pub opened: i64,
}

#[derive(Serialize, Deserialize, Default, Clone, PartialEq)]
pub struct ProjectState {
    #[serde(default)]
    pub files: Vec<String>,
    #[serde(default)]
    pub sessions: Vec<SavedSession>,
    /// Sessions (kuaky uid) whose subagent list was left unfolded.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub subagents_open: Vec<String>,
}

#[derive(Serialize, Deserialize, Clone, PartialEq)]
pub struct SavedSession {
    /// Harness key (`claude`, `codex`, `shell`…).
    pub harness: String,
    pub cwd: String,
    #[serde(default)]
    pub worktree: Option<String>,
    #[serde(default)]
    pub custom_name: Option<String>,
    #[serde(default)]
    pub custom_color: Option<u32>,
    /// kuaky's session ID (links the session to its metro line).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub uid: Option<String>,
    /// The agent's own conversation ID, to resume exactly this conversation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub conversation: Option<String>,
    /// Opened as kuaky's own chat (ACP) instead of a terminal.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub chat: bool,
    /// It was open when kuaky closed: reopened automatically on launch.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub open: bool,
    /// Listed from the agent's own history (not saved by kuaky).
    #[serde(skip)]
    pub from_history: bool,
    /// Last activity, for history entries.
    #[serde(skip)]
    pub updated: Option<jiff::Timestamp>,
}

struct Persisted(State);
impl Global for Persisted {}

fn path() -> Option<PathBuf> {
    Some(dirs::config_dir()?.join("kuaky").join("state.json"))
}

pub fn init(cx: &mut App) {
    let state = path()
        .and_then(|p| std::fs::read_to_string(p).ok())
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default();
    cx.set_global(Persisted(state));
}

pub fn get(cx: &App) -> &State {
    &cx.global::<Persisted>().0
}

/// Mutates state and writes it atomically to disk.
pub fn update(cx: &mut App, f: impl FnOnce(&mut State)) {
    let state = &mut cx.global_mut::<Persisted>().0;
    let before = serde_json::to_string(state).ok();
    f(state);
    let Ok(json) = serde_json::to_string_pretty(state) else { return };
    if before.as_deref() == serde_json::to_string(state).ok().as_deref() {
        return;
    }
    let Some(path) = path() else { return };
    cx.background_executor()
        .spawn(async move {
            if let Some(dir) = path.parent() {
                let _ = std::fs::create_dir_all(dir);
            }
            let tmp = path.with_extension("json.tmp");
            if std::fs::write(&tmp, json).is_ok() {
                let _ = std::fs::rename(&tmp, &path);
            }
        })
        .detach();
}

pub fn touch_recent(state: &mut State, project: &str) {
    state.recent.retain(|r| r.path != project);
    state.recent.insert(0, RecentProject { path: project.to_string(), opened: jiff::Timestamp::now().as_second() });
    state.recent.truncate(MAX_RECENT);
}
