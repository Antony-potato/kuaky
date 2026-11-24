//! Cliente mínimo del Language Server Protocol: JSON-RPC 2.0 con cabeceras
//! `Content-Length` sobre stdin/stdout del servidor (rust-analyzer, vtsls,
//! basedpyright…). Hilos y canales, sin runtime async.

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

#[derive(Debug, Clone)]
pub enum Event {
    /// `textDocument/publishDiagnostics`: `diagnostics` es el arreglo LSP crudo.
    Diagnostics { uri: String, diagnostics: Value },
    Exited,
}

type Pending = Arc<Mutex<HashMap<u64, flume::Sender<Result<Value, String>>>>>;

pub struct Client {
    stdin: Arc<Mutex<ChildStdin>>,
    child: Mutex<Child>,
    next_id: AtomicU64,
    pending: Pending,
    events: flume::Receiver<Event>,
    /// Capacidades que anunció el servidor en `initialize`.
    pub capabilities: Value,
    pub name: String,
}

impl Client {
    /// Lanza el servidor, hace `initialize` + `initialized` con `root` como
    /// carpeta del proyecto.
    pub fn spawn(name: &str, program: &Path, args: &[String], root: &Path, env: &[(String, String)]) -> anyhow::Result<Client> {
        let mut cmd = Command::new(program);
        cmd.args(args).current_dir(root).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null());
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
            let pending = pending.clone();
            let stdin = stdin.clone();
            std::thread::Builder::new().name(format!("lsp-{name}")).spawn(move || {
                let mut reader = BufReader::new(stdout);
                while let Some(msg) = read_message(&mut reader) {
                    handle_incoming(msg, &pending, &stdin, &tx);
                }
                for (_, waiter) in pending.lock().unwrap().drain() {
                    let _ = waiter.send(Err("el servidor terminó".into()));
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
            capabilities: Value::Null,
            name: name.to_string(),
        };
        let root_uri = path_to_uri(root);
        let init = client.request(
            "initialize",
            json!({
                "processId": std::process::id(),
                "clientInfo": { "name": "kuaky" },
                "rootUri": root_uri,
                "rootPath": root.to_string_lossy(),
                "workspaceFolders": [{ "uri": root_uri, "name": root.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default() }],
                "capabilities": {
                    "textDocument": {
                        "synchronization": { "didSave": true, "dynamicRegistration": false },
                        "completion": { "completionItem": { "snippetSupport": false, "documentationFormat": ["markdown", "plaintext"] }, "contextSupport": true },
                        "hover": { "contentFormat": ["markdown", "plaintext"] },
                        "definition": { "linkSupport": true },
                        "publishDiagnostics": { "relatedInformation": false },
                    },
                    "workspace": { "configuration": true, "workspaceFolders": true },
                    "window": { "workDoneProgress": false },
                },
            }),
            Duration::from_secs(60),
        )?;
        client.capabilities = init.get("capabilities").cloned().unwrap_or(Value::Null);
        client.notify("initialized", json!({}))?;
        Ok(client)
    }

    pub fn events(&self) -> flume::Receiver<Event> {
        self.events.clone()
    }

    fn send(&self, msg: &Value) -> anyhow::Result<()> {
        write_message(&mut *self.stdin.lock().unwrap(), msg)
    }

    pub fn notify(&self, method: &str, params: Value) -> anyhow::Result<()> {
        self.send(&json!({ "jsonrpc": "2.0", "method": method, "params": params }))
    }

    /// Petición bloqueante: llamarla fuera del hilo de la interfaz.
    pub fn request(&self, method: &str, params: Value, timeout: Duration) -> anyhow::Result<Value> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = flume::bounded(1);
        self.pending.lock().unwrap().insert(id, tx);
        self.send(&json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params }))?;
        match rx.recv_timeout(timeout) {
            Ok(Ok(v)) => Ok(v),
            Ok(Err(e)) => Err(anyhow!("{method}: {e}")),
            Err(_) => {
                self.pending.lock().unwrap().remove(&id);
                let _ = self.notify("$/cancelRequest", json!({ "id": id }));
                bail!("{method}: el servidor no respondió a tiempo")
            }
        }
    }

    pub fn did_open(&self, uri: &str, language_id: &str, version: i32, text: &str) -> anyhow::Result<()> {
        self.notify(
            "textDocument/didOpen",
            json!({ "textDocument": { "uri": uri, "languageId": language_id, "version": version, "text": text } }),
        )
    }

    /// Sincronización completa (se envía todo el texto).
    pub fn did_change(&self, uri: &str, version: i32, text: &str) -> anyhow::Result<()> {
        self.notify(
            "textDocument/didChange",
            json!({ "textDocument": { "uri": uri, "version": version }, "contentChanges": [{ "text": text }] }),
        )
    }

    pub fn did_save(&self, uri: &str, text: &str) -> anyhow::Result<()> {
        self.notify("textDocument/didSave", json!({ "textDocument": { "uri": uri }, "text": text }))
    }

    pub fn did_close(&self, uri: &str) -> anyhow::Result<()> {
        self.notify("textDocument/didClose", json!({ "textDocument": { "uri": uri } }))
    }

    /// Caracteres que disparan el autocompletado (p. ej. `.` o `::`).
    pub fn completion_triggers(&self) -> Vec<String> {
        self.capabilities
            .get("completionProvider")
            .and_then(|c| c.get("triggerCharacters"))
            .and_then(Value::as_array)
            .map(|a| a.iter().filter_map(Value::as_str).map(str::to_string).collect())
            .unwrap_or_default()
    }
}

