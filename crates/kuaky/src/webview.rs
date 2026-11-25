//! Webview nativo (wry: WebView2 en Windows, WKWebView en macOS) dentro de una pestaña de kuaky.
//! Lo usan el lienzo de diagramas y la vista previa.
//!
//! El webview es una ventana nativa encima de GPUI: solo se muestra en los cuadros en que su
//! pestaña se pintó y nada la tapa (ver `end_frame`).

use gpui_kit::*;
use std::cell::{Cell, RefCell};
use std::rc::{Rc, Weak};

pub(crate) enum Source {
    Html(String),
    Url(String),
}

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

/// Al final de cada cuadro (último hijo de la ventana): muestra solo los webviews que se pintaron.
pub(crate) fn end_frame() {
    let covered = COVERED.replace(false);
    LIVE.with_borrow_mut(|live| {
        live.retain(|weak| {
            let Some(s) = weak.upgrade() else {
                return false;
            };
            let show = s.painted.replace(false) && !covered;
            if s.visible.replace(show) != show {
                s.web.show(show);
            }
            true
        })
    });
}

#[derive(Clone)]
pub(crate) struct WebPanel(Rc<Shared>);

impl WebPanel {
    /// `init` corre en cada página antes que sus scripts; `on_message` recibe `window.ipc.postMessage`.
    pub fn new(window: &Window, source: Source, init: Option<&str>, on_message: impl Fn(&str) + 'static) -> Result<Self, String> {
        let web = web::build(window, source, init, on_message)?;
        let s = Rc::new(Shared { web, painted: Cell::new(false), visible: Cell::new(false), bounds: Cell::new(None) });
        LIVE.with_borrow_mut(|live| live.push(Rc::downgrade(&s)));
        Ok(Self(s))
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
    use super::Source;
    use gpui_kit::{Bounds, Pixels, Window};
    use wry::dpi::{LogicalPosition, LogicalSize};

    pub struct Web {
        view: wry::WebView,
        _context: wry::WebContext,
    }

    pub fn build(window: &Window, source: Source, init: Option<&str>, on_message: impl Fn(&str) + 'static) -> Result<Web, String> {
        // WebView2 guarda su perfil junto al .exe si no se le da carpeta (y en Program Files no puede).
        let mut context = wry::WebContext::new(dirs::data_local_dir().map(|d| d.join("kuaky").join("webview")));
        let mut builder = wry::WebViewBuilder::new_with_web_context(&mut context)
            .with_background_color((0x11, 0x12, 0x14, 0xff))
            .with_visible(false)
            .with_focused(false)
            .with_ipc_handler(move |req| on_message(req.body()));
        builder = match source {
            Source::Html(html) => builder.with_html(html),
            Source::Url(url) => builder.with_url(url),
        };
        if let Some(js) = init {
            builder = builder.with_initialization_script(js);
        }
        let view = builder.build_as_child(window).map_err(|e| format!("Could not open the webview: {e}"))?;
        Ok(Web { view, _context: context })
    }

    impl Web {
        pub fn show(&self, visible: bool) {
            self.view.set_visible(visible).ok();
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
    use super::Source;
    use gpui_kit::{Bounds, Pixels, Window};

    pub enum Web {}

    pub fn build(_: &Window, _: Source, _: Option<&str>, _: impl Fn(&str) + 'static) -> Result<Web, String> {
        Err("This panel only works on Windows and macOS for now.".into())
    }

    impl Web {
        pub fn show(&self, _: bool) {
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
