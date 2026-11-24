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

actions!(kuaky, [Quit, GoHome, OpenFolder, OpenSettings, Hide, HideOthers, ShowAll, Minimize, Zoom, OpenRepo, NextProject, PrevProject, CloseProject]);

const REPO_URL: &str = "https://github.com/Antony-potato/kuaky";

static QUITTING: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// kuaky is closing (sessions ending now must stay saved as open).
pub fn quitting() -> bool {
    QUITTING.load(std::sync::atomic::Ordering::Relaxed)
}

pub fn init(cx: &mut App) {
    cx.on_action(|_: &Quit, cx| {
        QUITTING.store(true, std::sync::atomic::Ordering::Relaxed);
        cx.quit()
    });
    cx.on_app_quit(|_| {
        QUITTING.store(true, std::sync::atomic::Ordering::Relaxed);
        async {}
    })
    .detach();
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
        KeyBinding::new("cmd-alt-right", NextProject, None),
        KeyBinding::new("cmd-alt-left", PrevProject, None),
        KeyBinding::new("cmd-shift-w", CloseProject, None),
    ]);
    #[cfg(not(target_os = "macos"))]
    cx.bind_keys([
        KeyBinding::new("ctrl-o", OpenFolder, Some("!Terminal")),
        KeyBinding::new("ctrl-shift-h", GoHome, Some("!Terminal")),
        KeyBinding::new("ctrl-,", OpenSettings, Some("!Terminal")),
        KeyBinding::new("ctrl-alt-right", NextProject, None),
        KeyBinding::new("ctrl-alt-left", PrevProject, None),
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
            MenuItem::action("Close Project", CloseProject),
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
            MenuItem::separator(),
            MenuItem::action("Next Project", NextProject),
            MenuItem::action("Previous Project", PrevProject),
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
    /// Open projects, as tabs in the title bar. They keep running when hidden.
    projects: Vec<Entity<ProjectView>>,
    /// Shown project (`None` = Home).
    active: Option<usize>,
    /// Observer of each open project (same order as `projects`).
    project_subs: Vec<Subscription>,
    settings: Option<Entity<SettingsView>>,
    /// Memoria de kuaky y sus agentes (se mide cada 5 s).
    memory: Option<crate::memory::Usage>,
    _subscriptions: Vec<Subscription>,
}

impl KuakyApp {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let home = cx.new(HomeView::new);
        let sub = cx.subscribe_in(&home, window, |this, _, event: &HomeEvent, window, cx| match event {
            HomeEvent::OpenProject(path) => this.open_project(path.clone(), window, cx),
            HomeEvent::OpenLine(path, line) => {
                this.open_project(path.clone(), window, cx);
                if let Some(project) = this.current() {
                    project.update(cx, |p, cx| p.show_line(line.clone(), window, cx));
                }
            }
            HomeEvent::PickFolder => this.pick_folder(window, cx),
        });
        let obs = cx.observe(&home, |this, home, cx| {
            if let Some(events) = home.read(cx).events() {
                for project in &this.projects {
                    project.update(cx, |p, cx| p.set_usage(&events, cx));
                }
            }
        });
        // Con el modo "system", el tema sigue al claro/oscuro del sistema operativo.
        let appearance = cx.observe_window_appearance(window, |_, window, cx| crate::theme::apply(Some(window), cx));
        let mut this = Self {
            home,
            projects: Vec::new(),
            active: None,
            project_subs: Vec::new(),
            settings: None,
            memory: None,
            _subscriptions: vec![sub, obs, appearance],
        };
        this.watch_memory(cx);
        this.watch_tools(window, cx);
        let capture = std::env::var_os("KUAKY_SCREENSHOT").is_some();
        if let Some(path) = std::env::args().nth(1).map(PathBuf::from).filter(|p| p.is_dir()) {
            this.open_project(path, window, cx);
        } else if !capture {
            // Reopen the projects that were open when kuaky was closed.
            let saved = state::get(cx).clone();
            let mut open = saved.open.clone();
            if open.is_empty() {
                open.extend(saved.last_open.clone());
            }
            for path in open.iter().map(PathBuf::from).filter(|p| p.is_dir()) {
                this.open_project(path, window, cx);
            }
            match saved.last_open.as_deref().and_then(|l| this.index_of(std::path::Path::new(l), cx)) {
                Some(i) => this.activate(Some(i), window, cx),
                None => this.activate(None, window, cx),
            }
        }
        #[cfg(feature = "screenshot")]
        {
            // `KUAKY_DEMO_PROJECTS=a,b` opens more project tabs; `KUAKY_DEMO_HOME` ends on Home.
            if let Ok(more) = std::env::var("KUAKY_DEMO_PROJECTS") {
                for p in more.split(',').map(PathBuf::from).filter(|p| p.is_dir()) {
                    this.open_project(p, window, cx);
                }
                if this.projects.len() > 1 {
                    this.activate(Some(0), window, cx);
                }
            }
            if std::env::var("KUAKY_DEMO_HOME").is_ok() {
                this.activate(None, window, cx);
            }
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
            // An off-screen window gets no frames from macOS, so animations would be
            // captured at their first frame. Draw once, let them finish, then capture.
            cx.update(|window, cx| {
                window.refresh();
                window.draw(cx).clear(cx);
            })
            .ok();
            cx.background_executor().timer(std::time::Duration::from_secs(1)).await;
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

    fn current(&self) -> Option<&Entity<ProjectView>> {
        self.active.and_then(|i| self.projects.get(i))
    }

    fn index_of(&self, path: &std::path::Path, cx: &App) -> Option<usize> {
        self.projects.iter().position(|p| p.read(cx).root() == path)
    }

    /// Shows project `i` (or Home) and saves which projects are open.
    fn activate(&mut self, index: Option<usize>, window: &mut Window, cx: &mut Context<Self>) {
        self.active = index.filter(|i| *i < self.projects.len());
        for (i, project) in self.projects.iter().enumerate() {
            let visible = Some(i) == self.active;
            project.update(cx, |p, _| p.set_visible(visible));
        }
        // Screenshot runs must not replace the user's open projects.
        if std::env::var_os("KUAKY_SCREENSHOT").is_none() {
            let open: Vec<String> = self.projects.iter().map(|p| p.read(cx).root().to_string_lossy().into_owned()).collect();
            let last = self.current().map(|p| p.read(cx).root().to_string_lossy().into_owned());
            state::update(cx, |s| {
                s.open = open;
                s.last_open = last;
            });
        }
        match self.current().cloned() {
            Some(project) => project.update(cx, |p, cx| p.focus_active(window, cx)),
            None => {
                self.home.update(cx, |home, cx| home.reload(cx));
                self.push_live(cx);
            }
        }
        cx.notify();
    }

    fn cycle(&mut self, delta: isize, window: &mut Window, cx: &mut Context<Self>) {
        let n = self.projects.len() as isize + 1; // + Home
        let pos = self.active.map(|i| i as isize + 1).unwrap_or(0);
        let next = (pos + delta).rem_euclid(n);
        self.activate((next > 0).then(|| next as usize - 1), window, cx);
    }

    /// Closes a project tab. Asks first if agents are still working in it.
    fn close_project(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(project) = self.projects.get(index) else { return };
        let live = project.read(cx).live_summary(cx);
        let busy = live.working + live.needs_you;
        if busy == 0 {
            self.remove_project(index, window, cx);
            return;
        }
        let name = project_name(project.read(cx).root());
        let detail = format!(
            "{busy} agent session{} still running. They stop now; you can resume them later from Previous.",
            if busy == 1 { " is" } else { "s are" }
        );
        let answer = window.prompt(PromptLevel::Warning, &format!("Close {name}?"), Some(&detail), &["Close", "Cancel"], cx);
        let target = project.clone();
        cx.spawn_in(window, async move |this, cx| {
            if answer.await.ok() == Some(0) {
                this.update_in(cx, |this, window, cx| {
                    if let Some(i) = this.projects.iter().position(|p| *p == target) {
                        this.remove_project(i, window, cx);
                    }
                })
                .ok();
            }
        })
        .detach();
    }

    fn remove_project(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        self.projects.remove(index);
        drop(self.project_subs.remove(index));
        let next = match self.active {
            Some(a) if a == index => index.checked_sub(1).or((!self.projects.is_empty()).then_some(0)),
            Some(a) if a > index => Some(a - 1),
            other => other,
        };
        self.activate(next, window, cx);
    }

    /// Live agent state of every open project, for Home's map.
    fn push_live(&self, cx: &mut Context<Self>) {
        let live: Vec<(PathBuf, crate::project::LiveSummary)> =
            self.projects.iter().map(|p| (p.read(cx).root().to_path_buf(), p.read(cx).live_summary(cx))).collect();
        self.home.update(cx, |home, cx| home.set_live(live, cx));
    }

    fn open_project(&mut self, path: PathBuf, window: &mut Window, cx: &mut Context<Self>) {
        let path = path.canonicalize().unwrap_or(path);
        if let Some(i) = self.index_of(&path, cx) {
            self.activate(Some(i), window, cx);
            return;
        }
        let usage = self.home.read(cx).events();
        let project = cx.new(|cx| ProjectView::new(path, usage, window, cx));
        let obs = cx.observe(&project, |this, _, cx| {
            if this.active.is_none() {
                this.push_live(cx);
            }
            cx.notify();
        });
        self.project_subs.push(obs);
        // Demo data (screenshots) only goes into the first project: the one on the
        // command line. Others opened with `KUAKY_DEMO_PROJECTS` keep their real map.
        #[cfg(feature = "screenshot")]
        if self.projects.is_empty() {
            if let Ok(which) = std::env::var("KUAKY_DEMO_TERMINAL") {
                let harness = if which == "claude" { crate::project::Harness::Claude } else { crate::project::Harness::Shell };
                project.update(cx, |p, cx| p.open_terminal(harness, window, cx));
            }
            project.update(cx, |p, cx| p.demo(window, cx));
        }
        self.projects.push(project);
        self.activate(Some(self.projects.len() - 1), window, cx);
    }

    fn go_home(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.activate(None, window, cx);
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
                for project in &this.projects {
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

    /// Atiende las herramientas que llaman los agentes de los chats (`kuaky mcp`):
    /// revisa el buzón cada 250 ms y pasa cada llamada al proyecto de ese chat.
    fn watch_tools(&self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(mailbox) = crate::project::delegate::mailbox() else { return };
        cx.spawn_in(window, async move |this, cx| loop {
            cx.background_executor().timer(std::time::Duration::from_millis(250)).await;
            let calls = cx.background_executor().spawn(async move { mailbox.take() }).await;
            for call in calls {
                let Ok(reply) = this.update_in(cx, |this, window, cx| this.tool_call(&call, window, cx)) else { return };
                if let Err(err) = mailbox.answer(&call.id, &reply) {
                    tracing::warn!(%err, tool = call.tool, "no se pudo contestar la herramienta");
                }
            }
        })
        .detach();
    }

    fn tool_call(&mut self, call: &kuaky_mcp::Call, window: &mut Window, cx: &mut Context<Self>) -> kuaky_mcp::Reply {
        let Some(project) = self.projects.iter().find(|p| p.read(cx).has_chat(&call.session, cx)).cloned() else {
            return kuaky_mcp::Reply::error("This session is no longer open in kuaky.");
        };
        project.update(cx, |p, cx| p.tool_call(call, window, cx))
    }

    /// Mide la memoria cada 5 s; solo redibuja si cambió al menos 1 MB.
    fn watch_memory(&self, cx: &mut Context<Self>) {
        cx.spawn(async move |this, cx| loop {
            let usage = cx.background_executor().spawn(async { crate::memory::sample() }).await;
            let Some(usage) = usage else { break };
            let alive = this
                .update(cx, |this, cx| {
                    let mb = |u: Option<crate::memory::Usage>| u.map(|u| (u.app >> 20, u.children >> 20, u.child_count));
                    if mb(this.memory) != mb(Some(usage)) {
                        this.memory = Some(usage);
                        cx.notify();
                    }
                })
                .is_ok();
            if !alive {
                break;
            }
            cx.background_executor().timer(std::time::Duration::from_secs(5)).await;
        })
        .detach();
    }

    /// Memoria en la barra de título: total, con el desglose al pasar el mouse.
    fn memory_chip(&self) -> Option<impl IntoElement> {
        use crate::memory::format;
        let usage = self.memory?;
        // Más de 1.5 GB solo en kuaky ya es para revisar.
        let heavy = usage.app > 1536 << 20;
        let tip: SharedString = format!(
            "Memory · kuaky {} · agents and tools {} ({} process{})",
            format(usage.app),
            format(usage.children),
            usage.child_count,
            if usage.child_count == 1 { "" } else { "es" },
        )
        .into();
        Some(
            div()
                .id("memory")
                .px_1p5()
                .py_0p5()
                .rounded_md()
                .text_xs()
                .text_color(if heavy { style::warn() } else { style::faint() })
                .hover(|d| d.bg(style::panel_hover()))
                .tooltip(move |window, cx| Tooltip::new(tip.clone()).build(window, cx))
                .child(format(usage.total())),
        )
    }

    fn title_bar(&self, window: &Window, cx: &mut Context<Self>) -> impl IntoElement {
        // The title bar keeps 80 px for the macOS window buttons, which are hidden
        // in full screen: there, pull the content back to the left edge.
        let fullscreen = cfg!(target_os = "macos") && window.is_fullscreen();
        let mac = cfg!(target_os = "macos");
        let home_tip: SharedString = if mac { "Home (⌘⇧H)".into() } else { "Home (Ctrl+Shift+H)".into() };
        let settings_tip: SharedString = if mac { "Settings (⌘,)".into() } else { "Settings (Ctrl+,)".into() };
        let search_tip: SharedString = if mac { "Search in project (⌘⇧F)".into() } else { "Search in project (Ctrl+Shift+F)".into() };
        let open_tip: SharedString = if mac { "Open folder (⌘O)".into() } else { "Open folder (Ctrl+O)".into() };
        let has_project = self.active.is_some();
        let on_home = self.active.is_none();

        let tabs: Vec<AnyElement> = self
            .projects
            .iter()
            .enumerate()
            .map(|(i, project)| {
                let p = project.read(cx);
                let root = p.root().to_path_buf();
                let live = p.live_summary(cx);
                self.project_tab(i, &root, &live, cx).into_any_element()
            })
            .collect();

        TitleBar::new().child(
            h_flex()
                .flex_1()
                .min_w_0()
                .when(fullscreen, |d| d.ml(px(-80.)))
                .gap_1()
                .items_center()
                .text_sm()
                .when_some(self.current().map(|p| p.read(cx).sidebar_collapsed()), |d, collapsed| {
                    d.child(crate::project::sidebar_toggle(collapsed).mr_1().on_click(cx.listener(|this, _, _, cx| {
                        if let Some(p) = this.current() {
                            p.update(cx, |p, cx| p.toggle_sidebar(cx));
                        }
                    })))
                })
                .child(
                    h_flex()
                        .id("brand")
                        .gap_1p5()
                        .items_center()
                        .cursor_pointer()
                        .mr_1()
                        .font_weight(FontWeight::SEMIBOLD)
                        .map(|d| match style::brand_mark() {
                            // Carteles japoneses: un sol de acento y el nombre en tinta.
                            true => d
                                .text_color(style::text())
                                .child(div().size(px(11.)).rounded_full().bg(style::accent())),
                            false => d.text_color(style::accent()),
                        })
                        .child("kuaky")
                        .on_click(cx.listener(|this, _, window, cx| this.go_home(window, cx))),
                )
                .child(
                    h_flex()
                        .id("go-home")
                        .gap_1()
                        .px_2()
                        .py_0p5()
                        .rounded_md()
                        .cursor_pointer()
                        .when(on_home, |d| d.bg(style::panel_hover()))
                        .hover(|d| d.bg(style::panel_hover()))
                        .tooltip(move |window, cx| Tooltip::new(home_tip.clone()).build(window, cx))
                        .child(icons::ui("house", px(14.), if on_home { style::text() } else { style::muted() }))
                        .on_click(cx.listener(|this, _, window, cx| this.go_home(window, cx))),
                )
                .when(!tabs.is_empty(), |d| d.child(div().w(px(1.)).h(px(14.)).mx_1().bg(style::border())))
                .child(
                    div()
                        .id("project-tabs")
                        .flex()
                        .flex_row()
                        .items_center()
                        .gap_1()
                        .min_w_0()
                        .overflow_x_scroll()
                        .children(tabs),
                )
                .child(
                    div()
                        .id("open-project")
                        .p_1()
                        .rounded_md()
                        .cursor_pointer()
                        .hover(|d| d.bg(style::panel_hover()))
                        .tooltip(move |window, cx| Tooltip::new(open_tip.clone()).build(window, cx))
                        .child(icons::ui("plus", px(14.), style::muted()))
                        .on_click(cx.listener(|this, _, window, cx| this.pick_folder(window, cx))),
                )
                .child(div().flex_1())
                .children(self.memory_chip())
                .children(crate::updater::title_button(cx))
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
                                if let Some(project) = this.current() {
                                    project.update(cx, |p, cx| p.open_search_tab(window, cx));
                                }
                            })),
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

    /// One project in the title bar: name, live agent state and close button.
    fn project_tab(&self, i: usize, root: &std::path::Path, live: &crate::project::LiveSummary, cx: &mut Context<Self>) -> impl IntoElement {
        let active = self.active == Some(i);
        let name = project_name(root);
        let group: SharedString = format!("ptab-{i}").into();
        let path_tip: SharedString = {
            let mut t = tilde(root);
            if live.needs_you > 0 {
                t.push_str(&format!(" · {} need{} you", live.needs_you, if live.needs_you == 1 { "s" } else { "" }));
            }
            if live.working > 0 {
                t.push_str(&format!(" · {} working", live.working));
            }
            t.into()
        };
        let dot = if live.needs_you > 0 {
            Some(style::bad())
        } else if live.working > 0 {
            Some(style::accent())
        } else {
            None
        };
        h_flex()
            .id(("project-tab", i))
            .group(group.clone())
            .flex_none()
            .gap_1p5()
            .pl_2()
            .pr_1()
            .py_0p5()
            .rounded_md()
            .cursor_pointer()
            .border_1()
            .border_color(if active { style::border().into() } else { gpui_kit::transparent_black() })
            .when(active, |d| d.bg(style::panel_hover()))
            .hover(|d| d.bg(style::panel_hover()))
            .tooltip(move |window, cx| Tooltip::new(path_tip.clone()).build(window, cx))
            .on_click(cx.listener(move |this, _, window, cx| this.activate(Some(i), window, cx)))
            .on_mouse_down(
                MouseButton::Middle,
                cx.listener(move |this, _, window, cx| this.close_project(i, window, cx)),
            )
            .child(icons::ui("folder", px(13.), if active { style::accent() } else { style::faint() }))
            .child(
                div()
                    .max_w(px(160.))
                    .truncate()
                    .text_color(if active { style::text() } else { style::muted() })
                    .child(name),
            )
            .when_some(dot, |d, c| d.child(div().size(px(6.)).rounded_full().bg(c)))
            .when(live.needs_you > 1, |d| d.child(div().text_xs().text_color(style::bad()).child(live.needs_you.to_string())))
            .child(
                div()
                    .id(("close-project", i))
                    .p_0p5()
                    .rounded_sm()
                    .when(!active, |d| d.invisible().group_hover(group, |s| s.visible()))
                    .hover(|d| d.bg(style::border()))
                    .child(icons::ui("x", px(11.), style::faint()))
                    .on_click(cx.listener(move |this, _, window, cx| {
                        cx.stop_propagation();
                        this.close_project(i, window, cx);
                    })),
            )
    }
}

fn project_name(root: &std::path::Path) -> String {
    root.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| tilde(root))
}

impl Render for KuakyApp {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .size_full()
            .relative()
            .flex()
            .flex_col()
            .bg(style::bg())
            .text_color(style::text())
            .on_action(cx.listener(|this, _: &GoHome, window, cx| this.go_home(window, cx)))
            .on_action(cx.listener(|this, _: &NextProject, window, cx| this.cycle(1, window, cx)))
            .on_action(cx.listener(|this, _: &PrevProject, window, cx| this.cycle(-1, window, cx)))
            .on_action(cx.listener(|this, _: &CloseProject, window, cx| {
                if let Some(i) = this.active {
                    this.close_project(i, window, cx);
                }
            }))
            .on_action(cx.listener(|this, _: &OpenFolder, window, cx| this.pick_folder(window, cx)))
            .on_action(cx.listener(|this, _: &OpenSettings, window, cx| this.toggle_settings(window, cx)))
            .on_action(|_: &Minimize, window, _| window.minimize_window())
            .on_action(|_: &Zoom, window, _| window.zoom_window())
            .child(self.title_bar(window, cx))
            .child(crate::motion::fade_in(
                // Al cambiar entre Home y un proyecto, la vista nueva entra con un fundido corto.
                div().flex_1().min_h_0().map(|d| match self.current() {
                    Some(project) => d.child(project.clone()),
                    None => d.child(self.home.clone()),
                }),
                SharedString::from(format!("view-{:?}", self.active)),
                crate::motion::FAST,
            ))
            .when_some(self.settings.clone(), |d, view| {
                d.child(
                    div()
                        .id("settings-overlay")
                        .absolute()
                        .inset_0()
                        // Nothing behind the modal gets the mouse (scrolling here
                        // used to scroll the chat too).
                        .occlude()
                        .flex()
                        .items_center()
                        .justify_center()
                        .bg(style::soft(style::bg(), 0.6))
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.settings = None;
                            cx.notify();
                        }))
                        .child(div().id("settings-box").on_click(|_, _, cx| cx.stop_propagation()).child(view))
                        .with_animation("settings-in", crate::motion::once(crate::motion::FAST), |d, t| d.opacity(t)),
                )
            })
    }
}
