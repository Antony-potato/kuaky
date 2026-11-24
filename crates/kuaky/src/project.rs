//! Vista de proyecto: sesiones de agentes a la izquierda, pestañas en el
//! centro (terminales, archivos, diffs) y archivos / git / PRs a la derecha.

use crate::agent_status::AgentState;
use crate::chat_view::{ChatEvent, ChatLaunch, ChatStatus, ChatView};
use crate::search_view::{SearchEvent, SearchView};
use crate::diff_view::DiffView;
use crate::file_tree::{FileTree, FileTreeEvent};
use crate::file_view::FileView;
use crate::home::tilde;
use crate::quick_open::{QuickOpen, QuickOpenEvent};
use crate::state::{self, SavedSession};
use crate::style::{self, human};
use crate::terminal_view::{TerminalView, TerminalViewEvent};
use gpui_kit::component::resizable::{ResizablePanelEvent, ResizableState, h_resizable, resizable_panel};
use gpui_kit::component::{h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use kuaky_git::{Branch, Change, PullRequest, Repo, Worktree};
use kuaky_term::Launch;
use kuaky_usage::{Agent, UsageEvent};
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::Arc,
    time::{Duration, Instant},
};

actions!(project, [NewTerminal, CloseTab, NextTab, PrevTab, QuickOpenFile, SearchProject]);

pub fn init(cx: &mut App) {
    #[cfg(target_os = "macos")]
    cx.bind_keys([
        KeyBinding::new("cmd-t", NewTerminal, Some("Project")),
        KeyBinding::new("cmd-p", QuickOpenFile, Some("Project")),
        KeyBinding::new("cmd-shift-f", SearchProject, Some("Project")),
        KeyBinding::new("cmd-w", CloseTab, Some("Project")),
        KeyBinding::new("cmd-shift-]", NextTab, Some("Project")),
        KeyBinding::new("cmd-shift-[", PrevTab, Some("Project")),
    ]);
    #[cfg(not(target_os = "macos"))]
    cx.bind_keys([
        KeyBinding::new("ctrl-shift-t", NewTerminal, Some("Project && !Terminal")),
        KeyBinding::new("ctrl-p", QuickOpenFile, Some("Project && !Terminal")),
        KeyBinding::new("ctrl-shift-f", SearchProject, Some("Project && !Terminal")),
        KeyBinding::new("ctrl-shift-w", CloseTab, Some("Project && !Terminal")),
        KeyBinding::new("ctrl-tab", NextTab, Some("Project && !Terminal")),
        KeyBinding::new("ctrl-shift-tab", PrevTab, Some("Project && !Terminal")),
    ]);
}

/// Agente (o shell) que se puede abrir en una terminal.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
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

    /// Programa y argumentos para hablar con el agente por ACP, si está instalado.
    fn chat_command(self) -> Option<(PathBuf, Vec<String>)> {
        let (program, args): (&str, &[&str]) = match self {
            Harness::Claude => ("claude-agent-acp", &[]),
            Harness::Codex => ("codex-acp", &[]),
            Harness::Antigravity => ("agy", &["acp"]),
            Harness::Gemini => ("gemini", &["--acp"]),
            Harness::OpenCode => ("opencode", &["acp"]),
            Harness::Shell => return None,
        };
        let path = kuaky_git::find_agent(program)?;
        Some((path, args.iter().map(|a| a.to_string()).collect()))
    }

    /// Clave estable para guardar el estado.
    fn key(self) -> &'static str {
        self.command().unwrap_or("shell")
    }

    fn from_key(key: &str) -> Option<Harness> {
        Harness::AGENTS.into_iter().chain([Harness::Shell]).find(|h| h.key() == key)
    }

    /// Argumentos para retomar la última conversación en esa carpeta.
    fn resume_args(self) -> &'static str {
        match self {
            Harness::Claude => " --continue",
            Harness::Codex => " resume --last",
            _ => "",
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
    Terminal { view: Entity<TerminalView>, harness: Harness, worktree: Option<String>, cwd: PathBuf },
    File(Entity<FileView>),
    Diff(Entity<DiffView>),
    Chat { view: Entity<ChatView>, harness: Harness },
    Search(Entity<SearchView>),
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
    worktrees: Vec<Worktree>,
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
    /// Agentes encontrados en este equipo y la ruta de su CLI.
    installed: HashMap<Harness, PathBuf>,
    /// Agentes con modo chat (ACP) disponible.
    chat_agents: HashMap<Harness, (PathBuf, Vec<String>)>,
    /// Abrir cada agente nuevo en su propio worktree y rama.
    isolate: bool,
    activity: Vec<ActivityRow>,
    focus: FocusHandle,
    /// Sesiones que estaban abiertas la última vez (para reanudarlas).
    previous: Vec<SavedSession>,
    quick_open: Option<Entity<QuickOpen>>,
    layout: Entity<ResizableState>,
    lsp: Entity<crate::lsp::LspStore>,
    _subs: Vec<Subscription>,
}

