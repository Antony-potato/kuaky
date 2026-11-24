//! Cliente mínimo del [Agent Client Protocol](https://agentclientprotocol.com)
//! (versión 1): JSON-RPC 2.0, un mensaje JSON por línea, sobre stdin/stdout
//! del proceso del agente (`claude-agent-acp`, `agy acp`, `gemini --acp`…).
//!
//! Usa hilos y canales (sin runtime async), así encaja con cualquier
//! interfaz. kuaky ofrece al agente leer y escribir archivos de texto (así ve
//! cada cambio), dentro de las carpetas permitidas; no ofrece terminal: el
//! agente ejecuta sus comandos por su cuenta y kuaky muestra lo que hace.

use anyhow::{Context as _, anyhow, bail};
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    io::{BufRead, BufReader, Write},
    path::{Path, PathBuf},
    process::{Child, ChildStdin, Command, Stdio},
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};

pub const PROTOCOL_VERSION: u64 = 1;

/// Algo que pasó en la sesión.
#[derive(Debug, Clone, PartialEq)]
pub enum Event {
    /// Mensaje del usuario repetido al retomar una sesión (`session/load`).
    UserMessageChunk(String),
    MessageChunk(String),
    ThoughtChunk(String),
    ToolCall(ToolCall),
    /// Actualiza campos de una herramienta ya anunciada (los `None` no cambian).
    ToolCallUpdate(ToolCall),
    Plan(Vec<PlanEntry>),
    Permission(PermissionRequest),
    /// Terminó el turno (`end_turn`, `cancelled`, `max_tokens`…).
    TurnEnded(String),
    /// Comandos `/` que ofrece el agente.
    Commands(Vec<SlashCommand>),
    /// Cambió el modo (p. ej. `plan`, `acceptEdits`).
    ModeChanged(String),
    /// Nuevas opciones de sesión (modelo, esfuerzo, modo…).
    ConfigOptions(Vec<ConfigOption>),
    /// Título de la sesión que puso el agente.
    Title(String),
    /// Contexto usado (`usage_update`): tokens usados, tamaño de la ventana y costo acumulado.
    Usage { used: u64, size: u64, cost: Option<f64> },
    /// El agente escribió un archivo a través de kuaky (`fs/write_text_file`).
    FileWritten { path: PathBuf, old_text: Option<String>, new_text: String },
    Error(String),
    Exited,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SlashCommand {
    pub name: String,
    pub description: String,
    pub hint: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ConfigOption {
    pub id: String,
    pub name: String,
    /// `model`, `mode`, `thought_level`, `speed`…
    pub category: String,
    pub current: String,
    pub options: Vec<ConfigValue>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ConfigValue {
    pub value: String,
    pub name: String,
    pub description: Option<String>,
}

/// Lo que devuelve el agente al crear o retomar una sesión.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct SessionInfo {
    pub id: String,
    pub config: Vec<ConfigOption>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SessionSummary {
    pub id: String,
    pub title: Option<String>,
    pub cwd: Option<String>,
    pub updated_at: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct AgentCapabilities {
    pub load_session: bool,
    pub list_sessions: bool,
    pub image: bool,
    pub embedded_context: bool,
}

/// Un bloque del mensaje que se envía al agente.
#[derive(Debug, Clone, PartialEq)]
pub enum PromptBlock {
    Text(String),
    /// Archivo mencionado con `@` (el agente lo lee si lo necesita).
    ResourceLink { uri: String, name: String },
    /// Imagen en base64.
    Image { data: String, mime: String },
}

impl PromptBlock {
    fn to_json(&self) -> Value {
        match self {
            PromptBlock::Text(t) => json!({ "type": "text", "text": t }),
            PromptBlock::ResourceLink { uri, name } => json!({ "type": "resource_link", "uri": uri, "name": name }),
            PromptBlock::Image { data, mime } => json!({ "type": "image", "data": data, "mimeType": mime }),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct Location {
    pub path: String,
    pub line: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct ToolCall {
    pub id: String,
    pub title: Option<String>,
    /// `read`, `edit`, `execute`, `search`, `fetch`, `think`, `other`…
    pub kind: Option<String>,
    /// `pending`, `in_progress`, `completed`, `failed`.
    pub status: Option<String>,
    pub diffs: Vec<FileDiff>,
    pub text: Option<String>,
    pub locations: Vec<Location>,
    /// Herramienta del agente (`_meta.claudeCode.toolName`), p. ej. `Bash`, `Agent`.
    pub tool_name: Option<String>,
    /// Subagente lanzado por esta herramienta: (tipo, descripción).
    pub subagent: Option<(String, String)>,
    /// Paso de un subagente: id del `Agent`/`Task` que lo lanzó
    /// (`_meta.claudeCode.parentToolUseId`).
    pub parent: Option<String>,
}

impl ToolCall {
    pub fn is_subagent(&self) -> bool {
        self.subagent.is_some() || matches!(self.tool_name.as_deref(), Some("Agent" | "Task"))
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct FileDiff {
    pub path: String,
    pub old_text: Option<String>,
    pub new_text: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct PlanEntry {
    pub content: String,
    pub status: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct PermissionRequest {
    /// Id JSON-RPC para responder.
    pub request_id: Value,
    pub title: String,
    pub options: Vec<PermissionOption>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct PermissionOption {
    pub id: String,
    pub name: String,
    /// `allow_once`, `allow_always`, `reject_once`, `reject_always`.
    pub kind: String,
}

type Pending = Arc<Mutex<HashMap<u64, flume::Sender<Result<Value, String>>>>>;

pub struct Client {
    stdin: Arc<Mutex<ChildStdin>>,
    child: Mutex<Child>,
    next_id: AtomicU64,
    pending: Pending,
    events: flume::Receiver<Event>,
    events_tx: flume::Sender<Event>,
    pub agent_name: Option<String>,
    pub capabilities: AgentCapabilities,
    /// Carpetas donde el agente puede escribir por `fs/write_text_file`.
    roots: Arc<Mutex<Vec<PathBuf>>>,
}

impl Client {
    /// Lanza el agente y hace `initialize`.
    pub fn spawn(program: &Path, args: &[String], cwd: &Path, env: &[(String, String)]) -> anyhow::Result<Client> {
        let mut cmd = Command::new(program);
        cmd.args(args).current_dir(cwd).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null());
        for (k, v) in env {
            cmd.env(k, v);
        }
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            cmd.creation_flags(0x0800_0000);
        }
        let mut child = cmd.spawn().with_context(|| format!("no se pudo iniciar {}", program.display()))?;
        let stdin = Arc::new(Mutex::new(child.stdin.take().context("sin stdin")?));
        let stdout = child.stdout.take().context("sin stdout")?;
        let pending: Pending = Arc::default();
        let (tx, rx) = flume::unbounded();
        let roots: Arc<Mutex<Vec<PathBuf>>> = Arc::new(Mutex::new(vec![cwd.to_path_buf()]));

        {
            let tx = tx.clone();
            let pending = pending.clone();
            let stdin = stdin.clone();
            let roots = roots.clone();
            std::thread::Builder::new().name("acp-reader".into()).spawn(move || {
                for line in BufReader::new(stdout).lines() {
                    let Ok(line) = line else { break };
                    if line.trim().is_empty() {
                        continue;
                    }
                    let Ok(msg) = serde_json::from_str::<Value>(&line) else { continue };
                    handle_incoming(msg, &pending, &stdin, &tx, &roots);
                }
                for (_, waiter) in pending.lock().unwrap().drain() {
                    let _ = waiter.send(Err("el agente terminó".into()));
                }
                let _ = tx.send(Event::Exited);
            })?;
        }

        let mut client = Client {
            stdin,
            child: Mutex::new(child),
            next_id: AtomicU64::new(1),
            pending,
            events: rx,
            events_tx: tx,
            agent_name: None,
            capabilities: AgentCapabilities::default(),
            roots,
        };
        let init = client.request(
            "initialize",
            json!({
                "protocolVersion": PROTOCOL_VERSION,
                "clientCapabilities": { "fs": { "readTextFile": true, "writeTextFile": true }, "terminal": false },
                "clientInfo": { "name": "kuaky", "version": env!("CARGO_PKG_VERSION") },
            }),
            Duration::from_secs(60),
        )?;
        client.agent_name = init
            .get("agentInfo")
            .and_then(|i| i.get("title").or_else(|| i.get("name")))
            .and_then(Value::as_str)
            .map(str::to_string);
        let caps = init.get("agentCapabilities").cloned().unwrap_or(Value::Null);
        client.capabilities = AgentCapabilities {
            load_session: caps.get("loadSession").and_then(Value::as_bool).unwrap_or(false),
            list_sessions: caps.pointer("/sessionCapabilities/list").is_some(),
            image: caps.pointer("/promptCapabilities/image").and_then(Value::as_bool).unwrap_or(false),
            embedded_context: caps.pointer("/promptCapabilities/embeddedContext").and_then(Value::as_bool).unwrap_or(false),
        };
        Ok(client)
    }

    /// Añade una carpeta donde el agente puede escribir (p. ej. un worktree).
    pub fn allow_root(&self, dir: &Path) {
        let mut roots = self.roots.lock().unwrap();
        if !roots.iter().any(|r| r == dir) {
            roots.push(dir.to_path_buf());
        }
    }

    pub fn events(&self) -> flume::Receiver<Event> {
        self.events.clone()
    }

    fn send(&self, msg: &Value) -> anyhow::Result<()> {
        let mut stdin = self.stdin.lock().unwrap();
        writeln!(stdin, "{msg}")?;
        stdin.flush()?;
        Ok(())
    }

    fn start_request(&self, method: &str, params: Value) -> anyhow::Result<flume::Receiver<Result<Value, String>>> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = flume::bounded(1);
        self.pending.lock().unwrap().insert(id, tx);
        self.send(&json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params }))?;
        Ok(rx)
    }

    fn request(&self, method: &str, params: Value, timeout: Duration) -> anyhow::Result<Value> {
        let rx = self.start_request(method, params)?;
        match rx.recv_timeout(timeout) {
            Ok(Ok(v)) => Ok(v),
            Ok(Err(e)) => Err(anyhow!("{method}: {e}")),
            Err(_) => bail!("{method}: el agente no respondió a tiempo"),
        }
    }

    /// Crea una sesión en `cwd` y devuelve su id.
    pub fn new_session(&self, cwd: &Path) -> anyhow::Result<String> {
        Ok(self.new_session_info(cwd)?.id)
    }

    /// Crea una sesión en `cwd` con sus opciones (modelo, modo…).
    pub fn new_session_info(&self, cwd: &Path) -> anyhow::Result<SessionInfo> {
        let result = self.request(
            "session/new",
            json!({ "cwd": cwd.to_string_lossy(), "mcpServers": [] }),
            Duration::from_secs(120),
        )?;
        let id = result.get("sessionId").and_then(Value::as_str).map(str::to_string).context("session/new sin sessionId")?;
        Ok(SessionInfo { id, config: parse_session_config(&result) })
    }

    /// Retoma una sesión anterior: el agente repite la conversación como
    /// eventos (`UserMessageChunk`, `MessageChunk`, herramientas…).
    pub fn load_session(&self, cwd: &Path, id: &str) -> anyhow::Result<SessionInfo> {
        let result = self.request(
            "session/load",
            json!({ "sessionId": id, "cwd": cwd.to_string_lossy(), "mcpServers": [] }),
            Duration::from_secs(120),
        )?;
        Ok(SessionInfo { id: id.to_string(), config: parse_session_config(&result) })
    }

    /// Sesiones anteriores del agente en `cwd`, si las ofrece.
    pub fn list_sessions(&self, cwd: &Path) -> anyhow::Result<Vec<SessionSummary>> {
        let result = self.request("session/list", json!({ "cwd": cwd.to_string_lossy() }), Duration::from_secs(30))?;
        Ok(result
            .get("sessions")
            .and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .filter_map(|s| {
                        Some(SessionSummary {
                            id: s.get("sessionId")?.as_str()?.to_string(),
                            title: s.get("title").and_then(Value::as_str).map(str::to_string),
                            cwd: s.get("cwd").and_then(Value::as_str).map(str::to_string),
                            updated_at: s.get("updatedAt").and_then(Value::as_str).map(str::to_string),
                        })
                    })
                    .collect()
            })
            .unwrap_or_default())
    }

    /// Cambia una opción de la sesión (modelo, esfuerzo, modo…). Devuelve las
    /// opciones actualizadas si el agente las manda.
    pub fn set_config_option(&self, session: &str, config_id: &str, value: &str) -> anyhow::Result<Vec<ConfigOption>> {
        let result = self.request(
            "session/set_config_option",
            json!({ "sessionId": session, "configId": config_id, "value": value }),
            Duration::from_secs(30),
        )?;
        Ok(parse_session_config(&result))
    }

    /// Cambia el modo con el método clásico (`session/set_mode`).
    pub fn set_mode(&self, session: &str, mode_id: &str) -> anyhow::Result<()> {
        self.request("session/set_mode", json!({ "sessionId": session, "modeId": mode_id }), Duration::from_secs(30))?;
        Ok(())
    }

    /// Envía un mensaje. La respuesta llega como [`Event::TurnEnded`] cuando
    /// el agente termina su turno; mientras tanto llegan los demás eventos.
    pub fn prompt(&self, session: &str, text: &str) -> anyhow::Result<()> {
        self.prompt_blocks(session, &[PromptBlock::Text(text.to_string())])
    }

    /// Como [`Client::prompt`], con archivos mencionados o imágenes.
    pub fn prompt_blocks(&self, session: &str, blocks: &[PromptBlock]) -> anyhow::Result<()> {
        let prompt: Vec<Value> = blocks.iter().map(PromptBlock::to_json).collect();
        let rx = self.start_request("session/prompt", json!({ "sessionId": session, "prompt": prompt }))?;
        let events = self.events_tx.clone();
        std::thread::spawn(move || {
            let event = match rx.recv() {
                Ok(Ok(v)) => Event::TurnEnded(v.get("stopReason").and_then(Value::as_str).unwrap_or("end_turn").to_string()),
                Ok(Err(e)) => Event::Error(e),
                Err(_) => return,
            };
            let _ = events.send(event);
        });
        Ok(())
    }

    /// Detiene el turno en curso.
    pub fn cancel(&self, session: &str) -> anyhow::Result<()> {
        self.send(&json!({ "jsonrpc": "2.0", "method": "session/cancel", "params": { "sessionId": session } }))
    }

    /// Responde a una solicitud de permiso; `None` la cancela.
    pub fn answer_permission(&self, request_id: &Value, option_id: Option<&str>) -> anyhow::Result<()> {
        let outcome = match option_id {
            Some(id) => json!({ "outcome": "selected", "optionId": id }),
            None => json!({ "outcome": "cancelled" }),
        };
        self.send(&json!({ "jsonrpc": "2.0", "id": request_id, "result": { "outcome": outcome } }))
    }
}

impl Drop for Client {
    fn drop(&mut self) {
        if let Ok(mut child) = self.child.lock() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

fn reply(stdin: &Arc<Mutex<ChildStdin>>, msg: Value) {
    if let Ok(mut s) = stdin.lock() {
        let _ = writeln!(s, "{msg}");
        let _ = s.flush();
    }
}

/// Ruta canónica aunque el archivo (o sus carpetas) aún no existan: se
/// canoniza el ancestro que sí existe y se le agrega el resto.
fn canonical(path: &Path) -> PathBuf {
    let mut existing = path.to_path_buf();
    let mut rest = Vec::new();
    while !existing.exists() {
        match (existing.file_name().map(|n| n.to_os_string()), existing.parent()) {
            (Some(name), Some(parent)) => {
                rest.push(name);
                existing = parent.to_path_buf();
            }
            _ => break,
        }
    }
    let mut out = existing.canonicalize().unwrap_or(existing);
    for name in rest.into_iter().rev() {
        out.push(name);
    }
    out
}

fn within(roots: &[PathBuf], path: &Path) -> bool {
    if path.components().any(|c| matches!(c, std::path::Component::ParentDir)) {
        return false;
    }
    let target = canonical(path);
    roots.iter().any(|r| target.starts_with(canonical(r)))
}

/// `fs/read_text_file`: `line` es 1-based; `limit` = número de líneas.
pub fn read_text(path: &Path, line: Option<u64>, limit: Option<u64>) -> std::io::Result<String> {
    let text = std::fs::read_to_string(path)?;
    if line.is_none() && limit.is_none() {
        return Ok(text);
    }
    let start = line.unwrap_or(1).saturating_sub(1) as usize;
    let lines = text.split_inclusive('\n').skip(start);
    Ok(match limit {
        Some(n) => lines.take(n as usize).collect(),
        None => lines.collect(),
    })
}

fn handle_fs(method: &str, id: Value, params: &Value, stdin: &Arc<Mutex<ChildStdin>>, tx: &flume::Sender<Event>, roots: &Arc<Mutex<Vec<PathBuf>>>) {
    let path = PathBuf::from(params.get("path").and_then(Value::as_str).unwrap_or(""));
    let error = |msg: String| json!({ "jsonrpc": "2.0", "id": id, "error": { "code": -32000, "message": msg } });
    if !path.is_absolute() {
        return reply(stdin, error(format!("path must be absolute: {}", path.display())));
    }
    match method {
        "fs/read_text_file" => {
            let line = params.get("line").and_then(Value::as_u64);
            let limit = params.get("limit").and_then(Value::as_u64);
            match read_text(&path, line, limit) {
                Ok(content) => reply(stdin, json!({ "jsonrpc": "2.0", "id": id, "result": { "content": content } })),
                Err(e) => reply(stdin, error(format!("{}: {e}", path.display()))),
            }
        }
        "fs/write_text_file" => {
            let allowed = within(&roots.lock().unwrap(), &path);
            if !allowed {
                return reply(stdin, error(format!("kuaky only allows writing inside the project: {}", path.display())));
            }
            let new_text = params.get("content").and_then(Value::as_str).unwrap_or("").to_string();
            let old_text = std::fs::read_to_string(&path).ok();
            if let Some(dir) = path.parent() {
                let _ = std::fs::create_dir_all(dir);
            }
            match std::fs::write(&path, &new_text) {
                Ok(()) => {
                    reply(stdin, json!({ "jsonrpc": "2.0", "id": id, "result": null }));
                    let _ = tx.send(Event::FileWritten { path, old_text, new_text });
                }
                Err(e) => reply(stdin, error(format!("{}: {e}", path.display()))),
            }
        }
        _ => {}
    }
}

fn handle_incoming(msg: Value, pending: &Pending, stdin: &Arc<Mutex<ChildStdin>>, tx: &flume::Sender<Event>, roots: &Arc<Mutex<Vec<PathBuf>>>) {
    let method = msg.get("method").and_then(Value::as_str);
    let id = msg.get("id").cloned();
    match (method, id) {
        // Respuesta a una petición nuestra.
        (None, Some(id)) => {
            let Some(id) = id.as_u64() else { return };
            if let Some(waiter) = pending.lock().unwrap().remove(&id) {
                let result = match msg.get("error") {
                    Some(err) => Err(err.get("message").and_then(Value::as_str).unwrap_or("error").to_string()),
                    None => Ok(msg.get("result").cloned().unwrap_or(Value::Null)),
                };
                let _ = waiter.send(result);
            }
        }
        // Petición del agente.
        (Some(method), Some(id)) => {
            let params = msg.get("params").cloned().unwrap_or(Value::Null);
            if method == "session/request_permission" {
                let _ = tx.send(Event::Permission(parse_permission(id, &params)));
            } else if method.starts_with("fs/") {
                handle_fs(method, id, &params, stdin, tx, roots);
            } else {
                // No ofrecemos terminal ni otros métodos: "método no encontrado".
                reply(stdin, json!({ "jsonrpc": "2.0", "id": id, "error": { "code": -32601, "message": format!("{method} no soportado por kuaky") } }));
            }
        }
        // Notificación.
        (Some("session/update"), None) => {
            if let Some(update) = msg.get("params").and_then(|p| p.get("update")) {
                if let Some(event) = parse_update(update) {
                    let _ = tx.send(event);
                }
            }
        }
        _ => {}
    }
}

fn text_of(content: &Value) -> Option<String> {
    match content.get("type").and_then(Value::as_str) {
        Some("text") => content.get("text").and_then(Value::as_str).map(str::to_string),
        _ => None,
    }
}

pub fn parse_update(update: &Value) -> Option<Event> {
    let kind = update.get("sessionUpdate").and_then(Value::as_str)?;
    match kind {
        "user_message_chunk" => text_of(update.get("content")?).map(Event::UserMessageChunk),
        "agent_message_chunk" => text_of(update.get("content")?).map(Event::MessageChunk),
        "available_commands_update" => Some(Event::Commands(
            update
                .get("availableCommands")?
                .as_array()?
                .iter()
                .filter_map(|c| {
                    Some(SlashCommand {
                        name: c.get("name")?.as_str()?.to_string(),
                        description: c.get("description").and_then(Value::as_str).unwrap_or("").to_string(),
                        hint: c.pointer("/input/hint").and_then(Value::as_str).map(str::to_string),
                    })
                })
                .collect(),
        )),
        "current_mode_update" => update.get("currentModeId").and_then(Value::as_str).map(|m| Event::ModeChanged(m.to_string())),
        "config_option_update" | "config_options_update" => Some(Event::ConfigOptions(parse_config_options(update.get("configOptions")?))),
        "session_info_update" => update.get("title").and_then(Value::as_str).map(|t| Event::Title(t.to_string())),
        "usage_update" => Some(Event::Usage {
            used: update.get("used").and_then(Value::as_u64).unwrap_or(0),
            size: update.get("size").and_then(Value::as_u64).unwrap_or(0),
            cost: update.pointer("/cost/amount").and_then(Value::as_f64),
        }),
        "agent_thought_chunk" => text_of(update.get("content")?).map(Event::ThoughtChunk),
        "tool_call" => Some(Event::ToolCall(parse_tool_call(update))),
        "tool_call_update" => Some(Event::ToolCallUpdate(parse_tool_call(update))),
        "plan" => Some(Event::Plan(
            update
                .get("entries")?
                .as_array()?
                .iter()
                .map(|e| PlanEntry {
                    content: e.get("content").and_then(Value::as_str).unwrap_or("").to_string(),
                    status: e.get("status").and_then(Value::as_str).unwrap_or("pending").to_string(),
                })
                .collect(),
        )),
        _ => None,
    }
}

fn parse_tool_call(v: &Value) -> ToolCall {
    let s = |k: &str| v.get(k).and_then(Value::as_str).map(str::to_string);
    let mut call = ToolCall {
        id: s("toolCallId").unwrap_or_default(),
        title: s("title"),
        kind: s("kind"),
        status: s("status"),
        tool_name: v.pointer("/_meta/claudeCode/toolName").and_then(Value::as_str).map(str::to_string),
        parent: v.pointer("/_meta/claudeCode/parentToolUseId").and_then(Value::as_str).filter(|p| !p.is_empty()).map(str::to_string),
        ..ToolCall::default()
    };
    if matches!(call.tool_name.as_deref(), Some("Agent" | "Task")) {
        let kind = v.pointer("/rawInput/subagent_type").and_then(Value::as_str).unwrap_or("Agent").to_string();
        let what = v
            .pointer("/rawInput/description")
            .and_then(Value::as_str)
            .map(str::to_string)
            .or_else(|| call.title.clone())
            .unwrap_or_default();
        call.subagent = Some((kind, what));
    }
    if let Some(locs) = v.get("locations").and_then(Value::as_array) {
        call.locations = locs
            .iter()
            .filter_map(|l| {
                Some(Location { path: l.get("path")?.as_str()?.to_string(), line: l.get("line").and_then(Value::as_u64).map(|n| n as u32) })
            })
            .collect();
    }
    if let Some(items) = v.get("content").and_then(Value::as_array) {
        let mut texts = Vec::new();
        for item in items {
            match item.get("type").and_then(Value::as_str) {
                Some("diff") => call.diffs.push(FileDiff {
                    path: item.get("path").and_then(Value::as_str).unwrap_or("").to_string(),
                    old_text: item.get("oldText").and_then(Value::as_str).map(str::to_string),
                    new_text: item.get("newText").and_then(Value::as_str).unwrap_or("").to_string(),
                }),
                Some("content") => {
                    if let Some(t) = item.get("content").and_then(text_of) {
                        texts.push(t);
                    }
                }
                _ => {}
            }
        }
        if !texts.is_empty() {
            call.text = Some(texts.join("\n"));
        }
    }
    call
}

fn parse_permission(id: Value, params: &Value) -> PermissionRequest {
    let title = params
        .get("toolCall")
        .and_then(|t| t.get("title"))
        .and_then(Value::as_str)
        .unwrap_or("El agente pide permiso")
        .to_string();
    let options = params
        .get("options")
        .and_then(Value::as_array)
        .map(|opts| {
            opts.iter()
                .map(|o| PermissionOption {
                    id: o.get("optionId").and_then(Value::as_str).unwrap_or("").to_string(),
                    name: o.get("name").and_then(Value::as_str).unwrap_or("").to_string(),
                    kind: o.get("kind").and_then(Value::as_str).unwrap_or("").to_string(),
                })
                .collect()
        })
        .unwrap_or_default();
    PermissionRequest { request_id: id, title, options }
}

fn parse_config_options(v: &Value) -> Vec<ConfigOption> {
    v.as_array()
        .map(|a| {
            a.iter()
                .filter_map(|o| {
                    Some(ConfigOption {
                        id: o.get("id")?.as_str()?.to_string(),
                        name: o.get("name").and_then(Value::as_str).unwrap_or("").to_string(),
                        category: o.get("category").and_then(Value::as_str).unwrap_or("").to_string(),
                        current: o.get("currentValue").map(value_str).unwrap_or_default(),
                        options: o
                            .get("options")
                            .and_then(Value::as_array)
                            .map(|opts| {
                                opts.iter()
                                    .filter_map(|x| {
                                        Some(ConfigValue {
                                            value: value_str(x.get("value")?),
                                            name: x.get("name").and_then(Value::as_str).unwrap_or("").to_string(),
                                            description: x.get("description").and_then(Value::as_str).map(str::to_string),
                                        })
                                    })
                                    .collect()
                            })
                            .unwrap_or_default(),
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

fn value_str(v: &Value) -> String {
    v.as_str().map(str::to_string).unwrap_or_else(|| v.to_string())
}

/// Opciones de sesión: `configOptions` si el agente las da; si no, se arman
/// a partir de `modes` y `models` (formato anterior).
pub fn parse_session_config(result: &Value) -> Vec<ConfigOption> {
    if let Some(opts) = result.get("configOptions") {
        let parsed = parse_config_options(opts);
        if !parsed.is_empty() {
            return parsed;
        }
    }
    let mut out = Vec::new();
    if let Some(models) = result.get("models") {
        let options: Vec<ConfigValue> = models
            .get("availableModels")
            .and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .filter_map(|m| Some(ConfigValue { value: m.get("modelId")?.as_str()?.to_string(), name: m.get("name").and_then(Value::as_str).unwrap_or("").to_string(), description: None }))
                    .collect()
            })
            .unwrap_or_default();
        if !options.is_empty() {
            out.push(ConfigOption {
                id: "model".into(),
                name: "Model".into(),
                category: "model".into(),
                current: models.get("currentModelId").map(value_str).unwrap_or_default(),
                options,
            });
        }
    }
    if let Some(modes) = result.get("modes") {
        let options: Vec<ConfigValue> = modes
            .get("availableModes")
            .and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .filter_map(|m| {
                        Some(ConfigValue {
                            value: m.get("id")?.as_str()?.to_string(),
                            name: m.get("name").and_then(Value::as_str).unwrap_or("").to_string(),
                            description: m.get("description").and_then(Value::as_str).map(str::to_string),
                        })
                    })
                    .collect()
            })
            .unwrap_or_default();
        if !options.is_empty() {
            out.push(ConfigOption {
                id: "mode".into(),
                name: "Mode".into(),
                category: "mode".into(),
                current: modes.get("currentModeId").map(value_str).unwrap_or_default(),
                options,
            });
        }
    }
    out
}

#[cfg(test)]
mod unit {
    use super::*;

    #[test]
    fn session_config_prefers_config_options_and_falls_back_to_modes() {
        let with_options = json!({ "configOptions": [{ "id": "model", "name": "Model", "category": "model", "currentValue": "opus",
            "options": [{ "value": "opus", "name": "Opus" }, { "value": "haiku", "name": "Haiku" }] }] });
        let c = parse_session_config(&with_options);
        assert_eq!(c[0].current, "opus");
        assert_eq!(c[0].options.len(), 2);
        let legacy = json!({ "modes": { "currentModeId": "plan", "availableModes": [{ "id": "plan", "name": "Plan" }] },
            "models": { "currentModelId": "m1", "availableModels": [{ "modelId": "m1", "name": "M1" }] } });
        let c = parse_session_config(&legacy);
        assert_eq!(c.iter().map(|o| o.category.as_str()).collect::<Vec<_>>(), ["model", "mode"]);
    }

    #[test]
    fn marks_subagent_tool_calls() {
        let e = parse_update(&json!({ "sessionUpdate": "tool_call", "toolCallId": "a1", "title": "Find auth code", "status": "in_progress",
            "rawInput": { "subagent_type": "Explore", "description": "Find auth code" }, "_meta": { "claudeCode": { "toolName": "Agent" } } }));
        let Some(Event::ToolCall(c)) = e else { panic!() };
        assert!(c.is_subagent());
        assert_eq!(c.subagent, Some(("Explore".into(), "Find auth code".into())));
        let step = parse_tool_call(&json!({ "toolCallId": "t2", "title": "Read lib.rs", "kind": "read",
            "_meta": { "claudeCode": { "toolName": "Read", "parentToolUseId": "t1" } } }));
        assert_eq!(step.parent.as_deref(), Some("t1"));
    }

    #[test]
    fn parses_usage_update() {
        let e = parse_update(&json!({ "sessionUpdate": "usage_update", "used": 1200, "size": 200000, "cost": { "amount": 0.5, "currency": "USD" } }));
        assert_eq!(e, Some(Event::Usage { used: 1200, size: 200000, cost: Some(0.5) }));
    }

    #[test]
    fn reads_line_ranges() {
        let path = std::env::temp_dir().join(format!("kuaky-acp-read-{}", std::process::id()));
        std::fs::write(&path, "a\nb\nc\nd\n").unwrap();
        assert_eq!(read_text(&path, Some(2), Some(2)).unwrap(), "b\nc\n");
        assert_eq!(read_text(&path, None, None).unwrap(), "a\nb\nc\nd\n");
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn writes_only_inside_roots() {
        let root = std::env::temp_dir();
        assert!(within(&[root.clone()], &root.join("x/new.txt")));
        assert!(!within(&[root.join("only-here")], Path::new("/etc/hosts")));
    }
}
