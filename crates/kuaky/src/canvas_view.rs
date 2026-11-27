//! Lienzo de diagramas de los agentes: `assets/canvas/canvas.html` en un webview.
//! Los agentes dibujan con la herramienta `draw` de `kuaky mcp`: el texto va tal cual al lienzo
//! (`kuaky.apply`) y su respuesta ("ok" o los errores) vuelve al agente.
//!
//! Cada diagrama es un artefacto con nombre, guardado por proyecto (ver `dir`) y ligado a las
//! conversaciones que lo dibujaron. El lienzo muestra uno a la vez y se puede cerrar y volver a
//! abrir desde el panel Artifacts.

use crate::style;
use crate::webview::WebPanel;
use base64::Engine as _;
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use kuaky_mcp::Reply;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

const HTML: &str = include_str!("../assets/canvas/canvas.html");
const ROUGH: &str = include_str!("../assets/canvas/rough.js");
const DAGRE: &str = include_str!("../assets/canvas/dagre.js");

pub struct Artifact {
    pub name: String,
    pub title: String,
    pub modified: SystemTime,
    /// Conversaciones (uid de kuaky) que lo dibujaron.
    pub sessions: Vec<String>,
}

/// Artefactos del proyecto, fuera del repo: `<datos locales>/kuaky/artifacts/<carpeta>-<hash>/`.
pub fn dir(root: &Path) -> PathBuf {
    let path = root.to_string_lossy();
    let hash = path.bytes().fold(0xcbf29ce484222325u64, |h, b| (h ^ b as u64).wrapping_mul(0x100000001b3));
    let folder = root.file_name().map(|n| slug(&n.to_string_lossy())).unwrap_or_default();
    dirs::data_local_dir().unwrap_or_default().join("kuaky").join("artifacts").join(format!("{folder}-{:08x}", hash as u32))
}

/// Nombre de archivo seguro: minúsculas, dígitos y guiones.
pub fn slug(name: &str) -> String {
    let mut out = String::new();
    for c in name.trim().to_lowercase().chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c);
        } else if !out.ends_with('-') {
            out.push('-');
        }
    }
    out.trim_matches('-').chars().take(48).collect()
}

fn file(root: &Path, name: &str, ext: &str) -> PathBuf {
    dir(root).join(format!("{name}.{ext}"))
}

pub fn read(root: &Path, name: &str) -> Option<String> {
    std::fs::read_to_string(file(root, name, "txt")).ok()
}

pub fn delete(root: &Path, name: &str) {
    std::fs::remove_file(file(root, name, "txt")).ok();
    std::fs::remove_file(file(root, name, "sessions")).ok();
}

/// Liga el artefacto a la conversación `session` (un uid por línea en `<name>.sessions`).
pub fn link(root: &Path, name: &str, session: &str) {
    let path = file(root, name, "sessions");
    let mut all = std::fs::read_to_string(&path).unwrap_or_default();
    if session.is_empty() || all.lines().any(|l| l == session) {
        return;
    }
    all.push_str(session);
    all.push('\n');
    std::fs::create_dir_all(dir(root)).and_then(|_| std::fs::write(&path, all)).ok();
}

/// Los más recientes primero.
pub fn list(root: &Path) -> Vec<Artifact> {
    let Ok(entries) = std::fs::read_dir(dir(root)) else { return Vec::new() };
    let mut out: Vec<Artifact> = entries
        .flatten()
        .filter_map(|e| {
            let path = e.path();
            (path.extension()? == "txt").then_some(())?;
            let name = path.file_stem()?.to_str()?.to_string();
            let src = std::fs::read_to_string(&path).ok()?;
            let modified = e.metadata().and_then(|m| m.modified()).unwrap_or(SystemTime::UNIX_EPOCH);
            let sessions = std::fs::read_to_string(path.with_extension("sessions")).unwrap_or_default().lines().map(str::to_string).collect();
            Some(Artifact { title: title_of(&src).unwrap_or_else(|| name.clone()), name, modified, sessions })
        })
        .collect();
    out.sort_by(|a, b| b.modified.cmp(&a.modified));
    out
}

/// `title "..."` del texto, si lo tiene.
fn title_of(src: &str) -> Option<String> {
    let t = src.lines().rev().find_map(|l| l.trim().strip_prefix("title "))?.trim();
    Some(t.strip_prefix('"').and_then(|t| t.strip_suffix('"')).unwrap_or(t).replace("\\\"", "\""))
}

enum Job {
    Open(String),
    Draw { name: String, id: String, text: String },
}

