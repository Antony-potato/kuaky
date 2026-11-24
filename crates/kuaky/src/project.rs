//! Project workspace view: open agent sessions on the left, tabs in the center,
//! collapsible files/git/PRs/graph + embedded terminals on the right, and a
//! compact activity/status bar at the bottom.

use crate::agent_status::AgentState;
use crate::agent_tasks::TaskWatcher;
use crate::lines::{LineKind, LineSession, LineStore};
use crate::chat_view::{ChatEvent, ChatLaunch, ChatStatus, ChatView};
use crate::conflict_view::{ConflictEvent, ConflictView};
use crate::diff_view::DiffView;
use crate::file_tree::{FileTree, FileTreeEvent};
use crate::file_view::FileView;
use crate::graph_view::{GraphEvent, GraphView};
use crate::home::tilde;
use crate::icons;
use crate::quick_open::{QuickOpen, QuickOpenEvent};
use crate::search_view::{SearchEvent, SearchView};
use crate::state::{self, SavedSession};
use crate::style::{self, human};
use crate::terminal_view::{TerminalView, TerminalViewEvent};
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::resizable::{ResizablePanelEvent, ResizableState, h_resizable, resizable_panel, v_resizable};
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::component::{h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use kuaky_git::{Branch, Change, MergeMethod, MergeOutcome, MergePreview, PrMergeMethod, PullRequest, Repo, Worktree};
use kuaky_term::Launch;
use kuaky_usage::{Agent, AgentLimits, UsageEvent};
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::Arc,
    time::{Duration, Instant},
};

actions!(project, [NewTerminal, CloseTab, NextTab, PrevTab, QuickOpenFile, SearchProject, ToggleSidebar]);

#[path = "metro.rs"]
mod metro;
#[path = "prs.rs"]
mod prs;
#[path = "delegate.rs"]
pub(crate) mod delegate;
pub(crate) use prs::pr_numbers_in;
pub use metro::LiveSummary;

/// How to open an agent chat (ACP).
#[derive(Default)]
struct ChatOpts {
    resume: Option<String>,
    uid: Option<String>,
    line: Option<String>,
    draft: Option<String>,
    role: Option<String>,
    cwd: Option<PathBuf>,
    /// Sesión que la abrió con las herramientas de kuaky.
    opened_by: Option<String>,
    /// Abrir la pestaña sin cambiar a ella ni tomar el foco.
    background: bool,
}

/// How to start an agent terminal.
#[derive(Default)]
struct SpawnOpts {
    /// `None` = new conversation; `Some("")` = latest in the folder; `Some(id)` = that one.
    resume: Option<String>,
    custom_name: Option<String>,
    custom_color: Option<u32>,
    /// Keep this kuaky session ID (resumed sessions keep their metro line).
    uid: Option<String>,
    /// Metro line to attach the session to.
    line: Option<String>,
    /// Text left typed in the agent's input (not sent).
    paste: Option<String>,
    /// Connection role (review, tester, second opinion, relay).
    role: Option<String>,
    /// Extra CLI arguments for this launch only.
    extra_args: Option<String>,
}

pub fn init(cx: &mut App) {
    #[cfg(target_os = "macos")]
    cx.bind_keys([
        KeyBinding::new("cmd-t", NewTerminal, Some("Project")),
        KeyBinding::new("cmd-p", QuickOpenFile, Some("Project")),
        KeyBinding::new("cmd-shift-f", SearchProject, Some("Project")),
        KeyBinding::new("cmd-w", CloseTab, Some("Project")),
        KeyBinding::new("cmd-shift-]", NextTab, Some("Project")),
        KeyBinding::new("cmd-shift-[", PrevTab, Some("Project")),
        KeyBinding::new("cmd-b", ToggleSidebar, Some("Project")),
    ]);
    #[cfg(not(target_os = "macos"))]
    cx.bind_keys([
        KeyBinding::new("ctrl-shift-t", NewTerminal, Some("Project && !Terminal")),
        KeyBinding::new("ctrl-p", QuickOpenFile, Some("Project && !Terminal")),
        KeyBinding::new("ctrl-shift-f", SearchProject, Some("Project && !Terminal")),
        KeyBinding::new("ctrl-shift-w", CloseTab, Some("Project && !Terminal")),
        KeyBinding::new("ctrl-tab", NextTab, Some("Project && !Terminal")),
        KeyBinding::new("ctrl-shift-tab", PrevTab, Some("Project && !Terminal")),
        KeyBinding::new("ctrl-b", ToggleSidebar, Some("Project && !Terminal")),
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
        if let Some(path) = kuaky_git::find_agent(program) {
            return Some((path, args.iter().map(|a| a.to_string()).collect()));
        }
        // Codex: sin `codex-acp` instalado, el adaptador oficial por npx (como Zed),
        // solo si la CLI de Codex está instalada (usa su misma sesión).
        if self == Harness::Codex && kuaky_git::find_agent("codex").is_some() {
            let npx = kuaky_git::find_agent("npx")?;
            return Some((npx, vec!["-y".into(), CODEX_ACP_PACKAGE.into()]));
        }
        None
    }

    /// Stable key used for persisted state and settings.
    pub(crate) fn key(self) -> &'static str {
        self.command().unwrap_or("shell")
    }

    pub(crate) fn from_key(key: &str) -> Option<Harness> {
        Harness::AGENTS.into_iter().chain([Harness::Shell]).find(|h| h.key() == key)
    }

    /// Arguments to resume a conversation: the given one, or the latest in the folder.
    fn resume_args(self, conversation: Option<&str>) -> String {
        let id = conversation.filter(|id| crate::agent_history::valid_id(id));
        match (self, id) {
            (Harness::Claude, Some(id)) => format!(" --resume {id}"),
            (Harness::Claude, None) => " --continue".into(),
            (Harness::Antigravity, Some(id)) => format!(" --conversation {id}"),
            (Harness::Antigravity, None) => " --continue".into(),
            (Harness::Codex, Some(id)) => format!(" resume {id}"),
            (Harness::Codex, None) => " resume --last".into(),
            _ => String::new(),
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

/// Adaptador ACP de Codex (versión fija, la misma que ofrece el registro de Zed).
const CODEX_ACP_PACKAGE: &str = "@agentclientprotocol/codex-acp@2.10.0";

const SESSION_COLORS: &[(&str, u32)] = &[
    ("Teal", 0x5eead4),
    ("Violet", 0xa78bfa),
    ("Coral", 0xe07a5f),
    ("Blue", 0x60a5fa),
    ("Green", 0x4ade80),
    ("Amber", 0xfbbf24),
    ("Pink", 0xf472b6),
    ("Red", 0xf87171),
];

struct RenameSession {
    tab_id: usize,
    input: Entity<InputState>,
    color: Option<u32>,
    _sub: Subscription,
}

struct MergeModal {
    branch: String,
    base: String,
    worktree: Option<PathBuf>,
    preview: Option<Result<MergePreview, String>>,
    busy: bool,
    error: Option<String>,
}

enum TabKind {
    Terminal {
        view: Entity<TerminalView>,
        harness: Harness,
        worktree: Option<String>,
        cwd: PathBuf,
        custom_name: Option<String>,
        custom_color: Option<u32>,
    },
    File(Entity<FileView>),
    Diff(Entity<DiffView>),
    Conflicts(Entity<ConflictView>),
    #[allow(dead_code)]
    Chat { view: Entity<ChatView>, harness: Harness },
    Search(Entity<SearchView>),
    Review(Entity<crate::review_view::ReviewView>),
    Pr(Entity<crate::pr_view::PrView>),
}

struct Tab {
    id: usize,
    kind: TabKind,
}

/// A session being dragged onto another metro line in the sidebar.
#[derive(Clone)]
pub(crate) struct DraggedSession {
    uid: String,
    title: SharedString,
}

impl Render for DraggedSession {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
            .px_2()
            .py_1()
            .rounded_md()
            .border_1()
            .border_color(style::accent())
            .bg(style::panel_hover())
            .text_sm()
            .text_color(style::text())
            .child(self.title.clone())
    }
}

/// Terminals and chats: shown in the left sidebar instead of the tab strip.
fn is_session(kind: &TabKind) -> bool {
    matches!(kind, TabKind::Terminal { .. } | TabKind::Chat { .. })
}

struct RightTerminal {
    id: usize,
    view: Entity<TerminalView>,
}

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum SidePanel {
    Files,
    Changes,
    Branches,
    Prs,
    Actions,
    Graph,
}

impl SidePanel {
    pub(crate) const ALL: [SidePanel; 6] =
        [SidePanel::Files, SidePanel::Changes, SidePanel::Branches, SidePanel::Prs, SidePanel::Actions, SidePanel::Graph];

    pub(crate) fn label(self) -> &'static str {
        match self {
            SidePanel::Files => "Files",
            SidePanel::Changes => "Changes",
            SidePanel::Branches => "Branches",
            SidePanel::Prs => "Pull Requests",
            SidePanel::Actions => "Actions",
            SidePanel::Graph => "Graph",
        }
    }

    pub(crate) fn key(self) -> &'static str {
        match self {
            SidePanel::Files => "files",
            SidePanel::Changes => "changes",
            SidePanel::Branches => "branches",
            SidePanel::Prs => "prs",
            SidePanel::Actions => "actions",
            SidePanel::Graph => "graph",
        }
    }

    /// One line for Settings, so each tab is clear.
    pub(crate) fn about(self) -> &'static str {
        match self {
            SidePanel::Files => "Project file tree.",
            SidePanel::Changes => "Uncommitted changes in this folder.",
            SidePanel::Branches => "Branches and worktrees (separate folders where an agent works on its own branch without touching yours).",
            SidePanel::Prs => "Open pull requests: merge, resolve conflicts, see which session made each one.",
            SidePanel::Actions => "GitHub Actions: launch a workflow by hand (branch and inputs), follow its runs, re-run or cancel.",
            SidePanel::Graph => "Commit history as a graph, like VS Code.",
        }
    }

    fn icon(self) -> &'static str {
        match self {
            SidePanel::Files => "folder",
            SidePanel::Changes => "git-compare",
            SidePanel::Branches => "git-branch",
            SidePanel::Prs => "git-pull-request",
            SidePanel::Actions => "circle-play",
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
    pr_action_error: Option<String>,
    pr_merging: Option<u64>,
    is_merging: bool,
}

pub struct ProjectView {
    root: PathBuf,
    repo: Option<Repo>,
    tabs: Vec<Tab>,
    active: Option<usize>,
    next_id: usize,
    tree: Entity<FileTree>,
    graph: Entity<GraphView>,
    actions: Entity<crate::actions_panel::ActionsPanel>,
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
    /// Línea a la que se agrega la sesión nueva (menú abierto en su encabezado).
    new_session_line: Option<String>,
    /// Cuándo se cerró el menú al pulsar fuera (para que su botón no lo reabra).
    new_session_closed: Option<Instant>,
    /// En el menú de una línea: la sesión se bifurca en una sub-línea.
    new_session_branch: bool,
    new_session_menu: bool,
    /// New sessions open as kuaky's own chat (ACP) instead of a terminal.
    previous_expanded: bool,
    activity_open: bool,
    activity: Vec<ActivityRow>,
    limits: Vec<AgentLimits>,
    focus: FocusHandle,
    /// Previous sessions from the last time this project was open.
    previous: Vec<SavedSession>,
    quick_open: Option<Entity<QuickOpen>>,
    rename_modal: Option<RenameSession>,
    merge_modal: Option<MergeModal>,
    /// Terminal currently resolving merge conflicts via an agent.
    conflict_agent_view: Option<Entity<TerminalView>>,
    /// Shown after the conflict-resolution agent finishes so the user can approve & complete merge.
    merge_approval_ready: bool,
    layout: Entity<ResizableState>,
    lsp: Entity<crate::lsp::LspStore>,
    /// Metro map: goals (lines) and their steps.
    lines: LineStore,
    /// Claude Code task lists per session uid.
    task_watchers: HashMap<String, TaskWatcher>,
    /// Full-screen board (`None` = closed).
    board: Option<metro::Board>,
    /// Chats conectados (revisor ↔ autor).
    relays: Vec<metro::Relay>,
    /// Chat al que se está siguiendo (abre los archivos que toca su agente).
    following: Option<Entity<ChatView>>,
    /// Center split: the session shown on the left and the file/diff/PR on the
    /// right (tab ids). Opening a file never hides the chat.
    shown_session: Option<usize>,
    shown_doc: Option<usize>,
    /// The right (document) side was closed with its ×.
    doc_hidden: bool,
    center_split: Entity<ResizableState>,
    /// Left sidebar folded into a thin rail.
    left_collapsed: bool,
    /// Barra plegada: el cursor está sobre la columna de íconos.
    rail_hovered: bool,
    /// Barra plegada abierta encima de todo mientras el cursor está en ella.
    left_peek: bool,
    layout_collapsed: Entity<ResizableState>,
    /// Sesión (uid) → PR que hizo.
    session_prs: HashMap<String, u64>,
    /// Sessions (tab id) whose subagent list is open in the sidebar.
    /// Sesiones (uid) con su lista de subagentes abierta; se guarda con el proyecto.
    subagents_open: std::collections::HashSet<String>,
    /// Sesión resolviendo conflictos de un PR → se une cuando termine su turno.
    pending_merges: HashMap<String, u64>,
    mini_map_collapsed: bool,
    /// Shown in the window (other projects stay open in the background).
    visible: bool,
    _subs: Vec<Subscription>,
}

