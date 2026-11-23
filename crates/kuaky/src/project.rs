//! Vista de proyecto: sesiones de agentes a la izquierda, pestañas en el
//! centro (terminales, archivos, diffs) y archivos / git / PRs a la derecha.

use crate::diff_view::DiffView;
use crate::file_tree::{FileTree, FileTreeEvent};
use crate::file_view::FileView;
use crate::home::tilde;
use crate::style::{self, human};
use crate::terminal_view::{TerminalView, TerminalViewEvent};
use gpui_kit::component::{h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use kuaky_git::{Branch, Change, PullRequest, Repo};
use kuaky_term::Launch;
use kuaky_usage::{Agent, UsageEvent};
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::Arc,
    time::{Duration, Instant},
};

actions!(project, [NewTerminal, CloseTab, NextTab, PrevTab]);

pub fn init(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new("cmd-t", NewTerminal, Some("Project")),
        KeyBinding::new("ctrl-shift-t", NewTerminal, Some("Project")),
        KeyBinding::new("cmd-w", CloseTab, Some("Project")),
        KeyBinding::new("ctrl-shift-w", CloseTab, Some("Project")),
        KeyBinding::new("ctrl-tab", NextTab, Some("Project")),
        KeyBinding::new("ctrl-shift-tab", PrevTab, Some("Project")),
        KeyBinding::new("cmd-shift-]", NextTab, Some("Project")),
        KeyBinding::new("cmd-shift-[", PrevTab, Some("Project")),
    ]);
}

/// Agente (o shell) que se puede abrir en una terminal.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Harness {
    Claude,
    Codex,
    Antigravity,
    Gemini,
    OpenCode,
    Shell,
}

impl Harness {
    const AGENTS: [Harness; 5] = [Harness::Claude, Harness::Codex, Harness::Antigravity, Harness::Gemini, Harness::OpenCode];

    fn label(self) -> &'static str {
        match self {
            Harness::Claude => "Claude Code",
            Harness::Codex => "Codex",
            Harness::Antigravity => "Antigravity",
            Harness::Gemini => "Gemini CLI",
            Harness::OpenCode => "OpenCode",
            Harness::Shell => "Terminal",
        }
    }

    fn command(self) -> Option<&'static str> {
        match self {
            Harness::Claude => Some("claude"),
            Harness::Codex => Some("codex"),
            Harness::Antigravity => Some("agy"),
            Harness::Gemini => Some("gemini"),
            Harness::OpenCode => Some("opencode"),
            Harness::Shell => None,
        }
    }

    /// Comando sugerido para instalar la CLI cuando no está.
    fn install_hint(self) -> Option<&'static str> {
        match self {
            Harness::Claude => Some("npm install -g @anthropic-ai/claude-code"),
            Harness::Codex => Some("npm install -g @openai/codex"),
            Harness::Gemini => Some("npm install -g @google/gemini-cli"),
            Harness::OpenCode => Some("npm install -g opencode-ai"),
            Harness::Antigravity | Harness::Shell => None,
        }
    }

    fn agent(self) -> Option<Agent> {
        match self {
            Harness::Claude => Some(Agent::ClaudeCode),
            Harness::Codex => Some(Agent::Codex),
            Harness::Antigravity => Some(Agent::Antigravity),
            Harness::Gemini => Some(Agent::GeminiCli),
            Harness::OpenCode => Some(Agent::OpenCode),
            Harness::Shell => None,
        }
    }

    fn color(self) -> Rgba {
        self.agent().map(style::agent_color).unwrap_or_else(style::muted)
    }

    fn glyph(self) -> &'static str {
        self.agent().map(style::agent_glyph).unwrap_or("›")
    }
}

enum TabKind {
    Terminal { view: Entity<TerminalView>, harness: Harness, worktree: Option<String> },
    File(Entity<FileView>),
    Diff(Entity<DiffView>),
}

struct Tab {
    id: usize,
    kind: TabKind,
}

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
enum SidePanel {
    Files,
    Changes,
    Branches,
    Prs,
}

impl SidePanel {
    const ALL: [SidePanel; 4] = [SidePanel::Files, SidePanel::Changes, SidePanel::Branches, SidePanel::Prs];
    fn label(self) -> &'static str {
        match self {
            SidePanel::Files => "Archivos",
            SidePanel::Changes => "Cambios",
            SidePanel::Branches => "Ramas",
            SidePanel::Prs => "PRs",
        }
    }
}

/// Resumen de una sesión de agente que trabajó en este proyecto.
struct ActivityRow {
    agent: Agent,
    model: Arc<str>,
    last: jiff::Timestamp,
    turns: u64,
    tokens: u64,
}

#[derive(Default)]
struct GitState {
    branch: Option<String>,
    default_branch: Option<String>,
    changes: Vec<Change>,
    branches: Vec<Branch>,
    prs: Option<Result<Vec<PullRequest>, String>>,
    prs_loading: bool,
}

