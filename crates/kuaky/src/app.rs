//! Raíz de la ventana: barra de título y pantalla actual (inicio o proyecto).

use crate::home::{HomeEvent, HomeView, tilde};
use crate::project::{ProjectEvent, ProjectView};
use crate::style;
use gpui_kit::component::{TitleBar, h_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use std::path::PathBuf;

actions!(kuaky, [Quit, GoHome, OpenFolder]);

pub fn init(cx: &mut App) {
    cx.on_action(|_: &Quit, cx| cx.quit());
    cx.bind_keys([
        KeyBinding::new("cmd-q", Quit, None),
        KeyBinding::new("cmd-o", OpenFolder, None),
        KeyBinding::new("ctrl-o", OpenFolder, None),
        KeyBinding::new("cmd-shift-h", GoHome, None),
        KeyBinding::new("ctrl-shift-h", GoHome, None),
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
        // Abrir directo una carpeta pasada por línea de comandos: `kuaky ~/proyecto`.
        let mut this = Self { home, project: None, _subscriptions: vec![sub] };
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
                window.refresh();
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
        let project = cx.new(|cx| ProjectView::new(path, window, cx));
        let sub = cx.subscribe_in(&project, window, |this, _, event: &ProjectEvent, _, cx| match event {
            ProjectEvent::GoHome => this.go_home(cx),
        });
        self._subscriptions.push(sub);
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
