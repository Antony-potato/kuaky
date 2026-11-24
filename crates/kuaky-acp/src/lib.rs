//! Cliente mínimo del [Agent Client Protocol](https://agentclientprotocol.com)
//! (versión 1): JSON-RPC 2.0, un mensaje JSON por línea, sobre stdin/stdout
//! del proceso del agente (`claude-agent-acp`, `agy acp`, `gemini --acp`…).
//!
//! Usa hilos y canales (sin runtime async), así encaja con cualquier
//! interfaz. kuaky no ofrece al agente lectura/escritura de archivos ni
//! terminal: el agente trabaja directo sobre el disco y kuaky muestra lo que
//! hace.

use anyhow::{Context as _, anyhow, bail};
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    io::{BufRead, BufReader, Write},
    path::Path,
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
    MessageChunk(String),
    ThoughtChunk(String),
    ToolCall(ToolCall),
    /// Actualiza campos de una herramienta ya anunciada (los `None` no cambian).
    ToolCallUpdate(ToolCall),
    Plan(Vec<PlanEntry>),
    Permission(PermissionRequest),
    /// Terminó el turno (`end_turn`, `cancelled`, `max_tokens`…).
    TurnEnded(String),
    Error(String),
    Exited,
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

        {
            let tx = tx.clone();
            let pending = pending.clone();
            let stdin = stdin.clone();
            std::thread::Builder::new().name("acp-reader".into()).spawn(move || {
                for line in BufReader::new(stdout).lines() {
                    let Ok(line) = line else { break };
                    if line.trim().is_empty() {
                        continue;
                    }
                    let Ok(msg) = serde_json::from_str::<Value>(&line) else { continue };
                    handle_incoming(msg, &pending, &stdin, &tx);
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
        };
        let init = client.request(
            "initialize",
            json!({
                "protocolVersion": PROTOCOL_VERSION,
                "clientCapabilities": { "fs": { "readTextFile": false, "writeTextFile": false }, "terminal": false },
                "clientInfo": { "name": "kuaky", "version": env!("CARGO_PKG_VERSION") },
            }),
            Duration::from_secs(60),
        )?;
        client.agent_name = init
            .get("agentInfo")
            .and_then(|i| i.get("title").or_else(|| i.get("name")))
            .and_then(Value::as_str)
            .map(str::to_string);
        Ok(client)
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
        let result = self.request(
            "session/new",
            json!({ "cwd": cwd.to_string_lossy(), "mcpServers": [] }),
            Duration::from_secs(120),
        )?;
        result
            .get("sessionId")
            .and_then(Value::as_str)
            .map(str::to_string)
            .context("session/new sin sessionId")
    }

    /// Envía un mensaje. La respuesta llega como [`Event::TurnEnded`] cuando
    /// el agente termina su turno; mientras tanto llegan los demás eventos.
    pub fn prompt(&self, session: &str, text: &str) -> anyhow::Result<()> {
        let rx = self.start_request(
            "session/prompt",
            json!({ "sessionId": session, "prompt": [{ "type": "text", "text": text }] }),
        )?;
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

fn handle_incoming(msg: Value, pending: &Pending, stdin: &Arc<Mutex<ChildStdin>>, tx: &flume::Sender<Event>) {
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
            } else {
                // No ofrecemos fs/terminal: responder "método no encontrado".
                let reply = json!({ "jsonrpc": "2.0", "id": id, "error": { "code": -32601, "message": format!("{method} no soportado por kuaky") } });
                if let Ok(mut s) = stdin.lock() {
                    let _ = writeln!(s, "{reply}");
                    let _ = s.flush();
                }
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
        "agent_message_chunk" => text_of(update.get("content")?).map(Event::MessageChunk),
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
        ..ToolCall::default()
    };
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
