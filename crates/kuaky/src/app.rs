//! Root window: title bar, active screen (Home or Project), and Settings modal.

use crate::home::{HomeEvent, HomeView, tilde};
use crate::icons;
use crate::project::{ProjectView, SearchProject};
use crate::settings::{SettingsEvent, SettingsView};
use crate::state;
use crate::style;
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::component::{TitleBar, h_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use std::path::PathBuf;

actions!(kuaky, [Quit, GoHome, OpenFolder, OpenSettings, Hide, HideOthers, ShowAll, Minimize, Zoom, OpenRepo]);

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
        KeyBinding::new("cmd-,", OpenSettings, None),
    ]);
    #[cfg(not(target_os = "macos"))]
    cx.bind_keys([
        KeyBinding::new("ctrl-o", OpenFolder, Some("!Terminal")),
        KeyBinding::new("ctrl-shift-h", GoHome, Some("!Terminal")),
        KeyBinding::new("ctrl-,", OpenSettings, Some("!Terminal")),
    ]);
    set_menus(cx);
}

/// Native menu bar (macOS top bar).
fn set_menus(cx: &mut App) {
    use crate::project::{CloseTab, NewTerminal, NextTab, PrevTab, QuickOpenFile};
    use gpui_kit::component::input::{Copy, Cut, Paste, Redo, SelectAll, Undo};
    cx.set_menus([
        Menu::new("kuaky").items([
            MenuItem::action("Settings…", OpenSettings),
            MenuItem::separator(),
            MenuItem::action("Hide kuaky", Hide),
            MenuItem::action("Hide Others", HideOthers),
            MenuItem::action("Show All", ShowAll),
            MenuItem::separator(),
            MenuItem::action("Quit kuaky", Quit),
        ]),
        Menu::new("File").items([
            MenuItem::action("Open Folder…", OpenFolder),
            MenuItem::action("Quick Open…", QuickOpenFile),
            MenuItem::action("Search in Project…", SearchProject),
            MenuItem::separator(),
            MenuItem::action("New Terminal", NewTerminal),
            MenuItem::action("Close Tab", CloseTab),
            MenuItem::separator(),
            MenuItem::action("Home", GoHome),
        ]),
        Menu::new("Edit").items([
            MenuItem::os_action("Undo", Undo, OsAction::Undo),
            MenuItem::os_action("Redo", Redo, OsAction::Redo),
            MenuItem::separator(),
            MenuItem::os_action("Cut", Cut, OsAction::Cut),
            MenuItem::os_action("Copy", Copy, OsAction::Copy),
            MenuItem::os_action("Paste", Paste, OsAction::Paste),
            MenuItem::os_action("Select All", SelectAll, OsAction::SelectAll),
        ]),
        Menu::new("View").items([
            MenuItem::action("Home", GoHome),
            MenuItem::separator(),
            MenuItem::action("Next Tab", NextTab),
            MenuItem::action("Previous Tab", PrevTab),
        ]),
        Menu::new("Window").items([
            MenuItem::action("Minimize", Minimize),
            MenuItem::action("Zoom", Zoom),
        ]),
        Menu::new("Help").items([MenuItem::action("kuaky Repository", OpenRepo)]),
    ]);
}

pub struct KuakyApp {
    home: Entity<HomeView>,
    project: Option<Entity<ProjectView>>,
    settings: Option<Entity<SettingsView>>,
    _subscriptions: Vec<Subscription>,
}

