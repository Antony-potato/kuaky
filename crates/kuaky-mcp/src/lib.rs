//! Herramientas de kuaky para los agentes de sus chats (ACP).
//!
//! kuaky pasa este servidor en `mcpServers` al crear cada sesión de chat: el
//! agente lo lanza como `kuaky mcp` (MCP por stdio, un JSON por línea) y con
//! él puede abrir una sesión de otro agente como sub-línea de su objetivo,
//! ver cómo va y mandarle mensajes.
//!
//! El proceso `kuaky mcp` no tiene la interfaz: cada llamada se deja como
//! archivo en el buzón de la app (`KUAKY_IPC_DIR`) y espera la respuesta.
//! Archivos en vez de un socket: funciona igual en macOS y en Windows, y la
//! app solo revisa una carpeta cada poco.

use serde_json::{Value, json};
use std::{
    io::{self, BufRead, Write},
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
    time::{Duration, Instant},
};

/// Nombre del servidor: el agente ve las herramientas como `mcp__kuaky__…`.
pub const SERVER_NAME: &str = "kuaky";
pub const ENV_DIR: &str = "KUAKY_IPC_DIR";
pub const ENV_TOKEN: &str = "KUAKY_IPC_TOKEN";
pub const ENV_SESSION: &str = "KUAKY_SESSION";

/// Sesiones abiertas a la vez desde una conversación, aparte de la principal.
pub const MAX_SESSIONS: usize = 3;

/// Agentes que se pueden abrir (claves de `Harness`, con `agy` como `antigravity`).
pub const AGENTS: [&str; 5] = ["antigravity", "claude", "codex", "gemini", "opencode"];

/// Cuánto espera `kuaky mcp` a que la app conteste.
const ASK_TIMEOUT: Duration = Duration::from_secs(30);

pub const INSTRUCTIONS: &str = "kuaky is the editor this session runs in. It shows every agent session on a map of goals (lines). \
These tools let you split work with other coding agents. When the user asks you to have another agent (Antigravity, Codex, Gemini, Claude, OpenCode) \
take part of the work, call open_session with a self-contained brief, then keep doing your own part. \
Check on those sessions with read_session only when the user asks, or when you need their result to continue. Use draw when a quick diagram explains something better than text (architecture, flows, comparisons).";

/// Llamada de un agente a una herramienta.
#[derive(Debug, Clone, PartialEq)]
pub struct Call {
    pub id: String,
    /// Sesión de kuaky (uid) que llama.
    pub session: String,
    pub tool: String,
    pub args: Value,
}

/// Respuesta de la app: texto para el agente.
#[derive(Debug, Clone, PartialEq)]
pub struct Reply {
    pub text: String,
    pub is_error: bool,
}

impl Reply {
    pub fn ok(text: impl Into<String>) -> Reply {
        Reply { text: text.into(), is_error: false }
    }

    pub fn error(text: impl Into<String>) -> Reply {
        Reply { text: text.into(), is_error: true }
    }
}