impl ProjectView {
    pub fn new(root: PathBuf, usage: Option<Arc<Vec<UsageEvent>>>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let tree = cx.new(|cx| FileTree::new(root.clone(), cx));
        let sub = cx.subscribe_in(&tree, window, |this, _, ev: &FileTreeEvent, window, cx| match ev {
            FileTreeEvent::OpenFile(path) => this.open_file(path.clone(), window, cx),
        });
        let repo = Repo::discover(&root);
        let graph = cx.new(|cx| GraphView::new(repo.clone(), cx));
        let actions = cx.new(|_| crate::actions_panel::ActionsPanel::new(repo.clone()));
        let actions_sub = cx.subscribe_in(&actions, window, |this, _, ev: &crate::actions_panel::ActionsEvent, window, cx| match ev {
            crate::actions_panel::ActionsEvent::Finished { title, message } => {
                let shown = this.panel == Some(SidePanel::Actions);
                this.notify(title.clone(), message.clone(), shown, window, cx);
                cx.notify();
            }
        });
        let graph_sub = cx.subscribe_in(&graph, window, |this, _, ev: &GraphEvent, window, cx| match ev {
            GraphEvent::OpenCommit { hash, short_hash, subject } => {
                let Some(repo) = this.repo.clone() else { return };
                let h = hash.clone();
                this.open_diff(
                    format!("Commit {short_hash}"),
                    subject.clone(),
                    move || repo.diff_commit(&h),
                    window,
                    cx,
                );
            }
        });
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
        // Sessions open when kuaky closed come back by themselves; the rest go to Previous.
        // (Agent terminals without a conversation ID stay in Previous: `--continue`
        // would reopen the same latest conversation for all of them.)
        let capture = std::env::var_os("KUAKY_SCREENSHOT").is_some();
        let (reopen, closed): (Vec<SavedSession>, Vec<SavedSession>) = saved.sessions.clone().into_iter().partition(|s| {
            s.open && !capture && (s.chat || s.harness == Harness::Shell.key() || s.conversation.as_deref().is_some_and(|c| !c.is_empty()))
        });
        let closed: Vec<SavedSession> = closed.into_iter().map(|mut s| {
            s.open = false;
            s
        }).collect();
        let previous = with_agent_history(closed, &root);
        let key_path = root.clone();
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
            graph,
            actions,
            panel: Some(SidePanel::Files),
            right_terminals: Vec::new(),
            active_right_terminal: None,
            right_split,
            git: GitState::default(),
            installed: HashMap::new(),
            chat_agents: HashMap::new(),
            new_session_line: None,
            new_session_closed: None,
            new_session_branch: false,
            new_session_menu: false,
            previous_expanded: false,
            activity_open: false,
            limits: Vec::new(),
            focus: cx.focus_handle(),
            previous,
            quick_open: None,
            rename_modal: None,
            merge_modal: None,
            conflict_agent_view: None,
            merge_approval_ready: false,
            layout,
            lsp,
            lines: {
                let mut lines = LineStore::load(&key_path);
                lines.prune_empty(&[]);
                lines
            },
            task_watchers: HashMap::new(),
            board: None,
            relays: Vec::new(),
            following: None,
            shown_session: None,
            shown_doc: None,
            doc_hidden: false,
            center_split: cx.new(|_| ResizableState::default()),
            left_collapsed: state::get(cx).left_collapsed || (cfg!(feature = "screenshot") && std::env::var("KUAKY_DEMO_COLLAPSED").is_ok()),
            rail_hovered: false,
            left_peek: cfg!(feature = "screenshot") && std::env::var("KUAKY_DEMO_PEEK").is_ok(),
            layout_collapsed: cx.new(|_| ResizableState::default()),
            session_prs: HashMap::new(),
            subagents_open: saved.subagents_open.iter().cloned().collect(),
            pending_merges: HashMap::new(),
            mini_map_collapsed: false,
            visible: true,
            _subs: vec![sub, graph_sub, layout_sub, right_split_sub, lsp_sub],
        };
        this._subs.push(actions_sub);
        let actions_obs = cx.observe(&this.actions, |_, _, cx| cx.notify());
        this._subs.push(actions_obs);
        // Actions se sigue en segundo plano si el repo tiene workflows (para avisar al terminar).
        if this.root.join(".github").join("workflows").is_dir() && kuaky_git::find_program("gh").is_some() {
            let branch = this.git.branch.clone();
            this.actions.update(cx, |a, cx| {
                a.set_branch(branch);
                a.ensure_started(cx);
            });
        }
        this.start_map_refresh(cx);
        this.detect_agents(cx);
        this.start_git_refresh(cx);
        this.start_pr_refresh(cx);
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
        for session in reopen {
            this.previous.insert(0, session);
            this.resume(0, window, cx);
        }
        this
    }

    pub fn sidebar_collapsed(&self) -> bool {
        self.left_collapsed
    }

    pub fn set_visible(&mut self, visible: bool) {
        self.visible = visible;
    }

    /// Focuses the active tab (when switching back to this project).
    pub fn focus_active(&self, window: &mut Window, cx: &mut App) {
        match self.active.and_then(|i| self.tabs.get(i)).map(|t| &t.kind) {
            Some(TabKind::Terminal { view, .. }) => {
                let handle = view.read(cx).focus_handle(cx);
                window.focus(&handle, cx);
            }
            _ => window.focus(&self.focus, cx),
        }
    }

    pub fn on_settings_changed(&mut self, cx: &mut Context<Self>) {
        self.detect_agents(cx);
        // A hidden tab can't stay open.
        if self.panel.is_some_and(|p| !self.visible_panels(cx).contains(&p)) {
            self.panel = self.visible_panels(cx).first().copied();
        }
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
                TabKind::Terminal { harness, worktree, cwd, view, custom_name, custom_color } if !view.read(cx).exited => {
                    let v = view.read(cx);
                    let conversation = v.agent_session.clone().or_else(|| match harness {
                        Harness::Antigravity => crate::agent_history::agy_current(cwd, v.started_at),
                        _ => None,
                    });
                    Some(SavedSession {
                        uid: Some(v.uid.clone()),
                        harness: harness.key().to_string(),
                        cwd: cwd.to_string_lossy().into_owned(),
                        worktree: worktree.clone(),
                        custom_name: custom_name.clone(),
                        custom_color: *custom_color,
                        conversation,
                        chat: false,
                        open: true,
                        from_history: false,
                        updated: None,
                    })
                }
                TabKind::Chat { view, harness } => {
                    let v = view.read(cx);
                    v.session.as_ref()?;
                    Some(SavedSession {
                        uid: Some(v.uid.clone()),
                        harness: harness.key().to_string(),
                        cwd: v.launch.cwd.to_string_lossy().into_owned(),
                        worktree: None,
                        custom_name: v.title.clone(),
                        custom_color: None,
                        conversation: v.session.clone(),
                        chat: true,
                        open: true,
                        from_history: false,
                        updated: None,
                    })
                }
                _ => None,
            })
            .collect();
        for p in self.previous.iter().filter(|p| !p.from_history) {
            let mut p = p.clone();
            p.open = false;
            if !sessions.iter().any(|s| s.uid.is_some() && s.uid == p.uid) && !sessions.contains(&p) {
                sessions.push(p);
            }
        }
        // Solo las de sesiones que siguen guardadas.
        let mut subagents_open: Vec<String> =
            self.subagents_open.iter().filter(|u| sessions.iter().any(|s| s.uid.as_deref() == Some(u.as_str()))).cloned().collect();
        subagents_open.sort();
        let key = self.root.to_string_lossy().into_owned();
        state::update(cx, |s| {
            s.projects.insert(key, state::ProjectState { files, sessions, subagents_open });
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
        if saved.chat {
            let opts = ChatOpts { resume: saved.conversation.clone(), uid: saved.uid.clone(), cwd: Some(cwd), ..Default::default() };
            self.open_chat_with(harness, opts, window, cx);
            return;
        }
        let opts = SpawnOpts {
            resume: saved.conversation.clone().or(Some(String::new())),
            custom_name: saved.custom_name,
            custom_color: saved.custom_color,
            uid: saved.uid,
            ..Default::default()
        };
        self.spawn_terminal(harness, cwd, saved.worktree, opts, window, cx);
    }

    fn open_rename_modal(&mut self, tab_id: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(tab) = self.tabs.iter().find(|t| t.id == tab_id) else { return };
        let TabKind::Terminal { view, custom_name, custom_color, .. } = &tab.kind else { return };
        let initial = custom_name.clone().unwrap_or_else(|| view.read(cx).auto_title().to_string());
        let color = *custom_color;
        let input = cx.new(|cx| {
            let mut s = InputState::new(window, cx).placeholder("Session name (empty = automatic)");
            s.set_value(initial, window, cx);
            s
        });
        let sub = cx.subscribe_in(&input, window, |this, _, ev: &InputEvent, _, cx| {
            if matches!(ev, InputEvent::PressEnter { .. }) {
                this.confirm_rename(cx);
            }
        });
        input.update(cx, |s, cx| s.focus(window, cx));
        self.rename_modal = Some(RenameSession { tab_id, input, color, _sub: sub });
        cx.notify();
    }

    fn confirm_rename(&mut self, cx: &mut Context<Self>) {
        let Some(modal) = self.rename_modal.take() else { return };
        let raw = modal.input.read(cx).value().trim().to_string();
        let name = (!raw.is_empty()).then_some(raw);
        if let Some(tab) = self.tabs.iter_mut().find(|t| t.id == modal.tab_id) {
            if let TabKind::Terminal { custom_name, custom_color, .. } = &mut tab.kind {
                *custom_name = name;
                *custom_color = modal.color;
            }
        }
        self.persist(cx);
        cx.notify();
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
                // Las capturas de desarrollo ponen agentes falsos antes de que termine la detección.
                #[cfg(feature = "screenshot")]
                let chat = {
                    let mut chat = chat;
                    chat.extend(this.chat_agents.iter().filter(|(_, c)| c.0 == Path::new("python3")).map(|(h, c)| (*h, c.clone())));
                    chat
                };
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
                "actions" => Some(SidePanel::Actions),
                "graph" => Some(SidePanel::Graph),
                "none" => None,
                _ => Some(SidePanel::Files),
            };
            self.panel = p;
            if p == Some(SidePanel::Prs) && self.git.prs.is_none() {
                self.load_prs(cx);
            }
            if p == Some(SidePanel::Actions) {
                let branch = self.git.branch.clone();
                self.actions.update(cx, |a, cx| {
                    a.set_branch(branch);
                    a.ensure_started(cx);
                });
            }
        }
        if std::env::var("KUAKY_DEMO_NEW_SESSION").is_ok() {
            self.new_session_menu = true;
        }
        if let Ok(mode) = std::env::var("KUAKY_DEMO_MAP") {
            self.demo_map(mode == "board", window, cx);
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
        if std::env::var("KUAKY_DEMO_RENAME").is_ok() {
            if let Some(tab) = self.tabs.iter_mut().find(|t| matches!(t.kind, TabKind::Terminal { .. })) {
                let id = tab.id;
                if let TabKind::Terminal { custom_name, custom_color, .. } = &mut tab.kind {
                    *custom_name = Some("Refactor auth middleware".into());
                    *custom_color = Some(0xa78bfa);
                }
                self.open_rename_modal(id, window, cx);
            }
        }
        if std::env::var("KUAKY_DEMO_MERGE_MODAL").is_ok() {
            self.merge_modal = Some(MergeModal {
                branch: "kuaky/claude-1124".into(),
                base: self.git.default_branch.clone().unwrap_or_else(|| "main".into()),
                worktree: None,
                preview: Some(Ok(MergePreview::Clean)),
                busy: false,
                error: None,
            });
        }
        if std::env::var("KUAKY_DEMO_CONFLICTS").is_ok() {
            if let Some(repo) = self.repo.clone() {
                let demo_path = std::env::temp_dir().join("kuaky-demo-conflict.rs");
                let sample = "fn greet(name: &str) -> String {\n<<<<<<< HEAD\n    format!(\"Hello, {name}!\")\n=======\n    format!(\"Welcome back, {name}!\")\n>>>>>>> feature/greeting\n}\n";
                let _ = std::fs::write(&demo_path, sample);
                let agents = vec![Harness::Claude, Harness::Antigravity];
                let view = cx.new(|cx| ConflictView::new(repo, vec![demo_path.to_string_lossy().into_owned()], agents, cx));
                self.push_tab(TabKind::Conflicts(view), window, cx);
            }
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
            Ok("delegate") => {
                // Claude abre una sesión de Antigravity con las herramientas de kuaky.
                let script = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../kuaky-mcp/tests/mock_delegator.py");
                for h in [Harness::Claude, Harness::Antigravity] {
                    self.chat_agents.insert(h, (PathBuf::from("python3"), vec![script.to_string_lossy().into_owned()]));
                }
                self.open_chat(Harness::Claude, window, cx);
                if let Some(TabKind::Chat { view, .. }) = self.tabs.last().map(|t| &t.kind) {
                    view.update(cx, |v, cx| v.submit_text("delegate".into(), cx));
                }
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
        let (cwd, worktree) = match self.new_worktree(harness, cx) {
            Some((path, branch)) => (path, Some(branch)),
            None => (self.root.clone(), None),
        };
        self.spawn_terminal(harness, cwd, worktree, SpawnOpts::default(), window, cx);
    }

    /// Carpeta aparte (worktree + rama `kuaky/…`) si Settings lo pide para este agente.
    fn new_worktree(&self, harness: Harness, cx: &App) -> Option<(PathBuf, String)> {
        let settings = &state::get(cx).settings;
        if harness == Harness::Shell || !settings.agent(harness.key()).in_worktree() {
            return None;
        }
        self.create_worktree_for(harness, cx)
    }

    /// Worktree nuevo (rama `kuaky/<agente>-<fecha>`) para un agente.
    fn create_worktree_for(&self, harness: Harness, cx: &App) -> Option<(PathBuf, String)> {
        let settings = &state::get(cx).settings;
        let repo = self.repo.as_ref()?;
        let name = format!("{}-{}", harness.command().unwrap_or("agent"), jiff::Zoned::now().strftime("%m%d-%H%M%S"));
        let created = match settings.resolved_worktrees_dir() {
            Some(dir) => repo.create_worktree_in(&dir, &name),
            None => repo.create_worktree(&name),
        };
        match created {
            Ok(path) => Some((path, format!("kuaky/{name}"))),
            Err(err) => {
                tracing::warn!(%err, "failed to create worktree");
                None
            }
        }
    }

    /// "New session" abre este agente como chat (Settings → Agents; por defecto sí), si tiene chat.
    pub(crate) fn opens_as_chat(&self, harness: Harness, cx: &App) -> bool {
        harness != Harness::Shell
            && state::get(cx).settings.agent(harness.key()).as_chat()
            && (self.chat_agents.contains_key(&harness) || harness.chat_command().is_some())
    }

    /// Sesión nueva desde el menú: chat o terminal y carpeta según Settings → Agents.
    pub fn launch(&mut self, harness: Harness, window: &mut Window, cx: &mut Context<Self>) {
        self.new_session_menu = false;
        self.new_session_line = None;
        self.left_peek = false;
        if self.opens_as_chat(harness, cx) {
            let cwd = self.new_worktree(harness, cx).map(|(path, _)| path);
            self.open_chat_with(harness, ChatOpts { cwd, ..Default::default() }, window, cx);
        } else {
            self.open_terminal(harness, window, cx);
        }
    }

    /// Lista de agentes para empezar una sesión (como el + de Zed). Con `line`,
    /// la sesión se suma a esa línea, en su carpeta.
    pub(crate) fn agent_menu(&self, line: Option<String>, cx: &mut Context<Self>) -> AnyElement {
        let harnesses = self.available_harnesses(cx);
        let item = |h: Harness, this: &Self, cx: &mut Context<Self>| {
            let hint = (h != Harness::Shell && !this.opens_as_chat(h, cx)).then_some("terminal");
            let line = line.clone();
            h_flex()
                .id(SharedString::from(format!("launch-{}-{}", h.label(), line.as_deref().unwrap_or(""))))
                .gap_2()
                .px_2()
                .py_1()
                .rounded_md()
                .text_sm()
                .cursor_pointer()
                .hover(|d| d.bg(style::panel_hover()))
                .child(harness_badge(h))
                .child(div().flex_1().min_w_0().truncate().child(h.label()))
                .when_some(hint, |d, t| d.child(div().text_xs().text_color(style::faint()).child(t)))
                .on_click(cx.listener(move |this, _, window, cx| {
                    cx.stop_propagation();
                    match line.clone() {
                        Some(line) if this.new_session_branch => this.branch_off_line(line, h, window, cx),
                        Some(line) => this.new_session_on_line(line, h, window, cx),
                        None => this.launch(h, window, cx),
                    }
                }))
                .into_any_element()
        };
        let agents: Vec<AnyElement> = harnesses.iter().filter(|h| **h != Harness::Shell).map(|h| item(*h, self, cx)).collect();
        let shell = item(Harness::Shell, self, cx);
        let sep = || div().my_1().h(px(1.)).bg(style::border());
        v_flex()
            .id("agent-menu")
            .occlude()
            .w(px(220.))
            .p_1()
            .rounded_lg()
            .border_1()
            .border_color(style::border())
            .bg(style::panel())
            .shadow_lg()
            .text_color(style::text())
            // En una línea: seguir en ella o bifurcar una sub-línea (trabajo aparte del mismo tema).
            .when(line.is_some(), |d| {
                let branch = self.new_session_branch;
                let chip = |id: &'static str, label: &'static str, on: bool, tip: &'static str| {
                    div()
                        .id(id)
                        .flex_1()
                        .px_2()
                        .py_0p5()
                        .rounded_md()
                        .text_xs()
                        .text_center()
                        .cursor_pointer()
                        .when(on, |d| d.bg(style::panel_hover()).text_color(style::text()))
                        .when(!on, |d| d.text_color(style::muted()))
                        .hover(|d| d.bg(style::panel_hover()))
                        .tooltip(move |window, cx| Tooltip::new(tip).build(window, cx))
                        .child(label)
                };
                d.child(
                    h_flex()
                        .gap_1()
                        .p_0p5()
                        .child(chip("menu-this-line", "This line", !branch, "Another session on the same line").on_click(cx.listener(|this, _, _, cx| {
                            cx.stop_propagation();
                            this.new_session_branch = false;
                            cx.notify();
                        })))
                        .child(chip("menu-sub-line", "Branch off", branch, "A sub-line under this one: separate work on the same topic").on_click(cx.listener(|this, _, _, cx| {
                            cx.stop_propagation();
                            this.new_session_branch = true;
                            cx.notify();
                        }))),
                )
                .child(div().my_1().h(px(1.)).bg(style::border()))
            })
            .when(agents.is_empty(), |d| d.child(div().px_2().py_1().text_xs().text_color(style::faint()).child("No agents found")))
            .children(agents)
            .child(sep())
            .child(shell)
            .child(sep())
            .child(
                h_flex()
                    .id("agent-menu-settings")
                    .gap_2()
                    .px_2()
                    .py_1()
                    .rounded_md()
                    .text_xs()
                    .text_color(style::muted())
                    .cursor_pointer()
                    .hover(|d| d.bg(style::panel_hover()).text_color(style::text()))
                    .child(icons::ui("settings", px(13.), style::muted()))
                    .child("Configure agents…")
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.new_session_menu = false;
                        this.new_session_line = None;
                        window.dispatch_action(Box::new(crate::app::OpenSettings), cx);
                        cx.notify();
                    })),
            )
            .on_mouse_down_out(cx.listener(|this, _, _, cx| {
                if this.new_session_menu || this.new_session_line.is_some() {
                    this.new_session_menu = false;
                    this.new_session_line = None;
                    this.new_session_closed = Some(Instant::now());
                    this.left_peek = false;
                    cx.notify();
                }
            }))
            .into_any_element()
    }

    /// ¿Se acaba de cerrar el menú al pulsar fuera? (el clic en su botón no lo reabre)
    pub(crate) fn menu_just_closed(&self) -> bool {
        self.new_session_closed.is_some_and(|t| t.elapsed() < Duration::from_millis(300))
    }

    #[allow(clippy::too_many_arguments)]
    fn spawn_terminal(
        &mut self,
        harness: Harness,
        cwd: PathBuf,
        worktree: Option<String>,
        opts: SpawnOpts,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let SpawnOpts { resume, custom_name, custom_color, uid, line, paste, role, extra_args } = opts;
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
            let mut full = match &resume {
                Some(id) => format!("{base}{}", harness.resume_args(Some(id.as_str()).filter(|s| !s.is_empty()))),
                None => base,
            };
            if !cfg.extra_args.trim().is_empty() {
                full.push(' ');
                full.push_str(cfg.extra_args.trim());
            }
            if let Some(extra) = extra_args.as_deref().filter(|e| !e.trim().is_empty()) {
                full.push(' ');
                full.push_str(extra.trim());
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
        let launch = Launch { cwd: cwd.clone(), command, prefill: None, env };
        let branch = worktree.clone();
        self.add_terminal(harness, launch, worktree, status_file, custom_name, custom_color, window, cx);
        let Some(Tab { kind: TabKind::Terminal { view, .. }, .. }) = self.tabs.last() else { return };
        let view = view.clone();
        let conversation = resume.filter(|id| crate::agent_history::valid_id(id));
        view.update(cx, |v, cx| {
            if let Some(uid) = uid {
                v.uid = uid;
            }
            if let Some(id) = &conversation {
                v.agent_session = Some(id.clone());
            }
            if let Some(text) = paste {
                v.paste_when_ready(text, cx);
            }
        });
        if harness != Harness::Shell {
            let uid = view.read(cx).uid.clone();
            let line_id = line
                .filter(|l| self.lines.get(l).is_some())
                .or_else(|| self.lines.line_of(&uid).map(|l| l.id.clone()))
                .unwrap_or_else(|| self.lines.create(&format!("New {} session", harness.label()), LineKind::Task, None, false));
            self.lines.attach(
                &line_id,
                LineSession {
                    uid: uid.clone(),
                    harness: harness.key().to_string(),
                    conversation: conversation.clone(),
                    cwd: cwd.to_string_lossy().into_owned(),
                    label: harness.label().to_string(),
                    last_active: jiff::Timestamp::now(),
                    role,
                    chat: false,
                    opened_by: None,
                },
            );
            if branch.is_some() {
                self.lines.set_branch(&line_id, branch);
            }
            if harness == Harness::Claude {
                if let Some(id) = &conversation {
                    self.task_watchers.insert(uid, TaskWatcher::for_claude_session(id));
                }
            }
        }
        self.persist(cx);
    }

    #[allow(clippy::too_many_arguments)]
    fn add_terminal(
        &mut self,
        harness: Harness,
        launch: Launch,
        worktree: Option<String>,
        status_file: Option<PathBuf>,
        custom_name: Option<String>,
        custom_color: Option<u32>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let cwd = launch.cwd.clone();
        let view = cx.new(|cx| TerminalView::new(harness.label(), launch, status_file, window, cx));
        let sub = cx.subscribe_in(&view, window, move |this, view, ev: &TerminalViewEvent, window, cx| {
            match ev {
                // While kuaky quits, terminals end too: don't save them as gone.
                TerminalViewEvent::Exited if !crate::app::quitting() => this.persist(cx),
                TerminalViewEvent::Exited => {}
                TerminalViewEvent::AgentState => {
                    this.on_agent_state(view, harness, window, cx);
                    let v = view.read(cx);
                    if matches!(v.agent_state, Some(AgentState::Done)) {
                        let uid = v.uid.clone();
                        this.lines.on_turn_done(&uid);
                    }
                    this.persist(cx);
                }
                TerminalViewEvent::TitleChanged => {
                    this.on_session_title(view, harness, cx);
                    this.persist(cx);
                }
                TerminalViewEvent::Prompt(text) => {
                    let uid = view.read(cx).uid.clone();
                    this.lines.on_prompt(&uid, text);
                }
            }
            cx.notify();
        });
        let obs = cx.observe(&view, |_, _, cx| cx.notify());
        self._subs.extend([sub, obs]);
        self.push_tab(TabKind::Terminal { view, harness, worktree, cwd, custom_name, custom_color }, window, cx);
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
        if matches!(state, AgentState::Done) && was_working {
            let uid = t.uid.clone();
            self.finish_pending_merge(&uid, window, cx);
        }
        if matches!(state, AgentState::Done) && self.conflict_agent_view.as_ref() == Some(view) {
            self.conflict_agent_view = None;
            self.merge_approval_ready = true;
            if let Some(repo) = self.repo.clone() {
                self.open_diff(
                    "Conflict resolution diff".into(),
                    "review changes before completing merge".into(),
                    move || repo.diff_worktree(),
                    window,
                    cx,
                );
            }
            self.notify(
                format!("{} resolved conflicts", harness.label()),
                "Review the diff and click Complete merge.".into(),
                false,
                window,
                cx,
            );
            cx.notify();
            return;
        }
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
        if !is_active_tab || !self.visible || !window_active {
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
        self.add_terminal(Harness::Shell, launch, None, None, None, None, window, cx);
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
                .child(
                    div()
                        .px_2()
                        .text_xs()
                        .text_color(style::faint())
                        .whitespace_normal()
                        .child("Separate folders where an agent works on its own branch without touching your files."),
                )
                .children(worktrees.into_iter().enumerate().map(|(i, wt)| {
                    let branch = wt.branch.clone().unwrap_or_else(|| "(detached)".into());
                    let name = wt.path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                    let (wt1, wt2, wt3) = (wt.clone(), wt.clone(), wt.clone());
                    let (b1, b2, b3) = (base.clone(), branch.clone(), branch.clone());
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
                                        None,
                                        None,
                                        window,
                                        cx,
                                    );
                                })))
                                .child(action(("wt-pr", i), "create PR").on_click(cx.listener(move |this, _, window, cx| {
                                    let text = format!("git push -u origin {b2} && gh pr create --fill --head {b2}");
                                    this.prefilled_shell(wt3.path.clone(), text, window, cx);
                                })))
                                .child(action(("wt-merge", i), "merge / discard").on_click(cx.listener({
                                    let wt_path = wt.path.clone();
                                    move |this, _, _, cx| {
                                        this.open_merge_modal(b3.clone(), Some(wt_path.clone()), cx);
                                    }
                                }))),
                        )
                }))
                .into_any_element(),
        )
    }

    #[allow(dead_code)]
    pub fn open_chat(&mut self, harness: Harness, window: &mut Window, cx: &mut Context<Self>) {
        self.open_chat_with(harness, ChatOpts::default(), window, cx);
    }

    /// Opens kuaky's own chat (ACP) with an agent, attached to a metro line.
    fn open_chat_with(&mut self, harness: Harness, opts: ChatOpts, window: &mut Window, cx: &mut Context<Self>) {
        self.new_session_menu = false;
        let Some((program, args)) = self.chat_agents.get(&harness).cloned().or_else(|| harness.chat_command()) else {
            return;
        };
        let cwd = opts.cwd.clone().filter(|c| c.is_dir()).unwrap_or_else(|| self.root.clone());
        // El uid va en las herramientas de kuaky (así sabe qué sesión las llama).
        let uid = opts.uid.clone().unwrap_or_else(crate::terminal_view::new_uid);
        let launch = ChatLaunch {
            label: harness.label(),
            program,
            args,
            cwd: cwd.clone(),
            resume: opts.resume.clone(),
            draft: opts.draft.clone(),
            tools: delegate::mcp_server(&uid).into_iter().collect(),
            background: opts.background,
        };
        let view = cx.new(|cx| ChatView::new(launch, window, cx));
        view.update(cx, |v, _| v.uid = uid.clone());
        let line_id = opts
            .line
            .filter(|l| self.lines.get(l).is_some())
            .or_else(|| self.lines.line_of(&uid).map(|l| l.id.clone()))
            .unwrap_or_else(|| self.lines.create(&format!("New {} chat", harness.label()), LineKind::Task, None, false));
        self.lines.attach(
            &line_id,
            LineSession {
                uid: uid.clone(),
                harness: harness.key().to_string(),
                conversation: opts.resume.clone(),
                cwd: cwd.to_string_lossy().into_owned(),
                label: format!("{} · chat", harness.label()),
                last_active: jiff::Timestamp::now(),
                role: opts.role.clone(),
                chat: true,
                opened_by: opts.opened_by.clone(),
            },
        );
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
                ChatEvent::OpenReview { title, files } => this.open_review(view.clone(), title.clone(), files.clone(), window, cx),
                ChatEvent::NeedsYou(msg) => {
                    this.notify(format!("{} needs your permission", harness.label()), msg.clone(), is_active_tab, window, cx)
                }
                ChatEvent::TurnDone => {
                    let uid = view.read(cx).uid.clone();
                    this.lines.on_turn_done(&uid);
                    this.relay_turn_done(&uid, cx);
                    this.finish_pending_merge(&uid, window, cx);
                    this.sync_session_prs(cx);
                    this.notify(format!("{} finished", harness.label()), "Your turn.".into(), is_active_tab, window, cx)
                }
                ChatEvent::Status => {}
                ChatEvent::Follow { path, line } => {
                    if view.read(cx).follow {
                        this.following = Some(view.clone());
                        this.open_file_at(path.clone(), line.saturating_sub(1), 0, window, cx);
                    }
                }
                ChatEvent::OpenPr(n) => this.open_pr(*n, window, cx),
                ChatEvent::MergePr(n) => {
                    let conflicts = this.open_prs().iter().find(|p| p.number == *n).is_some_and(|p| p.has_conflicts());
                    if conflicts {
                        this.resolve_and_merge(*n, window, cx);
                    } else {
                        this.merge_pr(*n, window, cx);
                    }
                }
                ChatEvent::RelayAuto(on) => {
                    let uid = view.read(cx).uid.clone();
                    this.set_relay_auto(&uid, *on, cx);
                }
                ChatEvent::OpenFile { path, line } => this.open_file_at(path.clone(), line.saturating_sub(1), 0, window, cx),
                ChatEvent::Prompt(text) => {
                    let uid = view.read(cx).uid.clone();
                    this.lines.on_prompt(&uid, text);
                }
                ChatEvent::Plan(entries) => {
                    let uid = view.read(cx).uid.clone();
                    let tasks: Vec<crate::agent_tasks::AgentTask> = entries
                        .iter()
                        .map(|e| crate::agent_tasks::AgentTask {
                            subject: e.content.clone(),
                            status: match e.status.as_str() {
                                "completed" => crate::agent_tasks::TaskStatus::Completed,
                                "in_progress" => crate::agent_tasks::TaskStatus::InProgress,
                                _ => crate::agent_tasks::TaskStatus::Pending,
                            },
                        })
                        .collect();
                    this.lines.set_tasks(&uid, &tasks);
                }
                ChatEvent::Session(id) => {
                    let uid = view.read(cx).uid.clone();
                    this.lines.set_conversation(&uid, id);
                    this.persist(cx);
                }
                ChatEvent::Title(t) => {
                    let uid = view.read(cx).uid.clone();
                    this.lines.on_title(&uid, t);
                    this.persist(cx);
                }
            }
            cx.notify();
        });
        self._subs.push(sub);
        if opts.background {
            self.tabs.push(Tab { id: self.next_id, kind: TabKind::Chat { view, harness } });
            self.next_id += 1;
            self.persist(cx);
            cx.notify();
        } else {
            self.push_tab(TabKind::Chat { view, harness }, window, cx);
        }
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
        // Closing the last open session of a metro line archives the line.
        match &removed.kind {
            TabKind::Terminal { view, harness, .. } if *harness != Harness::Shell => {
                let uid = view.read(cx).uid.clone();
                self.archive_if_last(&uid, cx);
            }
            TabKind::Chat { view, .. } => {
                let uid = view.read(cx).uid.clone();
                self.archive_if_last(&uid, cx);
            }
            _ => {}
        }
        if let TabKind::File(f) = &removed.kind {
            if let Some(uri) = f.read(cx).lsp_uri() {
                self.lsp.update(cx, |s, _| s.detach(&uri));
            }
        }
        self.persist(cx);
        let was_session = is_session(&removed.kind);
        if self.shown_session == Some(removed.id) {
            self.shown_session = None;
        }
        if self.shown_doc == Some(removed.id) {
            self.shown_doc = None;
        }
        if self.tabs.is_empty() {
            self.active = None;
            window.focus(&self.focus, cx);
        } else {
            // Next tab of the same kind (closing a file keeps the chat in place).
            let same = |t: &Tab| is_session(&t.kind) == was_session;
            let next = (index..self.tabs.len())
                .chain((0..index).rev())
                .find(|&i| same(&self.tabs[i]))
                .or_else(|| self.shown_session.and_then(|id| self.tabs.iter().position(|t| t.id == id)))
                .unwrap_or(index.min(self.tabs.len() - 1));
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

    /// Revisión de un turno del chat: bloques para aceptar/rechazar y comentarios.
    fn open_review(
        &mut self,
        chat: Entity<ChatView>,
        title: String,
        files: Vec<(PathBuf, Option<String>, String)>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        use crate::review_view::{ReviewEvent, ReviewView};
        if let Some(i) = self.tabs.iter().position(|t| matches!(&t.kind, TabKind::Review(r) if r.read(cx).title.as_ref() == title)) {
            self.tabs.remove(i);
        }
        let root = self.root.clone();
        let view = cx.new(|cx| ReviewView::new(title, root, files, window, cx));
        let weak_chat = chat.downgrade();
        let sub = cx.subscribe_in(&view, window, move |this, _, ev: &ReviewEvent, window, cx| match ev {
            ReviewEvent::SendToAgent(text) => {
                let Some(chat) = weak_chat.upgrade() else { return };
                chat.update(cx, |c, cx| c.submit_text(text.clone(), cx));
                if let Some(i) = this.tabs.iter().position(|t| matches!(&t.kind, TabKind::Chat { view, .. } if *view == chat)) {
                    this.activate(i, window, cx);
                }
            }
            ReviewEvent::OpenFile { path, line } => this.open_file_at(path.clone(), line.saturating_sub(1), 0, window, cx),
        });
        self._subs.push(sub);
        self.push_tab(TabKind::Review(view), window, cx);
    }

    // ── git ─────────────────────────────────────────────────────────────

    /// Open PRs every 2 minutes (to link each session with its PR).
    fn start_pr_refresh(&self, cx: &mut Context<Self>) {
        if self.repo.is_none() || kuaky_git::find_program("gh").is_none() {
            return;
        }
        cx.spawn(async move |this, cx| loop {
            if this.update(cx, |this, cx| this.load_prs(cx)).is_err() {
                break;
            }
            cx.background_executor().timer(Duration::from_secs(120)).await;
        })
        .detach();
    }

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
                let (branch, changes, branches, default_branch, worktrees, is_merging) = cx
                    .background_executor()
                    .spawn(async move {
                        (
                            r.current_branch().ok(),
                            r.status().unwrap_or_default(),
                            r.branches().unwrap_or_default(),
                            if with_branches { r.default_branch() } else { None },
                            r.worktrees().unwrap_or_default(),
                            r.is_merging(),
                        )
                    })
                    .await;
                let alive = this
                    .update(cx, |this, cx| {
                        let branches_changed = this.git.branches != branches || this.git.branch != branch;
                        if this.git.branch != branch {
                            let b = branch.clone();
                            this.actions.update(cx, |a, _| a.set_branch(b));
                        }
                        this.git.branch = branch;
                        this.git.branches = branches;
                        this.git.worktrees = worktrees;
                        if this.git.prs.is_some() {
                            this.sync_session_prs(cx);
                        }
                        this.git.is_merging = is_merging;
                        if !is_merging {
                            this.merge_approval_ready = false;
                        }
                        if first {
                            this.git.default_branch = default_branch;
                        }
                        if branches_changed {
                            this.graph.update(cx, |g, cx| g.refresh(cx));
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
                this.sync_session_prs(cx);
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
            if panel == SidePanel::Graph {
                self.graph.update(cx, |g, cx| g.refresh(cx));
            }
            if panel == SidePanel::Actions {
                let branch = self.git.branch.clone();
                self.actions.update(cx, |a, cx| {
                    a.set_branch(branch);
                    a.ensure_started(cx);
                });
            }
        }
        cx.notify();
    }

    fn open_merge_modal(&mut self, branch: String, worktree: Option<PathBuf>, cx: &mut Context<Self>) {
        let Some(repo) = self.repo.clone() else { return };
        let base = self.git.branch.clone().or_else(|| self.git.default_branch.clone()).unwrap_or_else(|| "main".into());
        self.merge_modal = Some(MergeModal {
            branch: branch.clone(),
            base: base.clone(),
            worktree,
            preview: None,
            busy: false,
            error: None,
        });
        cx.notify();
        cx.spawn(async move |this, cx| {
            let res = cx
                .background_executor()
                .spawn(async move { repo.preview_merge(&base, &branch).map_err(|e| format!("{e:#}")) })
                .await;
            this.update(cx, |this, cx| {
                if let Some(m) = &mut this.merge_modal {
                    m.preview = Some(res);
                    cx.notify();
                }
            })
            .ok();
        })
        .detach();
    }

    fn run_merge(&mut self, method: MergeMethod, window: &mut Window, cx: &mut Context<Self>) {
        let Some(repo) = self.repo.clone() else { return };
        let Some(modal) = self.merge_modal.as_mut() else { return };
        if modal.busy {
            return;
        }
        let branch = modal.branch.clone();
        modal.busy = true;
        modal.error = None;
        cx.notify();
        cx.spawn_in(window, async move |this, cx| {
            let b = branch.clone();
            let res = cx
                .background_executor()
                .spawn(async move { repo.merge_branch(&b, method).map_err(|e| format!("{e:#}")) })
                .await;
            this.update_in(cx, |this, window, cx| match res {
                Ok(MergeOutcome::Merged) => {
                    this.merge_modal = None;
                    this.graph.update(cx, |g, cx| g.refresh(cx));
                    this.notify(format!("Merged {branch}"), "Branch merged cleanly.".into(), false, window, cx);
                    cx.notify();
                }
                Ok(MergeOutcome::Conflicts(files)) => {
                    this.merge_modal = None;
                    this.git.is_merging = true;
                    this.open_conflicts(files, window, cx);
                    cx.notify();
                }
                Err(err) => {
                    if let Some(m) = &mut this.merge_modal {
                        m.busy = false;
                        m.error = Some(err);
                    }
                    cx.notify();
                }
            })
            .ok();
        })
        .detach();
    }

    fn run_discard(&mut self, cx: &mut Context<Self>) {
        let Some(repo) = self.repo.clone() else { return };
        let Some(modal) = self.merge_modal.as_mut() else { return };
        if modal.busy {
            return;
        }
        let branch = modal.branch.clone();
        let wt = modal.worktree.clone();
        modal.busy = true;
        modal.error = None;
        cx.notify();
        cx.spawn(async move |this, cx| {
            let res = cx
                .background_executor()
                .spawn(async move { repo.discard_branch(&branch, wt.as_deref()).map_err(|e| format!("{e:#}")) })
                .await;
            this.update(cx, |this, cx| match res {
                Ok(()) => {
                    this.merge_modal = None;
                    this.graph.update(cx, |g, cx| g.refresh(cx));
                    cx.notify();
                }
                Err(err) => {
                    if let Some(m) = &mut this.merge_modal {
                        m.busy = false;
                        m.error = Some(err);
                    }
                    cx.notify();
                }
            })
            .ok();
        })
        .detach();
    }

    fn open_conflicts(&mut self, files: Vec<String>, window: &mut Window, cx: &mut Context<Self>) {
        let Some(repo) = self.repo.clone() else { return };
        let agents: Vec<Harness> = self.available_harnesses(cx).into_iter().filter(|h| *h != Harness::Shell).collect();
        if let Some(i) = self.tabs.iter().position(|t| matches!(t.kind, TabKind::Conflicts(_))) {
            if let TabKind::Conflicts(v) = &self.tabs[i].kind {
                v.update(cx, |v, cx| {
                    v.set_agents(agents, cx);
                    v.reload(cx);
                });
            }
            self.activate(i, window, cx);
            return;
        }
        let view = cx.new(|cx| ConflictView::new(repo, files, agents, cx));
        let sub = cx.subscribe_in(&view, window, |this, _, ev: &ConflictEvent, window, cx| match ev {
            ConflictEvent::ResolveWithAgent { harness, files } => {
                this.resolve_conflicts_with_agent(*harness, files.clone(), window, cx);
            }
            ConflictEvent::MergeFinished => {
                this.merge_approval_ready = false;
                this.git.is_merging = false;
                this.graph.update(cx, |g, cx| g.refresh(cx));
                if let Some(i) = this.tabs.iter().position(|t| matches!(t.kind, TabKind::Conflicts(_))) {
                    this.close_tab(i, window, cx);
                }
                cx.notify();
            }
        });
        self._subs.push(sub);
        self.push_tab(TabKind::Conflicts(view), window, cx);
    }

    fn resolve_conflicts_with_agent(&mut self, harness: Harness, files: Vec<String>, window: &mut Window, cx: &mut Context<Self>) {
        let file_list = if files.is_empty() {
            "all conflicted files".to_string()
        } else {
            files.join(", ")
        };
        let prompt = format!(
            "Resolve the git merge conflicts in the following files: {file_list}. Keep the intent of both sides where appropriate, remove all conflict markers, and run the project tests to verify everything passes. Do NOT run git commit."
        );
        let cfg = state::get(cx).settings.agent(harness.key());
        let base = if !cfg.binary_path.trim().is_empty() {
            kuaky_git::shell_command(Path::new(cfg.binary_path.trim()))
        } else if let Some(name) = harness.command() {
            match self.installed.get(&harness) {
                Some(path) if kuaky_git::find_program(name).is_none() => kuaky_git::shell_command(path),
                _ => name.to_string(),
            }
        } else {
            return;
        };
        let mut cmd = base;
        if !cfg.extra_args.trim().is_empty() {
            cmd.push(' ');
            cmd.push_str(cfg.extra_args.trim());
        }
        let mut env = Vec::new();
        let mut status_file = None;
        if harness == Harness::Claude && cfg!(unix) {
            if let (Some(settings), Some(file)) = (crate::agent_status::claude_settings(), crate::agent_status::new_status_file()) {
                cmd = format!("{cmd} --settings {}", kuaky_git::shell_command(settings));
                env.push(("KUAKY_STATUS_FILE".to_string(), file.to_string_lossy().into_owned()));
                status_file = Some(file);
            }
        }
        let escaped_prompt = format!("'{}'", prompt.replace('\'', "'\\''"));
        let full_cmd = format!("{cmd} {escaped_prompt}");
        let launch = Launch { cwd: self.root.clone(), command: Some(full_cmd), prefill: None, env };
        self.add_terminal(
            harness,
            launch,
            None,
            status_file,
            Some("Resolve conflicts".into()),
            Some(0xf87171),
            window,
            cx,
        );
        if let Some(TabKind::Terminal { view, .. }) = self.tabs.last().map(|t| &t.kind) {
            self.conflict_agent_view = Some(view.clone());
        }
    }

    fn complete_active_merge(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(repo) = self.repo.clone() else { return };
        cx.spawn_in(window, async move |this, cx| {
            let res = cx
                .background_executor()
                .spawn(async move { repo.complete_merge(None).map_err(|e| format!("{e:#}")) })
                .await;
            this.update_in(cx, |this, window, cx| match res {
                Ok(()) => {
                    this.merge_approval_ready = false;
                    this.git.is_merging = false;
                    this.graph.update(cx, |g, cx| g.refresh(cx));
                    if let Some(i) = this.tabs.iter().position(|t| matches!(t.kind, TabKind::Conflicts(_))) {
                        this.close_tab(i, window, cx);
                    }
                    this.notify("Merge completed".into(), "All conflicts resolved and committed.".into(), false, window, cx);
                    cx.notify();
                }
                Err(err) => {
                    this.notify("Cannot complete merge".into(), err, false, window, cx);
                }
            })
            .ok();
        })
        .detach();
    }

    fn abort_active_merge(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(repo) = self.repo.clone() else { return };
        cx.spawn_in(window, async move |this, cx| {
            let res = cx.background_executor().spawn(async move { repo.abort_merge() }).await;
            this.update_in(cx, |this, window, cx| {
                if res.is_ok() {
                    this.merge_approval_ready = false;
                    this.git.is_merging = false;
                    this.graph.update(cx, |g, cx| g.refresh(cx));
                    if let Some(i) = this.tabs.iter().position(|t| matches!(t.kind, TabKind::Conflicts(_))) {
                        this.close_tab(i, window, cx);
                    }
                    cx.notify();
                }
            })
            .ok();
        })
        .detach();
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

    /// One row of the open-sessions list.
    fn session_row(&self, i: usize, tab: &Tab, now: Instant, cx: &mut Context<Self>) -> Option<AnyElement> {
                                let (harness, title, status_line, dot_color, worktree, accent_color, can_rename): (
                                    Harness,
                                    SharedString,
                                    String,
                                    Hsla,
                                    Option<String>,
                                    Option<u32>,
                                    bool,
                                ) = match &tab.kind {
                                    TabKind::Terminal { view, harness, worktree, custom_name, custom_color, .. } => {
                                        let t = view.read(cx);
                                        let (status, dot) = terminal_status(t, now);
                                        let title = custom_name
                                            .as_ref()
                                            .filter(|s| !s.is_empty())
                                            .map(|s| SharedString::from(s.clone()))
                                            .unwrap_or_else(|| t.auto_title());
                                        let when = ago(t.last_activity_at());
                                        (*harness, title, format!("{status} · {when}"), dot, worktree.clone(), *custom_color, true)
                                    }
                                    TabKind::Chat { view, harness } => {
                                        let v = view.read(cx);
                                        let (status, dot) = chat_status(v);
                                        let title = v
                                            .title
                                            .clone()
                                            .or_else(|| v.first_prompt().map(|p| crate::lines::short(p, 40)))
                                            .unwrap_or_else(|| format!("{} · chat", harness.label()));
                                        (*harness, title.into(), format!("chat · {status}"), dot, None, None, false)
                                    }
                                    _ => return None,
                                };
                                let tab_id = tab.id;
                                let uid = match &tab.kind {
                                    TabKind::Terminal { view, .. } => view.read(cx).uid.clone(),
                                    TabKind::Chat { view, .. } => view.read(cx).uid.clone(),
                                    _ => String::new(),
                                };
                                let pr = self.session_prs.get(&uid).copied().map(|n| {
                                    let conflicts = self.open_prs().iter().find(|p| p.number == n).is_some_and(|p| p.has_conflicts());
                                    (n, conflicts)
                                });
                                let dragged = DraggedSession { uid, title: title.clone() };
        let selected = self.shown_session == Some(tab_id);
        let group: SharedString = format!("session-row-{tab_id}").into();
        let tip: SharedString = match &worktree {
            Some(w) => format!("{title}\n{status_line} · worktree {w}").into(),
            None => format!("{title}\n{status_line}").into(),
        };
        let _ = accent_color;
        let row = h_flex()
                .id(("session", tab_id))
                .relative()
                .group(group.clone())
                .on_drag(dragged, |d, _, _, cx| cx.new(|_| d.clone()))
                .gap_2()
                .px_2()
                .py_1()
                .rounded_md()
                .cursor_pointer()
                .when(selected, |d| d.bg(style::panel_hover()))
                .hover(|d| d.bg(style::panel_hover()))
                .tooltip(move |window, cx| Tooltip::new(tip.clone()).build(window, cx))
                .child(div().flex_none().child(harness_badge(harness)))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .text_sm()
                        .text_color(if selected { style::text() } else { style::muted() })
                        .child(title),
                )
                .when_some(pr, |d, (n, conflicts)| {
                    let color = if conflicts { style::bad() } else { style::muted() };
                    d.child(
                        h_flex()
                            .id(("session-pr", tab_id))
                            .flex_none()
                            .gap_0p5()
                            .text_xs()
                            .text_color(color)
                            .cursor_pointer()
                            .hover(|d| d.text_color(style::text()))
                            .child(icons::ui("git-pull-request", px(11.), color))
                            .child(format!("{n}"))
                            .on_click(cx.listener(move |this, _, window, cx| {
                                cx.stop_propagation();
                                this.open_pr(n, window, cx);
                            })),
                    )
                })
                // State dot; on hover it gives way to rename / close.
                .child(status_dot(i, dot_color, group.clone()))
                .child(
                    h_flex()
                        .absolute()
                        .right(px(4.))
                        .top_0()
                        .bottom_0()
                        .px_1()
                        .gap_0p5()
                        .items_center()
                        .bg(style::panel_hover())
                        .invisible()
                        .group_hover(group, |s| s.visible())
                        .when(can_rename, |d| {
                            d.child(
                                div()
                                    .id(("session-rename", tab_id))
                                    .p_0p5()
                                    .rounded_sm()
                                    .hover(|d| d.bg(style::bg()))
                                    .child(icons::ui("pencil", px(11.), style::faint()))
                                    .on_click(cx.listener(move |this, _, window, cx| {
                                        cx.stop_propagation();
                                        this.open_rename_modal(tab_id, window, cx);
                                    })),
                            )
                        })
                        .child(
                            div()
                                .id(("session-close", tab_id))
                                .p_0p5()
                                .rounded_sm()
                                .hover(|d| d.bg(style::bg()))
                                .child(icons::ui("x", px(12.), style::faint()))
                                .on_click(cx.listener(move |this, _, window, cx| {
                                    cx.stop_propagation();
                                    this.close_tab(i, window, cx);
                                })),
                        ),
                )
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |this, ev: &MouseDownEvent, window, cx| {
                        if can_rename && ev.click_count >= 2 {
                            this.open_rename_modal(tab_id, window, cx);
                        } else {
                            this.activate(i, window, cx);
                        }
                    }),
                )
                .on_mouse_down(
                    MouseButton::Right,
                    cx.listener(move |this, _, window, cx| {
                        if can_rename {
                            this.open_rename_modal(tab_id, window, cx);
                        }
                    }),
                )
                .into_any_element();
        Some(self.with_subagents(row, i, tab, cx))
    }

    /// Subagentes visibles de una sesión (corriendo, o terminados hace menos de 2 min).
    fn session_subagents(&self, tab: &Tab, cx: &App) -> Vec<crate::agent_tasks::Subagent> {
        let now = jiff::Timestamp::now();
        let all = match &tab.kind {
            TabKind::Terminal { view, .. } => {
                let uid = view.read(cx).uid.clone();
                self.task_watchers.get(&uid).map(|w| w.subagents().to_vec()).unwrap_or_default()
            }
            TabKind::Chat { view, .. } => view.read(cx).subagents(),
            _ => Vec::new(),
        };
        all.into_iter().filter(|a| a.visible(now)).collect()
    }

    /// Debajo de la sesión: una línea plegable con sus subagentes.
    fn with_subagents(&self, row: AnyElement, i: usize, tab: &Tab, cx: &mut Context<Self>) -> AnyElement {
        let subs = self.session_subagents(tab, cx);
        if subs.is_empty() {
            return row;
        }
        let tab_id = tab.id;
        let uid = match &tab.kind {
            TabKind::Terminal { view, .. } => view.read(cx).uid.clone(),
            TabKind::Chat { view, .. } => view.read(cx).uid.clone(),
            _ => String::new(),
        };
        let open = self.subagents_open.contains(&uid) || (cfg!(feature = "screenshot") && std::env::var("KUAKY_DEMO_SUBAGENTS").is_ok());
        let running = subs.iter().filter(|a| a.running()).count();
        let summary = if running > 0 {
            format!("{running} subagent{} working", if running == 1 { "" } else { "s" })
        } else {
            format!("{} subagent{} done", subs.len(), if subs.len() == 1 { "" } else { "s" })
        };
        let now = jiff::Timestamp::now();
        let spinner = |id: SharedString, color: Hsla| crate::motion::spinner(id, color, px(11.)).into_any_element();
        const MAX_ROWS: usize = 4;
        let shown: Vec<&crate::agent_tasks::Subagent> = subs.iter().rev().take(MAX_ROWS).collect();
        let more = subs.len().saturating_sub(MAX_ROWS);
        v_flex()
            .child(row)
            .child(
                h_flex()
                    .id(("subagents", tab_id))
                    .pl(px(32.))
                    .pr_2()
                    .py_0p5()
                    .gap_1()
                    .items_center()
                    .rounded_md()
                    .cursor_pointer()
                    .text_xs()
                    .text_color(if running > 0 { style::muted() } else { style::faint() })
                    .hover(|d| d.bg(style::panel_hover()))
                    .child(icons::ui(if open { "chevron-down" } else { "chevron-right" }, px(11.), style::faint()))
                    .when(running > 0 && !open, |d| d.child(spinner(format!("subs-spin-{tab_id}").into(), style::muted().into())))
                    .child(div().truncate().child(summary))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        if !this.subagents_open.remove(&uid) {
                            this.subagents_open.insert(uid.clone());
                        }
                        this.persist(cx);
                        cx.notify();
                    })),
            )
            .when(open, |d| {
                d.children(shown.into_iter().enumerate().map(|(k, a)| {
                    let elapsed = a.done.unwrap_or(now).duration_since(a.started).as_secs().max(0) as u64;
                    let icon = if a.running() {
                        spinner(format!("sub-spin-{tab_id}-{k}").into(), style::muted().into())
                    } else if a.failed {
                        icons::ui("x", px(11.), style::bad()).into_any_element()
                    } else {
                        icons::ui("check", px(11.), style::faint()).into_any_element()
                    };
                    let id = a.id.clone();
                    let tip: SharedString = match &a.doing {
                        Some(doing) if a.running() => format!("{} · {}\nNow: {doing}", a.kind, a.what),
                        _ => format!("{} · {}", a.kind, a.what),
                    }
                    .into();
                    let doing = a.doing.clone().filter(|_| a.running());
                    h_flex()
                        .id(SharedString::from(format!("subagent-{tab_id}-{k}")))
                        .pl(px(44.))
                        .pr_2()
                        .py_0p5()
                        .gap_1p5()
                        .items_center()
                        .rounded_md()
                        .cursor_pointer()
                        .text_xs()
                        .text_color(if a.running() { style::muted() } else { style::faint() })
                        .hover(|d| d.bg(style::panel_hover()))
                        .tooltip(move |window, cx| Tooltip::new(tip.clone()).build(window, cx))
                        .child(icon)
                        .child(
                            v_flex()
                                .flex_1()
                                .min_w_0()
                                .child(div().truncate().child(if a.what.is_empty() { a.kind.clone() } else { a.what.clone() }))
                                .when_some(doing, |d, doing| d.child(div().truncate().text_color(style::faint()).font_family(style::mono_font()).child(doing))),
                        )
                        .child(div().flex_none().text_color(style::faint()).child(style::duration(elapsed)))
                        .on_click(cx.listener(move |this, _, window, cx| {
                            this.activate(i, window, cx);
                            if let Some(TabKind::Chat { view, .. }) = this.tabs.get(i).map(|t| &t.kind) {
                                view.update(cx, |v, cx| v.reveal_tool(&id, cx));
                            }
                        }))
                        .into_any_element()
                }))
                .when(more > 0, |d| d.child(div().pl(px(44.)).text_xs().text_color(style::faint()).child(format!("+{more} more"))))
            })
            .into_any_element()
    }

    pub fn toggle_sidebar(&mut self, cx: &mut Context<Self>) {
        self.left_collapsed = !self.left_collapsed;
        self.left_peek = false;
        let collapsed = self.left_collapsed;
        state::update(cx, |s| s.left_collapsed = collapsed);
        if collapsed {
            self.new_session_menu = false;
        }
        cx.notify();
    }

    /// Folded sidebar: expand, new session, and one icon per open session.
    fn sidebar_rail(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let now = Instant::now();
        v_flex()
            .id("sidebar-rail")
            // Al dejar el cursor encima, la barra completa se abre flotando (como en la app de Claude).
            .on_hover(cx.listener(|this, hovered: &bool, _, cx| {
                this.rail_hovered = *hovered;
                if *hovered && !this.left_peek {
                    cx.spawn(async move |this, cx| {
                        cx.background_executor().timer(Duration::from_millis(150)).await;
                        let _ = this.update(cx, |this, cx| {
                            if this.rail_hovered && this.left_collapsed && !this.left_peek {
                                this.left_peek = true;
                                cx.notify();
                            }
                        });
                    })
                    .detach();
                }
            }))
            .w(px(44.))
            .h_full()
            .flex_none()
            .py_2()
            .gap_1()
            .items_center()
            .border_r_1()
            .border_color(style::border())
            .bg(style::panel())
            .child(
                div()
                    .id("rail-new-session")
                    .p_1()
                    .rounded_md()
                    .cursor_pointer()
                    .hover(|d| d.bg(style::panel_hover()))
                    .tooltip(|window, cx| Tooltip::new("New session").build(window, cx))
                    .child(icons::ui("plus", px(15.), style::accent()))
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.left_collapsed = false;
                        this.new_session_menu = true;
                        cx.notify();
                    })),
            )
            .child(div().w(px(20.)).h(px(1.)).my_1().bg(style::border()))
            .children(self.tabs.iter().enumerate().filter(|(_, t)| is_session(&t.kind)).map(|(i, tab)| {
                let (harness, dot, title): (Harness, Hsla, String) = match &tab.kind {
                    TabKind::Terminal { view, harness, custom_name, .. } => {
                        let t = view.read(cx);
                        let (_, dot) = terminal_status(t, now);
                        (*harness, dot, custom_name.clone().filter(|s| !s.is_empty()).unwrap_or_else(|| t.auto_title().to_string()))
                    }
                    TabKind::Chat { view, harness } => {
                        let v = view.read(cx);
                        let (_, dot) = chat_status(v);
                        (*harness, dot, v.title.clone().or_else(|| v.first_prompt().map(|p| crate::lines::short(p, 40))).unwrap_or_default())
                    }
                    _ => return div().into_any_element(),
                };
                let selected = self.shown_session == Some(tab.id);
                let tip: SharedString = title.into();
                div()
                    .id(("rail-session", tab.id))
                    .relative()
                    .p_1p5()
                    .rounded_md()
                    .cursor_pointer()
                    .when(selected, |d| d.bg(style::panel_hover()))
                    .hover(|d| d.bg(style::panel_hover()))
                    .tooltip(move |window, cx| Tooltip::new(tip.clone()).build(window, cx))
                    .child(harness_badge(harness))
                    .child(div().absolute().top(px(3.)).right(px(3.)).size(px(7.)).rounded_full().border_1().border_color(style::panel()).bg(dot))
                    .on_click(cx.listener(move |this, _, window, cx| this.activate(i, window, cx)))
                    .into_any_element()
            }))
    }

    fn left_sidebar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let now = Instant::now();

        v_flex()
            .size_full()
            .border_r_1()
            .border_color(style::border())
            .bg(style::panel())
            .child(
                v_flex()
                    .px_3()
                    .pt_3()
                    .gap_2()
                    .child(
                        h_flex()
                            .pl_2()
                            .items_center()
                            .child(div().flex_1().text_xs().text_color(style::faint()).child("SESSIONS"))
                            .child(
                                div()
                                    .id("new-session-btn")
                                    .p_1()
                                    .rounded_md()
                                    .cursor_pointer()
                                    .when(self.new_session_menu, |d| d.bg(style::panel_hover()))
                                    .hover(|d| d.bg(style::panel_hover()))
                                    .tooltip(|window, cx| Tooltip::new("New session").build(window, cx))
                                    .relative()
                                    .child(icons::ui("plus", px(14.), style::muted()))
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.new_session_line = None;
                                        this.new_session_menu = !this.new_session_menu && !this.menu_just_closed();
                                        cx.notify();
                                    }))
                                    // Lista de agentes flotando debajo del +.
                                    .when(self.new_session_menu, |d| {
                                        d.child(deferred(div().absolute().top_full().right_0().mt_1().child(self.agent_menu(None, cx))).with_priority(1))
                                    }),
                            ),
                    )
            )
            .child(
                v_flex()
                    .flex_1()
                    .min_h_0()
                    .px_3()
                    .pt_1()
                    .pb_3()
                    .gap_1()
                    .when(!self.tabs.iter().any(|t| matches!(t.kind, TabKind::Terminal { .. } | TabKind::Chat { .. })), |d| {
                        d.child(div().px_2().text_xs().text_color(style::faint()).child("No open sessions"))
                    })
                    .child(
                        div().id("open-sessions").flex_1().overflow_y_scroll().child(
                            v_flex().gap_1().children(self.grouped_sessions(now, cx)),
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
                                let title = p.custom_name.clone().filter(|s| !s.is_empty()).unwrap_or_else(|| h.label().to_string());
                                let detail = p.worktree.clone().unwrap_or_else(|| {
                                    if p.from_history {
                                        let when = p.updated.map(|t| format!(" · {}", ago(t))).unwrap_or_default();
                                        format!("{} history{when}", h.label())
                                    } else if h.resume_args(None).is_empty() {
                                        "reopen".into()
                                    } else {
                                        "resume conversation".into()
                                    }
                                });
                                Some(
                                    h_flex()
                                        .id(("previous", i))
                                        .gap_2()
                                        .px_2()
                                        .py_1()
                                        .rounded_md()
                                        .cursor_pointer()
                                        .when_some(p.custom_color, |d, c| d.border_l_2().border_color(style::lane(c)))
                                        .hover(|d| d.bg(style::panel_hover()))
                                        .child(harness_badge(h))
                                        .child(
                                            v_flex()
                                                .flex_1()
                                                .min_w_0()
                                                .child(div().text_sm().truncate().child(title))
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

    /// "Following Claude Code" mientras el modo seguir te tiene fuera del chat.
    fn follow_bar(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let chat = self.following.clone().filter(|c| c.read(cx).follow)?;
        let chat_tab = self.tabs.iter().position(|t| matches!(&t.kind, TabKind::Chat { view, .. } if *view == chat))?;
        if self.shown_session == Some(self.tabs[chat_tab].id) {
            return None;
        }
        let label = chat.read(cx).launch.label;
        let working = chat.read(cx).status == ChatStatus::Running;
        let (back, stop) = (chat.clone(), chat.clone());
        Some(
            h_flex()
                .px_3()
                .py_1()
                .gap_2()
                .items_center()
                .text_xs()
                .bg(style::accent_alpha(0.1))
                .border_b_1()
                .border_color(style::accent_alpha(0.3))
                .child(icons::ui("locate-fixed", px(12.), style::accent()))
                .child(div().text_color(style::accent()).child(format!("Following {label}{}", if working { " · working" } else { "" })))
                .child(div().flex_1())
                .child(
                    div()
                        .id("follow-back")
                        .cursor_pointer()
                        .text_color(style::text())
                        .hover(|d| d.text_color(style::accent()))
                        .child("Back to chat")
                        .on_click(cx.listener(move |this, _, window, cx| {
                            back.update(cx, |c, cx| c.set_follow(false, cx));
                            this.following = None;
                            if let Some(i) = this.tabs.iter().position(|t| matches!(&t.kind, TabKind::Chat { view, .. } if *view == back)) {
                                this.activate(i, window, cx);
                            }
                        })),
                )
                .child(
                    div()
                        .id("follow-stop")
                        .cursor_pointer()
                        .text_color(style::muted())
                        .hover(|d| d.text_color(style::text()))
                        .child("Stop following")
                        .on_click(cx.listener(move |this, _, _, cx| {
                            stop.update(cx, |c, cx| c.set_follow(false, cx));
                            this.following = None;
                            cx.notify();
                        })),
                )
                .into_any_element(),
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
            // Sessions (terminals and chats) live in the left sidebar, not here.
            .children(self.tabs.iter().enumerate().filter(|(_, t)| !is_session(&t.kind)).map(|(i, tab)| {
                let active = self.shown_doc == Some(tab.id);
                let tab_id = tab.id;
                let (icon, status, title, dirty, accent_color, can_rename): (
                    AnyElement,
                    Option<Hsla>,
                    SharedString,
                    bool,
                    Option<u32>,
                    bool,
                ) = match &tab.kind {
                    TabKind::Terminal { view, harness, custom_name, custom_color, .. } => {
                        let t = view.read(cx);
                        let (_, dot) = terminal_status(t, now);
                        let title = custom_name
                            .as_ref()
                            .filter(|s| !s.is_empty())
                            .map(|s| SharedString::from(s.clone()))
                            .unwrap_or_else(|| t.auto_title());
                        (harness_badge(*harness), Some(dot), title, false, *custom_color, true)
                    }
                    TabKind::File(f) => {
                        let f = f.read(cx);
                        (icons::ui("file", px(14.), style::muted()).into_any_element(), None, f.title().into(), f.dirty, None, false)
                    }
                    TabKind::Diff(d) => (
                        icons::ui("git-compare", px(14.), style::accent()).into_any_element(),
                        None,
                        d.read(cx).title.clone(),
                        false,
                        None,
                        false,
                    ),
                    TabKind::Conflicts(_) => (
                        icons::ui("git-merge-conflict", px(14.), style::bad()).into_any_element(),
                        None,
                        "Merge Conflicts".into(),
                        false,
                        Some(0xf87171),
                        false,
                    ),
                    TabKind::Search(_) => {
                        (icons::ui("search", px(14.), style::accent()).into_any_element(), None, "Search".into(), false, None, false)
                    }
                    TabKind::Review(r) => (
                        icons::ui("list-checks", px(14.), style::accent()).into_any_element(),
                        None,
                        r.read(cx).title.clone(),
                        false,
                        None,
                        false,
                    ),
                    TabKind::Pr(v) => (
                        icons::ui("git-pull-request", px(14.), style::accent()).into_any_element(),
                        None,
                        format!("PR #{}", v.read(cx).number).into(),
                        false,
                        None,
                        false,
                    ),
                    TabKind::Chat { view, harness } => {
                        let v = view.read(cx);
                        let (_, dot) = chat_status(v);
                        let title = v
                            .title
                            .clone()
                            .or_else(|| v.first_prompt().map(str::to_string))
                            .map(|t| crate::lines::short(&t, 28))
                            .unwrap_or_else(|| format!("{} · chat", harness.label()));
                        (harness_badge(*harness), Some(dot), title.into(), false, None, false)
                    }
                };
                h_flex()
                    .id(("tab", tab_id))
                    .h_full()
                    .px_3()
                    .gap_2()
                    .max_w(px(240.))
                    .text_sm()
                    .cursor_pointer()
                    .border_r_1()
                    .border_color(style::border())
                    .when_some(accent_color, |d, c| d.border_t_2().border_color(style::lane(c)))
                    .text_color(if active { style::text() } else { style::muted() })
                    .when(active, |d| d.bg(style::bg()))
                    .hover(|d| d.text_color(style::text()))
                    .child(icon)
                    .when_some(status, |d, c| d.child(div().size(px(6.)).flex_none().rounded_full().bg(c)))
                    .child(div().truncate().child(title))
                    .when(dirty, |d| d.child(div().text_color(style::accent()).child("●")))
                    .child(
                        div()
                            .id(("close", tab_id))
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
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, ev: &MouseDownEvent, window, cx| {
                            if can_rename && ev.click_count >= 2 {
                                this.open_rename_modal(tab_id, window, cx);
                            } else {
                                this.activate(i, window, cx);
                            }
                        }),
                    )
                    .on_mouse_down(
                        MouseButton::Right,
                        cx.listener(move |this, _, window, cx| {
                            if can_rename {
                                this.open_rename_modal(tab_id, window, cx);
                            }
                        }),
                    )
            }))
            .child(div().flex_1())
            .when(self.shown_session.is_some(), |d| {
                d.child(
                    div()
                        .id("close-doc-pane")
                        .flex_none()
                        .mx_2()
                        .p_1()
                        .rounded_md()
                        .cursor_pointer()
                        .hover(|d| d.bg(style::panel_hover()))
                        .tooltip(|window, cx| Tooltip::new("Hide this side (tabs stay open)").build(window, cx))
                        .child(icons::ui("panel-right-close", px(14.), style::muted()))
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.doc_hidden = true;
                            if let Some(i) = this.shown_session.and_then(|id| this.tabs.iter().position(|t| t.id == id)) {
                                this.active = Some(i);
                            }
                            cx.notify();
                        })),
                )
            })
    }

    fn tab_view(kind: &TabKind) -> AnyElement {
        match kind {
            TabKind::Terminal { view, .. } => view.clone().into_any_element(),
            TabKind::File(v) => v.clone().into_any_element(),
            TabKind::Diff(v) => v.clone().into_any_element(),
            TabKind::Conflicts(v) => v.clone().into_any_element(),
            TabKind::Chat { view, .. } => view.clone().into_any_element(),
            TabKind::Search(v) => v.clone().into_any_element(),
            TabKind::Review(v) => v.clone().into_any_element(),
            TabKind::Pr(v) => v.clone().into_any_element(),
        }
    }

    /// Keeps track of what each side of the center shows (called from render).
    fn update_shown(&mut self) {
        if let Some(tab) = self.active.and_then(|i| self.tabs.get(i)) {
            if is_session(&tab.kind) {
                self.shown_session = Some(tab.id);
            } else {
                // Hiding the side makes the session active, so a document being
                // active again means you opened it: show the side (even the same tab).
                self.doc_hidden = false;
                self.shown_doc = Some(tab.id);
            }
        }
        if self.shown_session.is_some_and(|id| !self.tabs.iter().any(|t| t.id == id)) {
            self.shown_session = self.tabs.iter().rev().find(|t| is_session(&t.kind)).map(|t| t.id);
        }
        if self.shown_doc.is_some_and(|id| !self.tabs.iter().any(|t| t.id == id)) {
            self.shown_doc = self.tabs.iter().rev().find(|t| !is_session(&t.kind)).map(|t| t.id);
        }
    }

    fn center(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let session = self.shown_session.and_then(|id| self.tabs.iter().position(|t| t.id == id));
        let doc = self.shown_doc.filter(|_| !self.doc_hidden).and_then(|id| self.tabs.iter().position(|t| t.id == id));
        let doc_pane = doc.map(|i| {
            v_flex()
                .size_full()
                .min_w_0()
                .child(self.tab_bar(cx))
                .child(tab_in(&self.tabs[i], div().flex_1().min_h_0().child(Self::tab_view(&self.tabs[i].kind))))
        });
        let session_pane = session.map(|i| {
            div()
                .id("session-pane")
                .size_full()
                .min_w_0()
                // Clicking in the session makes it the active tab (⌘W, shortcuts).
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |this, _, _, cx| {
                        if this.active != Some(i) {
                            this.active = Some(i);
                            cx.notify();
                        }
                    }),
                )
                .child(tab_in(&self.tabs[i], div().size_full().child(Self::tab_view(&self.tabs[i].kind))))
        });
        let content = match (session_pane, doc_pane) {
            (Some(s), Some(d)) => h_resizable("center-split")
                .with_state(&self.center_split)
                .child(resizable_panel().size_range(px(320.)..Pixels::MAX).child(s))
                .child(resizable_panel().size_range(px(280.)..Pixels::MAX).child(d))
                .into_any_element(),
            (Some(s), None) => s.into_any_element(),
            (None, Some(d)) => d.into_any_element(),
            (None, None) => v_flex()
                .size_full()
                .when(self.tabs.iter().any(|t| !is_session(&t.kind)), |d| d.child(self.tab_bar(cx)))
                .child(div().flex_1().min_h_0().child(self.empty_state(cx)))
                .into_any_element(),
        };
        v_flex()
            .size_full()
            .min_w_0()
            .when_some(self.follow_bar(cx), |d, bar| d.child(bar))
            .when(self.merge_approval_ready, |d| {
                d.child(
                    h_flex()
                        .px_3()
                        .py_2()
                        .gap_2()
                        .items_center()
                        .border_b_1()
                        .border_color(style::soft(style::good(), 0.5))
                        .bg(style::soft(style::good(), 0.1))
                        .text_xs()
                        .child(icons::ui("git-merge", px(14.), style::good()))
                        .child(
                            div()
                                .flex_1()
                                .text_color(style::text())
                                .child("Agent finished resolving conflicts. Review the diff and approve to finish merging."),
                        )
                        .child(
                            div()
                                .id("banner-complete-merge")
                                .px_2p5()
                                .py_1()
                                .rounded_md()
                                .font_weight(FontWeight::MEDIUM)
                                .cursor_pointer()
                                .bg(style::good())
                                .text_color(style::bg())
                                .child("Complete merge")
                                .on_click(cx.listener(|this, _, window, cx| this.complete_active_merge(window, cx))),
                        )
                        .child(
                            div()
                                .id("banner-dismiss")
                                .px_2()
                                .py_1()
                                .rounded_md()
                                .cursor_pointer()
                                .text_color(style::muted())
                                .hover(|d| d.text_color(style::text()))
                                .child("Dismiss")
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.merge_approval_ready = false;
                                    cx.notify();
                                })),
                        ),
                )
            })
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
            .child(div().text_2xl().font_weight(FontWeight::SEMIBOLD).when_some(style::display_font(), |d, f| d.font_family(f).text_3xl()).child(name))
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

    /// Right-panel tabs not hidden in Settings.
    fn visible_panels(&self, cx: &App) -> Vec<SidePanel> {
        let hidden = &state::get(cx).settings.hidden_panels;
        SidePanel::ALL.into_iter().filter(|p| !hidden.iter().any(|h| h == p.key())).collect()
    }

    fn pr_merge_method(&self, cx: &App) -> PrMergeMethod {
        match state::get(cx).settings.pr_merge_method.as_str() {
            "squash" => PrMergeMethod::Squash,
            "rebase" => PrMergeMethod::Rebase,
            _ => PrMergeMethod::Merge,
        }
    }

    fn right_panel(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let counts: HashMap<SidePanel, usize> = HashMap::from([
            (SidePanel::Changes, self.git.changes.len()),
            (SidePanel::Branches, self.git.branches.len()),
            (SidePanel::Prs, self.git.prs.as_ref().and_then(|p| p.as_ref().ok()).map_or(0, |p| p.len())),
            (SidePanel::Actions, self.actions.read(cx).running_count()),
        ]);

        let top_toolbar = h_flex()
            .px_2()
            .py_1p5()
            .gap_1()
            .border_b_1()
            .border_color(style::border())
            .children(self.visible_panels(cx).into_iter().map(|p| {
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

        let panel_content = self.panel.map(|p| {
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
                    SidePanel::Actions => self.actions.clone().into_any_element(),
                    SidePanel::Graph => self.graph_panel(cx).into_any_element(),
                }))
        });
        // The metro mini map always sits under the active section.
        let mini_map = self.mini_map(cx);
        let top_content = Some(
            v_flex()
                .size_full()
                .child(div().flex_1().min_h_0().children(panel_content))
                .child(mini_map),
        );

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
                    style::warn()
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
        let conflicted: Vec<String> = self
            .git
            .changes
            .iter()
            .filter(|c| c.kind == kuaky_git::ChangeKind::Conflicted)
            .map(|c| c.path.clone())
            .collect();
        let has_conflicts = !conflicted.is_empty() || self.git.is_merging;
        div()
            .id("changes")
            .size_full()
            .overflow_y_scroll()
            .child(
                v_flex()
                    .p_2()
                    .gap_0p5()
                    .when(has_conflicts, |d| {
                        let files = conflicted.clone();
                        d.child(
                            v_flex()
                                .mb_2()
                                .p_2()
                                .gap_1p5()
                                .rounded_md()
                                .border_1()
                                .border_color(style::soft(style::bad(), 0.5))
                                .bg(style::soft(style::bad(), 0.08))
                                .child(
                                    h_flex()
                                        .gap_1p5()
                                        .items_center()
                                        .text_xs()
                                        .text_color(style::bad())
                                        .child(icons::ui("git-merge-conflict", px(13.), style::bad()))
                                        .child(if files.is_empty() {
                                            "Merge in progress".to_string()
                                        } else {
                                            format!("{} conflicted file(s)", files.len())
                                        }),
                                )
                                .child(
                                    h_flex()
                                        .gap_1()
                                        .flex_wrap()
                                        .child(
                                            div()
                                                .id("open-conflict-resolver")
                                                .px_2()
                                                .py_0p5()
                                                .rounded_sm()
                                                .text_xs()
                                                .cursor_pointer()
                                                .bg(style::accent())
                                                .text_color(style::on_accent())
                                                .child("Resolve conflicts")
                                                .on_click(cx.listener(move |this, _, window, cx| {
                                                    this.open_conflicts(files.clone(), window, cx);
                                                })),
                                        )
                                        .child(
                                            div()
                                                .id("changes-complete-merge")
                                                .px_2()
                                                .py_0p5()
                                                .rounded_sm()
                                                .text_xs()
                                                .cursor_pointer()
                                                .border_1()
                                                .border_color(style::soft(style::good(), 0.5))
                                                .text_color(style::good())
                                                .child("Complete merge")
                                                .on_click(cx.listener(|this, _, window, cx| this.complete_active_merge(window, cx))),
                                        )
                                        .child(
                                            div()
                                                .id("changes-abort-merge")
                                                .px_2()
                                                .py_0p5()
                                                .rounded_sm()
                                                .text_xs()
                                                .cursor_pointer()
                                                .text_color(style::muted())
                                                .hover(|d| d.text_color(style::bad()))
                                                .child("Abort")
                                                .on_click(cx.listener(|this, _, window, cx| this.abort_active_merge(window, cx))),
                                        ),
                                ),
                        )
                    })
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
                                let merge_branch = b.name.clone();
                                let wt_path = self
                                    .git
                                    .worktrees
                                    .iter()
                                    .find(|w| !w.main && w.branch.as_deref() == Some(b.name.as_str()))
                                    .map(|w| w.path.clone());
                                d.child(
                                    div()
                                        .id(("branch-merge", i))
                                        .flex_none()
                                        .p_1()
                                        .rounded_sm()
                                        .cursor_pointer()
                                        .hover(|d| d.bg(style::bg()))
                                        .tooltip(|window, cx| Tooltip::new("Merge, squash, or discard branch").build(window, cx))
                                        .child(icons::ui("git-merge", px(12.), style::muted()))
                                        .on_click(cx.listener(move |this, _, _, cx| {
                                            cx.stop_propagation();
                                            this.open_merge_modal(merge_branch.clone(), wt_path.clone(), cx);
                                        })),
                                )
                                .child(
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
                .children(prs.iter().map(|pr| self.pr_row(pr, cx)))
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
                    .when_some(self.git.pr_action_error.clone(), |d, e| {
                        d.child(div().px_2().py_1().text_xs().text_color(style::bad()).child(e))
                    })
                    .child(body),
            )
            .into_any_element()
    }

    fn graph_panel(&self, cx: &mut Context<Self>) -> impl IntoElement {
        if self.repo.is_none() {
            return panel_message("This folder is not a git repository.").into_any_element();
        }
        v_flex()
            .size_full()
            .child(
                h_flex()
                    .px_2()
                    .py_1()
                    .justify_between()
                    .items_center()
                    .border_b_1()
                    .border_color(style::border())
                    .child(div().text_xs().text_color(style::faint()).child("click a commit to view diff"))
                    .child(
                        div()
                            .id("graph-refresh")
                            .p_1()
                            .rounded_sm()
                            .cursor_pointer()
                            .hover(|d| d.bg(style::panel_hover()))
                            .tooltip(|window, cx| Tooltip::new("Refresh graph").build(window, cx))
                            .child(icons::ui("refresh-cw", px(12.), style::muted()))
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.graph.update(cx, |g, cx| g.refresh(cx));
                            })),
                    ),
            )
            .child(div().flex_1().min_h_0().child(self.graph.clone()))
            .into_any_element()
    }

    fn merge_overlay(&self, modal: &MergeModal, cx: &mut Context<Self>) -> impl IntoElement {
        let busy = modal.busy;
        let has_wt = modal.worktree.is_some();
        div()
            .id("merge-backdrop")
            .absolute()
            .inset_0()
            .flex()
            .items_center()
            .justify_center()
            .bg(style::soft(style::bg(), 0.55))
            .on_click(cx.listener(|this, _, _, cx| {
                if !this.merge_modal.as_ref().is_some_and(|m| m.busy) {
                    this.merge_modal = None;
                    cx.notify();
                }
            }))
            .child(
                v_flex()
                    .id("merge-box")
                    .w(px(440.))
                    .p_4()
                    .gap_3()
                    .rounded_xl()
                    .border_1()
                    .border_color(style::border())
                    .bg(style::panel())
                    .shadow_lg()
                    .on_click(|_, _, cx| cx.stop_propagation())
                    .child(
                        h_flex()
                            .justify_between()
                            .items_center()
                            .child(
                                h_flex()
                                    .gap_2()
                                    .items_center()
                                    .child(icons::ui("git-merge", px(16.), style::accent()))
                                    .child(div().text_sm().font_weight(FontWeight::SEMIBOLD).child("Merge or Discard")),
                            )
                            .child(
                                div()
                                    .id("merge-close")
                                    .p_0p5()
                                    .rounded_sm()
                                    .cursor_pointer()
                                    .hover(|d| d.bg(style::panel_hover()))
                                    .child(icons::ui("x", px(13.), style::muted()))
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.merge_modal = None;
                                        cx.notify();
                                    })),
                            ),
                    )
                    .child(
                        h_flex()
                            .gap_1p5()
                            .text_xs()
                            .text_color(style::muted())
                            .child(div().text_color(style::text()).font_weight(FontWeight::MEDIUM).child(modal.branch.clone()))
                            .child("→")
                            .child(div().text_color(style::accent()).child(modal.base.clone())),
                    )
                    .child(
                        div().p_3().rounded_lg().bg(style::bg()).border_1().border_color(style::border()).child(match &modal.preview {
                            None => div().text_xs().text_color(style::muted()).child("Checking merge preview (git merge-tree)…").into_any_element(),
                            Some(Ok(MergePreview::Clean)) => h_flex()
                                .gap_2()
                                .items_center()
                                .text_xs()
                                .text_color(style::good())
                                .child(icons::ui("check", px(14.), style::good()))
                                .child("Clean ✓ — no merge conflicts detected.")
                                .into_any_element(),
                            Some(Ok(MergePreview::Conflicts(files))) => v_flex()
                                .gap_1p5()
                                .child(
                                    h_flex()
                                        .gap_1p5()
                                        .items_center()
                                        .text_xs()
                                        .text_color(style::bad())
                                        .child(icons::ui("git-merge-conflict", px(14.), style::bad()))
                                        .child(format!("Conflicts in {} file(s):", files.len())),
                                )
                                .children(files.iter().take(10).map(|f| {
                                    div()
                                        .pl_4()
                                        .text_xs()
                                        .font_family(style::mono_font())
                                        .text_color(style::muted())
                                        .truncate()
                                        .child(f.clone())
                                }))
                                .into_any_element(),
                            Some(Err(err)) => div().text_xs().text_color(style::bad()).child(err.clone()).into_any_element(),
                        }),
                    )
                    .when_some(modal.error.clone(), |d, e| {
                        d.child(div().text_xs().text_color(style::bad()).child(e))
                    })
                    .child(
                        h_flex()
                            .mt_1()
                            .gap_2()
                            .items_center()
                            .child(
                                div()
                                    .id("modal-discard")
                                    .px_2p5()
                                    .py_1()
                                    .rounded_md()
                                    .text_xs()
                                    .cursor_pointer()
                                    .border_1()
                                    .border_color(style::soft(style::bad(), 0.45))
                                    .text_color(style::bad())
                                    .hover(|d| d.bg(style::soft(style::bad(), 0.12)))
                                    .tooltip(move |window, cx| {
                                        Tooltip::new(if has_wt {
                                            "Delete worktree and force-delete branch"
                                        } else {
                                            "Force-delete branch"
                                        })
                                        .build(window, cx)
                                    })
                                    .child("Discard")
                                    .on_click(cx.listener(|this, _, _, cx| this.run_discard(cx))),
                            )
                            .child(div().flex_1())
                            .child(
                                div()
                                    .id("modal-squash")
                                    .px_2p5()
                                    .py_1()
                                    .rounded_md()
                                    .text_xs()
                                    .cursor_pointer()
                                    .border_1()
                                    .border_color(style::border())
                                    .text_color(style::text())
                                    .hover(|d| d.bg(style::panel_hover()))
                                    .child(if busy { "Working…" } else { "Squash" })
                                    .on_click(cx.listener(|this, _, window, cx| this.run_merge(MergeMethod::Squash, window, cx))),
                            )
                            .child(
                                div()
                                    .id("modal-merge")
                                    .px_3()
                                    .py_1()
                                    .rounded_md()
                                    .text_xs()
                                    .font_weight(FontWeight::MEDIUM)
                                    .cursor_pointer()
                                    .bg(style::accent())
                                    .text_color(style::on_accent())
                                    .child(if busy { "Working…" } else { "Merge" })
                                    .on_click(cx.listener(|this, _, window, cx| this.run_merge(MergeMethod::Merge, window, cx))),
                            ),
                    ),
            )
    }

    fn rename_overlay(&self, modal: &RenameSession, cx: &mut Context<Self>) -> impl IntoElement {
        let selected_color = modal.color;
        div()
            .id("rename-backdrop")
            .absolute()
            .inset_0()
            .flex()
            .items_center()
            .justify_center()
            .bg(style::soft(style::bg(), 0.55))
            .on_click(cx.listener(|this, _, _, cx| {
                this.rename_modal = None;
                cx.notify();
            }))
            .child(
                v_flex()
                    .id("rename-box")
                    .w(px(360.))
                    .p_4()
                    .gap_3()
                    .rounded_xl()
                    .border_1()
                    .border_color(style::border())
                    .bg(style::panel())
                    .shadow_lg()
                    .on_click(|_, _, cx| cx.stop_propagation())
                    .capture_key_down(cx.listener(|this, ev: &KeyDownEvent, _, cx| {
                        if ev.keystroke.key == "escape" {
                            cx.stop_propagation();
                            this.rename_modal = None;
                            cx.notify();
                        }
                    }))
                    .child(
                        h_flex()
                            .justify_between()
                            .items_center()
                            .child(div().text_sm().font_weight(FontWeight::SEMIBOLD).child("Rename session"))
                            .child(
                                div()
                                    .id("rename-close")
                                    .p_0p5()
                                    .rounded_sm()
                                    .cursor_pointer()
                                    .hover(|d| d.bg(style::panel_hover()))
                                    .child(icons::ui("x", px(13.), style::muted()))
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.rename_modal = None;
                                        cx.notify();
                                    })),
                            ),
                    )
                    .child(
                        v_flex()
                            .gap_1()
                            .child(div().text_xs().text_color(style::muted()).child("Name"))
                            .child(Input::new(&modal.input)),
                    )
                    .child(
                        v_flex()
                            .gap_1p5()
                            .child(div().text_xs().text_color(style::muted()).child("Color"))
                            .child(
                                h_flex()
                                    .gap_2()
                                    .items_center()
                                    .child(
                                        div()
                                            .id("color-none")
                                            .px_2()
                                            .py_0p5()
                                            .rounded_md()
                                            .text_xs()
                                            .cursor_pointer()
                                            .border_1()
                                            .border_color(if selected_color.is_none() {
                                                style::accent()
                                            } else {
                                                style::border()
                                            })
                                            .text_color(if selected_color.is_none() { style::text() } else { style::muted() })
                                            .child("Auto")
                                            .on_click(cx.listener(|this, _, _, cx| {
                                                if let Some(m) = &mut this.rename_modal {
                                                    m.color = None;
                                                    cx.notify();
                                                }
                                            })),
                                    )
                                    .children(SESSION_COLORS.iter().enumerate().map(|(ci, &(name, hex))| {
                                        let active = selected_color == Some(hex);
                                        let tip: SharedString = name.into();
                                        div()
                                            .id(("swatch", ci))
                                            .size(px(20.))
                                            .rounded_full()
                                            .cursor_pointer()
                                            .bg(style::lane(hex))
                                            .when(active, |d| d.border_2().border_color(style::text()))
                                            .tooltip(move |window, cx| Tooltip::new(tip.clone()).build(window, cx))
                                            .on_click(cx.listener(move |this, _, _, cx| {
                                                if let Some(m) = &mut this.rename_modal {
                                                    m.color = Some(hex);
                                                    cx.notify();
                                                }
                                            }))
                                    })),
                            ),
                    )
                    .child(
                        h_flex()
                            .justify_end()
                            .gap_2()
                            .mt_1()
                            .child(
                                div()
                                    .id("rename-cancel")
                                    .px_3()
                                    .py_1()
                                    .rounded_md()
                                    .text_xs()
                                    .cursor_pointer()
                                    .border_1()
                                    .border_color(style::border())
                                    .text_color(style::muted())
                                    .hover(|d| d.bg(style::panel_hover()).text_color(style::text()))
                                    .child("Cancel")
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.rename_modal = None;
                                        cx.notify();
                                    })),
                            )
                            .child(
                                div()
                                    .id("rename-save")
                                    .px_3()
                                    .py_1()
                                    .rounded_md()
                                    .text_xs()
                                    .font_weight(FontWeight::MEDIUM)
                                    .cursor_pointer()
                                    .bg(style::accent())
                                    .text_color(style::on_accent())
                                    .child("Save")
                                    .on_click(cx.listener(|this, _, _, cx| this.confirm_rename(cx))),
                            ),
                    ),
            )
    }
}

