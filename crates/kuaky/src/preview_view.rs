//! Navegador: el proyecto corriendo (el servidor de desarrollo, por ejemplo) o cualquier página,
//! al lado del chat. Los agentes lo abren con la herramienta `preview` de `kuaky mcp` y reciben el
//! título de la página y sus errores de consola, sin capturas.

use crate::icons;
use crate::style;
use crate::webview::WebPanel;
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::component::{h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use kuaky_mcp::Reply;
use std::time::Duration;

/// Corre en cada página: junta sus errores y avisa a kuaky cuando terminó de cargar.
const WATCH: &str = r#"(() => {
  if (window.top !== window || location.protocol === "about:") return;
  const errors = [], push = s => errors.length < 20 && errors.push(String(s).slice(0, 300));
  addEventListener("error", e => push(e.message || "error loading " + (e.target?.src || e.target?.href || e.type)), true);
  addEventListener("unhandledrejection", e => push("Unhandled rejection: " + (e.reason?.message ?? e.reason)));
  const original = console.error;
  console.error = (...a) => { push(a.map(x => x?.message ?? String(x)).join(" ")); original.apply(console, a); };
  addEventListener("load", () => setTimeout(() => window.ipc?.postMessage(JSON.stringify({ title: document.title, url: location.href, errors })), 600));
})();"#;

const BLANK: &str = "<body style='margin:0;background:#111214'></body>";

/// Cuánto espera `preview` a que cargue la página antes de contestar.
const LOAD_TIMEOUT: Duration = Duration::from_secs(12);

/// Cargó otra dirección (el proyecto la recuerda para la próxima vez).
pub struct Navigated;

pub struct PreviewView {
    input: Entity<InputState>,
    /// Se crea al pintarse la primera vez (o al abrir una dirección): una pestaña restaurada no
    /// gasta memoria hasta que se ve.
    panel: Option<Result<WebPanel, String>>,
    creating: bool,
    /// Primera dirección, para cuando se cree el webview.
    start: Option<String>,
    title: String,
    pub url: String,
    /// Servidores locales que se ven en las terminales: botones mientras no hay página.
    suggestions: Vec<String>,
    /// Llamada a `preview` que espera a que cargue la página.
    waiting: Option<String>,
    _sub: Subscription,
}

impl EventEmitter<Navigated> for PreviewView {}

/// `localhost:5173` → `http://localhost:5173`.
pub fn normalize(url: &str) -> String {
    let url = url.trim().replace("0.0.0.0", "localhost");
    if url.contains("://") || url.starts_with("about:") { url } else { format!("http://{url}") }
}

impl PreviewView {
    pub fn new(url: Option<String>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let url = url.map(|u| normalize(&u));
        let input = cx.new(|cx| InputState::new(window, cx).placeholder("localhost:5173 or any URL").default_value(url.clone().unwrap_or_default()));
        let sub = cx.subscribe_in(&input, window, |this, input, ev: &InputEvent, _, cx| {
            if matches!(ev, InputEvent::PressEnter { .. }) {
                let url = input.read(cx).value().to_string();
                this.navigate(&url, cx);
            }
        });
        if url.is_none() {
            input.update(cx, |s, cx| s.focus(window, cx));
        }
        Self { input, panel: None, creating: false, start: url, title: String::new(), url: String::new(), suggestions: Vec::new(), waiting: None, _sub: sub }
    }

    /// Para la pestaña: `Browser · localhost:5173`.
    pub fn title(&self) -> SharedString {
        let host = self.url.split("://").nth(1).unwrap_or_default().split('/').next().unwrap_or_default();
        if host.is_empty() { "Browser".into() } else { format!("Browser · {host}").into() }
    }

    pub fn set_suggestions(&mut self, urls: Vec<String>) {
        self.suggestions = urls;
    }

    pub fn navigate(&mut self, url: &str, cx: &mut Context<Self>) {
        if url.trim().is_empty() {
            return;
        }
        let url = normalize(url);
        match &self.panel {
            Some(Ok(panel)) => {
                panel.load_url(&url).ok();
            }
            _ => self.start = Some(url),
        }
        cx.notify();
    }

    fn eval(&self, js: &str) {
        if let Some(Ok(panel)) = &self.panel {
            panel.eval(js).ok();
        }
    }