/// Cambió el artefacto que se ve (el proyecto lo recuerda para la próxima vez).
pub struct CanvasChanged;

pub struct CanvasView {
    root: PathBuf,
    /// Se crea al pintarse la primera vez (o al dibujar): una pestaña restaurada no gasta memoria
    /// hasta que se ve.
    panel: Option<Result<WebPanel, String>>,
    creating: bool,
    ready: bool,
    /// Artefacto que se ve; ahí se guarda lo que manda la página.
    current: Option<String>,
    /// El que se verá en cuanto cargue la página (pestaña restaurada que aún no se pinta).
    pending: Option<String>,
    title: String,
    /// Lo que llega antes de que cargue la página.
    queue: Vec<Job>,
}

impl EventEmitter<CanvasChanged> for CanvasView {}

impl CanvasView {
    pub fn new(root: PathBuf) -> Self {
        Self { root, panel: None, creating: false, ready: false, current: None, pending: None, title: String::new(), queue: Vec::new() }
    }

    /// El artefacto que se ve, o el que se verá en cuanto cargue.
    pub fn current(&self) -> Option<String> {
        self.current.clone().or_else(|| self.pending.clone())
    }

    /// Para la pestaña: el título del diagrama, o su nombre.
    pub fn title(&self) -> SharedString {
        match (&self.current, self.title.is_empty()) {
            (_, false) => self.title.clone().into(),
            (Some(name), true) => name.clone().into(),
            (None, true) => "Canvas".into(),
        }
    }

    pub fn open(&mut self, name: String, cx: &mut Context<Self>) {
        self.expect(&name);
        self.push(Job::Open(name), cx);
    }

    /// Título y nombre ya, aunque la página todavía no lo cargue.
    fn expect(&mut self, name: &str) {
        if self.current.as_deref() != Some(name) {
            self.title = read(&self.root, name).and_then(|src| title_of(&src)).unwrap_or_default();
            self.pending = Some(name.to_string());
        }
    }

    /// Manda órdenes al artefacto `name` (lo crea si no existe). `None`: la respuesta llega
    /// después, por el buzón.
    pub fn draw(&mut self, name: String, call_id: String, text: String, window: &mut Window, cx: &mut Context<Self>) -> Option<Reply> {
        // Aunque la pestaña quede detrás (otra se abrió después), el agente espera la respuesta.
        self.ensure_panel(window, cx);
        if let Some(Err(e)) = &self.panel {
            return Some(Reply::error(e.clone()));
        }
        self.expect(&name);
        if let Some(t) = title_of(&text) {
            self.title = t;
        }
        self.push(Job::Draw { name, id: call_id, text }, cx)
    }

    /// Se borró `name`: si es el que se ve, el lienzo queda vacío.
    pub fn forget(&mut self, name: &str, cx: &mut Context<Self>) {
        if self.current().as_deref() == Some(name) {
            self.current = None;
            self.pending = None;
            self.title.clear();
            if let Some(Ok(panel)) = &self.panel {
                panel.eval("kuaky.load('')").ok();
            }
            cx.emit(CanvasChanged);
            cx.notify();
        }
    }

    fn push(&mut self, job: Job, cx: &mut Context<Self>) -> Option<Reply> {
        if self.ready {
            return self.run(job, cx);
        }
        self.queue.push(job);
        cx.notify(); // que se pinte y cree el webview
        None
    }

    fn run(&mut self, job: Job, cx: &mut Context<Self>) -> Option<Reply> {
        let Some(Ok(panel)) = &self.panel else { return None };
        let name = match &job {
            Job::Open(name) | Job::Draw { name, .. } => name.clone(),
        };
        if self.current.as_deref() != Some(name.as_str()) {
            let src = read(&self.root, &name).unwrap_or_default();
            self.title = title_of(&src).unwrap_or_default();
            self.current = Some(name);
            self.pending = None;
            cx.emit(CanvasChanged);
            cx.notify();
            if let Err(e) = panel.eval(&format!("kuaky.load({})", serde_json::json!(src))) {
                return Some(Reply::error(format!("The canvas did not open it: {e}")));
            }
        }
        let Job::Draw { id, text, .. } = job else { return None };
        let js = format!("kuaky.apply({}, {})", serde_json::json!(text), serde_json::json!(id));
        panel.eval(&js).err().map(|e| Reply::error(format!("The canvas did not take it: {e}")))
    }