impl ProjectView {
    pub fn new(root: PathBuf, usage: Option<Arc<Vec<UsageEvent>>>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let tree = cx.new(|cx| FileTree::new(root.clone(), cx));
        let sub = cx.subscribe_in(&tree, window, |this, _, ev: &FileTreeEvent, window, cx| match ev {
            FileTreeEvent::OpenFile(path) => this.open_file(path.clone(), window, cx),
        });
        let repo = Repo::discover(&root);
        let layout = cx.new(|_| ResizableState::default());
        let layout_sub = cx.subscribe(&layout, |_, layout, _: &ResizablePanelEvent, cx| {
            let sizes = layout.read(cx).sizes().clone();
            if sizes.len() == 3 {
                state::update(cx, |s| {
                    s.left_width = Some(f32::from(sizes[0]));
                    s.right_width = Some(f32::from(sizes[2]));
                });
            }
        });
        let key = root.to_string_lossy().into_owned();
        state::update(cx, |s| state::touch_recent(s, &key));
        let saved = state::get(cx).projects.get(&key).cloned().unwrap_or_default();
        let lsp = cx.new(|_| crate::lsp::LspStore::new(root.clone()));
        let lsp_sub = cx.subscribe_in(&lsp, window, |this, _, ev: &crate::lsp::LspStoreEvent, window, cx| match ev {
            crate::lsp::LspStoreEvent::OpenLocation { path, line, column } => {
                this.open_file_at(path.clone(), *line, *column, window, cx)
            }
        });
        let mut this = Self {
            activity: usage.map(|u| activity_for(&root, &u)).unwrap_or_default(),
            root,
            repo,
            tabs: Vec::new(),
            active: None,
            next_id: 0,
            tree,
            panel: SidePanel::Files,
            git: GitState::default(),
            installed: HashMap::new(),
            chat_agents: HashMap::new(),
            isolate: false,
            focus: cx.focus_handle(),
            previous: saved.sessions.clone(),
            quick_open: None,
            layout,
            lsp,
            _subs: vec![sub, layout_sub, lsp_sub],
        };
        this.detect_agents(cx);
        this.start_git_refresh(cx);
        // Reabrir los archivos que estaban abiertos.
        for file in &saved.files {
            let path = this.root.join(file);
            if path.is_file() {
                this.open_file(path, window, cx);
            }
        }
        if !saved.files.is_empty() {
            this.active = this.tabs.len().checked_sub(1);
        }
        this
    }

    /// Guarda archivos y sesiones abiertas de este proyecto.
    fn persist(&self, cx: &mut Context<Self>) {
        let files: Vec<String> = self
            .tabs
            .iter()
            .filter_map(|t| match &t.kind {
                TabKind::File(f) => f.read(cx).path.strip_prefix(&self.root).ok().map(|p| p.to_string_lossy().into_owned()),
                _ => None,
            })
            .collect();
        let mut sessions: Vec<SavedSession> = self
            .tabs
            .iter()
            .filter_map(|t| match &t.kind {
                TabKind::Terminal { harness, worktree, cwd, view } if !view.read(cx).exited => Some(SavedSession {
                    harness: harness.key().to_string(),
                    cwd: cwd.to_string_lossy().into_owned(),
                    worktree: worktree.clone(),
                }),
                _ => None,
            })
            .collect();
        // Las anteriores que no se han reanudado se conservan.
        for p in &self.previous {
            if !sessions.contains(p) {
                sessions.push(p.clone());
            }
        }
        let key = self.root.to_string_lossy().into_owned();
        state::update(cx, |s| {
            s.projects.insert(key, state::ProjectState { files, sessions });
        });
    }

    fn resume(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        if index >= self.previous.len() {
            return;
        }
        let saved = self.previous.remove(index);
        let Some(harness) = Harness::from_key(&saved.harness) else { return };
        let cwd = PathBuf::from(&saved.cwd);
        let cwd = if cwd.is_dir() { cwd } else { self.root.clone() };
        self.spawn_terminal(harness, cwd, saved.worktree, true, window, cx);
    }