/// Herramientas que ofrece kuaky (`tools/list`).
pub fn tools() -> Value {
    let session = json!({ "type": "string", "description": "Session id returned by open_session or list_sessions." });
    json!([
        {
            "name": "open_session",
            "description": format!(
                "Open a session of another coding agent in kuaky and start it on a task right away. By default it is a sub-line \
                 of your current goal and works in parallel while you keep doing your part. Use it when the user asks you to hand \
                 part of the work to another agent (e.g. \"have Antigravity do the tests while you do the sidebar\"). \
                 Split the work by files so you don't both edit the same ones. At most {MAX_SESSIONS} sessions open besides the main one. \
                 Returns the session id for read_session and send_to_session."
            ),
            "inputSchema": {
                "type": "object",
                "properties": {
                    "agent": { "type": "string", "enum": AGENTS, "description": "Agent that does the task." },
                    "task": {
                        "type": "string",
                        "description": "Self-contained brief. The other agent does not see this conversation: include the goal, the relevant \
                                        files, decisions already taken, what to do, which files are yours (it must not edit them) and how \
                                        to check that it works."
                    },
                    "title": { "type": "string", "description": "Short name for its line on kuaky's map (3–6 words)." },
                    "mode": {
                        "type": "string",
                        "enum": ["branch", "handoff"],
                        "description": "branch (default): a sub-task that hangs from your goal while you keep working. \
                                        handoff: the other agent continues this same goal and you stop."
                    },
                    "worktree": {
                        "type": "boolean",
                        "description": "Give it its own git worktree and branch. Only when you both must edit the same files; \
                                        by default it works in your folder."
                    }
                },
                "required": ["agent", "task"]
            }
        },
        {
            "name": "list_sessions",
            "description": "List the main session and the sessions opened from it, with agent, line and status.",
            "inputSchema": { "type": "object", "properties": {} }
        },
        {
            "name": "read_session",
            "description": "Status, plan and latest answer of a session opened with open_session. Use it when the user asks how it is going or what it found.",
            "inputSchema": { "type": "object", "properties": { "session": session }, "required": ["session"] }
        },
        {
            "name": "draw",
            "description": "Draw a quick diagram in kuaky's canvas, shown beside the chat. One command per line; kuaky lays it out,                             so don't send positions. Re-sending an ID changes only the keys you send: send changes, not the whole                             diagram. Replies ok or the lines that failed.
                            box ID [\"label\"] [h=1 w=2 d=2 on=PARENT color=coral|blue|green|violet|amber|gray]  (on: stack on top)
                            A -> B [\"label\"] [accent]  (boxes flow in levels along arrows)
                            note ID \"text\" [at=top] [accent]  (callout on a box)
                            rm ID...  |  clear  |  view iso|flat  (iso 3D is the default, flat is 2D)",
            "inputSchema": {
                "type": "object",
                "properties": { "text": { "type": "string", "description": "Commands, one per line." } },
                "required": ["text"]
            }
        },
        {
            "name": "send_to_session",
            "description": "Send a follow-up message to a session opened with open_session. It is queued if that agent is busy.",
            "inputSchema": {
                "type": "object",
                "properties": { "session": session, "message": { "type": "string", "description": "What to tell it." } },
                "required": ["session", "message"]
            }
        }
    ])
}

fn tool_names() -> Vec<String> {
    tools().as_array().map(|a| a.iter().filter_map(|t| t["name"].as_str().map(str::to_string)).collect()).unwrap_or_default()
}

/// Atiende MCP (JSON-RPC, un mensaje por línea) hasta que se cierre la entrada.
/// `call` resuelve cada `tools/call`.
pub fn serve(input: impl BufRead, mut output: impl Write, mut call: impl FnMut(&str, &Value) -> Reply) -> io::Result<()> {
    let names = tool_names();
    for line in input.lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let Ok(msg) = serde_json::from_str::<Value>(&line) else {
            send(&mut output, &json!({ "jsonrpc": "2.0", "id": null, "error": { "code": -32700, "message": "parse error" } }))?;
            continue;
        };
        // Notificaciones (`notifications/initialized`…) y respuestas: nada que contestar.
        let (Some(id), Some(method)) = (msg.get("id").cloned(), msg.get("method").and_then(Value::as_str)) else { continue };
        let params = msg.get("params").cloned().unwrap_or(Value::Null);
        let result = match method {
            "initialize" => Ok(json!({
                "protocolVersion": params.get("protocolVersion").and_then(Value::as_str).unwrap_or("2025-06-18"),
                "capabilities": { "tools": {} },
                "serverInfo": { "name": SERVER_NAME, "version": env!("CARGO_PKG_VERSION") },
                "instructions": INSTRUCTIONS,
            })),
            "ping" => Ok(json!({})),
            "tools/list" => Ok(json!({ "tools": tools() })),
            "tools/call" => {
                let name = params.get("name").and_then(Value::as_str).unwrap_or_default();
                if names.iter().any(|n| n == name) {
                    let args = params.get("arguments").cloned().filter(Value::is_object).unwrap_or_else(|| json!({}));
                    let reply = call(name, &args);
                    Ok(json!({ "content": [{ "type": "text", "text": reply.text }], "isError": reply.is_error }))
                } else {
                    Err((-32602, format!("unknown tool: {name}")))
                }
            }
            _ => Err((-32601, format!("method not found: {method}"))),
        };
        let reply = match result {
            Ok(result) => json!({ "jsonrpc": "2.0", "id": id, "result": result }),
            Err((code, message)) => json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } }),
        };
        send(&mut output, &reply)?;
    }
    Ok(())
}

fn send(output: &mut impl Write, msg: &Value) -> io::Result<()> {
    writeln!(output, "{msg}")?;
    output.flush()
}

/// `kuaky mcp`: el servidor que lanza el agente, con el buzón y la sesión de
/// las variables de entorno que puso kuaky.
pub fn serve_stdio() -> io::Result<()> {
    let var = |key: &str| std::env::var(key).map_err(|_| io::Error::other(format!("{key} is not set: kuaky starts this server for its chats")));
    let mailbox = Mailbox::open(PathBuf::from(var(ENV_DIR)?), var(ENV_TOKEN)?);
    let session = var(ENV_SESSION)?;
    serve(io::stdin().lock(), io::stdout().lock(), |tool, args| mailbox.ask(&session, tool, args, ASK_TIMEOUT))
}

/// Buzón entre `kuaky mcp` y la app: `<id>.call` de ida, `<id>.reply` de vuelta.
#[derive(Debug, Clone)]
pub struct Mailbox {
    dir: PathBuf,
    token: String,
}

