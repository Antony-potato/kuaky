//! Mapa de metro: cada **línea** es un objetivo (plan, feature, bug o tarea);
//! sus **estaciones** son los pasos; las sesiones de agentes son los trenes.
//!
//! Las estaciones se crean solas mientras se habla con el agente:
//! - prompts del usuario (hooks de Claude Code);
//! - título de la terminal (resumen que pone el agente);
//! - lista de tareas del agente (`agent_tasks`);
//! - hitos de git (commits en la rama, PR, merge).
//!
//! Se guarda por proyecto en `<config>/kuaky/lines/<proyecto>.json`.

use crate::agent_tasks::{AgentTask, TaskStatus};
use jiff::Timestamp;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug, Default)]
#[serde(rename_all = "lowercase")]
pub enum LineKind {
    Plan,
    Feature,
    Bug,
    #[default]
    Task,
}

impl LineKind {
    pub const ALL: [LineKind; 4] = [LineKind::Plan, LineKind::Feature, LineKind::Bug, LineKind::Task];

    pub fn label(self) -> &'static str {
        match self {
            LineKind::Plan => "Plan",
            LineKind::Feature => "Feature",
            LineKind::Bug => "Bug",
            LineKind::Task => "Task",
        }
    }
}

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug)]
#[serde(rename_all = "lowercase")]
pub enum StationState {
    Done,
    Active,
    Pending,
}

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug)]
#[serde(rename_all = "lowercase")]
pub enum StationSource {
    Prompt,
    Title,
    Todo,
    Git,
    Manual,
    Connection,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Station {
    pub id: u64,
    pub title: String,
    /// Texto completo (p. ej. el prompt) cuando `title` es un resumen.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub detail: String,
    pub state: StationState,
    pub source: StationSource,
    /// Sesión que la creó o la está recorriendo.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session: Option<String>,
    pub at: Timestamp,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct LineSession {
    /// Identificador estable de la sesión en kuaky (`SavedSession::uid`).
    pub uid: String,
    pub harness: String,
    #[serde(default)]
    pub conversation: Option<String>,
    #[serde(default)]
    pub cwd: String,
    #[serde(default)]
    pub label: String,
    pub last_active: Timestamp,
    /// Tipo de conexión si llegó a revisar/probar lo de otra sesión.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Line {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub kind: LineKind,
    pub color: u32,
    /// Línea padre (sub-línea de un plan).
    #[serde(default)]
    pub parent: Option<String>,
    /// Rama propia (worktree) si la tiene.
    #[serde(default)]
    pub branch: Option<String>,
    #[serde(default)]
    pub stations: Vec<Station>,
    #[serde(default)]
    pub sessions: Vec<LineSession>,
    #[serde(default)]
    pub archived: bool,
    #[serde(default)]
    pub merged: bool,
    /// El nombre lo puso el usuario: no se cambia solo.
    #[serde(default)]
    pub named: bool,
    pub created: Timestamp,
    pub updated: Timestamp,
    #[serde(default)]
    next_station: u64,
}

pub const PALETTE: [u32; 8] = [0x2dd4bf, 0xa78bfa, 0xfb923c, 0x60a5fa, 0xf472b6, 0xa3e635, 0xfacc15, 0xf87171];

impl Line {
    fn station_mut(&mut self, id: u64) -> Option<&mut Station> {
        self.stations.iter_mut().find(|s| s.id == id)
    }

    fn push(&mut self, title: String, detail: String, state: StationState, source: StationSource, session: Option<String>, now: Timestamp) -> u64 {
        self.next_station += 1;
        let id = self.next_station;
        self.stations.push(Station { id, title, detail, state, source, session, at: now });
        self.updated = now;
        id
    }

    /// Estación en curso de una sesión (prompt o título).
    fn active_of(&self, session: &str) -> Option<u64> {
        self.stations
            .iter()
            .rev()
            .find(|s| {
                s.session.as_deref() == Some(session)
                    && s.state == StationState::Active
                    && matches!(s.source, StationSource::Prompt | StationSource::Title)
            })
            .map(|s| s.id)
    }

    pub fn done_count(&self) -> usize {
        self.stations.iter().filter(|s| s.state == StationState::Done).count()
    }

    pub fn is_finished(&self) -> bool {
        self.merged || (self.archived && !self.stations.iter().any(|s| s.state == StationState::Active))
    }

    /// Lo último hecho y lo siguiente pendiente, para "¿En qué me quedé?".
    pub fn last_done(&self) -> Option<&Station> {
        self.stations.iter().filter(|s| s.state == StationState::Done).max_by_key(|s| s.at)
    }

    pub fn next_pending(&self) -> Option<&Station> {
        self.stations.iter().find(|s| s.state == StationState::Pending)
    }

    pub fn current(&self) -> Option<&Station> {
        self.stations.iter().rev().find(|s| s.state == StationState::Active)
    }

    /// Nota de relevo para una sesión nueva en esta línea (se deja escrita, no se envía).
    pub fn relay_note(&self) -> String {
        let mut out = format!("Context from kuaky — goal: {} ({}).", self.name, self.kind.label().to_lowercase());
        if let Some(branch) = &self.branch {
            out.push_str(&format!(" Branch: {branch}."));
        }
        let done: Vec<&str> = self.stations.iter().filter(|s| s.state == StationState::Done).map(|s| s.title.as_str()).collect();
        if !done.is_empty() {
            let tail = &done[done.len().saturating_sub(6)..];
            out.push_str(&format!(" Done so far: {}.", tail.join("; ")));
        }
        let pending: Vec<&str> =
            self.stations.iter().filter(|s| s.state != StationState::Done).map(|s| s.title.as_str()).take(6).collect();
        if !pending.is_empty() {
            out.push_str(&format!(" Still open: {}.", pending.join("; ")));
        }
        out
    }
}

#[derive(Serialize, Deserialize, Default)]
struct Saved {
    #[serde(default)]
    lines: Vec<Line>,
}

pub struct LineStore {
    path: Option<PathBuf>,
    pub lines: Vec<Line>,
}

const MAX_STATIONS: usize = 60;

impl LineStore {
    pub fn load(root: &Path) -> LineStore {
        let path = store_path(root);
        let lines = path
            .as_ref()
            .and_then(|p| std::fs::read_to_string(p).ok())
            .and_then(|s| serde_json::from_str::<Saved>(&s).ok())
            .map(|s| s.lines)
            .unwrap_or_default();
        LineStore { path, lines }
    }

    #[cfg(test)]
    pub fn in_memory() -> LineStore {
        LineStore { path: None, lines: Vec::new() }
    }

    pub fn save(&self) {
        let Some(path) = &self.path else { return };
        let Ok(json) = serde_json::to_string_pretty(&Saved { lines: self.lines.clone() }) else { return };
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let tmp = path.with_extension("json.tmp");
        if std::fs::write(&tmp, json).is_ok() {
            let _ = std::fs::rename(&tmp, path);
        }
    }

    /// Carpeta para archivos de una línea (hallazgos de revisiones, propuestas).
    pub fn files_dir(&self, line_id: &str) -> Option<PathBuf> {
        let path = self.path.as_ref()?;
        Some(path.with_extension("").join(line_id))
    }

    pub fn get(&self, id: &str) -> Option<&Line> {
        self.lines.iter().find(|l| l.id == id)
    }

    pub fn get_mut(&mut self, id: &str) -> Option<&mut Line> {
        self.lines.iter_mut().find(|l| l.id == id)
    }

    /// Línea a la que pertenece una sesión.
    pub fn line_of(&self, uid: &str) -> Option<&Line> {
        self.lines.iter().find(|l| l.sessions.iter().any(|s| s.uid == uid))
    }

    pub fn create(&mut self, name: &str, kind: LineKind, parent: Option<String>, named: bool) -> String {
        let now = Timestamp::now();
        let id = format!("l{}", now.as_millisecond());
        let id = if self.get(&id).is_some() { format!("{id}-{}", self.lines.len()) } else { id };
        let used: Vec<u32> = self.lines.iter().filter(|l| !l.is_finished()).map(|l| l.color).collect();
        let color = PALETTE.iter().copied().find(|c| !used.contains(c)).unwrap_or(PALETTE[self.lines.len() % PALETTE.len()]);
        self.lines.push(Line {
            id: id.clone(),
            name: name.to_string(),
            kind,
            color,
            parent,
            branch: None,
            stations: Vec::new(),
            sessions: Vec::new(),
            archived: false,
            merged: false,
            named,
            created: now,
            updated: now,
            next_station: 0,
        });
        self.save();
        id
    }

    /// Asocia una sesión a una línea (la saca de cualquier otra).
    pub fn attach(&mut self, line_id: &str, session: LineSession) {
        for l in &mut self.lines {
            if l.id != line_id {
                l.sessions.retain(|s| s.uid != session.uid);
            }
        }
        if let Some(line) = self.get_mut(line_id) {
            match line.sessions.iter_mut().find(|s| s.uid == session.uid) {
                Some(existing) => {
                    existing.last_active = session.last_active;
                    if session.conversation.is_some() {
                        existing.conversation = session.conversation;
                    }
                    if session.role.is_some() {
                        existing.role = session.role;
                    }
                }
                None => line.sessions.push(session),
            }
            line.archived = false;
            line.updated = Timestamp::now();
        }
        self.save();
    }

    pub fn set_conversation(&mut self, uid: &str, conversation: &str) {
        let mut changed = false;
        for line in &mut self.lines {
            for s in line.sessions.iter_mut().filter(|s| s.uid == uid) {
                if s.conversation.as_deref() != Some(conversation) {
                    s.conversation = Some(conversation.to_string());
                    changed = true;
                }
            }
        }
        if changed {
            self.save();
        }
    }

    fn line_for_session_mut(&mut self, uid: &str) -> Option<&mut Line> {
        self.lines.iter_mut().find(|l| l.sessions.iter().any(|s| s.uid == uid))
    }

    /// Nuevo prompt del usuario: la estación anterior de esa sesión queda hecha
    /// y se abre una nueva.
    pub fn on_prompt(&mut self, uid: &str, prompt: &str) {
        let now = Timestamp::now();
        let Some(line) = self.line_for_session_mut(uid) else { return };
        if let Some(prev) = line.active_of(uid) {
            if let Some(s) = line.station_mut(prev) {
                s.state = StationState::Done;
            }
        }
        line.push(short(prompt, 60), prompt.to_string(), StationState::Active, StationSource::Prompt, Some(uid.to_string()), now);
        if !line.named && line.stations.iter().filter(|s| s.source == StationSource::Prompt).count() == 1 {
            line.name = short(prompt, 48);
        }
        touch_session(line, uid, now);
        trim(line);
        self.save();
    }

    /// El agente cambió el título de la terminal (resumen de la tarea actual).
    pub fn on_title(&mut self, uid: &str, title: &str) {
        let title = title.trim();
        if title.is_empty() {
            return;
        }
        let now = Timestamp::now();
        let Some(line) = self.line_for_session_mut(uid) else { return };
        let has_prompts = line.stations.iter().any(|s| s.session.as_deref() == Some(uid) && s.source == StationSource::Prompt);
        match line.active_of(uid) {
            // Con prompts (hooks): el título resume la estación en curso.
            Some(id) if has_prompts => {
                if let Some(s) = line.station_mut(id) {
                    if s.title != title {
                        s.title = short(title, 60);
                    }
                }
            }
            // Sin hooks: cada título distinto es un paso nuevo.
            Some(id) => {
                let same = line.station_mut(id).is_some_and(|s| s.title == short(title, 60));
                if !same {
                    if let Some(s) = line.station_mut(id) {
                        s.state = StationState::Done;
                    }
                    line.push(short(title, 60), String::new(), StationState::Active, StationSource::Title, Some(uid.to_string()), now);
                }
            }
            None => {
                line.push(short(title, 60), String::new(), StationState::Active, StationSource::Title, Some(uid.to_string()), now);
            }
        }
        // Sin prompts, el primer título nombra la línea.
        if !line.named && line.stations.len() == 1 && line.stations[0].source == StationSource::Title {
            line.name = short(title, 48);
        }
        touch_session(line, uid, now);
        trim(line);
        self.save();
    }

    /// Terminó el turno del agente: la estación en curso de esa sesión queda hecha.
    pub fn on_turn_done(&mut self, uid: &str) {
        let Some(line) = self.line_for_session_mut(uid) else { return };
        let Some(id) = line.active_of(uid) else { return };
        let has_prompts = line.stations.iter().any(|s| s.session.as_deref() == Some(uid) && s.source == StationSource::Prompt);
        if has_prompts {
            if let Some(s) = line.station_mut(id) {
                s.state = StationState::Done;
            }
            line.updated = Timestamp::now();
            self.save();
        }
    }

    /// Lista de tareas del agente → estaciones (conserva las existentes por título).
    pub fn set_tasks(&mut self, uid: &str, tasks: &[AgentTask]) {
        let now = Timestamp::now();
        let Some(line) = self.line_for_session_mut(uid) else { return };
        let mut keep: Vec<u64> = Vec::new();
        for task in tasks {
            let state = match task.status {
                TaskStatus::Completed => StationState::Done,
                TaskStatus::InProgress => StationState::Active,
                TaskStatus::Pending => StationState::Pending,
            };
            let existing = line
                .stations
                .iter()
                .find(|s| s.source == StationSource::Todo && s.session.as_deref() == Some(uid) && s.title == short(&task.subject, 60) && !keep.contains(&s.id))
                .map(|s| s.id);
            let id = match existing {
                Some(id) => {
                    if let Some(s) = line.station_mut(id) {
                        if s.state != state {
                            s.state = state;
                            s.at = now;
                        }
                    }
                    id
                }
                None => line.push(short(&task.subject, 60), task.subject.clone(), state, StationSource::Todo, Some(uid.to_string()), now),
            };
            keep.push(id);
        }
        line.stations.retain(|s| !(s.source == StationSource::Todo && s.session.as_deref() == Some(uid)) || keep.contains(&s.id));
        line.updated = now;
        self.save();
    }

    /// Hitos de git de la rama de la línea.
    pub fn set_git(&mut self, line_id: &str, ahead: u32, merged: bool, pr: Option<(u64, String)>) {
        let now = Timestamp::now();
        let Some(line) = self.get_mut(line_id) else { return };
        let before = line.clone();
        let commits = (ahead > 0).then(|| format!("{ahead} commit{}", if ahead == 1 { "" } else { "s" }));
        upsert_git(line, "commits", commits, now);
        upsert_git(line, "pr", pr.map(|(n, title)| format!("PR #{n} · {}", short(&title, 40))), now);
        // Se integró a main después de haber tenido commits.
        let had_commits = line.stations.iter().any(|s| s.source == StationSource::Git);
        if merged && had_commits && !line.merged {
            line.merged = true;
            for s in line.stations.iter_mut().filter(|s| s.state == StationState::Active) {
                s.state = StationState::Done;
            }
            line.push("Merged".into(), String::new(), StationState::Done, StationSource::Git, None, now);
        }
        if *line != before {
            line.updated = now;
            self.save();
        }
    }

    pub fn add_station(&mut self, line_id: &str, title: &str, source: StationSource, session: Option<String>, state: StationState) {
        let now = Timestamp::now();
        if let Some(line) = self.get_mut(line_id) {
            line.push(short(title, 60), title.to_string(), state, source, session, now);
            trim(line);
        }
        self.save();
    }

    pub fn rename(&mut self, line_id: &str, name: &str) {
        if let Some(line) = self.get_mut(line_id) {
            line.name = name.trim().to_string();
            line.named = true;
        }
        self.save();
    }

    pub fn set_kind(&mut self, line_id: &str, kind: LineKind) {
        if let Some(line) = self.get_mut(line_id) {
            line.kind = kind;
        }
        self.save();
    }

    pub fn set_parent(&mut self, line_id: &str, parent: Option<String>) {
        if parent.as_deref() == Some(line_id) {
            return;
        }
        if let Some(line) = self.get_mut(line_id) {
            line.parent = parent;
        }
        self.save();
    }

    pub fn set_branch(&mut self, line_id: &str, branch: Option<String>) {
        if let Some(line) = self.get_mut(line_id) {
            if line.branch.is_none() {
                line.branch = branch;
            }
        }
        self.save();
    }

    pub fn set_archived(&mut self, line_id: &str, archived: bool) {
        if let Some(line) = self.get_mut(line_id) {
            line.archived = archived;
        }
        self.save();
    }

    /// Junta `from` dentro de `into` (sesiones y estaciones).
    pub fn merge_into(&mut self, from: &str, into: &str) {
        if from == into {
            return;
        }
        let Some(pos) = self.lines.iter().position(|l| l.id == from) else { return };
        let source = self.lines.remove(pos);
        if let Some(target) = self.get_mut(into) {
            for st in source.stations {
                let state = st.state;
                let id = target.push(st.title, st.detail, state, st.source, st.session, st.at);
                if let Some(s) = target.station_mut(id) {
                    s.at = st.at;
                }
            }
            target.stations.sort_by_key(|s| s.at);
            for s in source.sessions {
                if !target.sessions.iter().any(|t| t.uid == s.uid) {
                    target.sessions.push(s);
                }
            }
            if target.branch.is_none() {
                target.branch = source.branch;
            }
            trim(target);
        }
        for l in &mut self.lines {
            if l.parent.as_deref() == Some(from) {
                l.parent = Some(into.to_string());
            }
        }
        self.save();
    }

    /// Quita líneas provisionales que nunca tuvieron pasos ni nombre.
    pub fn prune_empty(&mut self, live: &[String]) {
        let before = self.lines.len();
        let parents: Vec<String> = self.lines.iter().filter_map(|l| l.parent.clone()).collect();
        self.lines.retain(|l| {
            l.named || !l.stations.is_empty() || l.sessions.iter().any(|s| live.contains(&s.uid)) || parents.contains(&l.id)
        });
        if self.lines.len() != before {
            self.save();
        }
    }

    /// Líneas visibles en orden: sin terminar primero (más recientes arriba),
    /// cada sub-línea debajo de su padre.
    pub fn ordered(&self, include_finished: bool) -> Vec<(&Line, usize)> {
        let mut roots: Vec<&Line> = self
            .lines
            .iter()
            .filter(|l| l.parent.as_deref().is_none_or(|p| self.get(p).is_none()))
            .filter(|l| include_finished || !l.is_finished())
            .collect();
        roots.sort_by(|a, b| a.is_finished().cmp(&b.is_finished()).then(b.updated.cmp(&a.updated)));
        let mut out = Vec::new();
        for root in roots {
            self.push_tree(root, 0, include_finished, &mut out);
        }
        out
    }

    fn push_tree<'a>(&'a self, line: &'a Line, depth: usize, include_finished: bool, out: &mut Vec<(&'a Line, usize)>) {
        if out.iter().any(|(l, _)| l.id == line.id) || depth > 4 {
            return;
        }
        out.push((line, depth));
        let mut children: Vec<&Line> = self
            .lines
            .iter()
            .filter(|l| l.parent.as_deref() == Some(line.id.as_str()))
            .filter(|l| include_finished || !l.is_finished())
            .collect();
        children.sort_by_key(|l| l.created);
        for c in children {
            self.push_tree(c, depth + 1, include_finished, out);
        }
    }
}

