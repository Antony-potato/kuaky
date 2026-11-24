//! Raíz de la ventana: barra de título y pantalla actual (inicio o proyecto).

use crate::home::{HomeEvent, HomeView, tilde};
use crate::project::ProjectView;
use crate::style;
use gpui_kit::component::{TitleBar, h_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use std::path::PathBuf;

actions!(kuaky, [Quit, GoHome, OpenFolder, Hide, HideOthers, ShowAll, Minimize, Zoom, OpenRepo]);

const REPO_URL: &str = "https://github.com/Antony-potato/kuaky";

pub fn init(cx: &mut App) {
    cx.on_action(|_: &Quit, cx| cx.quit());
    cx.on_action(|_: &Hide, cx| cx.hide());
    cx.on_action(|_: &HideOthers, cx| cx.hide_other_apps());
    cx.on_action(|_: &ShowAll, cx| cx.unhide_other_apps());
    cx.on_action(|_: &OpenRepo, cx| cx.open_url(REPO_URL));
    #[cfg(target_os = "macos")]
    cx.bind_keys([
        KeyBinding::new("cmd-q", Quit, None),
        KeyBinding::new("cmd-h", Hide, None),
        KeyBinding::new("cmd-alt-h", HideOthers, None),
        KeyBinding::new("cmd-m", Minimize, None),
        KeyBinding::new("cmd-o", OpenFolder, None),
        KeyBinding::new("cmd-shift-h", GoHome, None),
    ]);
    #[cfg(not(target_os = "macos"))]
    cx.bind_keys([
        KeyBinding::new("ctrl-o", OpenFolder, Some("!Terminal")),
        KeyBinding::new("ctrl-shift-h", GoHome, Some("!Terminal")),
    ]);
    set_menus(cx);
}

/// Barra de menús nativa (en macOS arriba de la pantalla).
fn set_menus(cx: &mut App) {
    use crate::project::{CloseTab, NewTerminal, NextTab, PrevTab, QuickOpenFile, SearchProject};
    use gpui_kit::component::input::{Copy, Cut, Paste, Redo, SelectAll, Undo};
    cx.set_menus([
        Menu::new("kuaky").items([
            MenuItem::action("Ocultar kuaky", Hide),
            MenuItem::action("Ocultar otros", HideOthers),
            MenuItem::action("Mostrar todo", ShowAll),
            MenuItem::separator(),
            MenuItem::action("Salir de kuaky", Quit),
        ]),
        Menu::new("Archivo").items([
            MenuItem::action("Abrir carpeta…", OpenFolder),
            MenuItem::action("Buscar archivo…", QuickOpenFile),
            MenuItem::action("Buscar en el proyecto…", SearchProject),
            MenuItem::separator(),
            MenuItem::action("Nueva terminal", NewTerminal),
            MenuItem::action("Cerrar pestaña", CloseTab),
            MenuItem::separator(),
            MenuItem::action("Ir al inicio", GoHome),
        ]),
        Menu::new("Edición").items([
            MenuItem::os_action("Deshacer", Undo, OsAction::Undo),
            MenuItem::os_action("Rehacer", Redo, OsAction::Redo),
            MenuItem::separator(),
            MenuItem::os_action("Cortar", Cut, OsAction::Cut),
            MenuItem::os_action("Copiar", Copy, OsAction::Copy),
            MenuItem::os_action("Pegar", Paste, OsAction::Paste),
            MenuItem::os_action("Seleccionar todo", SelectAll, OsAction::SelectAll),
        ]),
        Menu::new("Ver").items([
            MenuItem::action("Inicio", GoHome),
            MenuItem::separator(),
            MenuItem::action("Pestaña siguiente", NextTab),
            MenuItem::action("Pestaña anterior", PrevTab),
        ]),
        Menu::new("Ventana").items([
            MenuItem::action("Minimizar", Minimize),
            MenuItem::action("Zoom", Zoom),
        ]),
        Menu::new("Ayuda").items([MenuItem::action("Repositorio de kuaky", OpenRepo)]),
    ]);
}

pub struct KuakyApp {
    home: Entity<HomeView>,
    project: Option<Entity<ProjectView>>,
    _subscriptions: Vec<Subscription>,
}

impl KuakyApp {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let home = cx.new(HomeView::new);
        let sub = cx.subscribe_in(&home, window, |this, _, event: &HomeEvent, window, cx| match event {
            HomeEvent::OpenProject(path) => this.open_project(path.clone(), window, cx),
            HomeEvent::PickFolder => this.pick_folder(window, cx),
        });
        // Cuando termina de cargar el consumo, la vista de proyecto actualiza su actividad.
        let obs = cx.observe(&home, |this, home, cx| {
            if let (Some(project), Some(events)) = (&this.project, home.read(cx).events()) {
                project.update(cx, |p, cx| p.set_usage(&events, cx));
            }
        });
        // Abrir directo una carpeta pasada por línea de comandos: `kuaky ~/proyecto`.
        let mut this = Self { home, project: None, _subscriptions: vec![sub, obs] };
        if let Some(arg) = std::env::args().nth(1) {
            let path = PathBuf::from(arg);
            if path.is_dir() {
                this.open_project(path, window, cx);
            }
        }
        #[cfg(feature = "screenshot")]
        Self::schedule_screenshot(window, cx);
        this
    }

    /// `KUAKY_SCREENSHOT=ruta.png` guarda una captura de la ventana tras unos
    /// segundos (`KUAKY_SCREENSHOT_DELAY`, por defecto 6) y cierra la app.
    #[cfg(feature = "screenshot")]
    fn schedule_screenshot(window: &mut Window, cx: &mut Context<Self>) {
        let Ok(path) = std::env::var("KUAKY_SCREENSHOT") else {
            return;
        };
        let delay: u64 = std::env::var("KUAKY_SCREENSHOT_DELAY")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(6);
        cx.spawn_in(window, async move |_, cx| {
            cx.background_executor().timer(std::time::Duration::from_secs(delay)).await;
            cx.update(|window, cx| {
                // Dibujar un cuadro nuevo aunque la ventana esté tapada (macOS
                // deja de pintar ventanas ocultas).
                window.refresh();
                let arena = window.draw(cx);
                arena.clear(cx);
                match window.render_to_image() {
                    Ok(img) => match img.save(&path) {
                        Ok(()) => tracing::info!(%path, "captura guardada"),
                        Err(err) => tracing::error!(%err, "no se pudo guardar la captura"),
                    },
                    Err(err) => tracing::error!(%err, "no se pudo renderizar la captura"),
                }
                cx.quit();
            })
            .ok();
        })
        .detach();
    }

    fn open_project(&mut self, path: PathBuf, window: &mut Window, cx: &mut Context<Self>) {
        let path = path.canonicalize().unwrap_or(path);
        let usage = self.home.read(cx).events();
        let project = cx.new(|cx| ProjectView::new(path, usage, window, cx));
        #[cfg(feature = "screenshot")]
        if let Ok(which) = std::env::var("KUAKY_DEMO_TERMINAL") {
            let harness = if which == "claude" { crate::project::Harness::Claude } else { crate::project::Harness::Shell };
            project.update(cx, |p, cx| p.open_terminal(harness, window, cx));
        }
        #[cfg(feature = "screenshot")]
        project.update(cx, |p, cx| p.demo(window, cx));
        self.project = Some(project);
        cx.notify();
    }

    fn go_home(&mut self, cx: &mut Context<Self>) {
        self.project = None;
        self.home.update(cx, |home, cx| home.reload(cx));
        cx.notify();
    }

    fn pick_folder(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let receiver = cx.prompt_for_paths(PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: Some("Abrir".into()),
        });
        cx.spawn_in(window, async move |this, cx| {
            if let Ok(Ok(Some(paths))) = receiver.await {
                if let Some(path) = paths.into_iter().next() {
                    this.update_in(cx, |this, window, cx| this.open_project(path, window, cx)).ok();
                }
            }
        })
        .detach();
    }

    fn title_bar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let location = self.project.as_ref().map(|p| tilde(p.read(cx).root()));
        TitleBar::new().child(
            h_flex()
                .flex_1()
                .gap_2()
                .text_sm()
                .child(
                    div()
                        .id("brand")
                        .cursor_pointer()
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(style::accent())
                        .child("kuaky")
                        .on_click(cx.listener(|this, _, _, cx| this.go_home(cx))),
                )
                .when_some(location, |d, loc| {
                    d.child(div().text_color(style::faint()).child("/"))
                        .child(div().text_color(style::muted()).child(loc))
                        .child(div().flex_1())
                        .child(
                            div()
                                .id("go-home")
                                .px_2()
                                .py_0p5()
                                .rounded_md()
                                .text_xs()
                                .cursor_pointer()
                                .border_1()
                                .border_color(style::border())
                                .text_color(style::muted())
                                .hover(|d| d.bg(style::panel_hover()).text_color(style::text()))
                                .child("← Inicio")
                                .on_click(cx.listener(|this, _, _, cx| this.go_home(cx))),
                        )
                }),
        )
    }
}

impl Render for KuakyApp {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .size_full()
            .flex()
            .flex_col()
            .bg(style::bg())
            .text_color(style::text())
            .on_action(cx.listener(|this, _: &GoHome, _, cx| this.go_home(cx)))
            .on_action(cx.listener(|this, _: &OpenFolder, window, cx| this.pick_folder(window, cx)))
            .on_action(|_: &Minimize, window, _| window.minimize_window())
            .on_action(|_: &Zoom, window, _| window.zoom_window())
            .child(self.title_bar(cx))
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .map(|d| match &self.project {
                        Some(project) => d.child(project.clone()),
                        None => d.child(self.home.clone()),
                    }),
            )
    }
}
