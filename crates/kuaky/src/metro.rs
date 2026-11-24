//! Mapa de metro en la interfaz: lista agrupada a la izquierda, mini mapa a la
//! derecha y board a pantalla completa con la ficha de cada línea y las
//! conexiones entre sesiones (review, tests, segunda opinión, relevo).

use super::*;
use crate::agent_tasks::TaskWatcher;
use crate::lines::{Line, LineKind, Station, StationSource, StationState, short};

const ROW: f32 = 76.;
const TOP: f32 = 28.;
const TRUNK_X: f32 = 18.;
const SPACING: f32 = 112.;
const MAX_ON_BOARD: usize = 12;
const LABEL_W: f32 = 236.;

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum BoardFilter {
    Active,
    NeedsYou,
    Finished,
    All,
    /// Solo las archivadas (en las demás vistas no salen).
    Archived,
}

impl BoardFilter {
    const ALL: [BoardFilter; 5] = [BoardFilter::Active, BoardFilter::NeedsYou, BoardFilter::Finished, BoardFilter::All, BoardFilter::Archived];

    fn label(self) -> &'static str {
        match self {
            BoardFilter::Active => "Active",
            BoardFilter::NeedsYou => "Needs you",
            BoardFilter::Finished => "Finished",
            BoardFilter::All => "All",
            BoardFilter::Archived => "Archived",
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum ConnKind {
    Review,
    Tester,
    SecondOpinion,
    Relay,
}

impl ConnKind {
    const ALL: [ConnKind; 4] = [ConnKind::Review, ConnKind::Tester, ConnKind::SecondOpinion, ConnKind::Relay];

    fn label(self) -> &'static str {
        match self {
            ConnKind::Review => "Review with…",
            ConnKind::Tester => "Test with…",
            ConnKind::SecondOpinion => "Second opinion…",
            ConnKind::Relay => "Hand off to…",
        }
    }

    fn role(self) -> &'static str {
        match self {
            ConnKind::Review => "review",
            ConnKind::Tester => "tester",
            ConnKind::SecondOpinion => "second-opinion",
            ConnKind::Relay => "relay",
        }
    }

    fn station(self) -> &'static str {
        match self {
            ConnKind::Review => "Review",
            ConnKind::Tester => "Tests",
            ConnKind::SecondOpinion => "Second opinion",
            ConnKind::Relay => "Hand-off",
        }
    }
}

#[derive(Clone, PartialEq)]
pub(super) enum BoardMenu {
    NewSession,
    Branch,
    Connection(ConnKind),
    MergeInto,
    Parent,
    MoveSession(String),
}

pub(super) struct Board {
    filter: BoardFilter,
    selected: Option<String>,
    menu: Option<BoardMenu>,
    name_input: Option<(String, Entity<InputState>)>,
    focus: FocusHandle,
    _sub: Option<Subscription>,
}

/// Sesión viva sobre una línea.
#[derive(Clone)]
pub(super) struct Train {
    harness: Harness,
    pub(super) tab: usize,
    working: bool,
    needs_you: bool,
}

fn line_color(c: u32) -> Rgba {
    rgb(c)
}

fn amber() -> Rgba {
    rgb(0xfbbf24)
}

/// Un título de terminal que vale como resumen de la tarea (no el nombre del
/// programa, la shell o una ruta).
fn meaningful_title(title: &str, harness: Harness) -> Option<String> {
    let t = title.trim();
    let lower = t.to_lowercase();
    let generic = ["zsh", "bash", "fish", "sh", "login", "claude", "claude code", "codex", "agy", "antigravity", "gemini", "opencode"];
    if t.chars().count() < 3
        || generic.contains(&lower.as_str())
        || harness.command().is_some_and(|c| lower == c)
        || lower == harness.label().to_lowercase()
        || t.starts_with('/')
        || t.starts_with('~')
    {
        return None;
    }
    Some(t.to_string())
}

/// Dos chats conectados (revisor ↔ autor) con rondas automáticas opcionales.
pub(super) struct Relay {
    author: String,
    reviewer: String,
    author_label: String,
    reviewer_label: String,
    verb: &'static str,
    auto: bool,
    rounds_left: u8,
}

const RELAY_ROUNDS: u8 = 3;

/// El revisor dice que ya no hay nada que corregir.
fn reviewer_is_done(answer: &str) -> bool {
    let a = answer.to_lowercase();
    a.trim_start().starts_with("lgtm") || a.contains("\nlgtm") || a.contains("no further issues") || a.contains("nothing left to fix")
}

/// Agentes vivos de un proyecto (para las pestañas de proyectos e Inicio).
#[derive(Clone, Default)]
pub struct LiveSummary {
    pub working: usize,
    pub needs_you: usize,
    /// `uid` → (trabajando, te necesita).
    pub sessions: HashMap<String, (bool, bool)>,
}

impl ProjectView {
    pub fn live_summary(&self, cx: &App) -> LiveSummary {
        let trains = self.trains(cx);
        LiveSummary {
            working: trains.values().filter(|t| t.working && !t.needs_you).count(),
            needs_you: trains.values().filter(|t| t.needs_you).count(),
            sessions: trains.into_iter().map(|(uid, t)| (uid, (t.working, t.needs_you))).collect(),
        }
    }

    /// Abre el board con una línea elegida (desde Inicio).
    pub fn show_line(&mut self, line: String, window: &mut Window, cx: &mut Context<Self>) {
        self.open_board(Some(line), window, cx);
    }

    /// Sesiones vivas por `uid`.
    pub(super) fn trains(&self, cx: &App) -> HashMap<String, Train> {
        let mut out = HashMap::new();
        for (i, tab) in self.tabs.iter().enumerate() {
            if let TabKind::Chat { view, harness } = &tab.kind {
                let v = view.read(cx);
                if !matches!(v.status, crate::chat_view::ChatStatus::Exited) {
                    out.insert(
                        v.uid.clone(),
                        Train {
                            harness: *harness,
                            tab: i,
                            working: matches!(v.status, crate::chat_view::ChatStatus::Running),
                            needs_you: v.is_waiting_permission(),
                        },
                    );
                }
                continue;
            }
            if let TabKind::Terminal { view, harness, .. } = &tab.kind {
                if *harness == Harness::Shell {
                    continue;
                }
                let v = view.read(cx);
                if v.exited {
                    continue;
                }
                out.insert(
                    v.uid.clone(),
                    Train {
                        harness: *harness,
                        tab: i,
                        working: matches!(v.agent_state, Some(AgentState::Working))
                            || (v.agent_state.is_none() && v.last_output.is_some_and(|t| t.elapsed() < Duration::from_secs(3))),
                        needs_you: matches!(v.agent_state, Some(AgentState::NeedsYou(_))),
                    },
                );
            }
        }
        out
    }

