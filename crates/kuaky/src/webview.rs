//! Webview nativo (wry: WebView2 en Windows, WKWebView en macOS) dentro de una pestaña de kuaky.
//! Lo usan el lienzo de diagramas y el navegador.
//!
//! El webview es una ventana nativa encima de GPUI:
//! - solo se muestra en los cuadros en que su pestaña se pintó y nada la tapa (`end_frame`);
//! - se queda con el teclado al darle clic: cualquier clic en kuaky, o Esc dentro de la página,
//!   se lo devuelve a kuaky (`release_focus`).

use gpui_kit::*;
use std::cell::{Cell, RefCell};
use std::rc::{Rc, Weak};

/// Esc dentro de la página devuelve el teclado a kuaky.
const ESCAPE: &str = r#"addEventListener("keydown", e => { if (e.key === "Escape") window.ipc?.postMessage("{\"blur\":true}"); }, true);"#;

struct Shared {
    web: web::Web,
    painted: Cell<bool>,
    visible: Cell<bool>,
    bounds: Cell<Option<Bounds<Pixels>>>,
}

thread_local! {
    static LIVE: RefCell<Vec<Weak<Shared>>> = RefCell::default();
    static COVERED: Cell<bool> = const { Cell::new(false) };
}

/// Un modal o menú flotante se dibuja en este cuadro: los webviews se esconden para no taparlo.
pub(crate) fn cover() {
    COVERED.set(true);
}

fn each(f: impl Fn(&Shared)) {
    // `try_borrow`: un mensaje de la página puede llegar mientras se recorre la lista.
    LIVE.with(|live| {
        if let Ok(live) = live.try_borrow() {
            live.iter().filter_map(Weak::upgrade).for_each(|s| f(&s));
        }
    });
}

/// Al final de cada cuadro (último hijo de la ventana): muestra solo los webviews que se pintaron.
pub(crate) fn end_frame() {
    let covered = COVERED.replace(false);
    each(|s| {
        let show = s.painted.replace(false) && !covered;
        if s.visible.replace(show) != show {
            s.web.show(show);
            if !show {
                // Escondido no puede quedarse con el teclado.
                s.web.focus_parent();
            }
        }
    });
    LIVE.with(|live| {
        if let Ok(mut live) = live.try_borrow_mut() {
            live.retain(|w| w.strong_count() > 0);
        }
    });
}

/// Un clic en kuaky (fuera de los webviews): el teclado vuelve a kuaky.
pub(crate) fn release_focus() {
    each(|s| s.web.focus_parent());
}

#[derive(Clone)]
pub(crate) struct WebPanel(Rc<Shared>);

impl WebPanel {
    /// Crea el webview y llama a `done` con él. `init` corre en cada página antes que sus scripts;
    /// lo que la página manda con `window.ipc.postMessage` le llega a la vista con `on_message`.
    ///
    /// Se crea desde una tarea, fuera de cualquier `update`: WebView2 atiende mensajes de Windows
    /// mientras se crea y GPUI correría ahí tareas que piden la app, que estaría prestada (crash).
    pub fn create<V: 'static>(
        html: String,
        init: Option<&str>,
        on_message: impl Fn(&mut V, &str, &mut Context<V>) + 'static,
        done: impl FnOnce(&mut V, Result<WebPanel, String>, &mut Context<V>) + 'static,
        window: &Window,
        cx: &mut Context<V>,
    ) {
        let handle = raw_window_handle::HasWindowHandle::window_handle(window).map(|h| h.as_raw()).map_err(|e| e.to_string());
        let init = format!("{ESCAPE}
{}", init.unwrap_or_default());
        cx.spawn(async move |this, cx| {
            let (tx, rx) = flume::unbounded::<String>();
            let built = handle.and_then(|raw| {
                web::build(&web::Parent(raw), html, &init, move |body| {
                    if body == r#"{"blur":true}"# {
                        release_focus();
                    } else {
                        tx.send(body.to_string()).ok();
                    }
                })
            });
            let panel = built.map(|web| {
                web.focus_parent();
                let s = Rc::new(Shared { web, painted: Cell::new(false), visible: Cell::new(false), bounds: Cell::new(None) });
                LIVE.with_borrow_mut(|live| live.push(Rc::downgrade(&s)));
                WebPanel(s)
            });
            if this.update(cx, |view, cx| done(view, panel, cx)).is_err() {
                return;
            }
            while let Ok(msg) = rx.recv_async().await {
                if this.update(cx, |view, cx| on_message(view, &msg, cx)).is_err() {
                    break;
                }
            }
        })
        .detach();
    }

    pub fn eval(&self, js: &str) -> Result<(), String> {
        self.0.web.eval(js)
    }

    pub fn load_url(&self, url: &str) -> Result<(), String> {
        self.0.web.load_url(url)
    }

    pub fn reload(&self) {
        self.0.web.reload();
    }

    /// Ocupa todo su contenedor y pone ahí el webview en cada cuadro que se pinta.
    pub fn element(&self) -> impl IntoElement {
        let s = self.0.clone();
        canvas(
            |_, _, _| {},
            move |bounds, _, _, _| {
                s.painted.set(true);
                if s.bounds.replace(Some(bounds)) != Some(bounds) {
                    s.web.place(bounds);
                }
            },
        )
        .size_full()
    }
}

