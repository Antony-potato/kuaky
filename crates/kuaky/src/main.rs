mod app;
mod diff_view;
mod file_tree;
mod file_view;
mod home;
mod icons;
mod project;
mod style;
mod terminal_view;

use gpui_kit::component::{Theme, ThemeMode, TitleBar};
use gpui_kit::*;

fn main() {
    // Si kuaky se abrió desde una sesión de Claude Code, no heredar sus marcas:
    // los agentes que se abren aquí deben comportarse como en tu terminal.
    // Se hace antes de crear cualquier hilo.
    for (key, _) in std::env::vars_os() {
        let k = key.to_string_lossy();
        if k == "CLAUDECODE" || k.starts_with("CLAUDE_CODE_") {
            // SAFETY: todavía no hay otros hilos leyendo el entorno.
            unsafe { std::env::remove_var(&key) };
        }
    }

    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "kuaky=info,kuaky_usage=info".into()),
        )
        .init();

    gpui_kit::application()
        .with_assets(gpui_kit::assets::Assets)
        .run(|cx| {
            gpui_kit::init(cx);
            Theme::change(ThemeMode::Dark, None, cx);
            app::init(cx);
            project::init(cx);
            file_view::init(cx);

            let mut options = TitleBar::window_options();
            // KUAKY_WINDOW_SIZE=1360x1700 sirve para capturas de desarrollo.
            let (w, h) = std::env::var("KUAKY_WINDOW_SIZE")
                .ok()
                .and_then(|v| {
                    let (w, h) = v.split_once('x')?;
                    Some((w.parse().ok()?, h.parse().ok()?))
                })
                .unwrap_or((1360., 900.));
            options.window_bounds = Some(WindowBounds::centered(None, size(px(w), px(h)), cx));
            options.window_min_size = Some(size(px(960.), px(640.)));
            gpui_kit::open_window(options, cx, |window, cx| cx.new(|cx| app::KuakyApp::new(window, cx)))
                .expect("no se pudo abrir la ventana");
            cx.activate(true);
        });
}
