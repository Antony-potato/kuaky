//! Artefactos del proyecto: los diagramas que dibujan los agentes (lienzo) y la vista previa del
//! proyecto corriendo. El panel **Artifacts** los lista para abrirlos o borrarlos, y aquí están
//! las herramientas `draw`, `artifacts` y `preview` de `kuaky mcp`.

use super::*;
use crate::canvas_view::{self, CanvasView};
use crate::preview_view::PreviewView;
use kuaky_mcp::{Call, Reply};
use serde_json::Value;

fn arg<'a>(args: &'a Value, key: &str) -> Option<&'a str> {
    args.get(key).and_then(Value::as_str).map(str::trim).filter(|s| !s.is_empty())
}

impl ProjectView {
    /// La pestaña del lienzo, al frente; la abre si no está.
    fn canvas_tab(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Entity<CanvasView> {
        if let Some(i) = self.tabs.iter().position(|t| matches!(t.kind, TabKind::Canvas(_))) {
            self.activate(i, window, cx);
            let TabKind::Canvas(view) = &self.tabs[i].kind else { unreachable!() };
            return view.clone();
        }
        let root = self.root.clone();
        let view = cx.new(|_| CanvasView::new(root, window));
        self.push_tab(TabKind::Canvas(view.clone()), window, cx);
        view
    }

    fn preview_tab(&mut self, url: Option<String>, window: &mut Window, cx: &mut Context<Self>) -> Entity<PreviewView> {
        if let Some(i) = self.tabs.iter().position(|t| matches!(t.kind, TabKind::Preview(_))) {
            self.activate(i, window, cx);
            let TabKind::Preview(view) = &self.tabs[i].kind else { unreachable!() };
            let view = view.clone();
            if let Some(url) = url {
                view.update(cx, |v, cx| v.navigate(&url, cx));
            }
            return view;
        }
        let view = cx.new(|cx| PreviewView::new(url, window, cx));
        self.push_tab(TabKind::Preview(view.clone()), window, cx);
        view
    }

    pub(super) fn open_artifact(&mut self, name: String, window: &mut Window, cx: &mut Context<Self>) {
        let view = self.canvas_tab(window, cx);
        view.update(cx, |v, cx| v.open(name, cx));
        cx.notify();
    }

    fn delete_artifact(&mut self, name: &str, cx: &mut Context<Self>) {
        canvas_view::delete(&self.root, name);
        for tab in &self.tabs {
            if let TabKind::Canvas(view) = &tab.kind {
                view.update(cx, |v, cx| v.forget(name, cx));
            }
        }
        self.deleting = None;
        cx.notify();
    }

    /// Direcciones locales que muestran las terminales del proyecto (servidores de desarrollo).
    pub(super) fn refresh_local_urls(&mut self, cx: &App) {
        let mut urls = Vec::new();
        for tab in &self.tabs {
            if let TabKind::Terminal { view, .. } = &tab.kind {
                for url in view.read(cx).local_urls() {
                    if !urls.contains(&url) {
                        urls.push(url);
                    }
                }
            }
        }
        self.local_urls = urls;
    }

    pub(super) fn artifacts_panel(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let row = |id: SharedString, icon: &'static str, color: Rgba| {
            h_flex()
                .id(id)
                .px_2()
                .py_1p5()
                .gap_2()
                .items_center()
                .rounded_md()
                .cursor_pointer()
                .hover(|d| d.bg(style::panel_hover()))
                .child(icons::ui(icon, px(14.), color))
        };
        let heading = |text: &'static str| div().px_2().pt_2().pb_1().text_xs().text_color(style::faint()).child(text);
        let shown = self.tabs.iter().find_map(|t| match &t.kind {
            TabKind::Canvas(v) => v.read(cx).current(),
            _ => None,
        });
        let artifacts = canvas_view::list(&self.root); // ponytail: lee la carpeta en cada render; son pocos archivos.
        let previews = self.local_urls.iter().map(|url| {
            let open = url.clone();
            row(format!("preview-{url}").into(), "globe", style::accent())
                .child(div().flex_1().min_w_0().truncate().text_sm().child(url.clone()))
                .on_click(cx.listener(move |this, _, window, cx| {
                    this.preview_tab(Some(open.clone()), window, cx);
                    cx.notify();
                }))
        });
        let diagrams = artifacts.into_iter().map(|a| {
            let (open, del) = (a.name.clone(), a.name.clone());
            let deleting = self.deleting.as_deref() == Some(a.name.as_str());
            let age = a.modified.duration_since(std::time::UNIX_EPOCH).map(|d| ago_unix(d.as_secs() as i64)).unwrap_or_default();
            row(format!("artifact-{}", a.name).into(), "shapes", if shown.as_deref() == Some(a.name.as_str()) { style::accent() } else { style::muted() })
                .group("artifact")
                .child(
                    v_flex()
                        .flex_1()
                        .min_w_0()
                        .child(div().truncate().text_sm().child(a.title.clone()))
                        .child(div().truncate().text_xs().text_color(style::faint()).child(format!("{} · {age}", a.name))),
                )
                .child(
                    div()
                        .id(SharedString::from(format!("artifact-del-{}", a.name)))
                        .px_1()
                        .py_0p5()
                        .rounded_sm()
                        .text_xs()
                        .text_color(style::bad())
                        .when(!deleting, |d| d.invisible().group_hover("artifact", |d| d.visible()))
                        .hover(|d| d.bg(style::panel_hover()))
                        .tooltip(|window, cx| Tooltip::new("Delete").build(window, cx))
                        .map(|d| if deleting { d.child("Delete?") } else { d.child(icons::ui("trash", px(12.), style::muted())) })
                        .on_click(cx.listener(move |this, _, _, cx| {
                            cx.stop_propagation();
                            // Dos clics: el primero pregunta.
                            if this.deleting.as_deref() == Some(del.as_str()) {
                                this.delete_artifact(&del, cx);
                            } else {
                                this.deleting = Some(del.clone());
                                cx.notify();
                            }
                        })),
                )
                .on_click(cx.listener(move |this, _, window, cx| this.open_artifact(open.clone(), window, cx)))
        });
        let diagrams: Vec<_> = diagrams.collect();
        div().id("artifacts").size_full().overflow_y_scroll().child(
            v_flex()
                .p_2()
                .gap_0p5()
                .child(
                    h_flex()
                        .justify_between()
                        .items_center()
                        .child(heading("PREVIEW"))
                        .child(
                            div()
                                .id("artifacts-refresh")
                                .p_1()
                                .rounded_sm()
                                .cursor_pointer()
                                .hover(|d| d.bg(style::panel_hover()))
                                .tooltip(|window, cx| Tooltip::new("Look for local servers in the terminals").build(window, cx))
                                .child(icons::ui("refresh-cw", px(12.), style::muted()))
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.refresh_local_urls(cx);
                                    cx.notify();
                                })),
                        ),
                )
                .children(previews)
                .child(
                    row("preview-new".into(), "app-window", style::muted())
                        .child(div().text_sm().text_color(style::muted()).child("Open a URL…"))
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.preview_tab(None, window, cx);
                            cx.notify();
                        })),
                )
                .child(heading("DIAGRAMS"))
                .when(diagrams.is_empty(), |d| {
                    d.child(panel_message("None yet. Ask an agent to draw one (it uses the draw tool)."))
                })
                .children(diagrams),
        )
    }

    // ── herramientas de los agentes ─────────────────────────────────────

    /// `draw`: dibuja en el artefacto `artifact` (lo crea si no existe) y lo muestra.
    pub(super) fn tool_draw(&mut self, call: &Call, window: &mut Window, cx: &mut Context<Self>) -> Option<Reply> {
        let Some(name) = arg(&call.args, "artifact").map(canvas_view::slug).filter(|n| !n.is_empty()) else {
            return Some(Reply::error("Missing `artifact`: a short name like `architecture` (a new name creates a diagram)."));
        };
        let Some(text) = call.args.get("text").and_then(Value::as_str) else {
            return Some(Reply::error("Missing `text`."));
        };
        let view = self.canvas_tab(window, cx);
        cx.notify();
        view.update(cx, |v, cx| v.draw(name, call.id.clone(), text.to_string(), cx))
    }

    /// `artifacts`: la lista, o lo que tiene uno (para editarlo con `draw`).
    pub(super) fn tool_artifacts(&self, args: &Value) -> Result<String, String> {
        if let Some(name) = arg(args, "name") {
            let name = canvas_view::slug(name);
            return canvas_view::read(&self.root, &name).ok_or_else(|| format!("No diagram named `{name}`."));
        }
        let all = canvas_view::list(&self.root);
        if all.is_empty() {
            return Ok("No diagrams yet.".into());
        }
        Ok(all.iter().map(|a| format!("{} — {}", a.name, a.title)).collect::<Vec<_>>().join("\n"))
    }

    /// `preview`: abre la dirección (o recarga) y contesta cuando cargue.
    pub(super) fn tool_preview(&mut self, call: &Call, window: &mut Window, cx: &mut Context<Self>) -> Option<Reply> {
        let url = arg(&call.args, "url").map(str::to_string);
        if url.is_none() && !self.tabs.iter().any(|t| matches!(t.kind, TabKind::Preview(_))) {
            return Some(Reply::error("Give a `url` (e.g. http://localhost:5173): there is no preview open yet."));
        }
        let view = self.preview_tab(None, window, cx);
        cx.notify();
        view.update(cx, |v, cx| v.visit(url.as_deref(), call.id.clone(), window, cx))
    }
}
