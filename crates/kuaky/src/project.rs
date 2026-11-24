//! Project workspace view: open agent sessions on the left, tabs in the center,
//! collapsible files/git/PRs/graph + embedded terminals on the right, and a
//! compact activity/status bar at the bottom.

use crate::agent_status::AgentState;
use crate::chat_view::{ChatEvent, ChatLaunch, ChatStatus, ChatView};
use crate::diff_view::DiffView;
use crate::file_tree::{FileTree, FileTreeEvent};
use crate::file_view::FileView;
use crate::home::tilde;
use crate::icons;
use crate::quick_open::{QuickOpen, QuickOpenEvent};
use crate::search_view::{SearchEvent, SearchView};
use crate::state::{self, SavedSession};
use crate::style::{self, human};
use crate::terminal_view::{TerminalView, TerminalViewEvent};
use gpui_kit::component::resizable::{ResizablePanelEvent, ResizableState, h_resizable, resizable_panel, v_resizable};
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::component::{h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use kuaky_git::{Branch, Change, PullRequest, Repo, Worktree};
use kuaky_term::Launch;
use kuaky_usage::{Agent, AgentLimits, UsageEvent};
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

/// Agent (or plain shell) that can be launched in a terminal session.
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
    pub(crate) const AGENTS: [Harness; 5] = [Harness::Claude, Harness::Codex, Harness::Antigravity, Harness::Gemini, Harness::OpenCode];

    pub(crate) fn label(self) -> &'static str {
        match self {
            Harness::Claude => "Claude Code",
            Harness::Codex => "Codex",
            Harness::Antigravity => "Antigravity",
            Harness::Gemini => "Gemini CLI",
            Harness::OpenCode => "OpenCode",
            Harness::Shell => "Terminal",
        }
    }

    pub(crate) fn command(self) -> Option<&'static str> {
        match self {
            Harness::Claude => Some("claude"),
            Harness::Codex => Some("codex"),
            Harness::Antigravity => Some("agy"),
            Harness::Gemini => Some("gemini"),
            Harness::OpenCode => Some("opencode"),
            Harness::Shell => None,
        }
    }

    /// Program and arguments for ACP chat mode (kept in code for future native agent UI).
    #[allow(dead_code)]
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

    /// Stable key used for persisted state and settings.
    pub(crate) fn key(self) -> &'static str {
        self.command().unwrap_or("shell")
    }

    fn from_key(key: &str) -> Option<Harness> {
        Harness::AGENTS.into_iter().chain([Harness::Shell]).find(|h| h.key() == key)
    }

    /// Arguments to resume the latest conversation in the folder.
    fn resume_args(self) -> &'static str {
        match self {
            Harness::Claude => " --continue",
            Harness::Codex => " resume --last",
            _ => "",
        }
    }

    pub(crate) fn agent(self) -> Option<Agent> {
        match self {
            Harness::Claude => Some(Agent::ClaudeCode),
            Harness::Codex => Some(Agent::Codex),
            Harness::Antigravity => Some(Agent::Antigravity),
            Harness::Gemini => Some(Agent::GeminiCli),
            Harness::OpenCode => Some(Agent::OpenCode),
            Harness::Shell => None,
        }
    }
}

enum TabKind {
    Terminal { view: Entity<TerminalView>, harness: Harness, worktree: Option<String>, cwd: PathBuf },
    File(Entity<FileView>),
    Diff(Entity<DiffView>),
    #[allow(dead_code)]
    Chat { view: Entity<ChatView>, harness: Harness },
    Search(Entity<SearchView>),
}

struct Tab {
    id: usize,
    kind: TabKind,
}

struct RightTerminal {
    id: usize,
    view: Entity<TerminalView>,
}

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
enum SidePanel {
    Files,
    Changes,
    Branches,
    Prs,
    Graph,
}

impl SidePanel {
    const ALL: [SidePanel; 5] = [SidePanel::Files, SidePanel::Changes, SidePanel::Branches, SidePanel::Prs, SidePanel::Graph];

    fn label(self) -> &'static str {
        match self {
            SidePanel::Files => "Files",
            SidePanel::Changes => "Changes",
            SidePanel::Branches => "Branches",
            SidePanel::Prs => "Pull Requests",
            SidePanel::Graph => "Graph",
        }
    }

    fn icon(self) -> &'static str {
        match self {
            SidePanel::Files => "folder",
            SidePanel::Changes => "git-compare",
            SidePanel::Branches => "git-branch",
            SidePanel::Prs => "git-pull-request",
            SidePanel::Graph => "git-graph",
        }
    }
}

