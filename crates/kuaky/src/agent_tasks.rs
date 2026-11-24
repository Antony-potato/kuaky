//! Lista de tareas que el agente va armando, leída de su propio registro.
//!
//! Claude Code guarda cada sesión en `~/.claude/projects/<carpeta>/<id>.jsonl`.
//! Sus tareas llegan como llamadas a herramientas:
//! - `TaskCreate { subject, activeForm }` → el resultado trae `task.id`;
//! - `TaskUpdate { taskId, status, subject? }`;
//! - `TodoWrite { todos: [{ content, status, activeForm }] }` (versiones anteriores:
//!   reemplaza toda la lista).

use serde_json::Value;
use std::{
    collections::HashMap,
    io::{BufRead, BufReader, Seek, SeekFrom},
    path::{Path, PathBuf},
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TaskStatus {
    Pending,
    InProgress,
    Completed,
}

impl TaskStatus {
    fn parse(s: &str) -> Option<TaskStatus> {
        match s {
            "pending" => Some(TaskStatus::Pending),
            "in_progress" => Some(TaskStatus::InProgress),
            "completed" => Some(TaskStatus::Completed),
            _ => None,
        }
    }
}

/// Subagente que lanzó la sesión (herramienta `Agent`/`Task` de Claude Code).
#[derive(Debug, Clone, PartialEq)]
pub struct Subagent {
    /// `tool_use` id (o `toolCallId` en ACP).
    pub id: String,
    /// `Explore`, `general-purpose`, `Plan`…
    pub kind: String,
    pub what: String,
    pub started: jiff::Timestamp,
    pub done: Option<jiff::Timestamp>,
    pub failed: bool,
}

impl Subagent {
    pub fn running(&self) -> bool {
        self.done.is_none()
    }

    /// Se muestra mientras corre (hasta 2 h, por si la sesión murió sin cerrarlo)
    /// y 2 minutos después de terminar.
    pub fn visible(&self, now: jiff::Timestamp) -> bool {
        match self.done {
            None => now.duration_since(self.started).as_secs() < 2 * 3600,
            Some(done) => now.duration_since(done).as_secs() < 120,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentTask {
    pub subject: String,
    pub status: TaskStatus,
}

/// Lee incrementalmente el registro de una sesión de Claude Code.
#[derive(Default)]
pub struct TaskWatcher {
    path: Option<PathBuf>,
    offset: u64,
    /// Tareas en orden de creación (id → posición).
    order: Vec<String>,
    tasks: HashMap<String, AgentTask>,
    /// `TaskCreate` sin resultado todavía (tool_use id → tarea).
    pending: HashMap<String, AgentTask>,
    deleted: Vec<String>,
    subagents: Vec<Subagent>,
    subagents_changed: bool,
}

impl TaskWatcher {
    /// Busca el archivo de la sesión `session_id` en cualquier carpeta de proyecto.
    pub fn for_claude_session(session_id: &str) -> TaskWatcher {
        let path = dirs::home_dir()
            .map(|h| h.join(".claude").join("projects"))
            .and_then(|dir| find_session_file(&dir, session_id));
        TaskWatcher { path, ..Default::default() }
    }

    #[cfg(test)]
    fn for_file(path: PathBuf) -> TaskWatcher {
        TaskWatcher { path: Some(path), ..Default::default() }
    }

    /// Lee lo nuevo del archivo. Devuelve la lista completa si cambió.
    pub fn poll(&mut self) -> Option<Vec<AgentTask>> {
        let path = self.path.as_ref()?;
        let mut file = std::fs::File::open(path).ok()?;
        let len = file.metadata().ok()?.len();
        if len < self.offset {
            // El archivo se reescribió: empezar de nuevo.
            *self = TaskWatcher { path: self.path.take(), ..Default::default() };
        }
        if len == self.offset {
            return None;
        }
        file.seek(SeekFrom::Start(self.offset)).ok()?;
        let mut reader = BufReader::new(file);
        let before = self.list();
        let mut line = String::new();
        loop {
            line.clear();
            let n = reader.read_line(&mut line).ok()?;
            if n == 0 || !line.ends_with('\n') {
                break;
            }
            self.offset += n as u64;
            let relevant = line.contains("\"Task")
                || line.contains("\"Agent\"")
                || line.contains("\"TodoWrite\"")
                || (line.contains("\"toolUseResult\"") && line.contains("\"task\""))
                || (line.contains("tool_result") && self.subagents.iter().any(|a| a.running() && line.contains(a.id.as_str())));
            if relevant {
                if let Ok(v) = serde_json::from_str::<Value>(&line) {
                    self.apply(&v);
                }
            }
        }
        let after = self.list();
        (after != before).then_some(after)
    }

    pub fn subagents(&self) -> &[Subagent] {
        &self.subagents
    }

    /// ¿Cambiaron los subagentes desde la última vez que se preguntó?
    pub fn take_subagents_changed(&mut self) -> bool {
        std::mem::take(&mut self.subagents_changed)
    }

    pub fn list(&self) -> Vec<AgentTask> {
        self.order.iter().filter(|id| !self.deleted.contains(id)).filter_map(|id| self.tasks.get(id).cloned()).collect()
    }

    fn apply(&mut self, v: &Value) {
        let at = v.get("timestamp").and_then(Value::as_str).and_then(|t| t.parse::<jiff::Timestamp>().ok()).unwrap_or_else(jiff::Timestamp::now);
        if let Some(content) = v.pointer("/message/content").and_then(Value::as_array) {
            for block in content {
                match block.get("type").and_then(Value::as_str) {
                    Some("tool_use") => {
                        if matches!(block.get("name").and_then(Value::as_str), Some("Agent" | "Task")) {
                            let input = block.get("input").cloned().unwrap_or(Value::Null);
                            let id = block.get("id").and_then(Value::as_str).unwrap_or("").to_string();
                            if !id.is_empty() && !self.subagents.iter().any(|a| a.id == id) {
                                self.subagents.push(Subagent {
                                    id,
                                    kind: input.get("subagent_type").and_then(Value::as_str).unwrap_or("Agent").to_string(),
                                    what: input.get("description").and_then(Value::as_str).unwrap_or("").to_string(),
                                    started: at,
                                    done: None,
                                    failed: false,
                                });
                                self.subagents_changed = true;
                            }
                        }
                        self.tool_use(block)
                    }
                    Some("tool_result") => {
                        let use_id = block.get("tool_use_id").and_then(Value::as_str).unwrap_or("");
                        if let Some(a) = self.subagents.iter_mut().find(|a| a.id == use_id && a.running()) {
                            a.done = Some(at);
                            a.failed = block.get("is_error").and_then(Value::as_bool).unwrap_or(false);
                            self.subagents_changed = true;
                        }
                        if let Some(task) = self.pending.remove(use_id) {
                            if let Some(id) = v.pointer("/toolUseResult/task/id").and_then(id_str) {
                                self.insert(id, task);
                            }
                        }
                    }
                    _ => {}
                }
            }
        }
    }

    fn tool_use(&mut self, block: &Value) {
        let input = block.get("input").cloned().unwrap_or(Value::Null);
        match block.get("name").and_then(Value::as_str) {
            Some("TaskCreate") => {
                let Some(subject) = input.get("subject").and_then(Value::as_str) else { return };
                let use_id = block.get("id").and_then(Value::as_str).unwrap_or("").to_string();
                self.pending.insert(use_id, AgentTask { subject: subject.to_string(), status: TaskStatus::Pending });
            }
            Some("TaskUpdate") => {
                let Some(id) = input.get("taskId").and_then(id_str) else { return };
                let status = input.get("status").and_then(Value::as_str);
                if status == Some("deleted") {
                    self.deleted.push(id);
                    return;
                }
                if let Some(task) = self.tasks.get_mut(&id) {
                    if let Some(s) = status.and_then(TaskStatus::parse) {
                        task.status = s;
                    }
                    if let Some(subject) = input.get("subject").and_then(Value::as_str) {
                        task.subject = subject.to_string();
                    }
                }
            }
            Some("TodoWrite") => {
                let Some(todos) = input.get("todos").and_then(Value::as_array) else { return };
                self.order.clear();
                self.tasks.clear();
                self.deleted.clear();
                for (i, t) in todos.iter().enumerate() {
                    let Some(content) = t.get("content").and_then(Value::as_str) else { continue };
                    let status = t.get("status").and_then(Value::as_str).and_then(TaskStatus::parse).unwrap_or(TaskStatus::Pending);
                    self.insert(format!("todo-{i}"), AgentTask { subject: content.to_string(), status });
                }
            }
            _ => {}
        }
    }

    fn insert(&mut self, id: String, task: AgentTask) {
        if !self.order.contains(&id) {
            self.order.push(id.clone());
        }
        self.tasks.insert(id, task);
    }
}

fn id_str(v: &Value) -> Option<String> {
    v.as_str().map(str::to_string).or_else(|| v.as_u64().map(|n| n.to_string()))
}

fn find_session_file(projects: &Path, session_id: &str) -> Option<PathBuf> {
    if !crate::agent_history::valid_id(session_id) {
        return None;
    }
    let name = format!("{session_id}.jsonl");
    std::fs::read_dir(projects).ok()?.filter_map(Result::ok).map(|d| d.path().join(&name)).find(|p| p.is_file())
}

#[cfg(test)]
mod tests {
    use super::{TaskStatus, TaskWatcher};
    use std::io::Write;

    #[test]
    fn follows_task_create_and_update() {
        let path = std::env::temp_dir().join(format!("kuaky-tasks-{}.jsonl", std::process::id()));
        let mut f = std::fs::File::create(&path).unwrap();
        writeln!(f, r#"{{"message":{{"content":[{{"type":"tool_use","id":"u1","name":"TaskCreate","input":{{"subject":"Leer código","activeForm":"Leyendo"}}}}]}}}}"#).unwrap();
        writeln!(f, r#"{{"message":{{"content":[{{"type":"tool_result","tool_use_id":"u1"}}]}},"toolUseResult":{{"task":{{"id":"1","subject":"Leer código"}}}}}}"#).unwrap();
        writeln!(f, r#"{{"message":{{"content":[{{"type":"tool_use","id":"u2","name":"TaskCreate","input":{{"subject":"Arreglar"}}}}]}}}}"#).unwrap();
        writeln!(f, r#"{{"message":{{"content":[{{"type":"tool_result","tool_use_id":"u2"}}]}},"toolUseResult":{{"task":{{"id":"2"}}}}}}"#).unwrap();
        writeln!(f, r#"{{"message":{{"content":[{{"type":"tool_use","id":"u3","name":"TaskUpdate","input":{{"taskId":"1","status":"completed"}}}}]}}}}"#).unwrap();
        drop(f);
        let mut w = TaskWatcher::for_file(path.clone());
        let list = w.poll().unwrap();
        assert_eq!(list.len(), 2);
        assert_eq!(list[0].status, TaskStatus::Completed);
        assert_eq!(list[1].subject, "Arreglar");
        assert!(w.poll().is_none(), "sin cambios no avisa");

        let mut f = std::fs::OpenOptions::new().append(true).open(&path).unwrap();
        writeln!(f, r#"{{"message":{{"content":[{{"type":"tool_use","id":"u4","name":"TaskUpdate","input":{{"taskId":"2","status":"in_progress"}}}}]}}}}"#).unwrap();
        drop(f);
        assert_eq!(w.poll().unwrap()[1].status, TaskStatus::InProgress);
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn follows_subagents() {
        let path = std::env::temp_dir().join(format!("kuaky-subagents-{}.jsonl", std::process::id()));
        std::fs::write(
            &path,
            concat!(
                r#"{"timestamp":"2026-11-24T10:00:00Z","message":{"content":[{"type":"tool_use","id":"t1","name":"Agent","input":{"subagent_type":"Explore","description":"Find auth code"}}]}}"#,
                "\n",
                r#"{"timestamp":"2026-11-24T10:00:05Z","message":{"content":[{"type":"tool_use","id":"t2","name":"Agent","input":{"description":"Review PR"}}]}}"#,
                "\n",
            ),
        )
        .unwrap();
        let mut w = TaskWatcher::for_file(path.clone());
        w.poll();
        assert!(w.take_subagents_changed());
        assert_eq!(w.subagents().len(), 2);
        assert_eq!(w.subagents()[0].kind, "Explore");
        assert!(w.subagents().iter().all(|a| a.running()));
        let mut f = std::fs::OpenOptions::new().append(true).open(&path).unwrap();
        writeln!(f, r#"{{"timestamp":"2026-11-24T10:01:00Z","message":{{"content":[{{"type":"tool_result","tool_use_id":"t1","is_error":false}}]}}}}"#).unwrap();
        drop(f);
        w.poll();
        assert!(w.take_subagents_changed());
        assert!(!w.subagents()[0].running());
        assert!(w.subagents()[1].running());
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn todo_write_replaces_the_list() {
        let path = std::env::temp_dir().join(format!("kuaky-todos-{}.jsonl", std::process::id()));
        std::fs::write(
            &path,
            "{\"message\":{\"content\":[{\"type\":\"tool_use\",\"id\":\"a\",\"name\":\"TodoWrite\",\"input\":{\"todos\":[{\"content\":\"Uno\",\"status\":\"completed\"},{\"content\":\"Dos\",\"status\":\"pending\"}]}}]}}\n",
        )
        .unwrap();
        let mut w = TaskWatcher::for_file(path.clone());
        let list = w.poll().unwrap();
        assert_eq!(list.iter().map(|t| t.subject.as_str()).collect::<Vec<_>>(), ["Uno", "Dos"]);
        let _ = std::fs::remove_file(path);
    }
}