impl Drop for Client {
    fn drop(&mut self) {
        let _ = self.notify("exit", Value::Null);
        if let Ok(mut child) = self.child.lock() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

fn write_message(w: &mut impl Write, msg: &Value) -> anyhow::Result<()> {
    let body = serde_json::to_vec(msg)?;
    write!(w, "Content-Length: {}\r\n\r\n", body.len())?;
    w.write_all(&body)?;
    w.flush()?;
    Ok(())
}

fn read_message(r: &mut impl BufRead) -> Option<Value> {
    let mut length = None;
    loop {
        let mut line = String::new();
        if r.read_line(&mut line).ok()? == 0 {
            return None;
        }
        let line = line.trim_end();
        if line.is_empty() {
            break;
        }
        if let Some(v) = line.strip_prefix("Content-Length:") {
            length = v.trim().parse::<usize>().ok();
        }
    }
    let mut body = vec![0; length?];
    r.read_exact(&mut body).ok()?;
    serde_json::from_slice(&body).ok()
}

fn handle_incoming(msg: Value, pending: &Pending, stdin: &Arc<Mutex<ChildStdin>>, tx: &flume::Sender<Event>) {
    let method = msg.get("method").and_then(Value::as_str);
    match (method, msg.get("id").cloned()) {
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
        // Peticiones del servidor: respuestas neutras para que no se quede esperando.
        (Some(method), Some(id)) => {
            let result = match method {
                "workspace/configuration" => {
                    let n = msg.pointer("/params/items").and_then(Value::as_array).map_or(0, |a| a.len());
                    Value::Array(vec![Value::Null; n])
                }
                "workspace/workspaceFolders" => Value::Array(Vec::new()),
                _ => Value::Null,
            };
            if let Ok(mut s) = stdin.lock() {
                let _ = write_message(&mut *s, &json!({ "jsonrpc": "2.0", "id": id, "result": result }));
            }
        }
        (Some("textDocument/publishDiagnostics"), None) => {
            if let Some(params) = msg.get("params") {
                let uri = params.get("uri").and_then(Value::as_str).unwrap_or("").to_string();
                let diagnostics = params.get("diagnostics").cloned().unwrap_or(Value::Array(Vec::new()));
                let _ = tx.send(Event::Diagnostics { uri, diagnostics });
            }
        }
        _ => {}
    }
}

/// `/a b/c.rs` → `file:///a%20b/c.rs` (en Windows `file:///C:/…`).
pub fn path_to_uri(path: &Path) -> String {
    let s = path.to_string_lossy().replace('\\', "/");
    let mut out = String::from("file://");
    if !s.starts_with('/') {
        out.push('/');
    }
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' | b'/' | b':' => out.push(b as char),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// Inversa de [`path_to_uri`]; `None` si no es una URI `file:`.
pub fn uri_to_path(uri: &str) -> Option<PathBuf> {
    let rest = uri.strip_prefix("file://")?;
    let bytes = rest.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let Ok(b) = u8::from_str_radix(std::str::from_utf8(&bytes[i + 1..i + 3]).ok()?, 16) {
                out.push(b);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    let mut s = String::from_utf8(out).ok()?;
    // `/C:/x` → `C:/x` en Windows.
    if s.len() > 2 && s.as_bytes()[0] == b'/' && s.as_bytes()[2] == b':' {
        s.remove(0);
    }
    Some(PathBuf::from(s))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uris_round_trip() {
        let p = Path::new("/Users/ana/mis proyectos/ñ.rs");
        let uri = path_to_uri(p);
        assert_eq!(uri, "file:///Users/ana/mis%20proyectos/%C3%B1.rs");
        assert_eq!(uri_to_path(&uri).unwrap(), p);
    }

    #[test]
    fn frames_messages() {
        let mut buf = Vec::new();
        write_message(&mut buf, &json!({ "a": 1 })).unwrap();
        assert!(buf.starts_with(b"Content-Length: 7\r\n\r\n"));
        let msg = read_message(&mut std::io::Cursor::new(buf)).unwrap();
        assert_eq!(msg["a"], 1);
    }
}