    fn line_trains<'a>(&self, line: &Line, trains: &'a HashMap<String, Train>) -> Vec<(&'a String, &'a Train)> {
        let mut v: Vec<_> = trains.iter().filter(|(uid, _)| line.sessions.iter().any(|s| &s.uid == *uid)).collect();
        v.sort_by_key(|(_, t)| t.tab);
        v
    }

    /// Título y sesión: alimentan la estación en curso y la lista de tareas.
    pub(super) fn on_session_title(&mut self, view: &Entity<TerminalView>, harness: Harness, cx: &mut Context<Self>) {
        let v = view.read(cx);
        let uid = v.uid.clone();
        let title = v.title.as_ref().and_then(|t| meaningful_title(t, harness));
        let session = v.agent_session.clone();
        if let Some(title) = title {
            self.lines.on_title(&uid, &title);
        }
        if let Some(id) = session {
            self.lines.set_conversation(&uid, &id);
            if harness == Harness::Claude && !self.task_watchers.contains_key(&uid) {
                self.task_watchers.insert(uid, TaskWatcher::for_claude_session(&id));
            }
        }
    }

    /// Tareas cada 3 s; hitos de git cada 30 s.
    pub(super) fn start_map_refresh(&self, cx: &mut Context<Self>) {
        cx.spawn(async move |this, cx| {
            let mut tick = 0u32;
            loop {
                cx.background_executor().timer(Duration::from_secs(3)).await;
                let Ok(git_job) = this.update(cx, |this, cx| {
                    let mut changed = false;
                    for (uid, watcher) in this.task_watchers.iter_mut() {
                        if let Some(list) = watcher.poll() {
                            this.lines.set_tasks(uid, &list);
                            changed = true;
                        }
                        watcher.poll_steps();
                        // Subagentes: nuevos/terminados, y los que corren (su tiempo avanza).
                        if watcher.take_subagents_changed() || watcher.subagents().iter().any(|a| a.running()) {
                            changed = true;
                        }
                    }
                    if changed {
                        cx.notify();
                    }
                    if tick % 10 != 0 {
                        return None;
                    }
                    let repo = this.repo.clone()?;
                    let base = this.git.default_branch.clone()?;
                    let prs: Vec<(String, u64, String)> = this
                        .git
                        .prs
                        .as_ref()
                        .and_then(|p| p.as_ref().ok())
                        .map(|p| p.iter().map(|pr| (pr.head_ref_name.clone(), pr.number, pr.title.clone())).collect())
                        .unwrap_or_default();
                    let lines: Vec<(String, String)> = this
                        .lines
                        .lines
                        .iter()
                        .filter(|l| !l.merged && !l.archived)
                        .filter_map(|l| Some((l.id.clone(), l.branch.clone()?)))
                        .collect();
                    (!lines.is_empty()).then_some((repo, base, prs, lines))
                }) else {
                    break;
                };
                tick = tick.wrapping_add(1);
                if let Some((repo, base, prs, lines)) = git_job {
                    let results = cx
                        .background_executor()
                        .spawn(async move {
                            lines
                                .into_iter()
                                .filter_map(|(id, branch)| {
                                    let progress = repo.branch_progress(&base, &branch)?;
                                    let pr = prs.iter().find(|(h, _, _)| *h == branch).map(|(_, n, t)| (*n, t.clone()));
                                    Some((id, progress, pr))
                                })
                                .collect::<Vec<_>>()
                        })
                        .await;
                    let _ = this.update(cx, |this, cx| {
                        for (id, progress, pr) in results {
                            this.lines.set_git(&id, progress.ahead, progress.merged, pr);
                        }
                        cx.notify();
                    });
                }
            }
        })
        .detach();
    }

    // ── Acciones ────────────────────────────────────────────────────────

    pub(super) fn open_board(&mut self, line: Option<String>, window: &mut Window, cx: &mut Context<Self>) {
        let focus = cx.focus_handle();
        window.focus(&focus, cx);
        self.board = Some(Board {
            filter: BoardFilter::Active,
            selected: None,
            menu: None,
            name_input: None,
            focus,
            _sub: None,
        });
        if let Some(id) = line {
            // Una línea archivada se abre en la vista de archivadas.
            if self.lines.get(&id).is_some_and(|l| l.archived) {
                if let Some(b) = &mut self.board {
                    b.filter = BoardFilter::Archived;
                }
            }
            self.select_line(id, window, cx);
        }
        cx.notify();
    }

    /// Se cerró una sesión: si era la última abierta de su línea, la línea se archiva.
    pub(super) fn archive_if_last(&mut self, uid: &str, cx: &mut Context<Self>) {
        let Some(line) = self.lines.line_of(uid) else { return };
        let id = line.id.clone();
        let live = self.trains(cx);
        if !line.sessions.iter().any(|s| s.uid != uid && live.contains_key(&s.uid)) {
            self.lines.set_archived(&id, true);
        }
    }

    fn close_board(&mut self, cx: &mut Context<Self>) {
        self.board = None;
        cx.notify();
    }

    /// Clic en una línea: la elige; clic otra vez en la elegida: la suelta.
    fn toggle_line(&mut self, id: String, window: &mut Window, cx: &mut Context<Self>) {
        if self.board.as_ref().is_some_and(|b| b.selected.as_deref() == Some(id.as_str())) {
            if let Some(b) = &mut self.board {
                b.selected = None;
                b.menu = None;
                b.name_input = None;
                b._sub = None;
            }
            cx.notify();
            return;
        }
        self.select_line(id, window, cx);
    }

    fn select_line(&mut self, id: String, window: &mut Window, cx: &mut Context<Self>) {
        let Some(name) = self.lines.get(&id).map(|l| l.name.clone()) else { return };
        let input = cx.new(|cx| {
            let mut s = InputState::new(window, cx).placeholder("Line name");
            s.set_value(name, window, cx);
            s
        });
        let line_id = id.clone();
        let sub = cx.subscribe_in(&input, window, move |this, input, ev: &InputEvent, _, cx| {
            if matches!(ev, InputEvent::PressEnter { .. } | InputEvent::Blur) {
                let value = input.read(cx).value().trim().to_string();
                let current = this.lines.get(&line_id).map(|l| l.name.clone()).unwrap_or_default();
                if !value.is_empty() && value != current {
                    this.lines.rename(&line_id, &value);
                    cx.notify();
                }
            }
        });
        if let Some(board) = &mut self.board {
            board.selected = Some(id.clone());
            board.menu = None;
            board.name_input = Some((id, input));
            board._sub = Some(sub);
        }
        cx.notify();
    }

    fn toggle_menu(&mut self, menu: BoardMenu, cx: &mut Context<Self>) {
        if let Some(board) = &mut self.board {
            board.menu = if board.menu.as_ref() == Some(&menu) { None } else { Some(menu) };
        }
        cx.notify();
    }

    /// Lleva a la terminal de la sesión; si está cerrada, la retoma.
    pub(super) fn focus_session(&mut self, uid: &str, window: &mut Window, cx: &mut Context<Self>) {
        let live = self.tabs.iter().position(|t| match &t.kind {
            TabKind::Terminal { view, .. } => view.read(cx).uid == uid && !view.read(cx).exited,
            TabKind::Chat { view, .. } => view.read(cx).uid == uid,
            _ => false,
        });
        if let Some(i) = live {
            self.board = None;
            self.activate(i, window, cx);
            return;
        }
        let Some(line) = self.lines.line_of(uid) else { return };
        let Some(s) = line.sessions.iter().find(|s| s.uid == uid).cloned() else { return };
        let Some(harness) = Harness::from_key(&s.harness) else { return };
        let cwd = PathBuf::from(&s.cwd);
        let cwd = if cwd.is_dir() { cwd } else { self.root.clone() };
        self.previous.retain(|p| p.uid.as_deref() != Some(uid));
        self.board = None;
        if s.chat {
            let opts = ChatOpts { resume: s.conversation.clone(), uid: Some(uid.to_string()), line: Some(line.id.clone()), cwd: Some(cwd), ..Default::default() };
            self.open_chat_with(harness, opts, window, cx);
            return;
        }
        let opts = SpawnOpts {
            resume: s.conversation.clone().or(Some(String::new())),
            uid: Some(uid.to_string()),
            line: Some(line.id.clone()),
            ..Default::default()
        };
        self.spawn_terminal(harness, cwd, None, opts, window, cx);
    }

    /// Carpeta de trabajo de la línea: la de su sesión más reciente que exista.
    fn line_cwd(&self, line: &Line) -> PathBuf {
        line.sessions
            .iter()
            .filter(|s| s.role.is_none())
            .max_by_key(|s| s.last_active)
            .map(|s| PathBuf::from(&s.cwd))
            .filter(|p| p.is_dir())
            .unwrap_or_else(|| self.root.clone())
    }

    pub(super) fn new_session_on_line(&mut self, line_id: String, harness: Harness, window: &mut Window, cx: &mut Context<Self>) {
        let Some(line) = self.lines.get(&line_id) else { return };
        let cwd = self.line_cwd(line);
        // Una terminal simple no recibe la nota de la línea.
        let paste = (!line.stations.is_empty() && harness != Harness::Shell).then(|| line.relay_note());
        self.board = None;
        self.new_session_menu = false;
        self.new_session_line = None;
        if self.opens_as_chat(harness, cx) {
            let opts = ChatOpts { line: Some(line_id), draft: paste, cwd: Some(cwd), ..Default::default() };
            self.open_chat_with(harness, opts, window, cx);
            return;
        }
        let opts = SpawnOpts { line: Some(line_id), paste, ..Default::default() };
        self.spawn_terminal(harness, cwd, None, opts, window, cx);
    }

    /// Sesión que se bifurca de una línea: crea una sub-línea debajo de ella (mismo
    /// tema, otra rama de trabajo), en la carpeta de la línea y con su contexto.
    pub(super) fn branch_off_line(&mut self, parent_id: String, harness: Harness, window: &mut Window, cx: &mut Context<Self>) {
        let Some(parent) = self.lines.get(&parent_id).cloned() else { return };
        let cwd = self.line_cwd(&parent);
        let kind = parent.kind;
        // Nombre provisional: el primer título del agente lo reemplaza.
        let sub = self.lines.create(&format!("Sub-task · {}", crate::lines::short(&parent.name, 40)), kind, Some(parent_id.clone()), false);
        let paste = (harness != Harness::Shell).then(|| {
            format!("{}\nThis session is a sub-task that branches off that goal. The sub-task is: ", parent.relay_note())
        });
        self.board = None;
        self.new_session_menu = false;
        self.new_session_line = None;
        if self.opens_as_chat(harness, cx) {
            let opts = ChatOpts { line: Some(sub), draft: paste, cwd: Some(cwd), ..Default::default() };
            self.open_chat_with(harness, opts, window, cx);
            return;
        }
        let opts = SpawnOpts { line: Some(sub), paste, ..Default::default() };
        self.spawn_terminal(harness, cwd, None, opts, window, cx);
    }

    fn start_connection(&mut self, line_id: String, kind: ConnKind, harness: Harness, window: &mut Window, cx: &mut Context<Self>) {
        let Some(line) = self.lines.get(&line_id).cloned() else { return };
        let cwd = self.line_cwd(&line);
        let n = line.sessions.iter().filter(|s| s.role.as_deref() == Some(kind.role())).count() + 1;
        let findings = self.lines.files_dir(&line_id).map(|d| d.join(format!("{}-{n}.md", kind.role())));
        if let Some(dir) = findings.as_ref().and_then(|f| f.parent()) {
            let _ = std::fs::create_dir_all(dir);
        }
        let findings_path = findings.as_ref().map(|p| p.to_string_lossy().into_owned()).unwrap_or_default();
        let base = self.git.default_branch.clone().unwrap_or_else(|| "main".into());
        let diff_hint = match &line.branch {
            Some(branch) => format!("`git diff {base}...{branch}`"),
            None => format!("`git diff {base}...HEAD` and `git diff` (uncommitted changes)"),
        };
        let context = line.relay_note();
        let prompt = match kind {
            ConnKind::Review => format!(
                "{context} Review the changes for this goal using {diff_hint} in this folder. Do not modify project files. \
                 Write your findings (file:line, severity, what to change) to {findings_path} and give a short summary here."
            ),
            ConnKind::Tester => format!(
                "{context} Write and run tests that cover the changes for this goal ({diff_hint}). Only add or edit test files. \
                 Report what you tested and any failures in {findings_path}."
            ),
            ConnKind::SecondOpinion => format!(
                "{context} Without editing project files, propose your own approach for this goal and point out risks in the current one. \
                 Write the proposal to {findings_path}."
            ),
            ConnKind::Relay => format!("{context} Continue the work from where it was left."),
        };
        let station = format!("{} · {}", kind.station(), harness.label());
        self.lines.add_station(&line_id, &station, StationSource::Connection, None, StationState::Active);
        self.board = None;
        // Si la sesión autora es un chat, el revisor también: así sus respuestas
        // pueden ir y venir solas (rondas automáticas).
        let trains = self.trains(cx);
        let author = line
            .sessions
            .iter()
            .filter(|s| s.chat && (s.role.is_none() || s.role.as_deref() == Some("relay")))
            .filter(|s| trains.contains_key(&s.uid))
            .max_by_key(|s| s.last_active)
            .and_then(|s| Some((s.uid.clone(), self.chat_view_of(&s.uid, cx)?)));
        let can_chat = self.chat_agents.contains_key(&harness) || harness.chat_command().is_some();
        if let (Some((author_uid, author_view)), true, false) = (author, can_chat, kind == ConnKind::Relay) {
            let draft = format!("{prompt} When nothing is left to fix, reply LGTM.");
            let opts = ChatOpts { line: Some(line_id), draft: Some(draft), role: Some(kind.role().to_string()), cwd: Some(cwd), ..Default::default() };
            self.open_chat_with(harness, opts, window, cx);
            let reviewer = self.tabs.last().and_then(|t| match &t.kind {
                TabKind::Chat { view, .. } => Some(view.read(cx).uid.clone()),
                _ => None,
            });
            if let Some(reviewer) = reviewer {
                self.relays.retain(|r| r.reviewer != reviewer);
                self.relays.push(Relay {
                    author: author_uid,
                    reviewer,
                    author_label: author_view.read(cx).launch.label.to_string(),
                    reviewer_label: harness.label().to_string(),
                    verb: kind.station(),
                    auto: false,
                    rounds_left: RELAY_ROUNDS,
                });
                self.sync_relay_banners(cx);
            }
            return;
        }
        let opts = SpawnOpts { line: Some(line_id), paste: Some(prompt), role: Some(kind.role().to_string()), ..Default::default() };
        self.spawn_terminal(harness, cwd, None, opts, window, cx);
    }

    /// Pasa una sesión a otra línea (arrastrándola en la barra izquierda).
    fn move_session_to(&mut self, uid: &str, target: &str, cx: &mut Context<Self>) {
        if self.lines.line_of(uid).is_some_and(|l| l.id == target) {
            return;
        }
        if let Some(session) = self.lines.line_of(uid).and_then(|l| l.sessions.iter().find(|s| s.uid == uid)).cloned() {
            self.lines.attach(target, session);
            let live: Vec<String> = self.trains(cx).into_keys().collect();
            self.lines.prune_empty(&live);
            self.lines.save();
        }
        cx.notify();
    }

    fn chat_view_of(&self, uid: &str, cx: &App) -> Option<Entity<crate::chat_view::ChatView>> {
        self.tabs.iter().find_map(|t| match &t.kind {
            TabKind::Chat { view, .. } if view.read(cx).uid == uid => Some(view.clone()),
            _ => None,
        })
    }

    /// Muestra en ambos chats con quién están conectados.
    fn sync_relay_banners(&self, cx: &mut Context<Self>) {
        for r in &self.relays {
            let pairs = [
                (&r.reviewer, format!("{} for {}", r.verb, r.author_label)),
                (&r.author, format!("{} by {}", r.verb, r.reviewer_label)),
            ];
            for (uid, text) in pairs {
                if let Some(view) = self.chat_view_of(uid, cx) {
                    let banner = crate::chat_view::RelayBanner { text, auto: r.auto, rounds_left: r.rounds_left };
                    view.update(cx, |v, cx| {
                        v.relay = Some(banner);
                        cx.notify();
                    });
                }
            }
        }
    }

    pub(super) fn set_relay_auto(&mut self, uid: &str, on: bool, cx: &mut Context<Self>) {
        for r in self.relays.iter_mut().filter(|r| r.author == uid || r.reviewer == uid) {
            r.auto = on;
            if on && r.rounds_left == 0 {
                r.rounds_left = RELAY_ROUNDS;
            }
        }
        self.sync_relay_banners(cx);
    }

    /// Terminó un turno de un chat conectado: con rondas automáticas, su respuesta
    /// pasa al otro lado (el revisor manda hallazgos; el autor, lo que cambió).
    pub(super) fn relay_turn_done(&mut self, uid: &str, cx: &mut Context<Self>) {
        let Some(i) = self.relays.iter().position(|r| r.auto && r.rounds_left > 0 && (r.author == uid || r.reviewer == uid)) else { return };
        let from_reviewer = self.relays[i].reviewer == uid;
        let (from, to) = if from_reviewer {
            (self.relays[i].reviewer.clone(), self.relays[i].author.clone())
        } else {
            (self.relays[i].author.clone(), self.relays[i].reviewer.clone())
        };
        let (Some(from_view), Some(to_view)) = (self.chat_view_of(&from, cx), self.chat_view_of(&to, cx)) else {
            self.relays[i].auto = false;
            self.sync_relay_banners(cx);
            return;
        };
        let Some(answer) = from_view.read(cx).last_answer() else { return };
        let r = &mut self.relays[i];
        if from_reviewer && reviewer_is_done(&answer) {
            r.auto = false;
            let note = format!("Auto rounds finished: {} has nothing left to fix.", r.reviewer_label);
            for v in [&from_view, &to_view] {
                v.update(cx, |v, cx| v.push_notice(note.clone(), cx));
            }
            self.sync_relay_banners(cx);
            return;
        }
        let message = if from_reviewer {
            format!("Feedback from {} (reviewer):\n\n{answer}\n\nPlease address it, then summarize what you changed.", r.reviewer_label)
        } else {
            r.rounds_left = r.rounds_left.saturating_sub(1);
            format!("{} (author) replied:\n\n{answer}\n\nCheck the updated changes. If nothing else needs fixing, reply LGTM.", r.author_label)
        };
        let finished = r.rounds_left == 0;
        if finished {
            r.auto = false;
        }
        to_view.update(cx, |v, cx| v.submit_text(message, cx));
        if finished {
            let note = format!("Auto rounds stopped after {RELAY_ROUNDS} rounds. Turn them on again to continue.");
            to_view.update(cx, |v, cx| v.push_notice(note, cx));
        }
        self.sync_relay_banners(cx);
    }

    /// Deja escrito en la sesión autora que atienda los hallazgos.
    fn send_findings(&mut self, line_id: &str, path: PathBuf, cx: &mut Context<Self>) {
        let Some(line) = self.lines.get(line_id) else { return };
        let trains = self.trains(cx);
        let author = line
            .sessions
            .iter()
            .filter(|s| s.role.is_none() || s.role.as_deref() == Some("relay"))
            .filter(|s| trains.contains_key(&s.uid))
            .max_by_key(|s| s.last_active)
            .map(|s| s.uid.clone());
        let Some(uid) = author else { return };
        let Some(tab) = trains.get(&uid).map(|t| t.tab) else { return };
        if let Some(Tab { kind: TabKind::Terminal { view, .. }, .. }) = self.tabs.get(tab) {
            let text = format!("Please address the findings in {}", path.to_string_lossy());
            view.update(cx, |v, cx| {
                v.paste_text(&text);
                cx.notify();
            });
        }
        self.board = None;
        self.active = Some(tab);
        cx.notify();
    }

    fn findings_for(&self, line_id: &str) -> Vec<PathBuf> {
        let Some(dir) = self.lines.files_dir(line_id) else { return Vec::new() };
        let mut files: Vec<PathBuf> = std::fs::read_dir(dir)
            .map(|rd| rd.filter_map(Result::ok).map(|e| e.path()).filter(|p| p.extension().is_some_and(|e| e == "md")).collect())
            .unwrap_or_default();
        files.sort();
        files
    }

    fn line_state(&self, line: &Line, trains: &HashMap<String, Train>) -> (&'static str, Hsla) {
        let mine = self.line_trains(line, trains);
        if line.merged {
            ("merged", style::muted().into())
        } else if mine.iter().any(|(_, t)| t.needs_you) {
            ("needs you", amber().into())
        } else if mine.iter().any(|(_, t)| t.working) {
            ("working", style::accent().into())
        } else if !mine.is_empty() {
            ("idle", style::muted().into())
        } else if line.archived {
            ("archived", style::faint().into())
        } else {
            ("paused", style::faint().into())
        }
    }

    // ── Barra izquierda: sesiones agrupadas por línea ──────────────────

    pub(super) fn grouped_sessions(&self, now: Instant, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let trains = self.trains(cx);
        let mut used: Vec<usize> = Vec::new();
        let mut out: Vec<AnyElement> = Vec::new();
        for (line, depth) in self.lines.ordered(true) {
            let tabs: Vec<usize> = self.line_trains(line, &trains).into_iter().map(|(_, t)| t.tab).collect();
            if tabs.is_empty() {
                continue;
            }
            let (state, state_color) = self.line_state(line, &trains);
            let total = line.stations.len();
            let done = line.done_count();
            let id = line.id.clone();
            let _ = state_color;
            out.push(
                h_flex()
                    .id(SharedString::from(format!("line-head-{}", line.id)))
                    .mt_2()
                    .ml(px(depth as f32 * 10.))
                    .px_2()
                    .gap_1()
                    .items_center()
                    .cursor_pointer()
                    .rounded_sm()
                    .text_xs()
                    .text_color(style::faint())
                    .hover(|d| d.text_color(style::muted()))
                    .group(SharedString::from(format!("line-g-{}", line.id)))
                    .relative()
                    // Sub-línea: se ve como rama del tema de arriba.
                    .when(depth > 0, |d| d.child(icons::ui("corner-down-right", px(11.), style::faint())))
                    .child(div().min_w_0().truncate().child(line.name.clone()))
                    .when(total > 0, |d| d.child(div().flex_none().child(format!("· {done}/{total}"))))
                    .child(div().flex_1())
                    // + al pasar el mouse: otra sesión en esta línea (solo eliges el agente).
                    .child({
                        let open = self.new_session_line.as_deref() == Some(line.id.as_str());
                        let target = line.id.clone();
                        div()
                            .id(SharedString::from(format!("line-add-{}", line.id)))
                            .flex_none()
                            .p_0p5()
                            .rounded_sm()
                            .cursor_pointer()
                            .when(!open, |d| d.invisible().group_hover(SharedString::from(format!("line-g-{}", line.id)), |s| s.visible()))
                            .when(open, |d| d.bg(style::panel_hover()))
                            .hover(|d| d.bg(style::panel_hover()))
                            .tooltip(|window, cx| Tooltip::new("New session in this line").build(window, cx))
                            .child(icons::ui("plus", px(12.), style::muted()))
                            .on_click(cx.listener(move |this, _, _, cx| {
                                cx.stop_propagation();
                                this.new_session_menu = false;
                                this.new_session_branch = false;
                                this.new_session_line = if open || this.menu_just_closed() { None } else { Some(target.clone()) };
                                cx.notify();
                            }))
                    })
                    .when(self.new_session_line.as_deref() == Some(line.id.as_str()), |d| {
                        d.child(deferred(div().absolute().top_full().right_0().mt_1().child(self.agent_menu(Some(line.id.clone()), cx))).with_priority(1))
                    })
                    .tooltip(move |window, cx| Tooltip::new(format!("{state} · open on the map · + adds a session · drop a session here to move it")).build(window, cx))
                    .drag_over::<super::DraggedSession>(|s, _, _, _| s.bg(style::accent_alpha(0.18)))
                    .on_drop(cx.listener({
                        let target = line.id.clone();
                        move |this, d: &super::DraggedSession, _, cx| this.move_session_to(&d.uid, &target, cx)
                    }))
                    .on_click(cx.listener(move |this, _, window, cx| this.open_board(Some(id.clone()), window, cx)))
                    .into_any_element(),
            );
            for i in tabs {
                if let Some(row) = self.tabs.get(i).and_then(|tab| self.session_row(i, tab, now, cx)) {
                    out.push(div().ml(px(depth as f32 * 10.)).child(row).into_any_element());
                    used.push(i);
                }
            }
        }
        let rest: Vec<usize> =
            (0..self.tabs.len()).filter(|i| !used.contains(i) && self.tabs.get(*i).is_some_and(|t| super::is_session(&t.kind))).collect();
        if !rest.is_empty() && !used.is_empty() {
            out.push(div().mt_2().px_2().text_xs().text_color(style::faint()).child("Other").into_any_element());
        }
        for i in rest {
            if let Some(row) = self.tabs.get(i).and_then(|tab| self.session_row(i, tab, now, cx)) {
                out.push(row);
            }
        }
        out
    }

    // ── Mini mapa (panel derecho) ───────────────────────────────────────

    pub(super) fn mini_map(&self, cx: &mut Context<Self>) -> AnyElement {
        let trains = self.trains(cx);
        let lines: Vec<(&Line, usize)> = self.lines.ordered(false).into_iter().filter(|(l, _)| !l.archived).collect();
        let collapsed = self.mini_map_collapsed;
        let shown = 5;
        v_flex()
            .flex_none()
            .border_t_1()
            .border_color(style::border())
            .child(
                h_flex()
                    .px_3()
                    .py_1()
                    .gap_1p5()
                    .items_center()
                    .child(
                        h_flex()
                            .id("mini-map-toggle")
                            .flex_1()
                            .gap_1()
                            .items_center()
                            .cursor_pointer()
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.mini_map_collapsed = !this.mini_map_collapsed;
                                cx.notify();
                            }))
                            .child(icons::ui(if collapsed { "chevron-right" } else { "chevron-down" }, px(12.), style::muted()))
                            .child(icons::ui("route", px(13.), style::muted()))
                            .child(div().text_xs().text_color(style::muted()).child(format!("MAP · {}", lines.len()))),
                    )
                    .child(
                        div()
                            .id("open-board")
                            .p_0p5()
                            .rounded_sm()
                            .cursor_pointer()
                            .hover(|d| d.bg(style::panel_hover()))
                            .tooltip(|window, cx| Tooltip::new("Open board").build(window, cx))
                            .child(icons::ui("maximize-2", px(13.), style::muted()))
                            .on_click(cx.listener(|this, _, window, cx| this.open_board(None, window, cx))),
                    ),
            )
            .when(!collapsed, |d| {
                d.child(
                    v_flex()
                        .px_2()
                        .pb_2()
                        .gap_0p5()
                        .when(lines.is_empty(), |d| {
                            d.child(div().px_1().text_xs().text_color(style::faint()).child("Start an agent session and its goal shows up here."))
                        })
                        .children(lines.iter().take(shown).map(|(line, depth)| {
                            let mine = self.line_trains(line, &trains);
                            let needs = mine.iter().any(|(_, t)| t.needs_you);
                            let id = line.id.clone();
                            let recent: Vec<&Station> = line.stations.iter().rev().take(9).collect::<Vec<_>>().into_iter().rev().collect();
                            let color = line_color(line.color);
                            let current = line.current().map(|s| s.title.clone()).unwrap_or_default();
                            v_flex()
                                .id(SharedString::from(format!("mini-{}", line.id)))
                                .ml(px(*depth as f32 * 8.))
                                .px_1()
                                .py_0p5()
                                .rounded_md()
                                .cursor_pointer()
                                .hover(|d| d.bg(style::panel_hover()))
                                .on_click(cx.listener(move |this, _, window, cx| this.open_board(Some(id.clone()), window, cx)))
                                .child(
                                    h_flex()
                                        .gap_1()
                                        .items_center()
                                        .child(div().flex_1().min_w_0().truncate().text_xs().child(line.name.clone()))
                                        .children(mine.iter().take(3).filter_map(|(_, t)| t.harness.agent()).map(|a| icons::logo(icons::agent(a), px(12.))))
                                        .when(needs, |d| d.child(div().size(px(7.)).rounded_full().bg(amber()))),
                                )
                                .child(
                                    h_flex()
                                        .h(px(12.))
                                        .items_center()
                                        .relative()
                                        .child(div().absolute().left_0().right_0().top(px(5.)).h(px(2.)).bg(style::soft(color, 0.55)))
                                        .child(h_flex().w_full().map(|d| if recent.len() >= 6 { d.justify_between() } else { d.gap(px(22.)) }).children(recent.into_iter().map(|s| {
                                            let (fill, border, size): (Hsla, Hsla, f32) = match s.state {
                                                StationState::Done => (color.into(), color.into(), 7.),
                                                StationState::Active => {
                                                    (style::bg().into(), if needs { amber().into() } else { color.into() }, 10.)
                                                }
                                                StationState::Pending => (style::panel().into(), style::soft(color, 0.8), 7.),
                                            };
                                            div().size(px(size)).rounded_full().bg(fill).border_2().border_color(border)
                                        }))),
                                )
                                .when(!current.is_empty(), |d| {
                                    d.child(div().text_size(px(10.)).text_color(style::faint()).truncate().child(current))
                                })
                        }))
                        .when(lines.len() > shown, |d| {
                            d.child(div().px_1().text_xs().text_color(style::faint()).child(format!("+{} more on the board", lines.len() - shown)))
                        }),
                )
            })
            .into_any_element()
    }

    // ── Board a pantalla completa ──────────────────────────────────────

    pub(super) fn board_overlay(&self, cx: &mut Context<Self>) -> AnyElement {
        let Some(board) = &self.board else { return div().into_any_element() };
        let trains = self.trains(cx);
        let filter = board.filter;
        let rows: Vec<(&Line, usize)> = self
            .lines
            .ordered(true)
            .into_iter()
            .filter(|(l, _)| match filter {
                BoardFilter::Archived => l.archived,
                _ if l.archived => false,
                BoardFilter::Active => !l.is_finished(),
                BoardFilter::NeedsYou => self.line_trains(l, &trains).iter().any(|(_, t)| t.needs_you),
                BoardFilter::Finished => l.is_finished(),
                BoardFilter::All => true,
            })
            .collect();
        let selected = board.selected.clone();
        let longest = rows.iter().map(|(l, _)| l.stations.len().min(MAX_ON_BOARD)).max().unwrap_or(0);
        let track_w = TRUNK_X + 80. + longest as f32 * SPACING + 160.;
        let height = TOP + rows.len().max(1) as f32 * ROW + 40.;

        let header = h_flex()
            .px_4()
            .py_2()
            .gap_2()
            .items_center()
            .border_b_1()
            .border_color(style::border())
            .child(icons::ui("route", px(16.), style::accent()))
            .child(div().text_sm().font_weight(FontWeight::MEDIUM).child("Project map"))
            .child(div().text_xs().text_color(style::faint()).child(self.root.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()))
            .child(div().flex_1())
            .children(BoardFilter::ALL.into_iter().map(|f| {
                let on = f == filter;
                div()
                    .id(SharedString::from(format!("filter-{}", f.label())))
                    .px_2()
                    .py_0p5()
                    .rounded_md()
                    .text_xs()
                    .cursor_pointer()
                    .when(on, |d| d.bg(style::panel_hover()).text_color(style::text()))
                    .when(!on, |d| d.text_color(style::muted()))
                    .hover(|d| d.bg(style::panel_hover()))
                    .child(f.label())
                    .on_click(cx.listener(move |this, _, _, cx| {
                        if let Some(b) = &mut this.board {
                            b.filter = f;
                        }
                        cx.notify();
                    }))
            }))
            .child(
                div()
                    .id("board-close")
                    .ml_2()
                    .p_1()
                    .rounded_md()
                    .cursor_pointer()
                    .hover(|d| d.bg(style::panel_hover()))
                    .tooltip(|window, cx| Tooltip::new("Close (Esc)").build(window, cx))
                    .child(icons::ui("x", px(14.), style::muted()))
                    .on_click(cx.listener(|this, _, _, cx| this.close_board(cx))),
            );

        // Posiciones de cada fila (para dibujar sub-líneas desde su padre).
        let geometry: Vec<RowGeom> = rows
            .iter()
            .enumerate()
            .map(|(r, (line, depth))| {
                let y = TOP + r as f32 * ROW + ROW * 0.5;
                let parent_row = line.parent.as_ref().and_then(|p| rows.iter().position(|(l, _)| &l.id == p));
                let start = TRUNK_X + 56. + *depth as f32 * (SPACING * 0.5 + 30.);
                let shown: Vec<Station> = line.stations.iter().rev().take(MAX_ON_BOARD).cloned().collect::<Vec<_>>().into_iter().rev().collect();
                let hidden = line.stations.len().saturating_sub(shown.len());
                let needs = self.line_trains(line, &trains).iter().any(|(_, t)| t.needs_you);
                RowGeom {
                    y,
                    start,
                    parent_y: parent_row.map(|pr| TOP + pr as f32 * ROW + ROW * 0.5),
                    color: line.color,
                    stations: shown,
                    hidden,
                    merged: line.merged,
                    finished: line.is_finished(),
                    needs,
                    depth: *depth,
                    selected: selected.as_deref() == Some(line.id.as_str()),
                    dim: selected.as_deref().is_some_and(|s| s != line.id),
                }
            })
            .collect();
        let paint_geom = geometry.clone();

        let labels = v_flex().w(px(LABEL_W)).flex_none().pt(px(TOP)).children(rows.iter().map(|(line, depth)| {
            let id = line.id.clone();
            let is_sel = selected.as_deref() == Some(line.id.as_str());
            let dim = selected.is_some() && !is_sel;
            let (state, state_color) = self.line_state(line, &trains);
            let c = line_color(line.color);
            v_flex()
                .id(SharedString::from(format!("board-label-{}", line.id)))
                .h(px(ROW - 8.))
                .my(px(4.))
                .ml_1()
                .justify_center()
                .pl(px(12. + *depth as f32 * 14.))
                .pr_2()
                .cursor_pointer()
                .rounded_l(px(10.))
                .when(is_sel, |d| d.bg(style::panel_hover()))
                .when(dim, |d| d.opacity(0.55))
                .hover(|d| d.bg(style::panel_hover()).opacity(1.))
                .on_click(cx.listener(move |this, _, window, cx| this.toggle_line(id.clone(), window, cx)))
                .child(
                    h_flex()
                        .gap_1p5()
                        .items_center()
                        .child(div().size(px(9.)).flex_none().rounded_full().bg(c))
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .truncate()
                                .text_sm()
                                .when(is_sel, |d| d.font_weight(FontWeight::SEMIBOLD))
                                .child(line.name.clone()),
                        ),
                )
                .child(
                    h_flex()
                        .gap_1p5()
                        .text_xs()
                        .text_color(style::faint())
                        .child(line.kind.label())
                        .child("·")
                        .child(format!("{}/{}", line.done_count(), line.stations.len()))
                        .child("·")
                        .child(div().text_color(state_color).child(state)),
                )
        }));

        let mut overlay: Vec<AnyElement> = Vec::new();
        for ((line, _), g) in rows.iter().zip(&geometry) {
            let mine = self.line_trains(line, &trains);
            for (k, st) in g.stations.iter().enumerate() {
                let x = g.start + k as f32 * SPACING;
                let tip = if st.detail.is_empty() || st.detail.starts_with("git:") {
                    st.title.clone()
                } else {
                    short(&st.detail, 240)
                };
                let when = ago(st.at);
                let source = match st.source {
                    StationSource::Prompt => "your prompt",
                    StationSource::Title => "agent",
                    StationSource::Todo => "agent task",
                    StationSource::Git => "git",
                    StationSource::Manual => "manual",
                    StationSource::Connection => "connection",
                };
                let tip: SharedString = format!("{tip}\n{source} · {when}").into();
                let id = line.id.clone();
                overlay.push(
                    div()
                        .id(SharedString::from(format!("st-{}-{}", line.id, st.id)))
                        .absolute()
                        .left(px(x - 12.))
                        .top(px(g.y - 12.))
                        .size(px(24.))
                        .rounded_full()
                        .cursor_pointer()
                        .tooltip(move |window, cx| Tooltip::new(tip.clone()).build(window, cx))
                        .on_click(cx.listener(move |this, _, window, cx| this.toggle_line(id.clone(), window, cx)))
                        .into_any_element(),
                );
                overlay.push(
                    div()
                        .absolute()
                        .left(px(x - SPACING * 0.5 + 4.))
                        .top(px(g.y + 13.))
                        .w(px(SPACING - 8.))
                        .text_size(px(10.))
                        .text_center()
                        .truncate()
                        .text_color(if st.state == StationState::Active || g.selected { style::text() } else { style::faint() })
                        .when(g.dim, |d| d.opacity(0.5))
                        .child(st.title.clone())
                        .into_any_element(),
                );
            }
            // Trenes: logo del agente sobre la estación en curso (o al final).
            let active_x = g
                .stations
                .iter()
                .rposition(|s| s.state == StationState::Active)
                .map(|k| g.start + k as f32 * SPACING)
                .unwrap_or(g.start + g.stations.len().saturating_sub(1) as f32 * SPACING);
            for (n, (uid, t)) in mine.iter().enumerate() {
                let Some(agent) = t.harness.agent() else { continue };
                let uid = (*uid).clone();
                let label: SharedString = format!("{} · {} — open", t.harness.label(), if t.needs_you { "needs you" } else if t.working { "working" } else { "idle" }).into();
                overlay.push(
                    div()
                        .id(SharedString::from(format!("train-{uid}")))
                        .absolute()
                        .left(px(active_x - 11. + n as f32 * 22.))
                        .top(px(g.y - 36.))
                        .p(px(2.))
                        .rounded_md()
                        .bg(style::panel())
                        .border_1()
                        .border_color(if t.needs_you { amber() } else { style::border() })
                        .cursor_pointer()
                        .hover(|d| d.bg(style::panel_hover()))
                        .tooltip(move |window, cx| Tooltip::new(label.clone()).build(window, cx))
                        .child(icons::logo(icons::agent(agent), px(16.)))
                        .on_click(cx.listener(move |this, _, window, cx| this.focus_session(&uid, window, cx)))
                        .into_any_element(),
                );
            }
            if g.hidden > 0 {
                overlay.push(
                    div()
                        .absolute()
                        .left(px(g.start - 46.))
                        .top(px(g.y - 22.))
                        .text_size(px(10.))
                        .text_color(style::faint())
                        .child(format!("+{}", g.hidden))
                        .into_any_element(),
                );
            }
        }

        let track = div()
            .relative()
            .w(px(track_w))
            .h(px(height))
            .flex_none()
            .child(canvas(|_, _, _| {}, move |bounds, _, window, _| paint_board(bounds, &paint_geom, window)).absolute().left_0().top_0().w(px(track_w)).h(px(height)))
            .child(div().absolute().left(px(TRUNK_X - 12.)).top(px(height - 22.)).text_xs().text_color(style::muted()).child("main"))
            .children(overlay);

        let body = if rows.is_empty() {
            div()
                .flex_1()
                .p_6()
                .text_sm()
                .text_color(style::faint())
                .child(match filter {
                    BoardFilter::Active => "No active lines. Each new agent session starts a line; its prompts, tasks and git milestones become stops.",
                    BoardFilter::NeedsYou => "No agent is waiting for you.",
                    BoardFilter::Finished => "Nothing finished yet.",
                    BoardFilter::All => "No lines yet.",
                    BoardFilter::Archived => "No archived lines. A line is archived when you close its last session, or with Archive line.",
                })
                .into_any_element()
        } else {
            div()
                .id("board-scroll")
                .flex_1()
                .min_w_0()
                .overflow_scroll()
                .child(h_flex().items_start().child(labels).child(track))
                .into_any_element()
        };

        let card = selected.as_ref().and_then(|id| self.lines.get(id)).map(|line| self.line_card(line, &trains, cx));

        div()
            .id("board-overlay")
            .absolute()
            .inset_0()
            .occlude()
            .bg(style::bg())
            .track_focus(&board.focus)
            .on_key_down(cx.listener(|this, ev: &KeyDownEvent, _, cx| {
                if ev.keystroke.key == "escape" {
                    cx.stop_propagation();
                    this.close_board(cx);
                }
            }))
            .child(
                v_flex()
                    .size_full()
                    .child(header)
                    .child(h_flex().flex_1().min_h_0().items_start().child(body).when_some(card, |d, c| d.child(c))),
            )
            .into_any_element()
    }

    fn harness_menu(&self, line_id: &str, kind: Option<ConnKind>, branch: bool, harnesses: &[Harness], cx: &mut Context<Self>) -> Div {
        v_flex().ml_4().children(harnesses.iter().copied().map(|h| {
            let line_id = line_id.to_string();
            h_flex()
                .id(SharedString::from(format!("hm-{}-{:?}-{branch}", h.label(), kind.map(|k| k.role()))))
                .px_2()
                .py_1()
                .gap_2()
                .items_center()
                .rounded_md()
                .text_xs()
                .cursor_pointer()
                .hover(|d| d.bg(style::panel_hover()))
                .child(harness_badge(h))
                .child(h.label())
                .on_click(cx.listener(move |this, _, window, cx| match kind {
                    Some(k) => this.start_connection(line_id.clone(), k, h, window, cx),
                    None if branch => this.branch_off_line(line_id.clone(), h, window, cx),
                    None => this.new_session_on_line(line_id.clone(), h, window, cx),
                }))
        }))
    }

    /// Datos de ejemplo para capturas (`KUAKY_DEMO_MAP`).
    #[cfg_attr(not(feature = "screenshot"), allow(dead_code))]
    pub(super) fn demo_map(&mut self, board: bool, window: &mut Window, cx: &mut Context<Self>) {
        use crate::agent_tasks::{AgentTask, TaskStatus};
        use crate::lines::LineSession;
        let session = |uid: &str, harness: &str, label: &str| LineSession {
            uid: uid.into(),
            harness: harness.into(),
            conversation: None,
            cwd: String::new(),
            label: label.into(),
            last_active: jiff::Timestamp::now(),
            role: None,
            chat: false,
        };
        self.lines.lines.clear();
        let plan = self.lines.create("Plan · search + LSP", LineKind::Plan, None, true);
        self.lines.attach(&plan, session("demo-a", "agy", "Antigravity"));
        let t = |s: &str, st| AgentTask { subject: s.into(), status: st };
        self.lines.set_tasks(
            "demo-a",
            &[
                t("Project search with regex", TaskStatus::Completed),
                t("LSP client crate", TaskStatus::Completed),
                t("Completions and hover", TaskStatus::Completed),
                t("Diagnostics underline", TaskStatus::InProgress),
                t("Go to definition", TaskStatus::Pending),
                t("Docs and release", TaskStatus::Pending),
            ],
        );
        let part = self.lines.create("Tests for the LSP client", LineKind::Feature, Some(plan.clone()), true);
        self.lines.attach(&part, session("demo-b", "claude", "Claude Code"));
        self.lines.on_prompt("demo-b", "Write integration tests against the mock server");
        self.lines.on_title("demo-b", "Mock server tests");
        self.lines.on_turn_done("demo-b");
        self.lines.on_prompt("demo-b", "Now cover hover and definition");
        let bug = self.lines.create("Bug · terminal jumps on resize", LineKind::Bug, None, true);
        self.lines.attach(&bug, session("demo-c", "claude", "Claude Code"));
        self.lines.on_prompt("demo-c", "The terminal jumps when I drag the panels");
        self.lines.on_turn_done("demo-c");
        self.lines.on_prompt("demo-c", "Debounce the PTY resize");
        let done = self.lines.create("Bug · ctrl+o opens files", LineKind::Bug, None, true);
        self.lines.add_station(&done, "Reproduce", StationSource::Manual, None, StationState::Done);
        self.lines.add_station(&done, "Scope shortcuts to !Terminal", StationSource::Manual, None, StationState::Done);
        self.lines.set_branch(&done, Some("kuaky/ctrl-o".into()));
        self.lines.set_git(&done, 2, false, Some((12, "Fix ctrl+o".into())));
        self.lines.set_git(&done, 0, true, None);
        if board {
            self.open_board(Some(plan), window, cx);
            if let Some(b) = &mut self.board {
                b.filter = BoardFilter::All;
            }
        }
        cx.notify();
    }

    /// Ficha "¿En qué me quedé?" de una línea.
    fn line_card(&self, line: &Line, trains: &HashMap<String, Train>, cx: &mut Context<Self>) -> AnyElement {
        let board = self.board.as_ref();
        let menu = board.and_then(|b| b.menu.clone());
        let line_id = line.id.clone();
        let harnesses = self.available_harnesses(cx).into_iter().filter(|h| *h != Harness::Shell).collect::<Vec<_>>();
        let name_input = board.and_then(|b| b.name_input.as_ref()).filter(|(id, _)| *id == line.id).map(|(_, i)| i.clone());
        let findings = self.findings_for(&line.id);
        let parent_name = line.parent.as_ref().and_then(|p| self.lines.get(p)).map(|p| p.name.clone());

        let section = |title: &'static str| div().mt_3().mb_1().text_xs().text_color(style::faint()).child(title);
        let step = |label: &'static str, st: Option<&Station>| {
            h_flex()
                .gap_2()
                .items_start()
                .text_xs()
                .child(div().w(px(64.)).flex_none().text_color(style::faint()).child(label))
                .child(div().flex_1().min_w_0().child(st.map(|s| s.title.clone()).unwrap_or_else(|| "—".into())))
        };
        let menu_item = |id: SharedString, label: String| {
            h_flex()
                .id(id)
                .px_2()
                .py_1()
                .gap_2()
                .rounded_md()
                .text_xs()
                .cursor_pointer()
                .hover(|d| d.bg(style::panel_hover()))
                .child(label)
        };
        let action = |id: SharedString, icon: &'static str, label: &'static str, open: bool| {
            h_flex()
                .id(id)
                .px_2()
                .py_1()
                .gap_2()
                .items_center()
                .rounded_md()
                .text_xs()
                .cursor_pointer()
                .when(open, |d| d.bg(style::panel_hover()))
                .hover(|d| d.bg(style::panel_hover()))
                .child(icons::ui(icon, px(13.), style::muted()))
                .child(div().flex_1().child(label))
                .child(icons::ui(if open { "chevron-down" } else { "chevron-right" }, px(11.), style::faint()))
        };

        let sessions = line.sessions.iter().rev().map(|s| {
            let live = trains.get(&s.uid);
            let harness = Harness::from_key(&s.harness);
            let uid = s.uid.clone();
            let uid_move = s.uid.clone();
            let status = match live {
                Some(t) if t.needs_you => "needs you",
                Some(t) if t.working => "working",
                Some(_) => "open",
                None if s.conversation.is_some() => "closed · resumable",
                None => "closed",
            };
            let role = s.role.clone().map(|r| format!(" · {r}")).unwrap_or_default();
            let moving = menu == Some(BoardMenu::MoveSession(s.uid.clone()));
            v_flex()
                .child(
                    h_flex()
                        .id(SharedString::from(format!("card-session-{}", s.uid)))
                        .px_1()
                        .py_1()
                        .gap_2()
                        .items_center()
                        .rounded_md()
                        .hover(|d| d.bg(style::panel_hover()))
                        .when_some(harness, |d, h| d.child(harness_badge(h)))
                        .child(
                            v_flex()
                                .flex_1()
                                .min_w_0()
                                .child(div().text_xs().truncate().child(format!("{}{role}", s.label)))
                                .child(div().text_size(px(10.)).text_color(style::faint()).child(format!("{status} · {}", ago(s.last_active)))),
                        )
                        .child(
                            div()
                                .id(SharedString::from(format!("card-move-{}", s.uid)))
                                .p_0p5()
                                .rounded_sm()
                                .cursor_pointer()
                                .hover(|d| d.bg(style::bg()))
                                .tooltip(|window, cx| Tooltip::new("Move to another line").build(window, cx))
                                .child(icons::ui("arrow-right-left", px(11.), style::faint()))
                                .on_click(cx.listener(move |this, _, _, cx| this.toggle_menu(BoardMenu::MoveSession(uid_move.clone()), cx))),
                        )
                        .child(
                            div()
                                .id(SharedString::from(format!("card-open-{}", s.uid)))
                                .px_1p5()
                                .rounded_sm()
                                .text_xs()
                                .text_color(style::accent())
                                .cursor_pointer()
                                .hover(|d| d.bg(style::bg()))
                                .child(if live.is_some() { "Open" } else { "Resume" })
                                .on_click(cx.listener(move |this, _, window, cx| this.focus_session(&uid, window, cx))),
                        ),
                )
                .when(moving, |d| {
                    let uid = s.uid.clone();
                    d.child(
                        v_flex()
                            .ml_4()
                            .children(self.lines.ordered(false).into_iter().filter(|(l, _)| l.id != line.id).map(|(l, _)| {
                                let target = l.id.clone();
                                let uid = uid.clone();
                                menu_item(SharedString::from(format!("move-{}-{}", uid, l.id)), l.name.clone()).on_click(cx.listener(
                                    move |this, _, _, cx| {
                                        if let Some(session) = this.lines.line_of(&uid).and_then(|l| l.sessions.iter().find(|s| s.uid == uid)).cloned() {
                                            this.lines.attach(&target, session);
                                        }
                                        if let Some(b) = &mut this.board {
                                            b.menu = None;
                                        }
                                        cx.notify();
                                    },
                                ))
                            }))
                            .child(
                                menu_item(SharedString::from(format!("move-new-{}", uid)), "New line".into()).on_click(cx.listener(move |this, _, _, cx| {
                                    if let Some(session) = this.lines.line_of(&uid).and_then(|l| l.sessions.iter().find(|s| s.uid == uid)).cloned() {
                                        let id = this.lines.create(&format!("{} session", session.label), LineKind::Task, None, false);
                                        this.lines.attach(&id, session);
                                    }
                                    if let Some(b) = &mut this.board {
                                        b.menu = None;
                                    }
                                    cx.notify();
                                })),
                            ),
                    )
                })
        });


        v_flex()
            .id("line-card")
            .w(px(340.))
            .h_full()
            .flex_none()
            .overflow_y_scroll()
            .p_4()
            .border_l_1()
            .border_color(style::border())
            .bg(style::panel())
            .child(
                h_flex()
                    .gap_2()
                    .items_center()
                    .child(div().size(px(10.)).flex_none().rounded_full().bg(line_color(line.color)))
                    .child(match name_input {
                        Some(input) => div().flex_1().child(Input::new(&input)).into_any_element(),
                        None => div().flex_1().text_sm().child(line.name.clone()).into_any_element(),
                    }),
            )
            .child(
                h_flex().mt_2().gap_1().children(LineKind::ALL.into_iter().map(|k| {
                    let on = line.kind == k;
                    let id = line.id.clone();
                    div()
                        .id(SharedString::from(format!("kind-{}", k.label())))
                        .px_2()
                        .py_0p5()
                        .rounded_md()
                        .text_xs()
                        .cursor_pointer()
                        .border_1()
                        .border_color(if on { line_color(line.color) } else { style::border() })
                        .text_color(if on { style::text() } else { style::muted() })
                        .child(k.label())
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.lines.set_kind(&id, k);
                            cx.notify();
                        }))
                })),
            )
            .when_some(line.branch.clone(), |d, b| {
                d.child(
                    h_flex()
                        .mt_2()
                        .gap_1()
                        .text_xs()
                        .text_color(style::muted())
                        .child(icons::ui("git-branch", px(12.), style::muted()))
                        .child(b)
                        .when(line.merged, |d| d.child(div().text_color(style::good()).child("· merged"))),
                )
            })
            .child(section("WHERE YOU LEFT OFF"))
            .child(step("Last done", line.last_done()))
            .child(step("Now", line.current()))
            .child(step("Next", line.next_pending()))
            .child(section("SESSIONS"))
            .when(line.sessions.is_empty(), |d| d.child(div().text_xs().text_color(style::faint()).child("No sessions yet.")))
            .children(sessions)
            .when(!findings.is_empty(), |d| {
                d.child(section("FINDINGS")).children(findings.into_iter().map(|path| {
                    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                    let id = line.id.clone();
                    let open_path = path.clone();
                    h_flex()
                        .gap_2()
                        .items_center()
                        .text_xs()
                        .child(icons::ui("file-text", px(12.), style::muted()))
                        .child(
                            div()
                                .id(SharedString::from(format!("finding-{name}")))
                                .flex_1()
                                .min_w_0()
                                .truncate()
                                .cursor_pointer()
                                .hover(|d| d.text_color(style::accent()))
                                .child(name.clone())
                                .on_click(cx.listener(move |this, _, window, cx| {
                                    this.board = None;
                                    this.open_file(open_path.clone(), window, cx);
                                })),
                        )
                        .child(
                            div()
                                .id(SharedString::from(format!("finding-send-{name}")))
                                .px_1p5()
                                .rounded_sm()
                                .text_color(style::accent())
                                .cursor_pointer()
                                .hover(|d| d.bg(style::bg()))
                                .tooltip(|window, cx| Tooltip::new("Type a request to address these findings in the author's session (not sent)").build(window, cx))
                                .child("Send to author")
                                .on_click(cx.listener(move |this, _, _, cx| this.send_findings(&id, path.clone(), cx))),
                        )
                }))
            })
            .child(section("ACTIONS"))
            .child(action("act-new".into(), "plus", "New session on this line", menu == Some(BoardMenu::NewSession)).on_click(cx.listener(|this, _, _, cx| this.toggle_menu(BoardMenu::NewSession, cx))))
            .when(menu == Some(BoardMenu::NewSession), |d| d.child(self.harness_menu(&line_id, None, false, &harnesses, cx)))
            .child(action("act-branch".into(), "git-branch-plus", "Branch off (sub-line)…", menu == Some(BoardMenu::Branch)).on_click(cx.listener(|this, _, _, cx| this.toggle_menu(BoardMenu::Branch, cx))))
            .when(menu == Some(BoardMenu::Branch), |d| d.child(self.harness_menu(&line_id, None, true, &harnesses, cx)))
            .children(ConnKind::ALL.into_iter().map(|k| {
                let open = menu == Some(BoardMenu::Connection(k));
                let icon = match k {
                    ConnKind::Review => "eye",
                    ConnKind::Tester => "flask-conical",
                    ConnKind::SecondOpinion => "git-compare",
                    ConnKind::Relay => "arrow-right-left",
                };
                v_flex()
                    .child(action(SharedString::from(format!("act-{}", k.role())), icon, k.label(), open).on_click(cx.listener(move |this, _, _, cx| this.toggle_menu(BoardMenu::Connection(k), cx))))
                    .when(open, |d| d.child(self.harness_menu(&line_id, Some(k), false, &harnesses, cx)))
            }))
            .child(action("act-parent".into(), "git-fork", "Part of…", menu == Some(BoardMenu::Parent)).on_click(cx.listener(|this, _, _, cx| this.toggle_menu(BoardMenu::Parent, cx))))
            .when_some(parent_name, |d, p| d.child(div().ml_8().text_xs().text_color(style::faint()).child(format!("now: {p}"))))
            .when(menu == Some(BoardMenu::Parent), |d| {
                let id = line.id.clone();
                d.child(
                    v_flex()
                        .ml_4()
                        .child(menu_item("parent-none".into(), "None (top level)".into()).on_click(cx.listener({
                            let id = id.clone();
                            move |this, _, _, cx| {
                                this.lines.set_parent(&id, None);
                                if let Some(b) = &mut this.board {
                                    b.menu = None;
                                }
                                cx.notify();
                            }
                        })))
                        .children(self.lines.ordered(false).into_iter().filter(|(l, _)| l.id != line.id && l.parent.as_deref() != Some(line.id.as_str())).map(|(l, _)| {
                            let id = id.clone();
                            let target = l.id.clone();
                            menu_item(SharedString::from(format!("parent-{}", l.id)), l.name.clone()).on_click(cx.listener(move |this, _, _, cx| {
                                this.lines.set_parent(&id, Some(target.clone()));
                                if let Some(b) = &mut this.board {
                                    b.menu = None;
                                }
                                cx.notify();
                            }))
                        })),
                )
            })
            .child(action("act-merge".into(), "merge", "Merge into another line…", menu == Some(BoardMenu::MergeInto)).on_click(cx.listener(|this, _, _, cx| this.toggle_menu(BoardMenu::MergeInto, cx))))
            .when(menu == Some(BoardMenu::MergeInto), |d| {
                let id = line.id.clone();
                d.child(v_flex().ml_4().children(self.lines.ordered(true).into_iter().filter(|(l, _)| l.id != line.id).map(|(l, _)| {
                    let id = id.clone();
                    let target = l.id.clone();
                    menu_item(SharedString::from(format!("merge-{}", l.id)), l.name.clone()).on_click(cx.listener(move |this, _, window, cx| {
                        this.lines.merge_into(&id, &target);
                        this.select_line(target.clone(), window, cx);
                    }))
                })))
            })
            .child({
                let id = line.id.clone();
                let archived = line.archived;
                h_flex()
                    .id("act-archive")
                    .mt_1()
                    .px_2()
                    .py_1()
                    .gap_2()
                    .items_center()
                    .rounded_md()
                    .text_xs()
                    .text_color(style::muted())
                    .cursor_pointer()
                    .hover(|d| d.bg(style::panel_hover()))
                    .child(icons::ui("archive", px(13.), style::muted()))
                    .child(if archived { "Unarchive line" } else { "Archive line" })
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.lines.set_archived(&id, !archived);
                        cx.notify();
                    }))
            })
            .into_any_element()
    }
}