/// Summary of an agent session that ran in this project.
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
    /// Active top-right panel (`None` when hidden).
    panel: Option<SidePanel>,
    /// Embedded terminals in the bottom-right zone.
    right_terminals: Vec<RightTerminal>,
    active_right_terminal: Option<usize>,
    right_split: Entity<ResizableState>,
    git: GitState,
    /// Agents detected on this machine and their CLI path.
    installed: HashMap<Harness, PathBuf>,
    /// ACP-capable agents (kept for future native agent UI).
    #[allow(dead_code)]
    chat_agents: HashMap<Harness, (PathBuf, Vec<String>)>,
    /// Launch target chosen in the New Session menu (`false` = This folder, `true` = New branch).
    isolate: bool,
    new_session_menu: bool,
    previous_expanded: bool,
    activity_open: bool,
    activity: Vec<ActivityRow>,
    limits: Vec<AgentLimits>,
    focus: FocusHandle,
    /// Previous sessions from the last time this project was open.
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
        let right_split = cx.new(|_| ResizableState::default());
        let right_split_sub = cx.subscribe(&right_split, |_, split, _: &ResizablePanelEvent, cx| {
            let sizes = split.read(cx).sizes().clone();
            if sizes.len() == 2 {
                state::update(cx, |s| {
                    s.right_bottom_height = Some(f32::from(sizes[1]));
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
            panel: Some(SidePanel::Files),
            right_terminals: Vec::new(),
            active_right_terminal: None,
            right_split,
            git: GitState::default(),
            installed: HashMap::new(),
            chat_agents: HashMap::new(),
            isolate: false,
            new_session_menu: false,
            previous_expanded: false,
            activity_open: false,
            limits: Vec::new(),
            focus: cx.focus_handle(),
            previous: saved.sessions.clone(),
            quick_open: None,
            layout,
            lsp,
            _subs: vec![sub, layout_sub, right_split_sub, lsp_sub],
        };
        this.detect_agents(cx);
        this.start_git_refresh(cx);
        this.start_limits_refresh(cx);
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

    pub fn on_settings_changed(&mut self, cx: &mut Context<Self>) {
        self.detect_agents(cx);
        cx.notify();
    }

    pub fn open_search_tab(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.open_search(window, cx);
    }

    /// Saves open files and sessions for this project.
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
                    custom_name: None,
                    custom_color: None,
                }),
                _ => None,
            })
            .collect();
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

    /// Agents that are both installed (or configured with a custom path) and visible in settings.
    fn available_harnesses(&self, cx: &App) -> Vec<Harness> {
        let settings = &state::get(cx).settings;
        let mut out: Vec<Harness> = Harness::AGENTS
            .into_iter()
            .filter(|h| {
                if !settings.is_visible(h.key()) {
                    return false;
                }
                let custom = settings.agent(h.key()).binary_path;
                !custom.trim().is_empty() || self.installed.contains_key(h)
            })
            .collect();
        out.push(Harness::Shell);
        out
    }

    // ── agents and terminals ───────────────────────────────────────────

    fn detect_agents(&self, cx: &mut Context<Self>) {
        let settings = state::get(cx).settings.clone();
        cx.spawn(async move |this, cx| {
            let found = cx
                .background_executor()
                .spawn(async move {
                    let installed = Harness::AGENTS
                        .into_iter()
                        .filter_map(|h| {
                            let custom = settings.agent(h.key()).binary_path;
                            if !custom.trim().is_empty() {
                                return Some((h, PathBuf::from(custom.trim())));
                            }
                            Some((h, kuaky_git::find_agent(h.command()?)?))
                        })
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

    /// Demo state for development screenshots.
    #[cfg(feature = "screenshot")]
    pub fn demo(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Ok(panel) = std::env::var("KUAKY_DEMO_PANEL") {
            let p = match panel.as_str() {
                "changes" => Some(SidePanel::Changes),
                "branches" => Some(SidePanel::Branches),
                "prs" => Some(SidePanel::Prs),
                "graph" => Some(SidePanel::Graph),
                "none" => None,
                _ => Some(SidePanel::Files),
            };
            self.panel = p;
            if p == Some(SidePanel::Prs) && self.git.prs.is_none() {
                self.load_prs(cx);
            }
        }
        if std::env::var("KUAKY_DEMO_NEW_SESSION").is_ok() {
            self.new_session_menu = true;
        }
        if std::env::var("KUAKY_DEMO_ACTIVITY").is_ok() {
            self.activity_open = true;
        }
        if std::env::var("KUAKY_DEMO_RIGHT_TERM").is_ok() {
            self.open_right_terminal(window, cx);
        }
        if std::env::var("KUAKY_DEMO_LIMITS").is_ok() {
            self.limits = crate::home::demo_limits();
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
                self.open_diff("Local changes".into(), "uncommitted".into(), move || repo.diff_worktree(), window, cx);
            }
        }
    }

    pub fn set_usage(&mut self, events: &[UsageEvent], cx: &mut Context<Self>) {
        self.activity = activity_for(&self.root, events);
        cx.notify();
    }

    pub fn open_terminal(&mut self, harness: Harness, window: &mut Window, cx: &mut Context<Self>) {
        self.new_session_menu = false;
        let settings = state::get(cx).settings.clone();
        let custom_path = settings.agent(harness.key()).binary_path;
        if harness != Harness::Shell && !self.installed.contains_key(&harness) {
            if !custom_path.trim().is_empty() {
                self.installed.insert(harness, PathBuf::from(custom_path.trim()));
            } else if let Some(path) = harness.command().and_then(kuaky_git::find_agent) {
                self.installed.insert(harness, path);
            }
        }
        let installed = harness == Harness::Shell || self.installed.contains_key(&harness);
        if !installed {
            return;
        }
        let mut cwd = self.root.clone();
        let mut worktree = None;
        if self.isolate && harness != Harness::Shell {
            if let Some(repo) = &self.repo {
                let name = format!(
                    "{}-{}",
                    harness.command().unwrap_or("agent"),
                    jiff::Zoned::now().strftime("%m%d-%H%M%S")
                );
                let created = match settings.resolved_worktrees_dir() {
                    Some(dir) => repo.create_worktree_in(&dir, &name),
                    None => repo.create_worktree(&name),
                };
                match created {
                    Ok(path) => {
                        cwd = path;
                        worktree = Some(format!("kuaky/{name}"));
                    }
                    Err(err) => tracing::warn!(%err, "failed to create worktree"),
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
        let cfg = state::get(cx).settings.agent(harness.key());
        if harness != Harness::Shell && !self.installed.contains_key(&harness) {
            if !cfg.binary_path.trim().is_empty() {
                self.installed.insert(harness, PathBuf::from(cfg.binary_path.trim()));
            } else if let Some(path) = harness.command().and_then(kuaky_git::find_agent) {
                self.installed.insert(harness, path);
            }
        }
        let command = harness.command().map(|name| {
            let base = if !cfg.binary_path.trim().is_empty() {
                kuaky_git::shell_command(Path::new(cfg.binary_path.trim()))
            } else {
                match self.installed.get(&harness) {
                    Some(path) if kuaky_git::find_program(name).is_none() => kuaky_git::shell_command(path),
                    _ => name.to_string(),
                }
            };
            let mut full = if resume { format!("{base}{}", harness.resume_args()) } else { base };
            if !cfg.extra_args.trim().is_empty() {
                full.push(' ');
                full.push_str(cfg.extra_args.trim());
            }
            full
        });
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

    fn open_right_terminal(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let launch = Launch { cwd: self.root.clone(), command: None, prefill: None, env: Vec::new() };
        let id = self.next_id;
        self.next_id += 1;
        let label = format!("Terminal {}", self.right_terminals.len() + 1);
        let view = cx.new(|cx| TerminalView::new(label, launch, None, window, cx));
        let obs = cx.observe(&view, |_, _, cx| cx.notify());
        self._subs.push(obs);
        self.right_terminals.push(RightTerminal { id, view });
        self.active_right_terminal = Some(self.right_terminals.len() - 1);
        cx.notify();
    }

    fn close_right_terminal(&mut self, index: usize, cx: &mut Context<Self>) {
        if index >= self.right_terminals.len() {
            return;
        }
        self.right_terminals.remove(index);
        if self.right_terminals.is_empty() {
            self.active_right_terminal = None;
        } else {
            self.active_right_terminal = Some(index.min(self.right_terminals.len() - 1));
        }
        cx.notify();
    }

    fn on_agent_state(&mut self, view: &Entity<TerminalView>, harness: Harness, window: &mut Window, cx: &mut Context<Self>) {
        let t = view.read(cx);
        let Some(state) = t.agent_state.clone() else { return };
        let was_working = t.previous_state == Some(AgentState::Working);
        let (title, message) = match &state {
            AgentState::NeedsYou(msg) => (format!("{} needs your permission", harness.label()), msg.clone()),
            AgentState::Done if was_working => (format!("{} finished", harness.label()), "Your turn.".to_string()),
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

    /// Opens a shell with `text` prefilled (not executed until user presses Enter).
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
                    let branch = wt.branch.clone().unwrap_or_else(|| "(detached)".into());
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
                                .child(icons::ui("git-branch", px(13.), style::accent()))
                                .child(div().truncate().child(name)),
                        )
                        .child(div().text_xs().truncate().text_color(style::faint()).child(branch.clone()))
                        .child(
                            h_flex()
                                .gap_1()
                                .flex_wrap()
                                .child(action(("wt-diff", i), "changes").on_click(cx.listener(move |this, _, window, cx| {
                                    let Some(repo) = this.repo.clone() else { return };
                                    let (base, wt) = (b1.clone(), wt1.clone());
                                    let title = format!("Worktree {}", wt.branch.clone().unwrap_or_default());
                                    this.open_diff(
                                        title,
                                        format!("worktree changes against {base}"),
                                        move || repo.diff_worktree_branch(&base, &wt),
                                        window,
                                        cx,
                                    );
                                })))
                                .child(action(("wt-term", i), "terminal").on_click(cx.listener(move |this, _, window, cx| {
                                    this.add_terminal(
                                        Harness::Shell,
                                        Launch { cwd: wt2.path.clone(), command: None, prefill: None, env: Vec::new() },
                                        wt2.branch.clone(),
                                        None,
                                        window,
                                        cx,
                                    );
                                })))
                                .child(action(("wt-pr", i), "create PR").on_click(cx.listener(move |this, _, window, cx| {
                                    let text = format!("git push -u origin {b2} && gh pr create --fill --head {b2}");
                                    this.prefilled_shell(wt3.path.clone(), text, window, cx);
                                })))
                                .child(action(("wt-merge", i), "merge").on_click(cx.listener({
                                    let root = root.clone();
                                    move |this, _, window, cx| {
                                        this.prefilled_shell(root.clone(), format!("git merge --no-ff {b3}"), window, cx);
                                    }
                                })))
                                .child(action(("wt-remove", i), "remove").on_click(cx.listener(move |this, _, window, cx| {
                                    let text = format!("git worktree remove {path_q} && git branch -D {b4}");
                                    this.prefilled_shell(root.clone(), text, window, cx);
                                }))),
                        )
                }))
                .into_any_element(),
        )
    }

    #[allow(dead_code)]
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
                    this.open_diff(title.clone(), "proposed agent change".into(), move || Ok(patch), window, cx);
                }
                ChatEvent::NeedsYou(msg) => {
                    this.notify(format!("{} needs your permission", harness.label()), msg.clone(), is_active_tab, window, cx)
                }
                ChatEvent::TurnDone => this.notify(format!("{} finished", harness.label()), "Your turn.".into(), is_active_tab, window, cx),
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

    /// Opens a file and places the cursor at 0-based `line`/`column`.
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

    fn start_limits_refresh(&self, cx: &mut Context<Self>) {
        cx.spawn(async move |this, cx| {
            loop {
                let limits = cx.background_executor().spawn(async { kuaky_usage::load_all_limits() }).await;
                let alive = this
                    .update(cx, |this, cx| {
                        #[cfg(feature = "screenshot")]
                        let limits = if limits.is_empty() && std::env::var("KUAKY_DEMO_LIMITS").is_ok() {
                            crate::home::demo_limits()
                        } else {
                            limits
                        };
                        if this.limits != limits {
                            this.limits = limits;
                            cx.notify();
                        }
                    })
                    .is_ok();
                if !alive {
                    break;
                }
                cx.background_executor().timer(Duration::from_secs(10)).await;
            }
        })
        .detach();
    }

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

    fn toggle_panel(&mut self, panel: SidePanel, cx: &mut Context<Self>) {
        if self.panel == Some(panel) {
            self.panel = None;
        } else {
            self.panel = Some(panel);
            if panel == SidePanel::Prs && self.git.prs.is_none() {
                self.load_prs(cx);
            }
        }
        cx.notify();
    }

    fn checkout(&mut self, branch: String, cx: &mut Context<Self>) {
        let Some(repo) = self.repo.clone() else { return };
        cx.spawn(async move |this, cx| {
            let result = cx.background_executor().spawn(async move { repo.checkout(&branch) }).await;
            this.update(cx, |this, cx| {
                if let Err(err) = result {
                    tracing::warn!(%err, "checkout failed");
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
        let harnesses = self.available_harnesses(cx);

        v_flex()
            .size_full()
            .border_r_1()
            .border_color(style::border())
            .bg(style::panel())
            .child(
                v_flex()
                    .p_3()
                    .gap_2()
                    .border_b_1()
                    .border_color(style::border())
                    .child(
                        h_flex()
                            .id("new-session-btn")
                            .px_3()
                            .py_1p5()
                            .gap_2()
                            .justify_between()
                            .items_center()
                            .rounded_lg()
                            .cursor_pointer()
                            .bg(style::accent())
                            .text_color(style::bg())
                            .text_sm()
                            .font_weight(FontWeight::MEDIUM)
                            .child("New session")
                            .child(icons::ui("plus", px(14.), style::bg()))
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.new_session_menu = !this.new_session_menu;
                                cx.notify();
                            })),
                    )
                    .when(self.new_session_menu, |d| {
                        let isolated = self.isolate;
                        d.child(
                            v_flex()
                                .p_2()
                                .gap_2()
                                .rounded_lg()
                                .bg(style::bg())
                                .border_1()
                                .border_color(style::border())
                                .when(self.repo.is_some(), |d| {
                                    d.child(
                                        v_flex()
                                            .gap_1()
                                            .child(div().px_1().text_xs().text_color(style::faint()).child("WORKSPACE"))
                                            .child(
                                                h_flex()
                                                    .id("target-folder")
                                                    .px_2()
                                                    .py_1()
                                                    .gap_2()
                                                    .rounded_md()
                                                    .text_xs()
                                                    .cursor_pointer()
                                                    .when(!isolated, |d| d.bg(style::panel_hover()).text_color(style::text()))
                                                    .when(isolated, |d| d.text_color(style::muted()))
                                                    .hover(|d| d.bg(style::panel_hover()))
                                                    .tooltip(|window, cx| {
                                                        Tooltip::new("Work directly in this project folder").build(window, cx)
                                                    })
                                                    .child(icons::ui("folder", px(13.), if !isolated { style::accent() } else { style::muted() }))
                                                    .child(div().flex_1().min_w_0().truncate().child("This folder"))
                                                    .when(!isolated, |d| d.child(icons::ui("check", px(12.), style::accent())))
                                                    .on_click(cx.listener(|this, _, _, cx| {
                                                        this.isolate = false;
                                                        cx.notify();
                                                    })),
                                            )
                                            .child(
                                                h_flex()
                                                    .id("target-branch")
                                                    .px_2()
                                                    .py_1()
                                                    .gap_2()
                                                    .rounded_md()
                                                    .text_xs()
                                                    .cursor_pointer()
                                                    .when(isolated, |d| d.bg(style::panel_hover()).text_color(style::text()))
                                                    .when(!isolated, |d| d.text_color(style::muted()))
                                                    .hover(|d| d.bg(style::panel_hover()))
                                                    .tooltip(|window, cx| {
                                                        Tooltip::new(
                                                            "Create a separate git worktree and branch (kuaky/…) without touching your current folder",
                                                        )
                                                        .build(window, cx)
                                                    })
                                                    .child(icons::ui("git-branch", px(13.), if isolated { style::accent() } else { style::muted() }))
                                                    .child(div().flex_1().min_w_0().truncate().child("New branch (separate copy)"))
                                                    .when(isolated, |d| d.child(icons::ui("check", px(12.), style::accent())))
                                                    .on_click(cx.listener(|this, _, _, cx| {
                                                        this.isolate = true;
                                                        cx.notify();
                                                    })),
                                            ),
                                    )
                                    .child(div().h(px(1.)).bg(style::border()))
                                })
                                .child(
                                    v_flex()
                                        .gap_0p5()
                                        .children(harnesses.into_iter().map(|h| {
                                            h_flex()
                                                .id(SharedString::from(format!("launch-{}", h.label())))
                                                .gap_2()
                                                .px_2()
                                                .py_1p5()
                                                .rounded_md()
                                                .text_sm()
                                                .cursor_pointer()
                                                .hover(|d| d.bg(style::panel_hover()))
                                                .child(harness_badge(h))
                                                .child(div().flex_1().min_w_0().truncate().child(h.label()))
                                                .on_click(cx.listener(move |this, _, window, cx| this.open_terminal(h, window, cx)))
                                        })),
                                ),
                        )
                    }),
            )
            .child(
                v_flex()
                    .flex_1()
                    .min_h_0()
                    .p_3()
                    .gap_1()
                    .child(side_title("Open sessions"))
                    .when(!self.tabs.iter().any(|t| matches!(t.kind, TabKind::Terminal { .. } | TabKind::Chat { .. })), |d| {
                        d.child(div().px_2().text_xs().text_color(style::faint()).child("No open sessions"))
                    })
                    .child(
                        div().id("open-sessions").flex_1().overflow_y_scroll().child(
                            v_flex().gap_1().children(self.tabs.iter().enumerate().filter_map(|(i, tab)| {
                                let (harness, title, status, color, worktree): (Harness, SharedString, &str, Hsla, Option<String>) =
                                    match &tab.kind {
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
                                        .py_1p5()
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
                                                    d.child(
                                                        h_flex()
                                                            .gap_1()
                                                            .min_w_0()
                                                            .text_xs()
                                                            .text_color(style::faint())
                                                            .child(icons::ui("git-branch", px(11.), style::faint()))
                                                            .child(div().flex_1().min_w_0().truncate().child(w)),
                                                    )
                                                }),
                                        )
                                        .child(
                                            div()
                                                .id(("session-close", tab.id))
                                                .flex_none()
                                                .p_0p5()
                                                .rounded_sm()
                                                .hover(|d| d.bg(style::bg()))
                                                .child(icons::ui("x", px(12.), style::faint()))
                                                .on_click(cx.listener(move |this, _, window, cx| {
                                                    cx.stop_propagation();
                                                    this.close_tab(i, window, cx);
                                                })),
                                        )
                                        .on_click(cx.listener(move |this, _, window, cx| this.activate(i, window, cx))),
                                )
                            })),
                        ),
                    ),
            )
            .when(!self.previous.is_empty(), |d| {
                let expanded = self.previous_expanded;
                d.child(
                    v_flex()
                        .p_3()
                        .gap_1()
                        .border_t_1()
                        .border_color(style::border())
                        .child(
                            h_flex()
                                .id("toggle-previous")
                                .gap_1p5()
                                .items_center()
                                .justify_between()
                                .cursor_pointer()
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.previous_expanded = !this.previous_expanded;
                                    cx.notify();
                                }))
                                .child(
                                    h_flex()
                                        .flex_1()
                                        .min_w_0()
                                        .gap_1()
                                        .items_center()
                                        .child(icons::ui(if expanded { "chevron-down" } else { "chevron-right" }, px(12.), style::muted()))
                                        .child(
                                            div()
                                                .flex_1()
                                                .min_w_0()
                                                .truncate()
                                                .text_xs()
                                                .text_color(style::muted())
                                                .child(format!("PREVIOUS ({})", self.previous.len())),
                                        ),
                                )
                                .child(
                                    div()
                                        .id("forget-previous")
                                        .flex_none()
                                        .px_1()
                                        .text_xs()
                                        .cursor_pointer()
                                        .text_color(style::faint())
                                        .hover(|d| d.text_color(style::text()))
                                        .child("Clear")
                                        .on_click(cx.listener(|this, _, _, cx| {
                                            cx.stop_propagation();
                                            this.previous.clear();
                                            this.persist(cx);
                                            cx.notify();
                                        })),
                                ),
                        )
                        .when(expanded, |d| {
                            d.children(self.previous.iter().enumerate().filter_map(|(i, p)| {
                                let h = Harness::from_key(&p.harness)?;
                                let detail = p.worktree.clone().unwrap_or_else(|| {
                                    if h.resume_args().is_empty() { "reopen".into() } else { "resume conversation".into() }
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
                                        .child(div().flex_none().text_xs().text_color(style::accent()).child("Resume"))
                                        .on_click(cx.listener(move |this, _, window, cx| this.resume(i, window, cx))),
                                )
                            }))
                        }),
                )
            })
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
                let (icon, status, title, dirty): (AnyElement, Option<Hsla>, SharedString, bool) = match &tab.kind {
                    TabKind::Terminal { view, harness, .. } => {
                        let t = view.read(cx);
                        let (_, dot) = terminal_status(t, now);
                        (harness_badge(*harness), Some(dot), t.title.clone().unwrap_or_else(|| t.label.clone()), false)
                    }
                    TabKind::File(f) => {
                        let f = f.read(cx);
                        (icons::ui("file", px(14.), style::muted()).into_any_element(), None, f.title().into(), f.dirty)
                    }
                    TabKind::Diff(d) => {
                        (icons::ui("git-compare", px(14.), style::accent()).into_any_element(), None, d.read(cx).title.clone(), false)
                    }
                    TabKind::Search(_) => {
                        (icons::ui("search", px(14.), style::accent()).into_any_element(), None, "Search".into(), false)
                    }
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
                            .flex_none()
                            .p_0p5()
                            .rounded_sm()
                            .hover(|d| d.bg(style::panel_hover()))
                            .child(icons::ui("x", px(12.), style::faint()))
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
        let launchers = self.available_harnesses(cx);
        let shortcuts = if cfg!(target_os = "macos") {
            "⌘T new terminal · ⌘P quick open · ⌘⇧F search · ⌘W close tab · ⌘, settings"
        } else {
            "Ctrl+Shift+T new terminal · Ctrl+P quick open · Ctrl+Shift+F search · Ctrl+, settings"
        };
        v_flex()
            .size_full()
            .items_center()
            .justify_center()
            .gap_4()
            .child(div().text_2xl().font_weight(FontWeight::SEMIBOLD).child(name))
            .child(div().text_sm().text_color(style::muted()).child(match &self.git.branch {
                Some(b) => format!("{} · branch {b}", tilde(&self.root)),
                None => tilde(&self.root),
            }))
            .child(h_flex().gap_2().mt_2().flex_wrap().justify_center().children(launchers.into_iter().map(|h| {
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
            .child(div().text_xs().text_color(style::faint()).child(shortcuts))
    }

    fn right_panel(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let counts: HashMap<SidePanel, usize> = HashMap::from([
            (SidePanel::Changes, self.git.changes.len()),
            (SidePanel::Branches, self.git.branches.len()),
            (SidePanel::Prs, self.git.prs.as_ref().and_then(|p| p.as_ref().ok()).map_or(0, |p| p.len())),
        ]);

        let top_toolbar = h_flex()
            .px_2()
            .py_1p5()
            .gap_1()
            .border_b_1()
            .border_color(style::border())
            .children(SidePanel::ALL.into_iter().map(|p| {
                let selected = self.panel == Some(p);
                let count = counts.get(&p).copied().unwrap_or(0);
                let tip: SharedString = p.label().into();
                h_flex()
                    .id(SharedString::from(format!("panel-{}", p.label())))
                    .flex_1()
                    .justify_center()
                    .items_center()
                    .gap_1()
                    .py_1()
                    .rounded_md()
                    .text_xs()
                    .cursor_pointer()
                    .when(selected, |d| d.bg(style::panel_hover()))
                    .hover(|d| d.bg(style::panel_hover()))
                    .tooltip(move |window, cx| Tooltip::new(tip.clone()).build(window, cx))
                    .child(icons::ui(p.icon(), px(15.), if selected { style::accent() } else { style::muted() }))
                    .when(count > 0 && p != SidePanel::Files && p != SidePanel::Graph, |d| {
                        d.child(div().text_color(style::accent()).child(count.to_string()))
                    })
                    .on_click(cx.listener(move |this, _, _, cx| this.toggle_panel(p, cx)))
            }));

        let top_content = self.panel.map(|p| {
            v_flex()
                .size_full()
                .when_some(self.git.branch.clone(), |d, b| {
                    d.child(
                        h_flex()
                            .px_3()
                            .py_1p5()
                            .gap_1p5()
                            .items_center()
                            .text_xs()
                            .border_b_1()
                            .border_color(style::border())
                            .child(icons::ui("git-branch", px(12.), style::muted()))
                            .child(div().text_color(style::accent()).truncate().child(b)),
                    )
                })
                .child(div().flex_1().min_h_0().child(match p {
                    SidePanel::Files => self.tree.clone().into_any_element(),
                    SidePanel::Changes => self.changes_panel(cx).into_any_element(),
                    SidePanel::Branches => self.branches_panel(cx).into_any_element(),
                    SidePanel::Prs => self.prs_panel(cx).into_any_element(),
                    SidePanel::Graph => self.graph_panel(cx).into_any_element(),
                }))
        });

        let has_terms = !self.right_terminals.is_empty();
        let bottom_height = state::get(cx).right_bottom_height.unwrap_or(240.).clamp(120., 600.);

        v_flex()
            .size_full()
            .border_l_1()
            .border_color(style::border())
            .bg(style::panel())
            .child(top_toolbar)
            .child(div().flex_1().min_h_0().map(|d| match (top_content, has_terms) {
                (Some(top), true) => d.child(
                    v_resizable("right-split")
                        .with_state(&self.right_split)
                        .child(resizable_panel().child(top))
                        .child(
                            resizable_panel()
                                .size(px(bottom_height))
                                .size_range(px(120.)..px(600.))
                                .child(self.right_terminal_zone(cx)),
                        ),
                ),
                (Some(top), false) => d.child(v_flex().size_full().child(div().flex_1().min_h_0().child(top)).child(self.right_terminal_bar(cx))),
                (None, true) => d.child(self.right_terminal_zone(cx)),
                (None, false) => d.child(
                    v_flex()
                        .size_full()
                        .justify_between()
                        .child(div().p_3().text_xs().text_color(style::faint()).child("Click an icon above to open a section."))
                        .child(self.right_terminal_bar(cx)),
                ),
            }))
    }

    fn right_terminal_bar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        h_flex()
            .h(px(32.))
            .flex_none()
            .px_2()
            .gap_1()
            .items_center()
            .border_t_1()
            .border_color(style::border())
            .bg(style::panel())
            .child(icons::ui("terminal", px(13.), style::muted()))
            .child(
                h_flex()
                    .id("right-term-tabs")
                    .flex_1()
                    .min_w_0()
                    .gap_1()
                    .overflow_x_scroll()
                    .when(self.right_terminals.is_empty(), |d| {
                        d.child(div().text_xs().text_color(style::muted()).child("Terminals"))
                    })
                    .children(self.right_terminals.iter().enumerate().map(|(i, rt)| {
                        let active = self.active_right_terminal == Some(i);
                        let label = rt.view.read(cx).title.clone().unwrap_or_else(|| rt.view.read(cx).label.clone());
                        h_flex()
                            .id(("rterm", rt.id))
                            .px_1p5()
                            .py_0p5()
                            .gap_1()
                            .rounded_sm()
                            .text_xs()
                            .cursor_pointer()
                            .when(active, |d| d.bg(style::bg()).text_color(style::text()))
                            .when(!active, |d| d.text_color(style::muted()))
                            .hover(|d| d.text_color(style::text()))
                            .child(div().max_w(px(90.)).truncate().child(label))
                            .child(
                                div()
                                    .id(("rterm-close", rt.id))
                                    .flex_none()
                                    .hover(|d| d.bg(style::panel_hover()))
                                    .child(icons::ui("x", px(11.), style::faint()))
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        cx.stop_propagation();
                                        this.close_right_terminal(i, cx);
                                    })),
                            )
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.active_right_terminal = Some(i);
                                if let Some(rt) = this.right_terminals.get(i) {
                                    let h = rt.view.read(cx).focus_handle(cx);
                                    window.focus(&h, cx);
                                }
                                cx.notify();
                            }))
                    })),
            )
            .child(
                div()
                    .id("right-term-new")
                    .flex_none()
                    .p_1()
                    .rounded_sm()
                    .cursor_pointer()
                    .hover(|d| d.bg(style::panel_hover()))
                    .tooltip(|window, cx| Tooltip::new("New terminal in side panel").build(window, cx))
                    .child(icons::ui("plus", px(13.), style::muted()))
                    .on_click(cx.listener(|this, _, window, cx| this.open_right_terminal(window, cx))),
            )
    }

    fn right_terminal_zone(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let active_view = self.active_right_terminal.and_then(|i| self.right_terminals.get(i)).map(|rt| rt.view.clone());
        v_flex()
            .size_full()
            .bg(style::bg())
            .child(self.right_terminal_bar(cx))
            .when_some(active_view, |d, view| d.child(div().flex_1().min_h_0().child(view)))
    }

    fn status_bar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let latest = self.activity.first();
        let total_turns: u64 = self.activity.iter().map(|a| a.turns).sum();
        let total_tokens: u64 = self.activity.iter().map(|a| a.tokens).sum();

        h_flex()
            .h(px(26.))
            .flex_none()
            .px_3()
            .gap_3()
            .items_center()
            .text_xs()
            .border_t_1()
            .border_color(style::border())
            .bg(style::panel())
            .text_color(style::muted())
            .when_some(self.git.branch.clone(), |d, b| {
                d.child(
                    h_flex()
                        .gap_1()
                        .items_center()
                        .child(icons::ui("git-branch", px(12.), style::accent()))
                        .child(div().text_color(style::text()).child(b)),
                )
            })
            .when_some(latest, |d, row| {
                let summary = format!(
                    "{} · {} resp · {} tok · {}",
                    style::model_name(&row.model),
                    total_turns,
                    human(total_tokens),
                    ago(row.last),
                );
                d.child(
                    h_flex()
                        .id("status-activity")
                        .gap_1p5()
                        .px_1p5()
                        .py_0p5()
                        .rounded_sm()
                        .cursor_pointer()
                        .when(self.activity_open, |d| d.bg(style::panel_hover()).text_color(style::text()))
                        .hover(|d| d.bg(style::panel_hover()).text_color(style::text()))
                        .tooltip(|window, cx| Tooltip::new("Click to view project agent activity").build(window, cx))
                        .child(icons::logo(icons::agent(row.agent), px(13.)))
                        .child(summary)
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.activity_open = !this.activity_open;
                            cx.notify();
                        })),
                )
            })
            .child(div().flex_1())
            .children(self.limits.iter().enumerate().map(|(i, al)| {
                let now = jiff::Timestamp::now();
                let tz = jiff::tz::TimeZone::system();
                let short_name = match al.agent {
                    Agent::ClaudeCode => "Claude",
                    other => other.name(),
                };
                let mut parts: Vec<String> = Vec::new();
                if let Some(p) = &al.primary {
                    parts.push(format!("{} {:.0}%", p.label, p.used_pct));
                }
                if let Some(s) = &al.secondary {
                    parts.push(format!("{} {:.0}%", s.label, s.used_pct));
                }
                let reset = al
                    .primary
                    .as_ref()
                    .and_then(|p| p.resets_at)
                    .and_then(|ts| kuaky_usage::limits::format_reset(ts, now, &tz));
                if let Some(r) = &reset {
                    parts.push(format!("resets {r}"));
                }
                let max_used = al.windows().map(|w| w.used_pct).fold(0.0_f64, f64::max);
                let color = if max_used >= 90.0 {
                    style::bad()
                } else if max_used >= 75.0 {
                    rgb(0xfbbf24)
                } else {
                    style::muted()
                };
                let tip_lines: Vec<String> = al
                    .windows()
                    .map(|w| {
                        let r = w
                            .resets_at
                            .and_then(|ts| kuaky_usage::limits::format_reset(ts, now, &tz))
                            .map(|t| format!(" · resets {t}"))
                            .unwrap_or_default();
                        format!("{}: {:.0}% left ({:.0}% used){r}", w.label, w.remaining_pct, w.used_pct)
                    })
                    .collect();
                let tip: SharedString = format!("{} — {}", al.agent.name(), tip_lines.join(" | ")).into();
                h_flex()
                    .id(("status-limit", i))
                    .gap_1()
                    .items_center()
                    .text_color(color)
                    .tooltip(move |window, cx| Tooltip::new(tip.clone()).build(window, cx))
                    .child(icons::logo(icons::agent(al.agent), px(12.)))
                    .child(format!("{short_name} {}", parts.join(" · ")))
            }))
            .child(div().text_color(style::faint()).child(if self.tabs.is_empty() {
                String::new()
            } else if self.tabs.len() == 1 {
                "1 tab".into()
            } else {
                format!("{} tabs", self.tabs.len())
            }))
    }

    fn activity_popover(&self, cx: &mut Context<Self>) -> impl IntoElement {
        v_flex()
            .id("activity-popover")
            .w(px(340.))
            .max_h(px(360.))
            .rounded_xl()
            .border_1()
            .border_color(style::border())
            .bg(style::panel())
            .shadow_lg()
            .overflow_hidden()
            .on_click(|_, _, cx| cx.stop_propagation())
            .child(
                h_flex()
                    .px_3()
                    .py_2()
                    .justify_between()
                    .items_center()
                    .border_b_1()
                    .border_color(style::border())
                    .child(div().text_xs().font_weight(FontWeight::SEMIBOLD).child("Project Activity"))
                    .child(
                        div()
                            .id("activity-close")
                            .p_0p5()
                            .rounded_sm()
                            .cursor_pointer()
                            .hover(|d| d.bg(style::panel_hover()))
                            .child(icons::ui("x", px(12.), style::muted()))
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.activity_open = false;
                                cx.notify();
                            })),
                    ),
            )
            .child(
                div().id("activity-list").flex_1().overflow_y_scroll().child(
                    v_flex()
                        .p_2()
                        .gap_1()
                        .when(self.activity.is_empty(), |d| {
                            d.child(div().p_2().text_xs().text_color(style::faint()).child("No recorded sessions"))
                        })
                        .children(self.activity.iter().take(30).map(|a| {
                            h_flex()
                                .gap_2()
                                .px_2()
                                .py_1p5()
                                .rounded_md()
                                .hover(|d| d.bg(style::panel_hover()))
                                .child(icons::logo(icons::agent(a.agent), px(18.)))
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
                        })),
                ),
            )
    }

    fn changes_panel(&self, cx: &mut Context<Self>) -> impl IntoElement {
        if self.repo.is_none() {
            return panel_message("This folder is not a git repository.").into_any_element();
        }
        div()
            .id("changes")
            .size_full()
            .overflow_y_scroll()
            .child(
                v_flex()
                    .p_2()
                    .gap_0p5()
                    .when(self.git.changes.is_empty(), |d| d.child(panel_message("No local changes.")))
                    .when(!self.git.changes.is_empty(), |d| {
                        d.child(
                            panel_button("View all changes").on_click(cx.listener(|this, _, window, cx| {
                                let Some(repo) = this.repo.clone() else { return };
                                let branch = this.git.branch.clone().unwrap_or_default();
                                this.open_diff(
                                    "Local changes".into(),
                                    format!("{branch} · uncommitted"),
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
                                this.open_diff(ch.path.clone(), "local change".into(), move || repo.diff_change(&ch), window, cx);
                            }))
                    })),
            )
            .into_any_element()
    }

    fn branches_panel(&self, cx: &mut Context<Self>) -> impl IntoElement {
        if self.repo.is_none() {
            return panel_message("This folder is not a git repository.").into_any_element();
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
                        d.child(div().px_2().pb_1().text_xs().text_color(style::faint()).child(format!("click: diff against {b}")))
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
                                        .flex_none()
                                        .px_1p5()
                                        .rounded_sm()
                                        .text_xs()
                                        .text_color(style::muted())
                                        .hover(|d| d.bg(style::bg()).text_color(style::text()))
                                        .child("switch")
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
                                    format!("changes in {head} relative to {base}"),
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
            return panel_message("This folder is not a git repository.").into_any_element();
        }
        let body = match &self.git.prs {
            None => panel_message("Loading PRs…").into_any_element(),
            Some(Err(e)) => panel_message(e.clone()).into_any_element(),
            Some(Ok(prs)) if prs.is_empty() => panel_message("No open PRs.").into_any_element(),
            Some(Ok(prs)) => v_flex()
                .gap_0p5()
                .children(prs.iter().map(|pr| {
                    let number = pr.number;
                    let title = pr.title.clone();
                    let review = match pr.review_decision.as_deref() {
                        Some("APPROVED") => Some(("approved", style::good())),
                        Some("CHANGES_REQUESTED") => Some(("changes requested", style::bad())),
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
                                .when(pr.is_draft, |d| d.child("draft"))
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
                    .child(panel_button(if self.git.prs_loading { "Refreshing…" } else { "Refresh" }).on_click(
                        cx.listener(|this, _, _, cx| {
                            this.load_prs(cx);
                            cx.notify();
                        }),
                    ))
                    .child(body),
            )
            .into_any_element()
    }

    fn graph_panel(&self, _: &mut Context<Self>) -> impl IntoElement {
        if self.repo.is_none() {
            return panel_message("This folder is not a git repository.").into_any_element();
        }
        panel_message("Git commit graph ready for Part E.").into_any_element()
    }
}

impl Render for ProjectView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        v_flex()
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
            .child(
                div().flex_1().min_h_0().child({
                    let saved = state::get(cx);
                    let (left, right) = (saved.left_width.unwrap_or(248.).max(220.), saved.right_width.unwrap_or(300.));
                    h_resizable("project-layout")
                        .with_state(&self.layout)
                        .child(resizable_panel().size(px(left)).size_range(px(220.)..px(420.)).child(self.left_sidebar(cx)))
                        .child(resizable_panel().child(self.center(cx)))
                        .child(resizable_panel().size(px(right)).size_range(px(220.)..px(560.)).child(self.right_panel(cx)))
                }),
            )
            .child(self.status_bar(cx))
            .when(self.activity_open, |d| {
                d.child(
                    div()
                        .id("activity-backdrop")
                        .absolute()
                        .inset_0()
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.activity_open = false;
                            cx.notify();
                        }))
                        .child(
                            div()
                                .absolute()
                                .bottom(px(34.))
                                .left(px(12.))
                                .child(self.activity_popover(cx)),
                        ),
                )
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

// ── helpers ──────────────────────────────────────────────────────────────

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
        ("exited", style::faint().into())
    } else if t.last_output.is_some_and(|o| now.duration_since(o) < Duration::from_secs(2)) {
        ("working", style::accent().into())
    } else {
        ("idle", style::good().into())
    }
}

fn chat_status(c: &ChatView) -> (&'static str, Hsla) {
    if c.is_waiting_permission() {
        return ("needs permission", style::bad().into());
    }
    match &c.status {
        ChatStatus::Connecting => ("connecting", style::faint().into()),
        ChatStatus::Ready => ("your turn", style::good().into()),
        ChatStatus::Running => ("working", style::accent().into()),
        ChatStatus::Error(_) => ("error", style::bad().into()),
        ChatStatus::Exited => ("exited", style::faint().into()),
    }
}

/// Background macOS system notification.
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

/// Agent logo, or Lucide terminal icon for plain shell.
fn harness_badge(h: Harness) -> AnyElement {
    match h.agent() {
        Some(agent) => icons::logo(icons::agent(agent), px(18.)).into_any_element(),
        None => div()
            .size(px(18.))
            .flex_none()
            .flex()
            .items_center()
            .justify_center()
            .rounded_md()
            .bg(style::soft(style::muted(), 0.16))
            .child(icons::ui("terminal", px(12.), style::muted()))
            .into_any_element(),
    }
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
        s if s < 60 => "just now".into(),
        s if s < 3600 => format!("{} min ago", s / 60),
        s if s < 86_400 => format!("{} h ago", s / 3600),
        s => format!("{} d ago", s / 86_400),
    }
}

/// Agent sessions whose working directory is inside `root`, newest first.
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
