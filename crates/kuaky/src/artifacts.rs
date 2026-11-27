//! Artefactos del proyecto: los diagramas que dibujan los agentes (lienzo) y el navegador con el
//! proyecto corriendo. El panel **Artifacts** los lista (primero los de la conversación que se
//! ve) para abrirlos o borrarlos, y aquí están las herramientas `draw`, `artifacts` y `preview`
//! de `kuaky mcp`.

use super::*;
use crate::canvas_view::{self, Artifact, CanvasChanged, CanvasView};
use crate::preview_view::{Navigated, PreviewView};
use kuaky_mcp::{Call, Reply};
use serde_json::Value;

fn arg<'a>(args: &'a Value, key: &str) -> Option<&'a str> {
    args.get(key).and_then(Value::as_str).map(str::trim).filter(|s| !s.is_empty())
}

impl ProjectView {
    /// La pestaña del lienzo, al frente; la abre si no está.
    pub(super) fn canvas_tab(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Entity<CanvasView> {
        if let Some(i) = self.tabs.iter().position(|t| matches!(t.kind, TabKind::Canvas(_))) {
            self.activate(i, window, cx);
            let TabKind::Canvas(view) = &self.tabs[i].kind else { unreachable!() };
            return view.clone();
        }
        let view = cx.new(|_| CanvasView::new(self.root.clone()));
        self._subs.push(cx.subscribe(&view, |this, _, _: &CanvasChanged, cx| this.persist(cx)));
        self.push_tab(TabKind::Canvas(view.clone()), window, cx);
        view
    }

    /// La pestaña del navegador, al frente; la abre si no está.
    pub(super) fn browser_tab(&mut self, url: Option<String>, window: &mut Window, cx: &mut Context<Self>) -> Entity<PreviewView> {
        self.refresh_local_urls(cx);
        let urls = self.local_urls.clone();
        if let Some(i) = self.tabs.iter().position(|t| matches!(t.kind, TabKind::Preview(_))) {
            self.activate(i, window, cx);
            let TabKind::Preview(view) = &self.tabs[i].kind else { unreachable!() };
            let view = view.clone();
            view.update(cx, |v, cx| {
                v.set_suggestions(urls);
                if let Some(url) = url {
                    v.navigate(&url, cx);
                }
            });
            return view;
        }
        let view = cx.new(|cx| PreviewView::new(url, window, cx));
        view.update(cx, |v, _| v.set_suggestions(urls));
        self._subs.push(cx.subscribe(&view, |this, _, _: &Navigated, cx| this.persist(cx)));
        self.push_tab(TabKind::Preview(view.clone()), window, cx);
        view
    }

    pub(super) fn open_browser(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.browser_tab(None, window, cx);
        cx.notify();
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

    /// Conversación (uid) que se ve a un lado.
    fn shown_uid(&self, cx: &App) -> Option<String> {
        let tab = self.tabs.iter().find(|t| Some(t.id) == self.shown_session)?;
        match &tab.kind {
            TabKind::Chat { view, .. } => Some(view.read(cx).uid.clone()),
            TabKind::Terminal { view, .. } => Some(view.read(cx).uid.clone()),
            _ => None,
        }
    }

    /// Nombre de una conversación, abierta o no.
    fn session_name(&self, uid: &str, cx: &App) -> Option<String> {
        for tab in &self.tabs {
            match &tab.kind {
                TabKind::Chat { view, .. } if view.read(cx).uid == uid => {
                    let v = view.read(cx);
                    return v.title.clone().or_else(|| v.first_prompt().map(str::to_string));
                }
                TabKind::Terminal { view, .. } if view.read(cx).uid == uid => return Some(view.read(cx).auto_title().to_string()),
                _ => {}
            }
        }
        self.lines.session(uid).map(|s| s.label.clone()).filter(|l| !l.is_empty()).or_else(|| self.lines.line_of(uid).map(|l| l.name.clone()))
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
        let heading = |text: &'static str| div().px_2().pt_3().pb_1().text_xs().text_color(style::faint()).child(text);
        let shown = self.tabs.iter().find_map(|t| match &t.kind {
            TabKind::Canvas(v) => v.read(cx).current(),
            _ => None,
        });
        let mine = self.shown_uid(cx);
        // ponytail: lee la carpeta en cada render del panel; son pocos archivos.
        let (this_chat, others): (Vec<Artifact>, Vec<Artifact>) =
            canvas_view::list(&self.root).into_iter().partition(|a| mine.as_ref().is_some_and(|m| a.sessions.contains(m)));
        let diagram = |a: Artifact, with_session: bool, cx: &mut Context<Self>| {
            let (open, del) = (a.name.clone(), a.name.clone());
            let deleting = self.deleting.as_deref() == Some(a.name.as_str());
            let age = a.modified.duration_since(std::time::UNIX_EPOCH).map(|d| ago_unix(d.as_secs() as i64)).unwrap_or_default();
            let from = with_session.then(|| a.sessions.last().and_then(|uid| self.session_name(uid, cx))).flatten();
            let detail = match from {
                Some(name) => format!("{} · {age}", crate::lines::short(&name, 32)),
                None => age,
            };
            row(format!("artifact-{}", a.name).into(), "shapes", if shown.as_deref() == Some(a.name.as_str()) { style::accent() } else { style::muted() })
                .group("artifact")
                .child(
                    v_flex()
                        .flex_1()
                        .min_w_0()
                        .child(div().truncate().text_sm().child(a.title.clone()))
                        .child(div().truncate().text_xs().text_color(style::faint()).child(detail)),
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
        };
        let empty = this_chat.is_empty() && others.is_empty();
        let has_mine = !this_chat.is_empty();
        let this_chat: Vec<_> = this_chat.into_iter().map(|a| diagram(a, false, cx)).collect();
        let others: Vec<_> = others.into_iter().map(|a| diagram(a, true, cx)).collect();
        let browsers: Vec<_> = self
            .local_urls
            .iter()
            .map(|url| {
                let open = url.clone();
                row(format!("browser-{url}").into(), "globe", style::accent())
                    .child(div().flex_1().min_w_0().truncate().text_sm().child(url.clone()))
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.browser_tab(Some(open.clone()), window, cx);
                        cx.notify();
                    }))
            })
            .collect();
        div().id("artifacts").size_full().overflow_y_scroll().child(
            v_flex()
                .p_2()
                .gap_0p5()
                .when(has_mine, |d| d.child(heading("THIS CONVERSATION")).children(this_chat))
                .child(heading(if has_mine { "OTHER CONVERSATIONS" } else { "DIAGRAMS" }))
                .when(empty, |d| d.child(panel_message("None yet. Ask an agent to draw one (it uses the draw tool).")))
                .children(others)
                .child(
                    h_flex()
                        .justify_between()
                        .items_center()
                        .child(heading("BROWSER"))
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
                .children(browsers)
                .child(
                    row("browser-new".into(), "app-window", style::muted())
                        .child(div().text_sm().text_color(style::muted()).child("Open the browser…"))
                        .on_click(cx.listener(|this, _, window, cx| this.open_browser(window, cx))),
                ),
        )
    }

    // ── herramientas de los agentes ─────────────────────────────────────

    /// `draw`: dibuja en el artefacto `artifact` (lo crea si no existe), lo liga a la conversación
    /// que llama y lo muestra.
    pub(super) fn tool_draw(&mut self, call: &Call, window: &mut Window, cx: &mut Context<Self>) -> Option<Reply> {
        let Some(name) = arg(&call.args, "artifact").map(canvas_view::slug).filter(|n| !n.is_empty()) else {
            return Some(Reply::error("Missing `artifact`: a short name like `architecture` (a new name creates a diagram)."));
        };
        let Some(text) = call.args.get("text").and_then(Value::as_str) else {
            return Some(Reply::error("Missing `text`."));
        };
        canvas_view::link(&self.root, &name, &call.session);
        let view = self.canvas_tab(window, cx);
        cx.notify();
        view.update(cx, |v, cx| v.draw(name, call.id.clone(), text.to_string(), window, cx))
    }

    /// `artifacts`: la lista (primero los de quien pregunta), o lo que tiene uno.
    pub(super) fn tool_artifacts(&self, call: &Call) -> Result<String, String> {
        if let Some(name) = arg(&call.args, "name") {
            let name = canvas_view::slug(name);
            return canvas_view::read(&self.root, &name).ok_or_else(|| format!("No diagram named `{name}`."));
        }
        let (mine, others): (Vec<Artifact>, Vec<Artifact>) =
            canvas_view::list(&self.root).into_iter().partition(|a| a.sessions.contains(&call.session));
        if mine.is_empty() && others.is_empty() {
            return Ok("No diagrams yet.".into());
        }
        let line = |a: &Artifact| format!("{} — {}", a.name, a.title);
        let mut out = Vec::new();
        if !mine.is_empty() {
            out.push(format!("Yours:\n{}", mine.iter().map(line).collect::<Vec<_>>().join("\n")));
        }
        if !others.is_empty() {
            out.push(format!("From other conversations:\n{}", others.iter().map(line).collect::<Vec<_>>().join("\n")));
        }
        Ok(out.join("\n\n"))
    }

    /// `preview`: abre la dirección (o recarga) y contesta cuando cargue.
    pub(super) fn tool_preview(&mut self, call: &Call, window: &mut Window, cx: &mut Context<Self>) -> Option<Reply> {
        let url = arg(&call.args, "url").map(str::to_string);
        if url.is_none() && !self.tabs.iter().any(|t| matches!(t.kind, TabKind::Preview(_))) {
            return Some(Reply::error("Give a `url` (e.g. http://localhost:5173): there is no browser open yet."));
        }
        let view = self.browser_tab(None, window, cx);
        cx.notify();
        view.update(cx, |v, cx| v.visit(url.as_deref(), call.id.clone(), window, cx))
    }
}
