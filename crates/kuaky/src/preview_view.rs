//! Vista previa: el proyecto corriendo (el servidor de desarrollo, por ejemplo) en un webview, al
//! lado del chat. Los agentes la abren con la herramienta `preview` de `kuaky mcp` y reciben el
//! título de la página y sus errores de consola, sin capturas.

use crate::icons;
use crate::style;
use crate::webview::{Source, WebPanel};
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::component::{h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use kuaky_mcp::Reply;
use std::cell::RefCell;
use std::rc::Rc;
use std::time::Duration;

/// Corre en cada página: junta sus errores y avisa a kuaky cuando terminó de cargar.
const WATCH: &str = r#"(() => {
  if (window.top !== window) return;
  const errors = [], push = s => errors.length < 20 && errors.push(String(s).slice(0, 300));
  addEventListener("error", e => push(e.message || "error loading " + (e.target?.src || e.target?.href || e.type)), true);
  addEventListener("unhandledrejection", e => push("Unhandled rejection: " + (e.reason?.message ?? e.reason)));
  const original = console.error;
  console.error = (...a) => { push(a.map(x => x?.message ?? String(x)).join(" ")); original.apply(console, a); };
  addEventListener("load", () => setTimeout(() => window.ipc?.postMessage(JSON.stringify({ title: document.title, url: location.href, errors })), 600));
})();"#;

const BLANK: &str = "<body style='margin:0;background:#111214;color:#62646d;font:13px system-ui;display:flex;align-items:center;justify-content:center;height:100vh'>Type a URL above, e.g. localhost:5173</body>";

/// Cuánto espera `preview` a que cargue la página antes de contestar.
const LOAD_TIMEOUT: Duration = Duration::from_secs(12);

pub struct PreviewView {
    input: Entity<InputState>,
    panel: Result<WebPanel, String>,
    /// Título y dirección de la página cargada (los manda `WATCH`).
    page: Rc<RefCell<(String, String)>>,
    /// Llamada a `preview` que espera a que cargue la página.
    waiting: Rc<RefCell<Option<String>>>,
    _sub: Subscription,
}

/// `localhost:5173` → `http://localhost:5173`.
pub fn normalize(url: &str) -> String {
    let url = url.trim().replace("0.0.0.0", "localhost");
    if url.contains("://") || url.starts_with("about:") { url } else { format!("http://{url}") }
}

impl PreviewView {
    pub fn new(url: Option<String>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let input = cx.new(|cx| InputState::new(window, cx).placeholder("localhost:5173").default_value(url.clone().unwrap_or_default()));
        let sub = cx.subscribe_in(&input, window, |this, input, ev: &InputEvent, _, cx| {
            if matches!(ev, InputEvent::PressEnter { .. }) {
                let url = input.read(cx).value().to_string();
                this.navigate(&url, cx);
            }
        });
        let (page, waiting) = (Rc::new(RefCell::new((String::new(), String::new()))), Rc::new(RefCell::new(None::<String>)));
        let (on_page, on_waiting) = (page.clone(), waiting.clone());
        let source = match &url {
            Some(url) => Source::Url(normalize(url)),
            None => Source::Html(BLANK.into()),
        };
        let panel = WebPanel::new(window, source, Some(WATCH), move |body| {
            let msg: serde_json::Value = serde_json::from_str(body).unwrap_or_default();
            let (Some(title), Some(url)) = (msg["title"].as_str(), msg["url"].as_str()) else { return };
            *on_page.borrow_mut() = (title.to_string(), url.to_string());
            let Some(id) = on_waiting.borrow_mut().take() else { return };
            let errors: Vec<&str> = msg["errors"].as_array().map(|a| a.iter().filter_map(|e| e.as_str()).collect()).unwrap_or_default();
            let head = format!("Loaded \"{title}\" ({url}).");
            let text = match errors.is_empty() {
                true => format!("{head} No console errors."),
                false => format!("{head} Console errors:\n- {}", errors.join("\n- ")),
            };
            answer(&id, &Reply::ok(text));
        });
        if url.is_none() {
            input.update(cx, |s, cx| s.focus(window, cx));
        }
        Self { input, panel, page, waiting, _sub: sub }
    }

    /// Para la pestaña: `Preview · localhost:5173`.
    pub fn title(&self) -> SharedString {
        let url = self.page.borrow().1.clone();
        let host = url.split("://").nth(1).unwrap_or(&url).split('/').next().unwrap_or_default().to_string();
        if host.is_empty() { "Preview".into() } else { format!("Preview · {host}").into() }
    }

    pub fn navigate(&mut self, url: &str, cx: &mut Context<Self>) {
        if url.trim().is_empty() {
            return;
        }
        if let Ok(panel) = &self.panel {
            panel.load_url(&normalize(url)).ok();
        }
        cx.notify();
    }

    /// Abre `url` (o recarga si no hay) y contesta a la llamada `call_id` cuando cargue.
    pub fn visit(&mut self, url: Option<&str>, call_id: String, window: &mut Window, cx: &mut Context<Self>) -> Option<Reply> {
        let panel = match &self.panel {
            Ok(p) => p.clone(),
            Err(e) => return Some(Reply::error(e.clone())),
        };
        match url {
            Some(url) => {
                let url = normalize(url);
                self.input.update(cx, |s, cx| s.set_value(url.clone(), window, cx));
                if let Err(e) = panel.load_url(&url) {
                    return Some(Reply::error(format!("Could not open {url}: {e}")));
                }
            }
            None => panel.reload(),
        }
        *self.waiting.borrow_mut() = Some(call_id.clone());
        let waiting = self.waiting.clone();
        cx.spawn(async move |_, cx| {
            cx.background_executor().timer(LOAD_TIMEOUT).await;
            if waiting.borrow().as_deref() == Some(call_id.as_str()) {
                waiting.borrow_mut().take();
                answer(&call_id, &Reply::error("The page did not finish loading in 12 s. Is the server running on that port?"));
            }
        })
        .detach();
        None
    }
}

fn answer(call_id: &str, reply: &Reply) {
    if let Some(mailbox) = crate::project::delegate::mailbox() {
        if let Err(err) = mailbox.answer(call_id, reply) {
            tracing::warn!(%err, "no se pudo contestar preview");
        }
    }
}

impl Render for PreviewView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
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
        let url = self.page.borrow().1.clone();
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
                    .child(button("preview-reload", "refresh-cw", "Reload").on_click(cx.listener(|this, _, _, cx| {
                        if let Ok(panel) = &this.panel {
                            panel.reload();
                        }
                        cx.notify();
                    })))
                    .child(div().flex_1().child(Input::new(&self.input)))
                    .when(!url.is_empty(), |d| {
                        d.child(button("preview-open", "external-link", "Open in your browser").on_click(move |_, _, cx| cx.open_url(&url)))
                    }),
            )
            .child(div().flex_1().min_h_0().map(|d| match &self.panel {
                Ok(panel) => d.child(panel.element()),
                Err(e) => d.p_4().text_sm().text_color(style::bad()).child(e.clone()),
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