impl Mailbox {
    /// Buzón de la app: carpeta vacía y clave nueva al azar.
    pub fn create(dir: PathBuf) -> io::Result<Mailbox> {
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir)?;
        Ok(Mailbox { dir, token: random_token() })
    }

    pub fn open(dir: PathBuf, token: String) -> Mailbox {
        Mailbox { dir, token }
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// Variables para el `kuaky mcp` de una sesión.
    pub fn env(&self, session: &str) -> Vec<(String, String)> {
        vec![
            (ENV_DIR.to_string(), self.dir.to_string_lossy().into_owned()),
            (ENV_TOKEN.to_string(), self.token.clone()),
            (ENV_SESSION.to_string(), session.to_string()),
        ]
    }

    /// Llamadas pendientes, en orden de llegada (se quitan del buzón). Las que
    /// no traen la clave se contestan con error.
    pub fn take(&self) -> Vec<Call> {
        let Ok(entries) = std::fs::read_dir(&self.dir) else {
            // Si alguien borró la carpeta, se vuelve a crear para las próximas llamadas.
            let _ = std::fs::create_dir_all(&self.dir);
            return Vec::new();
        };
        let mut files: Vec<PathBuf> = entries
            .filter_map(Result::ok)
            .map(|e| e.path())
            .filter(|p| p.extension().is_some_and(|x| x == "call"))
            .collect();
        files.sort();
        let mut calls = Vec::new();
        for path in files {
            let Some(id) = path.file_stem().and_then(|s| s.to_str()).filter(|s| valid_id(s)).map(str::to_string) else {
                let _ = std::fs::remove_file(&path);
                continue;
            };
            let text = std::fs::read_to_string(&path);
            let _ = std::fs::remove_file(&path);
            let Ok(msg) = text.map_err(|_| ()).and_then(|t| serde_json::from_str::<Value>(&t).map_err(|_| ())) else { continue };
            if msg.get("token").and_then(Value::as_str) != Some(self.token.as_str()) {
                let _ = self.answer(&id, &Reply::error("kuaky rejected this call (wrong token)."));
                continue;
            }
            calls.push(Call {
                id,
                session: msg.get("session").and_then(Value::as_str).unwrap_or_default().to_string(),
                tool: msg.get("tool").and_then(Value::as_str).unwrap_or_default().to_string(),
                args: msg.get("args").cloned().unwrap_or_else(|| json!({})),
            });
        }
        calls
    }

    pub fn answer(&self, id: &str, reply: &Reply) -> io::Result<()> {
        if !valid_id(id) {
            return Err(io::Error::other("invalid call id"));
        }
        let body = json!({ "text": reply.text, "isError": reply.is_error });
        write_atomic(&self.dir.join(format!("{id}.reply")), &body.to_string())
    }

    /// Lado del agente: deja la llamada y espera la respuesta.
    pub fn ask(&self, session: &str, tool: &str, args: &Value, timeout: Duration) -> Reply {
        let id = new_id();
        let body = json!({ "token": self.token, "session": session, "tool": tool, "args": args });
        let call = self.dir.join(format!("{id}.call"));
        if let Err(err) = write_atomic(&call, &body.to_string()) {
            return Reply::error(format!("Could not reach kuaky ({err}). Is it still open?"));
        }
        let reply = self.dir.join(format!("{id}.reply"));
        let start = Instant::now();
        while start.elapsed() < timeout {
            if let Ok(text) = std::fs::read_to_string(&reply) {
                let _ = std::fs::remove_file(&reply);
                let msg: Value = serde_json::from_str(&text).unwrap_or_default();
                return Reply {
                    text: msg.get("text").and_then(Value::as_str).unwrap_or_default().to_string(),
                    is_error: msg.get("isError").and_then(Value::as_bool).unwrap_or(true),
                };
            }
            std::thread::sleep(Duration::from_millis(40));
        }
        let _ = std::fs::remove_file(&call);
        Reply::error("kuaky did not answer in time. Is it still open?")
    }
}

/// Escribe en un temporal y renombra: quien lee nunca ve un archivo a medias.
fn write_atomic(path: &Path, text: &str) -> io::Result<()> {
    let tmp = PathBuf::from(format!("{}.tmp", path.display()));
    std::fs::write(&tmp, text)?;
    std::fs::rename(&tmp, path)
}

fn valid_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= 64 && id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
}

fn new_id() -> String {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0);
    format!("{nanos}-{}-{}", std::process::id(), COUNTER.fetch_add(1, Ordering::Relaxed))
}