    /// Abre `url` (o recarga si no hay) y contesta a la llamada `call_id` cuando cargue.
    pub fn visit(&mut self, url: Option<&str>, call_id: String, window: &mut Window, cx: &mut Context<Self>) -> Option<Reply> {
        // Aunque la pestaña quede detrás, el agente espera la respuesta.
        self.ensure_panel(window, cx);
        if let Some(Err(e)) = &self.panel {
            return Some(Reply::error(e.clone()));
        }
        match url {
            Some(url) => {
                let url = normalize(url);
                self.input.update(cx, |s, cx| s.set_value(url.clone(), window, cx));
                self.navigate(&url, cx);
            }
            None => {
                if let Some(Ok(panel)) = &self.panel {
                    panel.reload();
                }
            }
        }
        self.waiting = Some(call_id.clone());
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(LOAD_TIMEOUT).await;
            this.update(cx, |this, _| {
                if this.waiting.as_deref() == Some(call_id.as_str()) {
                    this.waiting = None;
                    answer(&call_id, &Reply::error("The page did not finish loading in 12 s. Is the server running on that port?"));
                }
            })
            .ok();
        })
        .detach();
        None
    }

    fn on_message(&mut self, body: &str, cx: &mut Context<Self>) {
        let msg: serde_json::Value = serde_json::from_str(body).unwrap_or_default();
        let (Some(title), Some(url)) = (msg["title"].as_str(), msg["url"].as_str()) else { return };
        let moved = self.url != url;
        self.title = title.to_string();
        self.url = url.to_string();
        if moved {
            cx.emit(Navigated);
        }
        cx.notify();
        let Some(id) = self.waiting.take() else { return };
        let errors: Vec<&str> = msg["errors"].as_array().map(|a| a.iter().filter_map(|e| e.as_str()).collect()).unwrap_or_default();
        let head = format!("Loaded \"{title}\" ({url}).");
        let text = match errors.is_empty() {
            true => format!("{head} No console errors."),
            false => format!("{head} Console errors:\n- {}", errors.join("\n- ")),
        };
        answer(&id, &Reply::ok(text));
    }
}

fn answer(call_id: &str, reply: &Reply) {
    if let Some(mailbox) = crate::project::delegate::mailbox() {
        if let Err(err) = mailbox.answer(call_id, reply) {
            tracing::warn!(%err, "no se pudo contestar preview");
        }
    }
}

impl PreviewView {
    fn ensure_panel(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.panel.is_some() || self.creating {
            return;
        }
        self.creating = true;
        let done = |this: &mut Self, panel: Result<WebPanel, String>, cx: &mut Context<Self>| {
            this.creating = false;
            match &panel {
                // La dirección pudo cambiar mientras se creaba.
                Ok(p) => drop(this.start.take().map(|url| p.load_url(&url))),
                Err(e) => drop(this.waiting.take().map(|id| answer(&id, &Reply::error(e.clone())))),
            }
            this.panel = Some(panel);
            cx.notify();
        };
        WebPanel::create(BLANK.into(), Some(WATCH), Self::on_message, done, window, cx);
    }
}

impl Render for PreviewView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.ensure_panel(window, cx);
        let button = |id: &'static str, icon: &'static str, tip: &'static str| {
            div()
                .id(id)
                .p_1p5()
                .rounded_md()
                .cursor_pointer()
                .hover(|d| d.bg(style::panel_hover()))
                .tooltip(move |window, cx| Tooltip::new(tip).build(window, cx))
                .child(icons::ui(icon, px(14.), style::muted()))
        };
        let url = self.url.clone();
        let empty = url.is_empty();
        v_flex()
            .size_full()
            .bg(rgb(0x111214))
            .child(
                h_flex()
                    .px_2()
                    .py_1p5()
                    .gap_1()
                    .items_center()
                    .border_b_1()
                    .border_color(style::border())
                    .child(button("browser-back", "arrow-left", "Back").on_click(cx.listener(|this, _, _, _| this.eval("history.back()"))))
                    .child(button("browser-forward", "arrow-right", "Forward").on_click(cx.listener(|this, _, _, _| this.eval("history.forward()"))))
                    .child(button("browser-reload", "refresh-cw", "Reload").on_click(cx.listener(|this, _, _, cx| {
                        if let Some(Ok(panel)) = &this.panel {
                            panel.reload();
                        }
                        cx.notify();
                    })))
                    .child(div().flex_1().child(Input::new(&self.input)))
                    .when(!empty, |d| {
                        d.child(button("browser-open", "external-link", "Open in your browser").on_click(move |_, _, cx| cx.open_url(&url)))
                    }),
            )
            .when(empty && !self.suggestions.is_empty(), |d| {
                // Servidores que corren en las terminales del proyecto: un clic y se abren.
                d.child(
                    h_flex()
                        .px_3()
                        .py_2()
                        .gap_2()
                        .flex_wrap()
                        .items_center()
                        .border_b_1()
                        .border_color(style::border())
                        .child(div().text_xs().text_color(style::faint()).child("Running in your terminals:"))
                        .children(self.suggestions.iter().enumerate().map(|(i, s)| {
                            let open = s.clone();
                            div()
                                .id(("browser-suggest", i))
                                .px_2()
                                .py_0p5()
                                .rounded_md()
                                .border_1()
                                .border_color(style::border())
                                .text_xs()
                                .text_color(style::accent())
                                .cursor_pointer()
                                .hover(|d| d.bg(style::panel_hover()))
                                .child(s.clone())
                                .on_click(cx.listener(move |this, _, window, cx| {
                                    this.input.update(cx, |s, cx| s.set_value(open.clone(), window, cx));
                                    this.navigate(&open, cx);
                                }))
                        })),
                )
            })
            .child(div().flex_1().min_h_0().map(|d| match &self.panel {
                Some(Ok(panel)) => d.child(panel.element()),
                Some(Err(e)) => d.p_4().text_sm().text_color(style::bad()).child(e.clone()),
                None => d,
            }))
    }
}

#[cfg(test)]
mod tests {
    use super::normalize;

    #[test]
    fn urls() {
        assert_eq!(normalize("localhost:5173"), "http://localhost:5173");
        assert_eq!(normalize(" http://0.0.0.0:3000/app "), "http://localhost:3000/app");
        assert_eq!(normalize("https://example.com"), "https://example.com");
    }
}