fn touch_session(line: &mut Line, uid: &str, now: Timestamp) {
    if let Some(s) = line.sessions.iter_mut().find(|s| s.uid == uid) {
        s.last_active = now;
    }
}

fn upsert_git(line: &mut Line, tag: &str, title: Option<String>, now: Timestamp) {
    let marker = format!("git:{tag}");
    let pos = line.stations.iter().position(|s| s.source == StationSource::Git && s.detail == marker);
    match (pos, title) {
        (Some(i), Some(t)) => {
            if line.stations[i].title != t {
                line.stations[i].title = t;
                line.stations[i].at = now;
            }
        }
        (None, Some(t)) => {
            line.push(t, marker, StationState::Done, StationSource::Git, None, now);
        }
        (Some(i), None) if tag == "pr" => {
            line.stations.remove(i);
        }
        _ => {}
    }
}

fn trim(line: &mut Line) {
    while line.stations.len() > MAX_STATIONS {
        // Primero se van los pasos hechos más viejos.
        match line.stations.iter().position(|s| s.state == StationState::Done && s.source != StationSource::Git) {
            Some(i) => {
                line.stations.remove(i);
            }
            None => {
                line.stations.remove(0);
            }
        }
    }
}

/// Primera línea, sin espacios repetidos, recortada a `max` caracteres.
pub fn short(text: &str, max: usize) -> String {
    let first = text.lines().map(str::trim).find(|l| !l.is_empty()).unwrap_or("");
    let collapsed: String = first.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut chars = collapsed.chars();
    let head: String = chars.by_ref().take(max).collect();
    if chars.next().is_some() { format!("{}…", head.trim_end()) } else { head }
}