impl KuakyApp {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let home = cx.new(HomeView::new);
        let sub = cx.subscribe_in(&home, window, |this, _, event: &HomeEvent, window, cx| match event {
            HomeEvent::OpenProject(path) => this.open_project(path.clone(), window, cx),
            HomeEvent::PickFolder => this.pick_folder(window, cx),
        });
        let obs = cx.observe(&home, |this, home, cx| {
            if let (Some(project), Some(events)) = (&this.project, home.read(cx).events()) {
                project.update(cx, |p, cx| p.set_usage(&events, cx));
            }
        });
        let mut this = Self { home, project: None, settings: None, _subscriptions: vec![sub, obs] };
        let capture = std::env::var_os("KUAKY_SCREENSHOT").is_some();
        let start = std::env::args().nth(1).map(PathBuf::from).or_else(|| {
            // Reopen the project that was open when kuaky was closed.
            (!capture).then(|| state::get(cx).last_open.clone().map(PathBuf::from)).flatten()
        });
        if let Some(path) = start.filter(|p| p.is_dir()) {
            this.open_project(path, window, cx);
        }
        #[cfg(feature = "screenshot")]
        {
            if std::env::var("KUAKY_DEMO_SETTINGS").is_ok() {
                this.toggle_settings(window, cx);
            }
            Self::schedule_screenshot(window, cx);
        }
        this
    }

    /// `KUAKY_SCREENSHOT=path.png` saves a window capture after a few seconds
    /// (`KUAKY_SCREENSHOT_DELAY`, default 6) and quits.
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
                let arena = window.draw(cx);
                arena.clear(cx);
                match window.render_to_image() {
                    Ok(img) => match img.save(&path) {
                        Ok(()) => tracing::info!(%path, "screenshot saved"),
                        Err(err) => tracing::error!(%err, "failed to save screenshot"),
                    },
                    Err(err) => tracing::error!(%err, "failed to render screenshot"),
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
        let key = path.to_string_lossy().into_owned();
        state::update(cx, |s| s.last_open = Some(key));
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
        state::update(cx, |s| s.last_open = None);
        self.home.update(cx, |home, cx| home.reload(cx));
        cx.notify();
    }

    fn toggle_settings(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.settings.take().is_some() {
            cx.notify();
            return;
        }
        let view = cx.new(|cx| SettingsView::new(window, cx));
        let sub = cx.subscribe(&view, |this, _, ev: &SettingsEvent, cx| match ev {
            SettingsEvent::Close => {
                this.settings = None;
                cx.notify();
            }
            SettingsEvent::Changed => {
                if let Some(project) = &this.project {
                    project.update(cx, |p, cx| p.on_settings_changed(cx));
                }
            }
        });
        self._subscriptions.push(sub);
        self.settings = Some(view);
        cx.notify();
    }

    fn pick_folder(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let receiver = cx.prompt_for_paths(PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: Some("Open".into()),
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
        let home_tip: SharedString = if cfg!(target_os = "macos") { "Home (⌘⇧H)".into() } else { "Home (Ctrl+Shift+H)".into() };
        let settings_tip: SharedString = if cfg!(target_os = "macos") { "Settings (⌘,)".into() } else { "Settings (Ctrl+,)".into() };
        let search_tip: SharedString = if cfg!(target_os = "macos") { "Search in project (⌘⇧F)".into() } else { "Search in project (Ctrl+Shift+F)".into() };
        let has_project = self.project.is_some();

        TitleBar::new().child(
            h_flex()
                .flex_1()
                .gap_2()
                .items_center()
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
                        .child(div().text_color(style::muted()).truncate().child(loc))
                })
                .child(div().flex_1())
                .when(has_project, |d| {
                    d.child(
                        div()
                            .id("title-search")
                            .p_1()
                            .rounded_md()
                            .cursor_pointer()
                            .hover(|d| d.bg(style::panel_hover()))
                            .tooltip(move |window, cx| Tooltip::new(search_tip.clone()).build(window, cx))
                            .child(icons::ui("search", px(15.), style::muted()))
                            .on_click(cx.listener(|this, _, window, cx| {
                                if let Some(project) = &this.project {
                                    project.update(cx, |p, cx| p.open_search_tab(window, cx));
                                }
                            })),
                    )
                    .child(
                        div()
                            .id("go-home")
                            .p_1()
                            .rounded_md()
                            .cursor_pointer()
                            .hover(|d| d.bg(style::panel_hover()))
                            .tooltip(move |window, cx| Tooltip::new(home_tip.clone()).build(window, cx))
                            .child(icons::ui("house", px(15.), style::muted()))
                            .on_click(cx.listener(|this, _, _, cx| this.go_home(cx))),
                    )
                })
                .child(
                    div()
                        .id("open-settings")
                        .p_1()
                        .rounded_md()
                        .cursor_pointer()
                        .hover(|d| d.bg(style::panel_hover()))
                        .tooltip(move |window, cx| Tooltip::new(settings_tip.clone()).build(window, cx))
                        .child(icons::ui("settings", px(15.), style::muted()))
                        .on_click(cx.listener(|this, _, window, cx| this.toggle_settings(window, cx))),
                ),
        )
    }
}

impl Render for KuakyApp {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .size_full()
            .relative()
            .flex()
            .flex_col()
            .bg(style::bg())
            .text_color(style::text())
            .on_action(cx.listener(|this, _: &GoHome, _, cx| this.go_home(cx)))
            .on_action(cx.listener(|this, _: &OpenFolder, window, cx| this.pick_folder(window, cx)))
            .on_action(cx.listener(|this, _: &OpenSettings, window, cx| this.toggle_settings(window, cx)))
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
            .when_some(self.settings.clone(), |d, view| {
                d.child(
                    div()
                        .id("settings-overlay")
                        .absolute()
                        .inset_0()
                        .flex()
                        .items_center()
                        .justify_center()
                        .bg(style::soft(style::bg(), 0.6))
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.settings = None;
                            cx.notify();
                        }))
                        .child(div().id("settings-box").on_click(|_, _, cx| cx.stop_propagation()).child(view)),
                )
            })
    }
}