#[derive(Clone)]
struct RowGeom {
    y: f32,
    start: f32,
    parent_y: Option<f32>,
    color: u32,
    stations: Vec<Station>,
    hidden: usize,
    merged: bool,
    finished: bool,
    needs: bool,
    depth: usize,
    /// Línea elegida (resaltada) y si hay otra elegida (esta se atenúa).
    selected: bool,
    dim: bool,
}

fn paint_board(bounds: Bounds<Pixels>, rows: &[RowGeom], window: &mut Window) {
    let ox = bounds.origin.x;
    let oy = bounds.origin.y;
    let pt = |x: f32, y: f32| point(ox + px(x), oy + px(y));

    // Vía principal (main), vertical a la izquierda.
    if !rows.is_empty() {
        let bottom = rows.iter().map(|r| r.y).fold(0., f32::max) + ROW * 0.5 + 4.;
        let mut p = PathBuilder::stroke(px(5.));
        p.move_to(pt(TRUNK_X, TOP - 6.));
        p.line_to(pt(TRUNK_X, bottom));
        if let Ok(path) = p.build() {
            window.paint_path(path, style::soft(style::muted(), 0.6));
        }
    }

    // Franja detrás de la línea elegida, de lado a lado.
    for row in rows.iter().filter(|r| r.selected) {
        window.paint_quad(PaintQuad {
            bounds: Bounds { origin: pt(0., row.y - ROW * 0.5 + 4.), size: size(bounds.size.width, px(ROW - 8.)) },
            corner_radii: Corners::all(px(10.)),
            background: style::soft(line_color(row.color), 0.10).into(),
            border_widths: Edges::all(px(1.)),
            border_color: style::soft(line_color(row.color), 0.35),
            border_style: BorderStyle::Solid,
        });
    }

    for row in rows {
        let base: Hsla = if row.finished && !row.merged { style::soft(line_color(row.color), 0.45) } else { line_color(row.color).into() };
        // Con una línea elegida, las demás quedan atenuadas.
        let color = if row.dim { Hsla { a: base.a * 0.35, ..base } } else { base };
        let width = if row.selected { 6. } else { 4. };
        let n = row.stations.len();
        let last_x = row.start + n.saturating_sub(1) as f32 * SPACING;

        // Salida: desde main (raíz) o desde la línea padre (sub-línea).
        let mut p = PathBuilder::stroke(px(width));
        match row.parent_y {
            Some(py) if row.depth > 0 => {
                let bx = row.start - 46.;
                p.move_to(pt(bx, py));
                p.curve_to(pt(bx + 28., row.y), pt(bx, row.y));
            }
            _ => {
                p.move_to(pt(TRUNK_X, row.y));
            }
        }
        p.line_to(pt(if n > 0 { last_x } else { row.start }, row.y));
        if let Ok(path) = p.build() {
            window.paint_path(path, color);
        }

        // Lo que falta: tramo punteado; integrada: vuelve a main con una marca.
        let tail_start = if n > 0 { last_x + 12. } else { row.start + 12. };
        if !row.finished {
            let mut x = tail_start;
            while x < tail_start + 70. {
                let mut d = PathBuilder::stroke(px(3.));
                d.move_to(pt(x, row.y));
                d.line_to(pt(x + 7., row.y));
                if let Ok(path) = d.build() {
                    window.paint_path(path, Hsla { a: color.a * 0.6, ..color });
                }
                x += 13.;
            }
        } else if row.merged {
            let mut d = PathBuilder::stroke(px(4.));
            d.move_to(pt(last_x, row.y));
            d.line_to(pt(last_x + 48., row.y));
            if let Ok(path) = d.build() {
                window.paint_path(path, color);
            }
            dot(window, pt(last_x + 56., row.y), 8., style::bg().into(), style::muted().into(), 3.);
        }
        // Empalme con main.
        if row.depth == 0 || row.parent_y.is_none() {
            dot(window, pt(TRUNK_X, row.y), 5., style::muted().into(), style::muted().into(), 0.);
        }

        for (k, st) in row.stations.iter().enumerate() {
            let c = pt(row.start + k as f32 * SPACING, row.y);
            match st.state {
                StationState::Done => dot(window, c, 7., color, color, 0.),
                StationState::Pending => dot(window, c, 6., style::bg().into(), color, 2.5),
                StationState::Active => {
                    let ring: Hsla = if row.needs { amber().into() } else { color };
                    dot(window, c, 11., style::bg().into(), ring, 2.5);
                    dot(window, c, 5., color, color, 0.);
                }
            }
        }
    }
}

