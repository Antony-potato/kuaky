//! Estado que kuaky recuerda entre aperturas: proyectos recientes, anchos de
//! paneles y, por proyecto, archivos abiertos y sesiones de agentes.
//!
//! Se guarda en `<config>/kuaky/state.json` (en macOS,
//! `~/Library/Application Support/kuaky/state.json`).

use gpui_kit::{App, Global};
use serde::{Deserialize, Serialize};
use std::{collections::HashMap, path::PathBuf};

const MAX_RECENT: usize = 20;

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
}

#[derive(Serialize, Deserialize, Clone)]
pub struct RecentProject {
    pub path: String,
    /// Última apertura (segundos Unix).
    pub opened: i64,
}

#[derive(Serialize, Deserialize, Default, Clone, PartialEq)]
pub struct ProjectState {
    #[serde(default)]
    pub files: Vec<String>,
    #[serde(default)]
    pub sessions: Vec<SavedSession>,
}

#[derive(Serialize, Deserialize, Clone, PartialEq)]
pub struct SavedSession {
    /// Clave del agente (`claude`, `codex`, `shell`…).
    pub harness: String,
    pub cwd: String,
    #[serde(default)]
    pub worktree: Option<String>,
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

/// Modifica el estado y lo guarda en disco.
pub fn update(cx: &mut App, f: impl FnOnce(&mut State)) {
    let state = &mut cx.global_mut::<Persisted>().0;
    let before = serde_json::to_string(state).ok();
    f(state);
    let Ok(json) = serde_json::to_string_pretty(state) else { return };
    if before.as_deref() == serde_json::to_string(state).ok().as_deref() {
        return;
    }
    let Some(path) = path() else { return };
    // Escritura atómica: archivo temporal y luego renombrar.
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
