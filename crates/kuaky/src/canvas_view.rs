//! Lienzo de diagramas de los agentes: `assets/canvas/canvas.html` en un webview.
//! Los agentes dibujan con la herramienta `draw` de `kuaky mcp`: el texto va tal cual al lienzo
//! (`kuaky.apply`) y su respuesta ("ok" o los errores) vuelve al agente.
//!
//! Cada diagrama es un artefacto con nombre, guardado por proyecto (ver `dir`): el lienzo muestra
//! uno a la vez y se puede cerrar y volver a abrir desde el panel Artifacts.

use crate::style;
use crate::webview::{Source, WebPanel};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use kuaky_mcp::Reply;
use std::cell::{Cell, RefCell};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::{Duration, SystemTime};

const HTML: &str = include_str!("../assets/canvas/canvas.html");
const ROUGH: &str = include_str!("../assets/canvas/rough.js");
const DAGRE: &str = include_str!("../assets/canvas/dagre.js");

pub struct Artifact {
    pub name: String,
    pub title: String,
    pub modified: SystemTime,
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

fn file(root: &Path, name: &str) -> PathBuf {
    dir(root).join(format!("{name}.txt"))
}

pub fn read(root: &Path, name: &str) -> Option<String> {
    std::fs::read_to_string(file(root, name)).ok()
}

pub fn delete(root: &Path, name: &str) {
    std::fs::remove_file(file(root, name)).ok();
}

/// Los más recientes primero.
pub fn list(root: &Path) -> Vec<Artifact> {
    let Ok(entries) = std::fs::read_dir(dir(root)) else { return Vec::new() };
    let mut out: Vec<Artifact> = entries
        .flatten()
        .filter_map(|e| {
            let path = e.path();
            let name = path.file_stem()?.to_str()?.to_string();
            (path.extension()? == "txt").then_some(())?;
            let src = std::fs::read_to_string(&path).ok()?;
            let modified = e.metadata().and_then(|m| m.modified()).unwrap_or(SystemTime::UNIX_EPOCH);
            Some(Artifact { title: title_of(&src).unwrap_or_else(|| name.clone()), name, modified })
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

pub struct CanvasView {
    root: PathBuf,
    panel: Result<WebPanel, String>,
    ready: Rc<Cell<bool>>,
    /// Artefacto que se ve; ahí se guarda lo que manda la página.
    current: Rc<RefCell<Option<String>>>,
    title: Rc<RefCell<String>>,
    /// Lo que llega antes de que cargue la página.
    queue: Vec<Job>,
}

impl CanvasView {
    pub fn new(root: PathBuf, window: &mut Window) -> Self {
        let html = HTML
            .replace(r#"<script src="rough.js"></script>"#, &format!("<script>{ROUGH}</script>"))
            .replace(r#"<script src="dagre.js"></script>"#, &format!("<script>{DAGRE}</script>"));
        let (ready, current, title) = (Rc::new(Cell::new(false)), Rc::new(RefCell::new(None::<String>)), Rc::new(RefCell::new(String::new())));
        let (on_ready, on_current, on_title, on_root) = (ready.clone(), current.clone(), title.clone(), root.clone());
        let panel = WebPanel::new(window, Source::Html(html), None, move |body| {
            let msg: serde_json::Value = serde_json::from_str(body).unwrap_or_default();
            if msg["ready"] == true {
                on_ready.set(true);
            }
            if let (Some(src), Some(name)) = (msg["src"].as_str(), on_current.borrow().as_ref()) {
                let path = file(&on_root, name);
                if let Err(err) = std::fs::create_dir_all(dir(&on_root)).and_then(|_| std::fs::write(&path, src)) {
                    tracing::warn!(%err, path = %path.display(), "no se pudo guardar el artefacto");
                }
            }
            if let Some(t) = msg["title"].as_str() {
                *on_title.borrow_mut() = t.to_string();
            }
            if let (Some(id), Some(out)) = (msg["id"].as_str(), msg["out"].as_str()) {
                answer(id, &if out == "ok" { Reply::ok(out) } else { Reply::error(out) });
            }
        });
        Self { root, panel, ready, current, title, queue: Vec::new() }
    }

    pub fn current(&self) -> Option<String> {
        self.current.borrow().clone()
    }

    /// Para la pestaña: el título del diagrama, o su nombre.
    pub fn title(&self) -> SharedString {
        let t = self.title.borrow();
        match (t.is_empty(), self.current.borrow().as_ref()) {
            (false, _) => t.clone().into(),
            (true, Some(name)) => name.clone().into(),
            (true, None) => "Canvas".into(),
        }
    }

    pub fn open(&mut self, name: String, cx: &mut Context<Self>) {
        self.push(Job::Open(name), cx);
        cx.notify();
    }

    /// Manda órdenes al artefacto `name` (lo crea si no existe). `None`: la respuesta llega después,
    /// por el buzón.
    pub fn draw(&mut self, name: String, call_id: String, text: String, cx: &mut Context<Self>) -> Option<Reply> {
        if let Err(e) = &self.panel {
            return Some(Reply::error(e.clone()));
        }
        if let Some(t) = title_of(&text) {
            *self.title.borrow_mut() = t;
        }
        self.push(Job::Draw { name, id: call_id, text }, cx)
    }

    /// Se borró `name`: si es el que se ve, el lienzo queda vacío.
    pub fn forget(&mut self, name: &str, cx: &mut Context<Self>) {
        if self.current.borrow().as_deref() == Some(name) {
            *self.current.borrow_mut() = None;
            self.title.borrow_mut().clear();
            if let Ok(panel) = &self.panel {
                panel.eval("kuaky.load('')").ok();
            }
            cx.notify();
        }
    }

    fn push(&mut self, job: Job, cx: &mut Context<Self>) -> Option<Reply> {
        if self.ready.get() {
            return self.run(job);
        }
        self.queue.push(job);
        if self.queue.len() == 1 {
            // ponytail: espera la carga revisando cada 50 ms; solo pasa al abrir el lienzo (< 1 s).
            let ready = self.ready.clone();
            cx.spawn(async move |this, cx| {
                for _ in 0..200 {
                    cx.background_executor().timer(Duration::from_millis(50)).await;
                    if ready.get() {
                        break;
                    }
                }
                this.update(cx, |this, cx| {
                    for job in std::mem::take(&mut this.queue) {
                        let id = match &job {
                            Job::Draw { id, .. } => Some(id.clone()),
                            Job::Open(_) => None,
                        };
                        if let (Some(reply), Some(id)) = (this.run(job), id) {
                            answer(&id, &reply);
                        }
                    }
                    cx.notify();
                })
                .ok();
            })
            .detach();
        }
        None
    }

    fn run(&mut self, job: Job) -> Option<Reply> {
        let Ok(panel) = &self.panel else { return None };
        let name = match &job {
            Job::Open(name) | Job::Draw { name, .. } => name.clone(),
        };
        if self.current.borrow().as_deref() != Some(name.as_str()) {
            let src = read(&self.root, &name).unwrap_or_default();
            *self.title.borrow_mut() = title_of(&src).unwrap_or_default();
            *self.current.borrow_mut() = Some(name);
            if let Err(e) = panel.eval(&format!("kuaky.load({})", serde_json::json!(src))) {
                return Some(Reply::error(format!("The canvas did not open it: {e}")));
            }
        }
        let Job::Draw { id, text, .. } = job else { return None };
        let js = format!("kuaky.apply({}, {})", serde_json::json!(text), serde_json::json!(id));
        panel.eval(&js).err().map(|e| Reply::error(format!("The canvas did not take it: {e}")))
    }
}

fn answer(call_id: &str, reply: &Reply) {
    if let Some(mailbox) = crate::project::delegate::mailbox() {
        if let Err(err) = mailbox.answer(call_id, reply) {
            tracing::warn!(%err, "no se pudo contestar draw");
        }
    }
}

impl Render for CanvasView {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
            .size_full()
            .bg(rgb(0x111214))
            .map(|d| match &self.panel {
                Ok(panel) => d.child(panel.element()),
                Err(e) => d.p_4().text_sm().text_color(style::bad()).child(e.clone()),
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