pub struct ProjectView {
    root: PathBuf,
    repo: Option<Repo>,
    tabs: Vec<Tab>,
    active: Option<usize>,
    next_id: usize,
    tree: Entity<FileTree>,
    panel: SidePanel,
    git: GitState,
    installed: Vec<Harness>,
    /// Abrir cada agente nuevo en su propio worktree y rama.
    isolate: bool,
    activity: Vec<ActivityRow>,
    focus: FocusHandle,
    _subs: Vec<Subscription>,
}

impl ProjectView {
    pub fn new(root: PathBuf, usage: Option<Arc<Vec<UsageEvent>>>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let tree = cx.new(|cx| FileTree::new(root.clone(), cx));
        let sub = cx.subscribe_in(&tree, window, |this, _, ev: &FileTreeEvent, window, cx| match ev {
            FileTreeEvent::OpenFile(path) => this.open_file(path.clone(), window, cx),
        });
        let repo = Repo::discover(&root);
        let this = Self {
            activity: usage.map(|u| activity_for(&root, &u)).unwrap_or_default(),
            root,
            repo,
            tabs: Vec::new(),
            active: None,
            next_id: 0,
            tree,
            panel: SidePanel::Files,
            git: GitState::default(),
            installed: Vec::new(),
            isolate: false,
            focus: cx.focus_handle(),
            _subs: vec![sub],
        };
        this.detect_agents(cx);
        this.start_git_refresh(cx);
        this
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    // ── agentes y terminales ───────────────────────────────────────────

    fn detect_agents(&self, cx: &mut Context<Self>) {
        cx.spawn(async move |this, cx| {
            let found = cx
                .background_executor()
                .spawn(async {
                    Harness::AGENTS
                        .into_iter()
                        .filter(|h| h.command().is_some_and(|c| kuaky_git::find_program(c).is_some()))
                        .collect::<Vec<_>>()
                })
                .await;
            this.update(cx, |this, cx| {
                this.installed = found;
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Estado de demostración para las capturas de desarrollo
    /// (`KUAKY_DEMO_PANEL`, `KUAKY_DEMO_FILE`, `KUAKY_DEMO_DIFF`).
    #[cfg(feature = "screenshot")]
    pub fn demo(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Ok(panel) = std::env::var("KUAKY_DEMO_PANEL") {
            let p = match panel.as_str() {
                "changes" => SidePanel::Changes,
                "branches" => SidePanel::Branches,
                "prs" => SidePanel::Prs,
                _ => SidePanel::Files,
            };
            self.set_panel(p, cx);
        }
        if let Ok(file) = std::env::var("KUAKY_DEMO_FILE") {
            self.open_file(self.root.join(file), window, cx);
        }
        if std::env::var("KUAKY_DEMO_DIFF").is_ok() {
            if let Some(repo) = self.repo.clone() {
                self.open_diff("Cambios locales".into(), "sin commit".into(), move || repo.diff_worktree(), window, cx);
            }
        }
    }

    pub fn set_usage(&mut self, events: &[UsageEvent], cx: &mut Context<Self>) {
        self.activity = activity_for(&self.root, events);
        cx.notify();
    }

    pub fn open_terminal(&mut self, harness: Harness, window: &mut Window, cx: &mut Context<Self>) {
        let installed = harness == Harness::Shell || self.installed.contains(&harness);
        let mut cwd = self.root.clone();
        let mut worktree = None;
        if installed && self.isolate && harness != Harness::Shell {
            if let Some(repo) = &self.repo {
                let name = format!(
                    "{}-{}",
                    harness.command().unwrap_or("agente"),
                    jiff::Zoned::now().strftime("%m%d-%H%M%S")
                );
                match repo.create_worktree(&name) {
                    Ok(path) => {
                        cwd = path;
                        worktree = Some(format!("kuaky/{name}"));
                    }
                    Err(err) => tracing::warn!(%err, "no se pudo crear el worktree"),
                }
            }
        }
        let launch = if installed {
            Launch { cwd, command: harness.command().map(str::to_string), prefill: None }
        } else {
            // La CLI no está: abrir un shell con el comando de instalación escrito, sin ejecutarlo.
            Launch { cwd: self.root.clone(), command: None, prefill: harness.install_hint().map(str::to_string) }
        };
        let view = cx.new(|cx| TerminalView::new(harness.label(), launch, window, cx));
        let sub = cx.subscribe(&view, |_, _, _: &TerminalViewEvent, cx| cx.notify());
        let obs = cx.observe(&view, |_, _, cx| cx.notify());
        self._subs.extend([sub, obs]);
        self.push_tab(TabKind::Terminal { view, harness, worktree }, window, cx);
    }

    fn push_tab(&mut self, kind: TabKind, window: &mut Window, cx: &mut Context<Self>) {
        self.tabs.push(Tab { id: self.next_id, kind });
        self.next_id += 1;
        self.activate(self.tabs.len() - 1, window, cx);
    }

    fn activate(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        self.active = Some(index);
        if let Some(Tab { kind: TabKind::Terminal { view, .. }, .. }) = self.tabs.get(index) {
            let handle = view.read(cx).focus_handle(cx);
            window.focus(&handle, cx);
        }
        cx.notify();
    }

    fn close_tab(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        if index >= self.tabs.len() {
            return;
        }
        self.tabs.remove(index);
        if self.tabs.is_empty() {
            self.active = None;
            window.focus(&self.focus, cx);
        } else {
            let next = index.min(self.tabs.len() - 1);
            self.activate(next, window, cx);
        }
        cx.notify();
    }

    fn open_file(&mut self, path: PathBuf, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(i) = self
            .tabs
            .iter()
            .position(|t| matches!(&t.kind, TabKind::File(f) if f.read(cx).path == path))
        {
            self.activate(i, window, cx);
            return;
        }
        let view = cx.new(|cx| FileView::new(path.clone(), window, cx));
        self._subs.push(cx.observe(&view, |_, _, cx| cx.notify()));
        self.tree.update(cx, |t, cx| t.select(&path, cx));
        self.push_tab(TabKind::File(view), window, cx);
    }

    fn open_diff(
        &mut self,
        title: String,
        subtitle: String,
        load: impl FnOnce() -> anyhow::Result<String> + Send + 'static,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(i) = self
            .tabs
            .iter()
            .position(|t| matches!(&t.kind, TabKind::Diff(d) if d.read(cx).title == title))
        {
            self.tabs.remove(i);
        }
        let view = cx.new(|cx| DiffView::new(title, subtitle, load, cx));
        self.push_tab(TabKind::Diff(view), window, cx);
    }

    // ── git ─────────────────────────────────────────────────────────────

    fn start_git_refresh(&self, cx: &mut Context<Self>) {
        let Some(repo) = self.repo.clone() else { return };
        cx.spawn(async move |this, cx| {
            let mut first = true;
            loop {
                let r = repo.clone();
                let with_branches = first;
                let (branch, changes, branches, default_branch) = cx
                    .background_executor()
                    .spawn(async move {
                        (
                            r.current_branch().ok(),
                            r.status().unwrap_or_default(),
                            r.branches().unwrap_or_default(),
                            if with_branches { r.default_branch() } else { None },
                        )
                    })
                    .await;
                let alive = this
                    .update(cx, |this, cx| {
                        this.git.branch = branch;
                        this.git.branches = branches;
                        if first {
                            this.git.default_branch = default_branch;
                        }
                        if this.git.changes != changes {
                            this.git.changes = changes;
                            let root = repo.root().to_path_buf();
                            let changes = this.git.changes.clone();
                            this.tree.update(cx, |t, cx| t.set_changes(&root, &changes, cx));
                        }
                        cx.notify();
                    })
                    .is_ok();
                if !alive {
                    break;
                }
                first = false;
                cx.background_executor().timer(Duration::from_secs(3)).await;
            }
        })
        .detach();
    }

    fn load_prs(&mut self, cx: &mut Context<Self>) {
        let Some(repo) = self.repo.clone() else { return };
        if self.git.prs_loading {
            return;
        }
        self.git.prs_loading = true;
        cx.spawn(async move |this, cx| {
            let prs = cx.background_executor().spawn(async move { repo.pull_requests() }).await;
            this.update(cx, |this, cx| {
                this.git.prs_loading = false;
                this.git.prs = Some(prs.map_err(|e| format!("{e:#}")));
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn set_panel(&mut self, panel: SidePanel, cx: &mut Context<Self>) {
        self.panel = panel;
        if panel == SidePanel::Prs && self.git.prs.is_none() {
            self.load_prs(cx);
        }
        cx.notify();
    }

    fn checkout(&mut self, branch: String, cx: &mut Context<Self>) {
        let Some(repo) = self.repo.clone() else { return };
        cx.spawn(async move |this, cx| {
            let result = cx.background_executor().spawn(async move { repo.checkout(&branch) }).await;
            this.update(cx, |this, cx| {
                if let Err(err) = result {
                    tracing::warn!(%err, "checkout falló");
                    this.git.prs = this.git.prs.take();
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    // ── render ──────────────────────────────────────────────────────────

    fn left_sidebar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let now = Instant::now();
        let mut launchers: Vec<Harness> = Harness::AGENTS.to_vec();
        launchers.push(Harness::Shell);

        v_flex()
            .w(px(232.))
            .h_full()
            .flex_none()
            .border_r_1()
            .border_color(style::border())
            .bg(style::panel())
            .child(
                v_flex()
                    .p_3()
                    .gap_2()
                    .child(side_title("Nueva sesión"))
                    .children(launchers.into_iter().map(|h| {
                        let installed = h == Harness::Shell || self.installed.contains(&h);
                        let tip: SharedString = if installed {
                            format!("Abrir {} en esta carpeta", h.label()).into()
                        } else {
                            match h.install_hint() {
                                Some(cmd) => format!("No está instalado. Clic para abrir una terminal con: {cmd}").into(),
                                None => format!("No está instalado: instala la CLI `{}`", h.command().unwrap_or_default()).into(),
                            }
                        };
                        h_flex()
                            .id(SharedString::from(format!("launch-{}", h.label())))
                            .gap_2()
                            .px_2()
                            .py_1p5()
                            .rounded_md()
                            .text_sm()
                            .cursor_pointer()
                            .when(!installed, |d| d.opacity(0.45))
                            .hover(|d| d.bg(style::panel_hover()))
                            .tooltip(move |window, cx| gpui_kit::component::tooltip::Tooltip::new(tip.clone()).build(window, cx))
                            .child(glyph_badge(h.glyph(), h.color()))
                            .child(div().flex_1().child(h.label()))
                            .child(div().text_xs().text_color(style::faint()).child(if installed { "+" } else { "instalar" }))
                            .on_click(cx.listener(move |this, _, window, cx| this.open_terminal(h, window, cx)))
                    }))
                    .when(self.repo.is_some(), |d| {
                        let on = self.isolate;
                        d.child(
                            h_flex()
                                .id("isolate")
                                .mt_1()
                                .gap_2()
                                .px_2()
                                .py_1()
                                .rounded_md()
                                .text_xs()
                                .cursor_pointer()
                                .text_color(if on { style::text() } else { style::muted() })
                                .hover(|d| d.bg(style::panel_hover()))
                                .tooltip(|window, cx| {
                                    gpui_kit::component::tooltip::Tooltip::new(
                                        "Cada agente nuevo trabaja en su propio worktree y rama kuaky/…, sin tocar tu carpeta",
                                    )
                                    .build(window, cx)
                                })
                                .child(
                                    div()
                                        .w(px(26.))
                                        .h(px(14.))
                                        .p(px(2.))
                                        .rounded_full()
                                        .flex()
                                        .when(on, |d| d.justify_end())
                                        .bg(if on { style::soft(style::accent(), 0.8) } else { style::soft(style::faint(), 0.6) })
                                        .child(div().size(px(10.)).rounded_full().bg(style::text())),
                                )
                                .child("Aislar en worktree")
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.isolate = !this.isolate;
                                    cx.notify();
                                })),
                        )
                    }),
            )
            .child(
                v_flex()
                    .p_3()
                    .gap_1()
                    .border_t_1()
                    .border_color(style::border())
                    .child(side_title("Sesiones abiertas"))
                    .when(!self.tabs.iter().any(|t| matches!(t.kind, TabKind::Terminal { .. })), |d| {
                        d.child(div().px_2().text_xs().text_color(style::faint()).child("Ninguna todavía"))
                    })
                    .children(self.tabs.iter().enumerate().filter_map(|(i, tab)| {
                        let TabKind::Terminal { view, harness, worktree } = &tab.kind else { return None };
                        let t = view.read(cx);
                        let (status, color) = terminal_status(t, now);
                        let selected = self.active == Some(i);
                        Some(
                            h_flex()
                                .id(("session", tab.id))
                                .gap_2()
                                .px_2()
                                .py_1()
                                .rounded_md()
                                .cursor_pointer()
                                .when(selected, |d| d.bg(style::panel_hover()))
                                .hover(|d| d.bg(style::panel_hover()))
                                .child(glyph_badge(harness.glyph(), harness.color()))
                                .child(
                                    v_flex()
                                        .flex_1()
                                        .min_w_0()
                                        .child(div().text_sm().truncate().child(t.title.clone().unwrap_or_else(|| t.label.clone())))
                                        .child(
                                            h_flex()
                                                .gap_1()
                                                .text_xs()
                                                .text_color(style::muted())
                                                .child(div().size(px(6.)).rounded_full().bg(color))
                                                .child(status),
                                        )
                                        .when_some(worktree.clone(), |d, w| {
                                            d.child(div().text_xs().text_color(style::faint()).truncate().child(format!("⑂ {w}")))
                                        }),
                                )
                                .on_click(cx.listener(move |this, _, window, cx| this.activate(i, window, cx))),
                        )
                    })),
            )
            .child(
                v_flex()
                    .flex_1()
                    .min_h_0()
                    .p_3()
                    .gap_1()
                    .border_t_1()
                    .border_color(style::border())
                    .child(side_title("Actividad en este proyecto"))
                    .when(self.activity.is_empty(), |d| {
                        d.child(div().px_2().text_xs().text_color(style::faint()).child("Sin sesiones registradas"))
                    })
                    .child(
                        div().id("activity").flex_1().overflow_y_scroll().child(v_flex().gap_1().children(
                            self.activity.iter().take(30).map(|a| {
                                h_flex()
                                    .gap_2()
                                    .px_2()
                                    .py_1()
                                    .child(glyph_badge(style::agent_glyph(a.agent), style::agent_color(a.agent)))
                                    .child(
                                        v_flex()
                                            .flex_1()
                                            .min_w_0()
                                            .child(div().text_xs().truncate().child(format!(
                                                "{} · {}",
                                                style::model_name(&a.model),
                                                ago(a.last)
                                            )))
                                            .child(div().text_xs().text_color(style::faint()).child(format!(
                                                "{} resp · {} tokens",
                                                a.turns,
                                                human(a.tokens)
                                            ))),
                                    )
                            }),
                        )),
                    ),
            )
    }

    fn tab_bar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let now = Instant::now();
        h_flex()
            .id("tabs")
            .h(px(36.))
            .flex_none()
            .overflow_x_scroll()
            .border_b_1()
            .border_color(style::border())
            .bg(style::panel())
            .children(self.tabs.iter().enumerate().map(|(i, tab)| {
                let active = self.active == Some(i);
                let (icon, color, title, dirty): (&str, Hsla, SharedString, bool) = match &tab.kind {
                    TabKind::Terminal { view, harness, .. } => {
                        let t = view.read(cx);
                        let (_, dot) = terminal_status(t, now);
                        (harness.glyph(), dot, t.title.clone().unwrap_or_else(|| t.label.clone()), false)
                    }
                    TabKind::File(f) => {
                        let f = f.read(cx);
                        ("◇", style::muted().into(), f.title().into(), f.dirty)
                    }
                    TabKind::Diff(d) => ("±", style::accent().into(), d.read(cx).title.clone(), false),
                };
                h_flex()
                    .id(("tab", tab.id))
                    .h_full()
                    .px_3()
                    .gap_2()
                    .max_w(px(240.))
                    .text_sm()
                    .cursor_pointer()
                    .border_r_1()
                    .border_color(style::border())
                    .text_color(if active { style::text() } else { style::muted() })
                    .when(active, |d| d.bg(style::bg()))
                    .hover(|d| d.text_color(style::text()))
                    .child(div().text_xs().text_color(color).child(icon))
                    .child(div().truncate().child(title))
                    .when(dirty, |d| d.child(div().text_color(style::accent()).child("●")))
                    .child(
                        div()
                            .id(("close", tab.id))
                            .px_1()
                            .rounded_sm()
                            .text_xs()
                            .text_color(style::faint())
                            .hover(|d| d.bg(style::panel_hover()).text_color(style::text()))
                            .child("✕")
                            .on_click(cx.listener(move |this, _, window, cx| {
                                cx.stop_propagation();
                                this.close_tab(i, window, cx);
                            })),
                    )
                    .on_click(cx.listener(move |this, _, window, cx| this.activate(i, window, cx)))
            }))
    }

    fn center(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let content = match self.active.and_then(|i| self.tabs.get(i)) {
            Some(tab) => match &tab.kind {
                TabKind::Terminal { view, .. } => view.clone().into_any_element(),
                TabKind::File(v) => v.clone().into_any_element(),
                TabKind::Diff(v) => v.clone().into_any_element(),
            },
            None => self.empty_state(cx).into_any_element(),
        };
        v_flex()
            .flex_1()
            .min_w_0()
            .h_full()
            .when(!self.tabs.is_empty(), |d| d.child(self.tab_bar(cx)))
            .child(div().flex_1().min_h_0().child(content))
    }

    fn empty_state(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let name = self.root.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        let mut launchers: Vec<Harness> = self.installed.clone();
        launchers.push(Harness::Shell);
        v_flex()
            .size_full()
            .items_center()
            .justify_center()
            .gap_4()
            .child(div().text_2xl().font_weight(FontWeight::SEMIBOLD).child(name))
            .child(div().text_sm().text_color(style::muted()).child(match &self.git.branch {
                Some(b) => format!("{} · rama {b}", tilde(&self.root)),
                None => tilde(&self.root),
            }))
            .child(h_flex().gap_2().mt_2().children(launchers.into_iter().map(|h| {
                h_flex()
                    .id(SharedString::from(format!("empty-{}", h.label())))
                    .gap_2()
                    .px_3()
                    .py_2()
                    .rounded_lg()
                    .border_1()
                    .border_color(style::border())
                    .bg(style::panel())
                    .text_sm()
                    .cursor_pointer()
                    .hover(|d| d.bg(style::panel_hover()))
                    .child(glyph_badge(h.glyph(), h.color()))
                    .child(h.label())
                    .on_click(cx.listener(move |this, _, window, cx| this.open_terminal(h, window, cx)))
            })))
            .child(div().text_xs().text_color(style::faint()).child("⌘T nueva terminal · ⌘W cerrar pestaña · ⌘O abrir carpeta"))
    }

    fn right_panel(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let counts: HashMap<SidePanel, usize> = HashMap::from([
            (SidePanel::Changes, self.git.changes.len()),
            (SidePanel::Branches, self.git.branches.len()),
            (SidePanel::Prs, self.git.prs.as_ref().and_then(|p| p.as_ref().ok()).map_or(0, |p| p.len())),
        ]);
        v_flex()
            .w(px(300.))
            .h_full()
            .flex_none()
            .border_l_1()
            .border_color(style::border())
            .bg(style::panel())
            .child(
                h_flex()
                    .p_2()
                    .gap_1()
                    .border_b_1()
                    .border_color(style::border())
                    .children(SidePanel::ALL.into_iter().map(|p| {
                        let selected = p == self.panel;
                        let count = counts.get(&p).copied().unwrap_or(0);
                        h_flex()
                            .id(SharedString::from(format!("panel-{}", p.label())))
                            .flex_1()
                            .justify_center()
                            .gap_1()
                            .py_1()
                            .rounded_md()
                            .text_xs()
                            .cursor_pointer()
                            .text_color(if selected { style::text() } else { style::muted() })
                            .when(selected, |d| d.bg(style::panel_hover()))
                            .hover(|d| d.text_color(style::text()))
                            .child(p.label())
                            .when(count > 0 && p != SidePanel::Files, |d| {
                                d.child(div().text_color(style::accent()).child(count.to_string()))
                            })
                            .on_click(cx.listener(move |this, _, _, cx| this.set_panel(p, cx)))
                    })),
            )
            .when_some(self.git.branch.clone(), |d, b| {
                d.child(
                    h_flex()
                        .px_3()
                        .py_2()
                        .gap_2()
                        .text_xs()
                        .border_b_1()
                        .border_color(style::border())
                        .child(div().text_color(style::muted()).child("rama"))
                        .child(div().text_color(style::accent()).truncate().child(b)),
                )
            })
            .child(div().flex_1().min_h_0().child(match self.panel {
                SidePanel::Files => self.tree.clone().into_any_element(),
                SidePanel::Changes => self.changes_panel(cx).into_any_element(),
                SidePanel::Branches => self.branches_panel(cx).into_any_element(),
                SidePanel::Prs => self.prs_panel(cx).into_any_element(),
            }))
    }

    fn changes_panel(&self, cx: &mut Context<Self>) -> impl IntoElement {
        if self.repo.is_none() {
            return panel_message("Esta carpeta no es un repositorio git.").into_any_element();
        }
        div()
            .id("changes")
            .size_full()
            .overflow_y_scroll()
            .child(
                v_flex()
                    .p_2()
                    .gap_0p5()
                    .when(self.git.changes.is_empty(), |d| d.child(panel_message("Sin cambios locales.")))
                    .when(!self.git.changes.is_empty(), |d| {
                        d.child(
                            panel_button("Ver todos los cambios").on_click(cx.listener(|this, _, window, cx| {
                                let Some(repo) = this.repo.clone() else { return };
                                let branch = this.git.branch.clone().unwrap_or_default();
                                this.open_diff(
                                    "Cambios locales".into(),
                                    format!("{branch} · sin commit"),
                                    move || repo.diff_worktree(),
                                    window,
                                    cx,
                                );
                            })),
                        )
                    })
                    .children(self.git.changes.iter().enumerate().map(|(i, c)| {
                        let change = c.clone();
                        let color: Hsla = match c.kind {
                            kuaky_git::ChangeKind::Untracked | kuaky_git::ChangeKind::Added => style::good().into(),
                            kuaky_git::ChangeKind::Deleted | kuaky_git::ChangeKind::Conflicted => style::bad().into(),
                            _ => style::accent().into(),
                        };
                        let (dir, file) = split_path(&c.path);
                        h_flex()
                            .id(("change", i))
                            .gap_2()
                            .px_2()
                            .py_1()
                            .rounded_md()
                            .text_sm()
                            .cursor_pointer()
                            .hover(|d| d.bg(style::panel_hover()))
                            .child(div().w(px(12.)).text_xs().text_color(color).child(c.kind.letter()))
                            .child(
                                h_flex()
                                    .flex_1()
                                    .min_w_0()
                                    .gap_1()
                                    .child(div().flex_none().child(file))
                                    .child(div().truncate().text_xs().text_color(style::faint()).child(dir)),
                            )
                            .on_click(cx.listener(move |this, _, window, cx| {
                                let Some(repo) = this.repo.clone() else { return };
                                let ch = change.clone();
                                this.open_diff(ch.path.clone(), "cambio local".into(), move || repo.diff_change(&ch), window, cx);
                            }))
                    })),
            )
            .into_any_element()
    }

    fn branches_panel(&self, cx: &mut Context<Self>) -> impl IntoElement {
        if self.repo.is_none() {
            return panel_message("Esta carpeta no es un repositorio git.").into_any_element();
        }
        let base = self.git.default_branch.clone();
        div()
            .id("branches")
            .size_full()
            .overflow_y_scroll()
            .child(
                v_flex()
                    .p_2()
                    .gap_0p5()
                    .when_some(base.clone(), |d, b| {
                        d.child(div().px_2().pb_1().text_xs().text_color(style::faint()).child(format!("clic: diff contra {b}")))
                    })
                    .children(self.git.branches.iter().enumerate().map(|(i, b)| {
                        let name = b.name.clone();
                        let checkout_name = b.name.clone();
                        let base = base.clone();
                        let is_base = base.as_deref() == Some(b.name.as_str());
                        h_flex()
                            .id(("branch", i))
                            .gap_2()
                            .px_2()
                            .py_1()
                            .rounded_md()
                            .cursor_pointer()
                            .hover(|d| d.bg(style::panel_hover()))
                            .child(
                                div()
                                    .w(px(8.))
                                    .text_xs()
                                    .text_color(style::accent())
                                    .child(if b.current { "●" } else { "" }),
                            )
                            .child(
                                v_flex()
                                    .flex_1()
                                    .min_w_0()
                                    .child(
                                        h_flex()
                                            .gap_1()
                                            .text_sm()
                                            .child(div().truncate().child(b.name.clone()))
                                            .when(b.ahead > 0, |d| d.child(div().text_xs().text_color(style::good()).child(format!("↑{}", b.ahead))))
                                            .when(b.behind > 0, |d| d.child(div().text_xs().text_color(style::bad()).child(format!("↓{}", b.behind)))),
                                    )
                                    .child(div().text_xs().text_color(style::faint()).truncate().child(format!(
                                        "{} · {}",
                                        ago_unix(b.updated),
                                        b.subject
                                    ))),
                            )
                            .when(!b.current, |d| {
                                d.child(
                                    div()
                                        .id(("checkout", i))
                                        .px_1p5()
                                        .rounded_sm()
                                        .text_xs()
                                        .text_color(style::muted())
                                        .hover(|d| d.bg(style::bg()).text_color(style::text()))
                                        .child("cambiar")
                                        .on_click(cx.listener(move |this, _, _, cx| {
                                            cx.stop_propagation();
                                            this.checkout(checkout_name.clone(), cx);
                                        })),
                                )
                            })
                            .on_click(cx.listener(move |this, _, window, cx| {
                                let Some(repo) = this.repo.clone() else { return };
                                let head = name.clone();
                                let Some(base) = base.clone().filter(|_| !is_base) else { return };
                                let b2 = base.clone();
                                let h2 = head.clone();
                                this.open_diff(
                                    format!("{head} vs {base}"),
                                    format!("lo que {head} agrega sobre {base}"),
                                    move || repo.diff_refs(&b2, &h2),
                                    window,
                                    cx,
                                );
                            }))
                    })),
            )
            .into_any_element()
    }

    fn prs_panel(&self, cx: &mut Context<Self>) -> impl IntoElement {
        if self.repo.is_none() {
            return panel_message("Esta carpeta no es un repositorio git.").into_any_element();
        }
        let body = match &self.git.prs {
            None => panel_message("Cargando PRs…").into_any_element(),
            Some(Err(e)) => panel_message(e.clone()).into_any_element(),
            Some(Ok(prs)) if prs.is_empty() => panel_message("No hay PRs abiertos.").into_any_element(),
            Some(Ok(prs)) => v_flex()
                .gap_0p5()
                .children(prs.iter().map(|pr| {
                    let number = pr.number;
                    let title = pr.title.clone();
                    let review = match pr.review_decision.as_deref() {
                        Some("APPROVED") => Some(("aprobado", style::good())),
                        Some("CHANGES_REQUESTED") => Some(("cambios pedidos", style::bad())),
                        _ => None,
                    };
                    v_flex()
                        .id(("pr", number as usize))
                        .px_2()
                        .py_1p5()
                        .rounded_md()
                        .cursor_pointer()
                        .hover(|d| d.bg(style::panel_hover()))
                        .child(
                            h_flex()
                                .gap_1p5()
                                .text_sm()
                                .child(div().text_color(style::muted()).child(format!("#{number}")))
                                .child(div().truncate().child(pr.title.clone())),
                        )
                        .child(
                            h_flex()
                                .gap_2()
                                .text_xs()
                                .text_color(style::faint())
                                .child(pr.author.login.clone())
                                .child(format!("{} → {}", pr.head_ref_name, pr.base_ref_name))
                                .child(div().text_color(style::good()).child(format!("+{}", pr.additions)))
                                .child(div().text_color(style::bad()).child(format!("−{}", pr.deletions)))
                                .when(pr.is_draft, |d| d.child("borrador"))
                                .when_some(review, |d, (t, c)| d.child(div().text_color(c).child(t))),
                        )
                        .on_click(cx.listener(move |this, _, window, cx| {
                            let Some(repo) = this.repo.clone() else { return };
                            this.open_diff(format!("PR #{number}"), title.clone(), move || repo.pr_diff(number), window, cx);
                        }))
                }))
                .into_any_element(),
        };
        div()
            .id("prs")
            .size_full()
            .overflow_y_scroll()
            .child(
                v_flex()
                    .p_2()
                    .gap_1()
                    .child(panel_button(if self.git.prs_loading { "Actualizando…" } else { "Actualizar" }).on_click(
                        cx.listener(|this, _, _, cx| {
                            this.load_prs(cx);
                            cx.notify();
                        }),
                    ))
                    .child(body),
            )
            .into_any_element()
    }
}

impl Render for ProjectView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        h_flex()
            .key_context("Project")
            .track_focus(&self.focus)
            .on_action(cx.listener(|this, _: &NewTerminal, window, cx| this.open_terminal(Harness::Shell, window, cx)))
            .on_action(cx.listener(|this, _: &CloseTab, window, cx| {
                if let Some(i) = this.active {
                    this.close_tab(i, window, cx);
                }
            }))
            .on_action(cx.listener(|this, _: &NextTab, window, cx| {
                if let Some(i) = this.active {
                    let n = this.tabs.len();
                    this.activate((i + 1) % n, window, cx);
                }
            }))
            .on_action(cx.listener(|this, _: &PrevTab, window, cx| {
                if let Some(i) = this.active {
                    let n = this.tabs.len();
                    this.activate((i + n - 1) % n, window, cx);
                }
            }))
            .size_full()
            .bg(style::bg())
            .child(self.left_sidebar(cx))
            .child(self.center(cx))
            .child(self.right_panel(cx))
    }
}

// ── piezas ───────────────────────────────────────────────────────────────

fn terminal_status(t: &TerminalView, now: Instant) -> (&'static str, Hsla) {
    if t.exited {
        ("terminó", style::faint().into())
    } else if t.last_output.is_some_and(|o| now.duration_since(o) < Duration::from_secs(2)) {
        ("trabajando", style::accent().into())
    } else {
        ("en espera", style::good().into())
    }
}

fn side_title(text: &'static str) -> impl IntoElement {
    div().px_2().pb_1().text_xs().text_color(style::muted()).child(text.to_uppercase())
}

fn glyph_badge(glyph: &'static str, color: Rgba) -> impl IntoElement {
    div()
        .size(px(18.))
        .flex_none()
        .flex()
        .items_center()
        .justify_center()
        .rounded_md()
        .bg(style::soft(color, 0.18))
        .text_color(color)
        .text_xs()
        .font_weight(FontWeight::BOLD)
        .child(glyph)
}

fn panel_message(text: impl Into<SharedString>) -> impl IntoElement {
    div().p_3().text_xs().text_color(style::muted()).child(text.into())
}

fn panel_button(label: &'static str) -> Stateful<Div> {
    div()
        .id(label)
        .mb_1()
        .px_2()
        .py_1()
        .rounded_md()
        .text_xs()
        .text_center()
        .cursor_pointer()
        .border_1()
        .border_color(style::border())
        .text_color(style::muted())
        .hover(|d| d.bg(style::panel_hover()).text_color(style::text()))
        .child(label)
}

fn split_path(path: &str) -> (String, String) {
    match path.rsplit_once('/') {
        Some((dir, file)) => (dir.to_string(), file.to_string()),
        None => (String::new(), path.to_string()),
    }
}

fn ago(ts: jiff::Timestamp) -> String {
    ago_unix(ts.as_second())
}

fn ago_unix(secs: i64) -> String {
    let now = jiff::Timestamp::now().as_second();
    match (now - secs).max(0) {
        s if s < 60 => "ahora".into(),
        s if s < 3600 => format!("hace {} min", s / 60),
        s if s < 86_400 => format!("hace {} h", s / 3600),
        s => format!("hace {} d", s / 86_400),
    }
}

/// Sesiones de agentes cuya carpeta está dentro de `root`, de la más reciente a
/// la más antigua.
fn activity_for(root: &Path, events: &[UsageEvent]) -> Vec<ActivityRow> {
    let mut by_session: HashMap<(Agent, Arc<str>), ActivityRow> = HashMap::new();
    for e in events {
        let Some(p) = &e.project else { continue };
        if !Path::new(&**p).starts_with(root) {
            continue;
        }
        let row = by_session.entry((e.agent, e.session.clone())).or_insert_with(|| ActivityRow {
            agent: e.agent,
            model: e.model.clone(),
            last: e.ts,
            turns: 0,
            tokens: 0,
        });
        row.turns += 1;
        row.tokens += e.input_total() + e.output;
        if e.ts >= row.last {
            row.last = e.ts;
            row.model = e.model.clone();
        }
    }
    let mut rows: Vec<ActivityRow> = by_session.into_values().collect();
    rows.sort_by_key(|r| std::cmp::Reverse(r.last));
    rows
}
