//! Lienzo de diagramas de los agentes: un webview (wry) con `assets/canvas/canvas.html`.
//! Los agentes dibujan con la herramienta `draw` de `kuaky mcp`: el texto va tal cual al
//! lienzo (`kuaky.apply`) y su respuesta ("ok" o los errores) vuelve al agente.
//!
//! El webview es una ventana nativa encima de GPUI: solo se muestra en los cuadros en que su
//! pestaña se pintó y nada la tapa (ver `end_frame`).

use crate::style;
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use kuaky_mcp::Reply;
use std::cell::{Cell, RefCell};
use std::rc::{Rc, Weak};
use std::time::Duration;

const HTML: &str = include_str!("../assets/canvas/canvas.html");
const ROUGH: &str = include_str!("../assets/canvas/rough.js");

struct Shared {
    web: web::Web,
    ready: Rc<Cell<bool>>,
    painted: Cell<bool>,
    visible: Cell<bool>,
    bounds: Cell<Option<Bounds<Pixels>>>,
}

thread_local! {
    static LIVE: RefCell<Vec<Weak<Shared>>> = RefCell::default();
    static COVERED: Cell<bool> = const { Cell::new(false) };
}

/// Un modal o menú flotante se dibuja en este cuadro: los lienzos se esconden para no taparlo.
pub(crate) fn cover() {
    COVERED.set(true);
}

/// Al final de cada cuadro (último hijo de la ventana): muestra solo los lienzos que se pintaron.
pub(crate) fn end_frame() {
    let covered = COVERED.replace(false);
    LIVE.with_borrow_mut(|live| {
        live.retain(|weak| {
            let Some(s) = weak.upgrade() else { return false };
            let show = s.painted.replace(false) && !covered;
            if s.visible.replace(show) != show {
                s.web.show(show);
            }
            true
        })
    });
}

pub struct CanvasView {
    shared: Result<Rc<Shared>, String>,
    /// Llamadas a `draw` que esperan a que cargue la página: (id de la llamada, texto).
    queue: Vec<(String, String)>,
}

impl CanvasView {
    pub fn new(window: &mut Window) -> Self {
        let html = HTML.replace(r#"<script src="rough.js"></script>"#, &format!("<script>{ROUGH}</script>"));
        let ready = Rc::new(Cell::new(false));
        let on_ready = ready.clone();
        let shared = web::build(window, html, move |body| {
            let msg: serde_json::Value = serde_json::from_str(body).unwrap_or_default();
            if msg["ready"] == true {
                on_ready.set(true);
            } else if let (Some(id), Some(out)) = (msg["id"].as_str(), msg["out"].as_str()) {
                answer(id, &if out == "ok" { Reply::ok(out) } else { Reply::error(out) });
            }
        })
        .map(|web| {
            let s = Rc::new(Shared { web, ready, painted: Cell::new(false), visible: Cell::new(false), bounds: Cell::new(None) });
            LIVE.with_borrow_mut(|live| live.push(Rc::downgrade(&s)));
            s
        });
        Self { shared, queue: Vec::new() }
    }

    /// Manda órdenes al lienzo. `None`: la respuesta llega después, por el buzón.
    pub fn draw(&mut self, call_id: String, text: String, cx: &mut Context<Self>) -> Option<Reply> {
        let shared = match &self.shared {
            Ok(s) => s.clone(),
            Err(e) => return Some(Reply::error(e.clone())),
        };
        if shared.ready.get() {
            return run(&shared, &call_id, &text);
        }
        self.queue.push((call_id, text));
        if self.queue.len() == 1 {
            // ponytail: espera la carga revisando cada 50 ms; solo pasa al abrir el lienzo (< 1 s).
            cx.spawn(async move |this, cx| {
                for _ in 0..200 {
                    cx.background_executor().timer(Duration::from_millis(50)).await;
                    if shared.ready.get() {
                        break;
                    }
                }
                this.update(cx, |this, _| {
                    for (id, text) in std::mem::take(&mut this.queue) {
                        if let Some(reply) = run(&shared, &id, &text) {
                            answer(&id, &reply);
                        }
                    }
                })
                .ok();
            })
            .detach();
        }
        None
    }
}

fn run(shared: &Shared, call_id: &str, text: &str) -> Option<Reply> {
    let js = format!("kuaky.apply({}, {})", serde_json::json!(text), serde_json::json!(call_id));
    shared.web.eval(&js).err().map(|e| Reply::error(format!("The canvas did not take it: {e}")))
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
        let shared = self.shared.as_ref().ok().cloned();
        div()
            .size_full()
            .bg(rgb(0x111214))
            .when_some(self.shared.as_ref().err(), |d, e| d.p_4().text_sm().text_color(style::bad()).child(e.clone()))
            .child(
                canvas(
                    |_, _, _| {},
                    move |bounds, _, _, _| {
                        let Some(s) = &shared else { return };
                        s.painted.set(true);
                        if s.bounds.replace(Some(bounds)) != Some(bounds) {
                            s.web.place(bounds);
                        }
                    },
                )
                .size_full(),
            )
    }
}

#[cfg(any(windows, target_os = "macos"))]
mod web {
    use gpui_kit::{Bounds, Pixels, Window};
    use wry::dpi::{LogicalPosition, LogicalSize};

    pub struct Web {
        view: wry::WebView,
        _context: wry::WebContext,
    }

    pub fn build(window: &Window, html: String, on_message: impl Fn(&str) + 'static) -> Result<Web, String> {
        // WebView2 guarda su perfil junto al .exe si no se le da carpeta (y en Program Files no puede).
        let mut context = wry::WebContext::new(dirs::data_local_dir().map(|d| d.join("kuaky").join("webview")));
        let view = wry::WebViewBuilder::new_with_web_context(&mut context)
            .with_html(html)
            .with_background_color((0x11, 0x12, 0x14, 0xff))
            .with_visible(false)
            .with_ipc_handler(move |req| on_message(req.body()))
            .build_as_child(window)
            .map_err(|e| format!("Could not open the canvas: {e}"))?;
        Ok(Web { view, _context: context })
    }

    impl Web {
        pub fn show(&self, visible: bool) {
            self.view.set_visible(visible).ok();
        }

        pub fn eval(&self, js: &str) -> Result<(), String> {
            self.view.evaluate_script(js).map_err(|e| e.to_string())
        }

        pub fn place(&self, b: Bounds<Pixels>) {
            let rect = wry::Rect {
                position: LogicalPosition::new(f32::from(b.origin.x), f32::from(b.origin.y)).into(),
                size: LogicalSize::new(f32::from(b.size.width), f32::from(b.size.height)).into(),
            };
            self.view.set_bounds(rect).ok();
        }
    }
}

// ponytail: en Linux wry pide GTK/WebKitGTK, que GPUI no usa; ahí el lienzo solo avisa.
#[cfg(not(any(windows, target_os = "macos")))]
mod web {
    use gpui_kit::{Bounds, Pixels, Window};

    pub enum Web {}

    pub fn build(_: &Window, _: String, _: impl Fn(&str) + 'static) -> Result<Web, String> {
        Err("The canvas only works on Windows and macOS for now.".into())
    }

    impl Web {
        pub fn show(&self, _: bool) {
            match *self {}
        }

        pub fn eval(&self, _: &str) -> Result<(), String> {
            match *self {}
        }

        pub fn place(&self, _: Bounds<Pixels>) {
            match *self {}
        }
    }
}