fn store_path(root: &Path) -> Option<PathBuf> {
    let name: String = root
        .to_string_lossy()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect();
    Some(dirs::config_dir()?.join("kuaky").join("lines").join(format!("{}.json", name.trim_matches('-'))))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn session(uid: &str) -> LineSession {
        LineSession {
            uid: uid.into(),
            harness: "claude".into(),
            conversation: None,
            cwd: String::new(),
            label: String::new(),
            last_active: Timestamp::now(),
            role: None,
        }
    }

    #[test]
    fn prompts_become_stations_and_name_the_line() {
        let mut store = LineStore::in_memory();
        let id = store.create("New Claude Code session", LineKind::Task, None, false);
        store.attach(&id, session("s1"));
        store.on_prompt("s1", "Arregla el subrayado de errores del LSP\ncon detalles");
        store.on_title("s1", "Fix LSP underline");
        store.on_prompt("s1", "Ahora corre los tests");
        let line = store.get(&id).unwrap();
        assert_eq!(line.name, "Arregla el subrayado de errores del LSP");
        assert_eq!(line.stations.len(), 2);
        assert_eq!(line.stations[0].title, "Fix LSP underline");
        assert_eq!(line.stations[0].state, StationState::Done);
        assert_eq!(line.stations[1].state, StationState::Active);
        store.on_turn_done("s1");
        assert_eq!(store.get(&id).unwrap().stations[1].state, StationState::Done);
    }

    #[test]
    fn titles_without_hooks_make_steps() {
        let mut store = LineStore::in_memory();
        let id = store.create("New Antigravity session", LineKind::Task, None, false);
        store.attach(&id, session("a1"));
        store.on_title("a1", "Reading code");
        store.on_title("a1", "Reading code");
        store.on_title("a1", "Writing tests");
        let line = store.get(&id).unwrap();
        assert_eq!(line.stations.len(), 2);
        assert_eq!(line.stations[0].state, StationState::Done);
        assert_eq!(line.name, "Reading code");
    }

    #[test]
    fn tasks_sync_and_git_merge_closes_the_line() {
        let mut store = LineStore::in_memory();
        let id = store.create("Plan", LineKind::Plan, None, true);
        store.attach(&id, session("s1"));
        let t = |s: &str, st| AgentTask { subject: s.into(), status: st };
        store.set_tasks("s1", &[t("A", TaskStatus::Completed), t("B", TaskStatus::InProgress), t("C", TaskStatus::Pending)]);
        store.set_tasks("s1", &[t("A", TaskStatus::Completed), t("B", TaskStatus::Completed), t("C", TaskStatus::InProgress)]);
        let line = store.get(&id).unwrap();
        assert_eq!(line.stations.len(), 3);
        assert_eq!(line.done_count(), 2);
        assert_eq!(line.name, "Plan", "un nombre puesto por el usuario no cambia");

        store.set_git(&id, 0, true, None);
        assert!(!store.get(&id).unwrap().merged, "sin commits no cuenta como merge");
        store.set_git(&id, 2, false, Some((7, "Add map".into())));
        store.set_git(&id, 0, true, None);
        let line = store.get(&id).unwrap();
        assert!(line.merged);
        assert!(line.is_finished());
        assert!(line.stations.iter().all(|s| s.state != StationState::Active));
    }

    #[test]
    fn merge_and_order_with_sublines() {
        let mut store = LineStore::in_memory();
        let plan = store.create("Plan", LineKind::Plan, None, true);
        let part = store.create("Parte", LineKind::Feature, Some(plan.clone()), true);
        let stray = store.create("Suelta", LineKind::Task, None, false);
        store.attach(&stray, session("x"));
        store.on_prompt("x", "algo");
        store.merge_into(&stray, &part);
        assert!(store.get(&stray).is_none());
        assert_eq!(store.get(&part).unwrap().stations.len(), 1);
        let order: Vec<(&str, usize)> = store.ordered(false).into_iter().map(|(l, d)| (l.name.as_str(), d)).collect();
        assert_eq!(order, [("Plan", 0), ("Parte", 1)]);
        assert!(store.get(&part).unwrap().relay_note().contains("Still open: algo"));
    }
}