    fn toggle_quick_open(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.quick_open.take().is_some() {
            cx.notify();
            return;
        }
        let qo = cx.new(|cx| QuickOpen::new(self.root.clone(), window, cx));
        self._subs.push(cx.subscribe_in(&qo, window, |this, _, ev: &QuickOpenEvent, window, cx| {
            this.quick_open = None;
            if let QuickOpenEvent::Open(path) = ev {
                this.open_file(path.clone(), window, cx);
            }
            cx.notify();
        }));
        self.quick_open = Some(qo);
        cx.notify();
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
                    let installed = Harness::AGENTS
                        .into_iter()
                        .filter_map(|h| Some((h, kuaky_git::find_agent(h.command()?)?)))
                        .collect::<HashMap<_, _>>();
                    let chat = Harness::AGENTS
                        .into_iter()
                        .filter_map(|h| Some((h, h.chat_command()?)))
                        .collect::<HashMap<_, _>>();
                    (installed, chat)
                })
                .await;
            this.update(cx, |this, cx| {
                let (found, chat) = found;
                this.chat_agents = chat;
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
            if let Ok(mode) = std::env::var("KUAKY_DEMO_LSP") {
                if let Some(TabKind::File(f)) = self.active.and_then(|i| self.tabs.get(i)).map(|t| &t.kind) {
                    f.update(cx, |f, cx| f.demo_lsp(mode, window, cx));
                }
            }
        }
        match std::env::var("KUAKY_DEMO_CHAT").as_deref() {
            // Agente falso de las pruebas de kuaky-acp: no usa red ni tokens.
            Ok("mock") => {
                let script = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../kuaky-acp/tests/mock_agent.py");
                self.chat_agents.insert(Harness::Claude, (PathBuf::from("python3"), vec![script.to_string_lossy().into_owned()]));
                self.open_chat(Harness::Claude, window, cx);
            }
            Ok(_) => self.open_chat(Harness::Claude, window, cx),
            Err(_) => {}
        }
        if std::env::var("KUAKY_DEMO_QUICKOPEN").is_ok() {
            self.toggle_quick_open(window, cx);
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
        if harness != Harness::Shell && !self.installed.contains_key(&harness) {
            // La detección en segundo plano pudo no haber terminado: buscar ahora.
            if let Some(path) = harness.command().and_then(kuaky_git::find_agent) {
                self.installed.insert(harness, path);
            }
        }
        let installed = harness == Harness::Shell || self.installed.contains_key(&harness);
        let mut cwd = self.root.clone();
        let mut worktree = None;
        if !installed {
            // La CLI no está: abrir un shell con el comando de instalación escrito, sin ejecutarlo.
            let launch = Launch { cwd, command: None, prefill: harness.install_hint().map(str::to_string), env: Vec::new() };
            self.add_terminal(harness, launch, None, None, window, cx);
            return;
        }
        if self.isolate && harness != Harness::Shell {
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
        self.spawn_terminal(harness, cwd, worktree, false, window, cx);
    }

    fn spawn_terminal(
        &mut self,
        harness: Harness,
        cwd: PathBuf,
        worktree: Option<String>,
        resume: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if harness != Harness::Shell && !self.installed.contains_key(&harness) {
            if let Some(path) = harness.command().and_then(kuaky_git::find_agent) {
                self.installed.insert(harness, path);
            }
        }
        // Si la CLI no está en el PATH del shell (p. ej. la instaló Zed), se usa su ruta completa.
        let command = harness.command().map(|name| {
            let base = match self.installed.get(&harness) {
                Some(path) if kuaky_git::find_program(name).is_none() => kuaky_git::shell_command(path),
                _ => name.to_string(),
            };
            if resume { format!("{base}{}", harness.resume_args()) } else { base }
        });
        // Claude Code: hooks de kuaky para conocer su estado exacto (sin tocar tus ajustes).
        let mut env = Vec::new();
        let mut status_file = None;
        let mut command = command;
        if harness == Harness::Claude && cfg!(unix) {
            if let (Some(settings), Some(file)) = (crate::agent_status::claude_settings(), crate::agent_status::new_status_file()) {
                command = command.map(|c| format!("{c} --settings {}", kuaky_git::shell_command(settings)));
                env.push(("KUAKY_STATUS_FILE".to_string(), file.to_string_lossy().into_owned()));
                status_file = Some(file);
            }
        }
        let launch = Launch { cwd, command, prefill: None, env };
        self.add_terminal(harness, launch, worktree, status_file, window, cx);
    }

    fn add_terminal(
        &mut self,
        harness: Harness,
        launch: Launch,
        worktree: Option<String>,
        status_file: Option<PathBuf>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let cwd = launch.cwd.clone();
        let view = cx.new(|cx| TerminalView::new(harness.label(), launch, status_file, window, cx));
        let sub = cx.subscribe_in(&view, window, move |this, view, ev: &TerminalViewEvent, window, cx| {
            match ev {
                TerminalViewEvent::Exited => this.persist(cx),
                TerminalViewEvent::AgentState => this.on_agent_state(view, harness, window, cx),
                TerminalViewEvent::TitleChanged => {}
            }
            cx.notify();
        });
        let obs = cx.observe(&view, |_, _, cx| cx.notify());
        self._subs.extend([sub, obs]);
        self.push_tab(TabKind::Terminal { view, harness, worktree, cwd }, window, cx);
    }

    /// Avisa cuando un agente termina o necesita tu permiso: dentro de kuaky si
    /// estás en otra pestaña, y con una notificación de macOS si kuaky no está
    /// en primer plano.
    fn on_agent_state(&mut self, view: &Entity<TerminalView>, harness: Harness, window: &mut Window, cx: &mut Context<Self>) {
        let t = view.read(cx);
        let Some(state) = t.agent_state.clone() else { return };
        let was_working = t.previous_state == Some(AgentState::Working);
        let (title, message) = match &state {
            AgentState::NeedsYou(msg) => (format!("{} necesita tu permiso", harness.label()), msg.clone()),
            AgentState::Done if was_working => (format!("{} terminó", harness.label()), "Es tu turno.".to_string()),
            _ => return,
        };
        let is_active_tab = self
            .active
            .and_then(|i| self.tabs.get(i))
            .is_some_and(|tab| matches!(&tab.kind, TabKind::Terminal { view: v, .. } if v == view));
        self.notify(title, message, is_active_tab, window, cx);
    }

    fn notify(&mut self, title: String, message: String, is_active_tab: bool, window: &mut Window, cx: &mut Context<Self>) {
        let project = self.root.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        let window_active = window.is_window_active();
        if !is_active_tab || !window_active {
            use gpui_kit::component::WindowExt as _;
            window.push_notification(
                gpui_kit::component::notification::Notification::new()
                    .title(format!("{title} · {project}"))
                    .message(message.clone()),
                cx,
            );
        }
        if !window_active {
            system_notification(&format!("{title} · {project}"), &message);
        }
    }

    /// Abre una terminal con `text` escrito, sin ejecutarlo: tú confirmas con Enter.
    fn prefilled_shell(&mut self, cwd: PathBuf, text: String, window: &mut Window, cx: &mut Context<Self>) {
        let launch = Launch { cwd, command: None, prefill: Some(text), env: Vec::new() };
        self.add_terminal(Harness::Shell, launch, None, None, window, cx);
    }

    fn worktree_section(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let base = self.git.default_branch.clone().unwrap_or_else(|| "main".into());
        let worktrees: Vec<Worktree> = self.git.worktrees.iter().filter(|w| !w.main).cloned().collect();
        if worktrees.is_empty() {
            return None;
        }
        let action = |id: (&'static str, usize), label: &'static str| {
            div()
                .id(id)
                .px_1p5()
                .py_0p5()
                .rounded_sm()
                .text_xs()
                .cursor_pointer()
                .text_color(style::muted())
                .hover(|d| d.bg(style::bg()).text_color(style::text()))
                .child(label)
        };
        Some(
            v_flex()
                .gap_1()
                .pb_2()
                .mb_1()
                .border_b_1()
                .border_color(style::border())
                .child(div().px_2().text_xs().text_color(style::faint()).child(format!("WORKTREES · base {base}")))
                .children(worktrees.into_iter().enumerate().map(|(i, wt)| {
                    let branch = wt.branch.clone().unwrap_or_else(|| "(sin rama)".into());
                    let name = wt.path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                    let path_q = kuaky_git::shell_command(&wt.path);
                    let (wt1, wt2, wt3) = (wt.clone(), wt.clone(), wt.clone());
                    let (b1, b2, b3, b4) = (base.clone(), branch.clone(), branch.clone(), branch.clone());
                    let root = self.root.clone();
                    v_flex()
                        .px_2()
                        .py_1()
                        .gap_0p5()
                        .rounded_md()
                        .hover(|d| d.bg(style::panel_hover()))
                        .child(
                            h_flex()
                                .gap_1p5()
                                .text_sm()
                                .child(div().text_color(style::accent()).child("⑂"))
                                .child(div().truncate().child(name)),
                        )
                        .child(div().text_xs().truncate().text_color(style::faint()).child(branch.clone()))
                        .child(
                            h_flex()
                                .gap_1()
                                .flex_wrap()
                                .child(action(("wt-diff", i), "cambios").on_click(cx.listener(move |this, _, window, cx| {
                                    let Some(repo) = this.repo.clone() else { return };
                                    let (base, wt) = (b1.clone(), wt1.clone());
                                    let title = format!("⑂ {}", wt.branch.clone().unwrap_or_default());
                                    this.open_diff(title, format!("cambios del worktree contra {base}"), move || repo.diff_worktree_branch(&base, &wt), window, cx);
                                })))
                                .child(action(("wt-term", i), "terminal").on_click(cx.listener(move |this, _, window, cx| {
                                    this.add_terminal(Harness::Shell, Launch { cwd: wt2.path.clone(), command: None, prefill: None, env: Vec::new() }, wt2.branch.clone(), None, window, cx);
                                })))
                                .child(action(("wt-pr", i), "crear PR").on_click(cx.listener(move |this, _, window, cx| {
                                    let text = format!("git push -u origin {b2} && gh pr create --fill --head {b2}");
                                    this.prefilled_shell(wt3.path.clone(), text, window, cx);
                                })))
                                .child(action(("wt-merge", i), "fusionar").on_click(cx.listener({
                                    let root = root.clone();
                                    move |this, _, window, cx| {
                                        this.prefilled_shell(root.clone(), format!("git merge --no-ff {b3}"), window, cx);
                                    }
                                })))
                                .child(action(("wt-remove", i), "eliminar").on_click(cx.listener(move |this, _, window, cx| {
                                    let text = format!("git worktree remove {path_q} && git branch -D {b4}");
                                    this.prefilled_shell(root.clone(), text, window, cx);
                                }))),
                        )
                }))
                .into_any_element(),
        )
    }

    pub fn open_chat(&mut self, harness: Harness, window: &mut Window, cx: &mut Context<Self>) {
        let Some((program, args)) = self.chat_agents.get(&harness).cloned().or_else(|| harness.chat_command()) else {
            return;
        };
        let launch = ChatLaunch { label: harness.label(), program, args, cwd: self.root.clone() };
        let view = cx.new(|cx| ChatView::new(launch, window, cx));
        let sub = cx.subscribe_in(&view, window, move |this, view, ev: &ChatEvent, window, cx| {
            let is_active_tab = this
                .active
                .and_then(|i| this.tabs.get(i))
                .is_some_and(|tab| matches!(&tab.kind, TabKind::Chat { view: v, .. } if v == view));
            match ev {
                ChatEvent::OpenDiff { title, patch } => {
                    let patch = patch.clone();
                    this.open_diff(title.clone(), "cambio propuesto por el agente".into(), move || Ok(patch), window, cx);
                }
                ChatEvent::NeedsYou(msg) => {
                    this.notify(format!("{} necesita tu permiso", harness.label()), msg.clone(), is_active_tab, window, cx)
                }
                ChatEvent::TurnDone => this.notify(format!("{} terminó", harness.label()), "Es tu turno.".into(), is_active_tab, window, cx),
                ChatEvent::Status => {}
            }
            cx.notify();
        });
        self._subs.push(sub);
        self.push_tab(TabKind::Chat { view, harness }, window, cx);
    }

    fn open_search(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(i) = self.tabs.iter().position(|t| matches!(t.kind, TabKind::Search(_))) {
            if let TabKind::Search(v) = &self.tabs[i].kind {
                v.update(cx, |v, cx| v.focus(window, cx));
            }
            self.active = Some(i);
            cx.notify();
            return;
        }
        let view = cx.new(|cx| SearchView::new(self.root.clone(), window, cx));
        self._subs.push(cx.subscribe_in(&view, window, |this, _, ev: &SearchEvent, window, cx| match ev {
            SearchEvent::Open { path, line, column } => this.open_file_at(path.clone(), *line, *column, window, cx),
        }));
        self.tabs.push(Tab { id: self.next_id, kind: TabKind::Search(view) });
        self.next_id += 1;
        self.active = Some(self.tabs.len() - 1);
        cx.notify();
    }

    /// Abre un archivo y pone el cursor en `line`/`column` (base 0).
    pub fn open_file_at(&mut self, path: PathBuf, line: u32, column: u32, window: &mut Window, cx: &mut Context<Self>) {
        self.open_file(path.clone(), window, cx);
        if let Some(TabKind::File(f)) = self.active.and_then(|i| self.tabs.get(i)).map(|t| &t.kind) {
            f.update(cx, |f, cx| f.goto(line, column, window, cx));
        }
    }

    fn push_tab(&mut self, kind: TabKind, window: &mut Window, cx: &mut Context<Self>) {
        self.tabs.push(Tab { id: self.next_id, kind });
        self.next_id += 1;
        self.activate(self.tabs.len() - 1, window, cx);
        self.persist(cx);
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
        let removed = self.tabs.remove(index);
        if let TabKind::File(f) = &removed.kind {
            if let Some(uri) = f.read(cx).lsp_uri() {
                self.lsp.update(cx, |s, _| s.detach(&uri));
            }
        }
        self.persist(cx);
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
        let lsp = self.lsp.clone();
        let view = cx.new(|cx| FileView::new(path.clone(), Some(lsp), window, cx));
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
                let (branch, changes, branches, default_branch, worktrees) = cx
                    .background_executor()
                    .spawn(async move {
                        (
                            r.current_branch().ok(),
                            r.status().unwrap_or_default(),
                            r.branches().unwrap_or_default(),
                            if with_branches { r.default_branch() } else { None },
                            r.worktrees().unwrap_or_default(),
                        )
                    })
                    .await;
                let alive = this
                    .update(cx, |this, cx| {
                        this.git.branch = branch;
                        this.git.branches = branches;
                        this.git.worktrees = worktrees;
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
            .size_full()
            .border_r_1()
            .border_color(style::border())
            .bg(style::panel())
            .child(
                v_flex()
                    .p_3()
                    .gap_2()
                    .child(side_title("Nueva sesión"))
                    .children(launchers.into_iter().map(|h| {
                        let installed = h == Harness::Shell || self.installed.contains_key(&h);
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
                            .child(harness_badge(h))
                            .child(div().flex_1().min_w_0().truncate().child(h.label()))
                            .when(self.chat_agents.contains_key(&h), |d| {
                                d.child(
                                    div()
                                        .id(SharedString::from(format!("chat-{}", h.label())))
                                        .flex_none()
                                        .px_1p5()
                                        .rounded_sm()
                                        .text_xs()
                                        .text_color(style::accent())
                                        .border_1()
                                        .border_color(style::soft(style::accent(), 0.4))
                                        .hover(|d| d.bg(style::soft(style::accent(), 0.12)))
                                        .tooltip(|window, cx| {
                                            gpui_kit::component::tooltip::Tooltip::new("Abrir en modo chat (ACP)").build(window, cx)
                                        })
                                        .child("chat")
                                        .on_click(cx.listener(move |this, _, window, cx| {
                                            cx.stop_propagation();
                                            this.open_chat(h, window, cx);
                                        })),
                                )
                            })
                            .child(div().flex_none().text_xs().text_color(style::faint()).child(if installed { "+" } else { "instalar" }))
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
                    .when(!self.tabs.iter().any(|t| matches!(t.kind, TabKind::Terminal { .. } | TabKind::Chat { .. })), |d| {
                        d.child(div().px_2().text_xs().text_color(style::faint()).child("Ninguna todavía"))
                    })
                    .children(self.tabs.iter().enumerate().filter_map(|(i, tab)| {
                        let (harness, title, status, color, worktree): (Harness, SharedString, &str, Hsla, Option<String>) = match &tab.kind {
                            TabKind::Terminal { view, harness, worktree, .. } => {
                                let t = view.read(cx);
                                let (status, color) = terminal_status(t, now);
                                (*harness, t.title.clone().unwrap_or_else(|| t.label.clone()), status, color, worktree.clone())
                            }
                            TabKind::Chat { view, harness } => {
                                let (status, color) = chat_status(view.read(cx));
                                (*harness, format!("{} · chat", harness.label()).into(), status, color, None)
                            }
                            _ => return None,
                        };
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
                                .child(harness_badge(harness))
                                .child(
                                    v_flex()
                                        .flex_1()
                                        .min_w_0()
                                        .child(div().text_sm().truncate().child(title))
                                        .child(
                                            h_flex()
                                                .gap_1()
                                                .min_w_0()
                                                .text_xs()
                                                .text_color(style::muted())
                                                .child(div().size(px(6.)).flex_none().rounded_full().bg(color))
                                                .child(div().flex_1().min_w_0().truncate().child(status)),
                                        )
                                        .when_some(worktree, |d, w| {
                                            d.child(div().text_xs().text_color(style::faint()).truncate().child(format!("⑂ {w}")))
                                        }),
                                )
                                .on_click(cx.listener(move |this, _, window, cx| this.activate(i, window, cx))),
                        )
                    })),
            )
            .when(!self.previous.is_empty(), |d| {
                d.child(
                    v_flex()
                        .p_3()
                        .gap_1()
                        .border_t_1()
                        .border_color(style::border())
                        .child(
                            h_flex()
                                .gap_2()
                                .items_center()
                                .justify_between()
                                .child(div().flex_1().min_w_0().truncate().child(side_title("Sesiones anteriores")))
                                .child(
                                    div()
                                        .id("forget-previous")
                                        .flex_none()
                                        .px_1()
                                        .text_xs()
                                        .cursor_pointer()
                                        .text_color(style::faint())
                                        .hover(|d| d.text_color(style::text()))
                                        .child("olvidar")
                                        .on_click(cx.listener(|this, _, _, cx| {
                                            this.previous.clear();
                                            this.persist(cx);
                                            cx.notify();
                                        })),
                                ),
                        )
                        .children(self.previous.iter().enumerate().filter_map(|(i, p)| {
                            let h = Harness::from_key(&p.harness)?;
                            let detail = p.worktree.clone().map(|w| format!("⑂ {w}")).unwrap_or_else(|| {
                                if h.resume_args().is_empty() { "abrir de nuevo".into() } else { "retomar conversación".into() }
                            });
                            Some(
                                h_flex()
                                    .id(("previous", i))
                                    .gap_2()
                                    .px_2()
                                    .py_1()
                                    .rounded_md()
                                    .cursor_pointer()
                                    .hover(|d| d.bg(style::panel_hover()))
                                    .child(harness_badge(h))
                                    .child(
                                        v_flex()
                                            .flex_1()
                                            .min_w_0()
                                            .child(div().text_sm().truncate().child(h.label()))
                                            .child(div().text_xs().truncate().text_color(style::faint()).child(detail)),
                                    )
                                    .child(div().flex_none().text_xs().text_color(style::accent()).child("reanudar"))
                                    .on_click(cx.listener(move |this, _, window, cx| this.resume(i, window, cx))),
                            )
                        })),
                )
            })
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
                                    .child(crate::icons::logo(crate::icons::agent(a.agent), px(18.)))
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
                let text_icon = |glyph: &'static str, color: Hsla| div().text_xs().text_color(color).child(glyph).into_any_element();
                let (icon, status, title, dirty): (AnyElement, Option<Hsla>, SharedString, bool) = match &tab.kind {
                    TabKind::Terminal { view, harness, .. } => {
                        let t = view.read(cx);
                        let (_, dot) = terminal_status(t, now);
                        let icon = match harness.agent() {
                            Some(_) => harness_badge(*harness),
                            None => text_icon(harness.glyph(), style::muted().into()),
                        };
                        (icon, Some(dot), t.title.clone().unwrap_or_else(|| t.label.clone()), false)
                    }
                    TabKind::File(f) => {
                        let f = f.read(cx);
                        (text_icon("◇", style::muted().into()), None, f.title().into(), f.dirty)
                    }
                    TabKind::Diff(d) => (text_icon("±", style::accent().into()), None, d.read(cx).title.clone(), false),
                    TabKind::Search(_) => (text_icon("⌕", style::accent().into()), None, "Buscar".into(), false),
                    TabKind::Chat { view, harness } => {
                        let (_, dot) = chat_status(view.read(cx));
                        (harness_badge(*harness), Some(dot), format!("{} · chat", harness.label()).into(), false)
                    }
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
                    .child(icon)
                    .when_some(status, |d, c| d.child(div().size(px(6.)).flex_none().rounded_full().bg(c)))
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
                TabKind::Chat { view, .. } => view.clone().into_any_element(),
                TabKind::Search(v) => v.clone().into_any_element(),
            },
            None => self.empty_state(cx).into_any_element(),
        };
        v_flex()
            .size_full()
            .min_w_0()
            .when(!self.tabs.is_empty(), |d| d.child(self.tab_bar(cx)))
            .child(div().flex_1().min_h_0().child(content))
    }

    fn empty_state(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let name = self.root.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        let mut launchers: Vec<Harness> = Harness::AGENTS.into_iter().filter(|h| self.installed.contains_key(h)).collect();
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
                    .child(harness_badge(h))
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
            .size_full()
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
        let worktrees = self.worktree_section(cx);
        div()
            .id("branches")
            .size_full()
            .overflow_y_scroll()
            .child(
                v_flex()
                    .p_2()
                    .gap_0p5()
                    .children(worktrees)
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
            .on_action(cx.listener(|this, _: &QuickOpenFile, window, cx| this.toggle_quick_open(window, cx)))
            .on_action(cx.listener(|this, _: &SearchProject, window, cx| this.open_search(window, cx)))
            .size_full()
            .relative()
            .bg(style::bg())
            .child({
                let saved = state::get(cx);
                let (left, right) = (saved.left_width.unwrap_or(248.).max(220.), saved.right_width.unwrap_or(300.));
                h_resizable("project-layout")
                    .with_state(&self.layout)
                    .child(resizable_panel().size(px(left)).size_range(px(220.)..px(420.)).child(self.left_sidebar(cx)))
                    .child(resizable_panel().child(self.center(cx)))
                    .child(resizable_panel().size(px(right)).size_range(px(220.)..px(560.)).child(self.right_panel(cx)))
            })
            .when_some(self.quick_open.clone(), |d, qo| {
                d.child(
                    div()
                        .id("quick-open-overlay")
                        .absolute()
                        .inset_0()
                        .flex()
                        .justify_center()
                        .pt(px(72.))
                        .bg(style::soft(style::bg(), 0.55))
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.quick_open = None;
                            cx.notify();
                        }))
                        .child(div().id("quick-open-box").on_click(|_, _, cx| cx.stop_propagation()).child(qo)),
                )
            })
    }
}

// ── piezas ───────────────────────────────────────────────────────────────

fn terminal_status(t: &TerminalView, now: Instant) -> (&'static str, Hsla) {
    if !t.exited {
        if let Some(state) = &t.agent_state {
            let color: Hsla = match state {
                AgentState::Working => style::accent().into(),
                AgentState::NeedsYou(_) => style::bad().into(),
                AgentState::Done => style::good().into(),
                AgentState::Ended => style::faint().into(),
            };
            return (state.label(), color);
        }
    }
    if t.exited {
        ("terminó", style::faint().into())
    } else if t.last_output.is_some_and(|o| now.duration_since(o) < Duration::from_secs(2)) {
        ("trabajando", style::accent().into())
    } else {
        ("en espera", style::good().into())
    }
}

fn chat_status(c: &ChatView) -> (&'static str, Hsla) {
    if c.is_waiting_permission() {
        return ("necesita tu permiso", style::bad().into());
    }
    match &c.status {
        ChatStatus::Connecting => ("conectando", style::faint().into()),
        ChatStatus::Ready => ("tu turno", style::good().into()),
        ChatStatus::Running => ("trabajando", style::accent().into()),
        ChatStatus::Error(_) => ("error", style::bad().into()),
        ChatStatus::Exited => ("terminó", style::faint().into()),
    }
}

/// Notificación del sistema (macOS) en segundo plano.
fn system_notification(title: &str, message: &str) {
    #[cfg(target_os = "macos")]
    {
        let esc = |s: &str| s.replace('\\', "\\\\").replace('"', "\\\"");
        let script = format!("display notification \"{}\" with title \"kuaky\" subtitle \"{}\"", esc(message), esc(title));
        std::thread::spawn(move || {
            let _ = std::process::Command::new("osascript").args(["-e", &script]).output();
        });
    }
    #[cfg(not(target_os = "macos"))]
    let _ = (title, message);
}

fn side_title(text: &'static str) -> impl IntoElement {
    div().px_2().pb_1().text_xs().text_color(style::muted()).child(text.to_uppercase())
}

/// Logo del agente; la terminal simple usa el símbolo `›`.
fn harness_badge(h: Harness) -> AnyElement {
    match h.agent() {
        Some(agent) => crate::icons::logo(crate::icons::agent(agent), px(18.)).into_any_element(),
        None => glyph_badge(h.glyph(), h.color()).into_any_element(),
    }
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