impl Render for ProjectView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.update_shown();
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
            .on_action(cx.listener(|this, _: &ToggleSidebar, _, cx| this.toggle_sidebar(cx)))
            .size_full()
            .relative()
            .bg(style::bg())
            .child(
                div().flex_1().min_h_0().child({
                    let saved = state::get(cx);
                    let (left, right) = (saved.left_width.unwrap_or(248.).max(220.), saved.right_width.unwrap_or(300.));
                    if self.left_collapsed {
                        h_flex()
                            .size_full()
                            .relative()
                            .child(self.sidebar_rail(cx))
                            .child(
                                div().flex_1().min_w_0().h_full().child(
                                    h_resizable("project-layout-folded")
                                        .with_state(&self.layout_collapsed)
                                        .child(resizable_panel().child(self.center(cx)))
                                        .child(resizable_panel().size(px(right)).size_range(px(220.)..px(560.)).child(self.right_panel(cx))),
                                ),
                            )
                            // Barra completa flotando encima mientras el cursor está en ella.
                            .when(self.left_peek, |d| {
                                d.child(
                                    div()
                                        .id("sidebar-peek")
                                        .absolute()
                                        .top_0()
                                        .bottom_0()
                                        .left_0()
                                        .w(px(left))
                                        .occlude()
                                        .shadow_lg()
                                        .on_hover(cx.listener(|this, hovered: &bool, _, cx| {
                                            // Con un menú suyo abierto no se pliega (el menú sale fuera de la barra).
                                            if !*hovered && !this.new_session_menu && this.new_session_line.is_none() {
                                                this.left_peek = false;
                                                this.rail_hovered = false;
                                                cx.notify();
                                            }
                                        }))
                                        .child(self.left_sidebar(cx)),
                                )
                            })
                            .into_any_element()
                    } else {
                        h_resizable("project-layout")
                            .with_state(&self.layout)
                            .child(resizable_panel().size(px(left)).size_range(px(220.)..px(420.)).child(self.left_sidebar(cx)))
                            .child(resizable_panel().child(self.center(cx)))
                            .child(resizable_panel().size(px(right)).size_range(px(220.)..px(560.)).child(self.right_panel(cx)))
                            .into_any_element()
                    }
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
                        .child(div().id("quick-open-box").on_click(|_, _, cx| cx.stop_propagation()).child(qo))
                        .with_animation("quick-open-in", crate::motion::once(crate::motion::FAST), |d, t| d.opacity(t)),
                )
            })
            .when_some(self.rename_modal.as_ref().map(|m| self.rename_overlay(m, cx)), |d, el| d.child(el))
            .when(self.board.is_some(), |d| d.child(self.board_overlay(cx)))
            .when_some(self.merge_modal.as_ref().map(|m| self.merge_overlay(m, cx)), |d, el| d.child(el))
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