/// 128 bits al azar sin dependencias: `RandomState` trae claves nuevas del sistema.
fn random_token() -> String {
    use std::hash::{BuildHasher, Hasher};
    (0..2u8)
        .map(|i| {
            let mut h = std::collections::hash_map::RandomState::new().build_hasher();
            h.write_u8(i);
            h.write_u32(std::process::id());
            h.write_u128(std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0));
            format!("{:016x}", h.finish())
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!("kuaky-mcp-{name}-{}", std::process::id()))
    }

    fn run(lines: &[Value], call: impl FnMut(&str, &Value) -> Reply) -> Vec<Value> {
        let input: String = lines.iter().map(|l| format!("{l}\n")).collect();
        let mut out = Vec::new();
        serve(input.as_bytes(), &mut out, call).unwrap();
        String::from_utf8(out).unwrap().lines().map(|l| serde_json::from_str(l).unwrap()).collect()
    }

    #[test]
    fn answers_the_mcp_handshake_and_tools() {
        let mut seen = Vec::new();
        let out = run(
            &[
                json!({ "jsonrpc": "2.0", "id": 1, "method": "initialize", "params": { "protocolVersion": "2025-03-26" } }),
                json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }),
                json!({ "jsonrpc": "2.0", "id": 2, "method": "tools/list" }),
                json!({ "jsonrpc": "2.0", "id": 3, "method": "tools/call", "params": { "name": "open_session", "arguments": { "agent": "antigravity", "task": "tests" } } }),
                json!({ "jsonrpc": "2.0", "id": 4, "method": "tools/call", "params": { "name": "nope" } }),
                json!({ "jsonrpc": "2.0", "id": 5, "method": "resources/list" }),
            ],
            |tool, args| {
                seen.push((tool.to_string(), args.clone()));
                Reply::ok("opened s1")
            },
        );
        assert_eq!(out.len(), 5, "la notificación no se contesta");
        assert_eq!(out[0]["result"]["protocolVersion"], "2025-03-26");
        assert_eq!(out[0]["result"]["serverInfo"]["name"], "kuaky");
        let names: Vec<&str> = out[1]["result"]["tools"].as_array().unwrap().iter().map(|t| t["name"].as_str().unwrap()).collect();
        assert_eq!(names, ["open_session", "list_sessions", "read_session", "draw", "send_to_session"]);
        assert_eq!(out[2]["result"]["content"][0]["text"], "opened s1");
        assert_eq!(out[2]["result"]["isError"], false);
        assert_eq!(out[3]["error"]["code"], -32602);
        assert_eq!(out[4]["error"]["code"], -32601);
        assert_eq!(seen, [("open_session".to_string(), json!({ "agent": "antigravity", "task": "tests" }))]);
    }

    #[test]
    fn mailbox_round_trip() {
        let app = Mailbox::create(temp_dir("round")).unwrap();
        let env: std::collections::HashMap<String, String> = app.env("s42").into_iter().collect();
        let agent = Mailbox::open(PathBuf::from(&env[ENV_DIR]), env[ENV_TOKEN].clone());
        let server = app.clone();
        let handle = std::thread::spawn(move || loop {
            if let Some(call) = server.take().pop() {
                server.answer(&call.id, &Reply::ok(format!("{} {} {}", call.session, call.tool, call.args["agent"]))).unwrap();
                return call;
            }
            std::thread::sleep(Duration::from_millis(10));
        });
        let reply = agent.ask("s42", "open_session", &json!({ "agent": "codex" }), Duration::from_secs(5));
        assert_eq!(reply, Reply::ok("s42 open_session \"codex\""));
        assert_eq!(handle.join().unwrap().tool, "open_session");
        assert!(std::fs::read_dir(app.dir()).unwrap().next().is_none(), "no quedan archivos");
        let _ = std::fs::remove_dir_all(app.dir());
    }

    #[test]
    fn wrong_token_is_rejected() {
        let app = Mailbox::create(temp_dir("token")).unwrap();
        let intruder = Mailbox::open(app.dir().to_path_buf(), "nope".into());
        let handle = std::thread::spawn(move || intruder.ask("s1", "list_sessions", &json!({}), Duration::from_secs(5)));
        while !handle.is_finished() {
            assert!(app.take().is_empty(), "una llamada sin la clave no llega a la app");
            std::thread::sleep(Duration::from_millis(10));
        }
        let reply = handle.join().unwrap();
        assert!(reply.is_error && reply.text.contains("wrong token"), "{reply:?}");
        let _ = std::fs::remove_dir_all(app.dir());
    }

    #[test]
    fn gives_up_when_kuaky_does_not_answer() {
        let app = Mailbox::create(temp_dir("timeout")).unwrap();
        let reply = app.ask("s1", "list_sessions", &json!({}), Duration::from_millis(100));
        assert!(reply.is_error);
        assert!(app.take().is_empty(), "la llamada vencida se quita del buzón");
        let _ = std::fs::remove_dir_all(app.dir());
    }
}