#[cfg(any(windows, target_os = "macos"))]
mod web {
    use gpui_kit::{Bounds, Pixels};
    use raw_window_handle::{HandleError, HasWindowHandle, RawWindowHandle, WindowHandle};
    use wry::dpi::{LogicalPosition, LogicalSize};

    /// La ventana de kuaky, por su handle crudo (vive más que el webview).
    pub struct Parent(pub RawWindowHandle);

    impl HasWindowHandle for Parent {
        fn window_handle(&self) -> Result<WindowHandle<'_>, HandleError> {
            Ok(unsafe { WindowHandle::borrow_raw(self.0) })
        }
    }

    pub struct Web {
        view: wry::WebView,
        _context: wry::WebContext,
    }

    pub fn build(parent: &Parent, html: String, init: &str, on_message: impl Fn(&str) + 'static) -> Result<Web, String> {
        // WebView2 guarda su perfil junto al .exe si no se le da carpeta (y en Program Files no puede).
        let mut context = wry::WebContext::new(dirs::data_local_dir().map(|d| d.join("kuaky").join("webview")));
        let view = wry::WebViewBuilder::new_with_web_context(&mut context)
            .with_html(html)
            .with_background_color((0x11, 0x12, 0x14, 0xff))
            .with_visible(false)
            .with_focused(false)
            .with_initialization_script(init)
            .with_ipc_handler(move |req| on_message(req.body()))
            .build_as_child(parent)
            .map_err(|e| format!("Could not open the webview: {e}"))?;
        Ok(Web { view, _context: context })
    }

    impl Web {
        pub fn show(&self, visible: bool) {
            self.view.set_visible(visible).ok();
        }

        pub fn focus_parent(&self) {
            self.view.focus_parent().ok();
        }

        pub fn eval(&self, js: &str) -> Result<(), String> {
            self.view.evaluate_script(js).map_err(|e| e.to_string())
        }

        pub fn load_url(&self, url: &str) -> Result<(), String> {
            self.view.load_url(url).map_err(|e| e.to_string())
        }

        pub fn reload(&self) {
            self.view.reload().ok();
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

// ponytail: en Linux wry pide GTK/WebKitGTK, que GPUI no usa; ahí el panel solo avisa.
#[cfg(not(any(windows, target_os = "macos")))]
mod web {
    use gpui_kit::{Bounds, Pixels};

    pub struct Parent(pub raw_window_handle::RawWindowHandle);

    pub enum Web {}

    pub fn build(_: &Parent, _: String, _: &str, _: impl Fn(&str) + 'static) -> Result<Web, String> {
        Err("This panel only works on Windows and macOS for now.".into())
    }

    impl Web {
        pub fn show(&self, _: bool) {
            match *self {}
        }

        pub fn focus_parent(&self) {
            match *self {}
        }

        pub fn eval(&self, _: &str) -> Result<(), String> {
            match *self {}
        }

        pub fn load_url(&self, _: &str) -> Result<(), String> {
            match *self {}
        }

        pub fn reload(&self) {
            match *self {}
        }

        pub fn place(&self, _: Bounds<Pixels>) {
            match *self {}
        }
    }
}