/// Al cambiar de pestaña, su contenido entra con un fundido corto.
fn tab_in(tab: &Tab, content: Div) -> gpui_kit::AnimationElement<Div> {
    crate::motion::fade_in(content, SharedString::from(format!("tab-in-{:?}", tab.id)), crate::motion::FAST)
}

/// Punto de estado de una sesión. Cuando cambia de estado entra con un fundido;
/// si pasa a "te necesita" (rojo), destella dos veces y se queda quieto.
fn status_dot(i: usize, color: Hsla, hover_group: SharedString) -> gpui_kit::AnimationElement<Div> {
    let alarm = color == Hsla::from(style::bad());
    let id = SharedString::from(format!("dot-{i}-{:.3}-{:.3}", color.h, color.l));
    let (duration, ease) = (if alarm { Duration::from_millis(700) } else { crate::motion::NORMAL }, crate::motion::enter());
    // Al pasar el mouse por la fila, el punto deja lugar a renombrar / cerrar.
    let dot = div().size(px(7.)).flex_none().rounded_full().bg(color).group_hover(hover_group, |s| s.invisible());
    dot.with_animation(id, Animation::new(duration), move |d, t| {
        let o = if alarm { 1. - 0.75 * (t * 2. * std::f32::consts::PI).sin().abs() } else { ease(t) };
        d.opacity(o)
    })
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

/// Saved sessions plus the agent's own recent conversations in this folder
/// that kuaky does not already know, so they can be resumed.
fn with_agent_history(mut sessions: Vec<SavedSession>, root: &Path) -> Vec<SavedSession> {
    const MAX_FROM_HISTORY: usize = 6;
    let known: Vec<String> = sessions.iter().filter_map(|s| s.conversation.clone()).collect();
    let extra = crate::agent_history::agy_conversations(root)
        .into_iter()
        .filter(|c| !known.contains(&c.id))
        .take(MAX_FROM_HISTORY)
        .map(|c| SavedSession {
            uid: None,
            harness: Harness::Antigravity.key().to_string(),
            cwd: root.to_string_lossy().into_owned(),
            worktree: None,
            custom_name: Some(c.title),
            custom_color: None,
            conversation: Some(c.id),
            chat: false,
            open: false,
            from_history: true,
            updated: c.updated,
        });
    sessions.extend(extra);
    sessions
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

/// Fold/unfold button for the left sidebar.
pub(crate) fn sidebar_toggle(collapsed: bool) -> Stateful<Div> {
    let tip = if cfg!(target_os = "macos") {
        if collapsed { "Show sidebar (⌘B)" } else { "Hide sidebar (⌘B)" }
    } else if collapsed {
        "Show sidebar (Ctrl+B)"
    } else {
        "Hide sidebar (Ctrl+B)"
    };
    div()
        .id(if collapsed { "sidebar-expand" } else { "sidebar-collapse" })
        .flex_none()
        .p_1()
        .rounded_md()
        .cursor_pointer()
        .hover(|d| d.bg(style::panel_hover()))
        .tooltip(move |window, cx| Tooltip::new(tip).build(window, cx))
        .child(icons::ui(if collapsed { "panel-left-open" } else { "panel-left-close" }, px(15.), style::muted()))
}