    fn flush(&mut self, cx: &mut Context<Self>) {
        for job in std::mem::take(&mut self.queue) {
            let id = match &job {
                Job::Draw { id, .. } => Some(id.clone()),
                Job::Open(_) => None,
            };
            let reply = match &self.panel {
                Some(Err(e)) => Some(Reply::error(e.clone())),
                _ => self.run(job, cx),
            };
            if let (Some(reply), Some(id)) = (reply, id) {
                answer(&id, &reply);
            }
        }
    }

    fn on_message(&mut self, body: &str, cx: &mut Context<Self>) {
        let msg: serde_json::Value = serde_json::from_str(body).unwrap_or_default();
        if let (Some(src), Some(name)) = (msg["src"].as_str(), &self.current) {
            let path = file(&self.root, name, "txt");
            if let Err(err) = std::fs::create_dir_all(dir(&self.root)).and_then(|_| std::fs::write(&path, src)) {
                tracing::warn!(%err, path = %path.display(), "no se pudo guardar el artefacto");
            }
        }
        if let Some(t) = msg["title"].as_str() {
            if self.title != t {
                self.title = t.to_string();
                cx.notify();
            }
        }
        if let (Some(id), Some(out)) = (msg["id"].as_str(), msg["out"].as_str()) {
            answer(id, &if out == "ok" { Reply::ok(out) } else { Reply::error(out) });
        }
        if let (Some(kind), Some(data)) = (msg["export"].as_str(), msg["data"].as_str()) {
            self.export(kind, data, cx);
        }
        if msg["ready"] == true {
            self.ready = true;
            self.flush(cx);
        }
    }

    /// `copy`: PNG al portapapeles; `png` / `svg`: guardar con el diálogo del sistema.
    fn export(&mut self, kind: &str, data: &str, cx: &mut Context<Self>) {
        let bytes = match kind {
            "svg" => data.as_bytes().to_vec(),
            _ => match base64::engine::general_purpose::STANDARD.decode(data) {
                Ok(b) => b,
                Err(err) => return tracing::warn!(%err, "imagen del lienzo inválida"),
            },
        };
        if kind == "copy" {
            return cx.write_to_clipboard(ClipboardItem::new_image(&Image::from_bytes(ImageFormat::Png, bytes)));
        }
        let name = format!("{}.{}", self.current.clone().unwrap_or_else(|| "diagram".into()), if kind == "svg" { "svg" } else { "png" });
        let pick = cx.prompt_for_new_path(&self.root, Some(&name));
        cx.spawn(async move |_, _| {
            if let Ok(Ok(Some(path))) = pick.await {
                if let Err(err) = std::fs::write(&path, bytes) {
                    tracing::warn!(%err, path = %path.display(), "no se pudo exportar el diagrama");
                }
            }
        })
        .detach();
    }
}

fn answer(call_id: &str, reply: &Reply) {
    if let Some(mailbox) = crate::project::delegate::mailbox() {
        if let Err(err) = mailbox.answer(call_id, reply) {
            tracing::warn!(%err, "no se pudo contestar draw");
        }
    }
}

impl CanvasView {
    fn ensure_panel(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.panel.is_some() || self.creating {
            return;
        }
        self.creating = true;
        let html = HTML
            .replace(r#"<script src="rough.js"></script>"#, &format!("<script>{ROUGH}</script>"))
            .replace(r#"<script src="dagre.js"></script>"#, &format!("<script>{DAGRE}</script>"));
        let done = |this: &mut Self, panel: Result<WebPanel, String>, cx: &mut Context<Self>| {
            this.creating = false;
            let failed = panel.is_err();
            this.panel = Some(panel);
            if failed {
                this.flush(cx); // contesta el error a quien esperaba
            }
            cx.notify();
        };
        WebPanel::create(html, None, Self::on_message, done, window, cx);
    }
}

impl Render for CanvasView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.ensure_panel(window, cx);
        div()
            .size_full()
            .bg(rgb(0x111214))
            .map(|d| match &self.panel {
                Some(Ok(panel)) => d.child(panel.element()),
                Some(Err(e)) => d.p_4().text_sm().text_color(style::bad()).child(e.clone()),
                None => d,
            })
    }
}

#[cfg(test)]
mod tests {
    use super::{slug, title_of};

    #[test]
    fn names_and_titles() {
        assert_eq!(slug("  Arquitectura de kuaky! "), "arquitectura-de-kuaky");
        assert_eq!(slug("../../etc"), "etc");
        assert_eq!(slug("ñ"), "");
        assert_eq!(title_of("view 3d\ntitle \"Login \\\"v2\\\"\"\nnode a"), Some("Login \"v2\"".into()));
        assert_eq!(title_of("node a"), None);
    }
}