fn dot(window: &mut Window, center: Point<Pixels>, r: f32, fill: Hsla, border: Hsla, border_w: f32) {
    let r = px(r);
    window.paint_quad(PaintQuad {
        bounds: Bounds { origin: point(center.x - r, center.y - r), size: size(r * 2., r * 2.) },
        corner_radii: Corners::all(r),
        background: fill.into(),
        border_widths: Edges::all(px(border_w)),
        border_color: border,
        border_style: BorderStyle::Solid,
    });
}

#[cfg(test)]
mod tests {
    use super::{meaningful_title, reviewer_is_done};
    use crate::project::Harness;

    #[test]
    fn skips_generic_terminal_titles() {
        assert_eq!(meaningful_title("zsh", Harness::Claude), None);
        assert_eq!(meaningful_title("claude", Harness::Claude), None);
        assert_eq!(meaningful_title("~/Desktop/kuaky", Harness::Antigravity), None);
        assert_eq!(meaningful_title("Fix LSP underline", Harness::Claude).as_deref(), Some("Fix LSP underline"));
    }

    #[test]
    fn detects_when_the_reviewer_is_done() {
        assert!(reviewer_is_done("LGTM"));
        assert!(reviewer_is_done("Checked again.\nLGTM — nothing else."));
        assert!(reviewer_is_done("I found no further issues."));
        assert!(!reviewer_is_done("Two problems: the LGTM check in foo() is wrong."));
        assert!(!reviewer_is_done("1. src/a.rs:10 leaks the handle"));
    }
}
