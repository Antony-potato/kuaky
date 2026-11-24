//! Chat propio con agentes por ACP (Claude Code, Antigravity, Codex…).
//!
//! Pensado para conversaciones largas:
//! - tus mensajes se distinguen a primera vista (tarjeta con barra de color,
//!   número `#n` y hora);
//! - el prompt del turno que estás leyendo queda fijo arriba;
//! - un riel a la derecha marca cada mensaje tuyo y salta a él;
//! - `⌥↑` / `⌥↓` saltan entre tus mensajes y `⌘⇧J` los lista;
//! - editar un mensaje anterior crea una versión nueva sin perder la anterior;
//! - antes de cada turno se guarda un punto de restauración del código.

use crate::style;
use crate::terminal_view::new_uid;
use gpui_kit::component::input::{InputEvent, Textarea, TextareaState};
use gpui_kit::component::text::TextView;
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::component::{h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use kuaky_acp::{Client, ConfigOption, Event, FileDiff, PermissionRequest, PlanEntry, PromptBlock, SlashCommand, ToolCall};
use std::{
    collections::{BTreeMap, HashSet},
    path::PathBuf,
    sync::Arc,
    time::{Duration, Instant},
};

actions!(chat, [PrevPrompt, NextPrompt, TogglePromptList, StopTurn]);

pub fn init(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new("alt-up", PrevPrompt, Some("ChatView")),
        KeyBinding::new("alt-down", NextPrompt, Some("ChatView")),
        KeyBinding::new("cmd-shift-j", TogglePromptList, Some("ChatView")),
        KeyBinding::new("ctrl-shift-j", TogglePromptList, Some("ChatView")),
    ]);
}

/// Cómo arrancar el agente en modo ACP.
#[derive(Clone)]
pub struct ChatLaunch {
    pub label: &'static str,
    pub program: PathBuf,
    pub args: Vec<String>,
    pub cwd: PathBuf,
    /// Sesión del agente a retomar (`session/load`).
    pub resume: Option<String>,
    /// Texto que queda escrito en la caja (no se envía).
    pub draft: Option<String>,
}

#[derive(Clone, PartialEq)]
pub enum ChatStatus {
    Connecting,
    Ready,
    Running,
    Error(String),
    Exited,
}

pub enum ChatEvent {
    OpenDiff { title: String, patch: String },
    OpenFile { path: PathBuf, line: u32 },
    /// Cambió el estado (para la barra de sesiones y los avisos).
    Status,
    NeedsYou(String),
    TurnDone,
    /// El usuario envió un mensaje (para el mapa).
    Prompt(String),
    /// El agente actualizó su plan (para el mapa).
    Plan(Vec<PlanEntry>),
    /// ID de la sesión del agente (para retomarla después).
    Session(String),
    Title(String),
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Detail {
    Normal,
    Thinking,
    Verbose,
}

impl Detail {
    fn label(self) -> &'static str {
        match self {
            Detail::Normal => "Normal",
            Detail::Thinking => "Thinking",
            Detail::Verbose => "Verbose",
        }
    }
    fn next(self) -> Detail {
        match self {
            Detail::Normal => Detail::Thinking,
            Detail::Thinking => Detail::Verbose,
            Detail::Verbose => Detail::Normal,
        }
    }
}

#[derive(Clone)]
enum Item {
    Agent(String),
    Thought(String),
    Tool(ToolCall),
    Permission { req: PermissionRequest, answer: Option<String> },
    Notice(String),
}

#[derive(Clone)]
struct UserMsg {
    text: String,
    n: usize,
    at: jiff::Timestamp,
    /// Punto de restauración del código antes de este mensaje.
    checkpoint: Option<String>,
}

#[derive(Clone)]
struct Turn {
    user: Option<UserMsg>,
    items: Vec<Item>,
    started: Option<Instant>,
    duration: Option<Duration>,
    /// Archivos cambiados en el turno: ruta → (texto original, texto final).
    files: BTreeMap<PathBuf, (Option<String>, String)>,
}

impl Turn {
    fn new(user: Option<UserMsg>) -> Turn {
        Turn { user, items: Vec::new(), started: Some(Instant::now()), duration: None, files: BTreeMap::new() }
    }

    fn record_file(&mut self, path: PathBuf, old: Option<String>, new: String) {
        match self.files.get_mut(&path) {
            Some(entry) => entry.1 = new,
            None => {
                self.files.insert(path, (old, new));
            }
        }
    }

    fn stats(&self) -> (usize, usize) {
        self.files.iter().fold((0, 0), |(a, r), (path, (old, new))| {
            let (x, y) = count_changes(&FileDiff { path: path.to_string_lossy().into_owned(), old_text: old.clone(), new_text: new.clone() });
            (a + x, r + y)
        })
    }
}

/// Versión anterior de la conversación a partir del mensaje `at`.
struct Branch {
    at: usize,
    turns: Vec<Turn>,
}

/// Lo que se muestra en el selector de archivos/comandos.
#[derive(Clone)]
enum Suggestion {
    File(String),
    Command(SlashCommand),
}

pub struct ChatView {
    pub launch: ChatLaunch,
    pub status: ChatStatus,
    pub uid: String,
    pub session: Option<String>,
    pub title: Option<String>,
    client: Option<Arc<Client>>,
    turns: Vec<Turn>,
    branches: Vec<Branch>,
    /// Versión que se está viendo de cada punto de edición (`None` = la actual).
    viewing: Option<(usize, usize)>,
    plan: Vec<PlanEntry>,
    plan_open: bool,
    config: Vec<ConfigOption>,
    open_config: Option<String>,
    commands: Vec<SlashCommand>,
    queue: Vec<String>,
    detail: Detail,
    expanded: HashSet<(usize, usize)>,
    editing: Option<usize>,
    prompt_list: bool,
    files: Option<Arc<Vec<String>>>,
    suggestions: Vec<Suggestion>,
    suggestion_ix: usize,
    input: Entity<TextareaState>,
    scroll: ScrollHandle,
    focus: FocusHandle,
    _sub: Subscription,
}

impl EventEmitter<ChatEvent> for ChatView {}

impl Focusable for ChatView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl ChatView {
    pub fn new(launch: ChatLaunch, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let label = launch.label;
        let input = cx.new(|cx| {
            TextareaState::new(window, cx)
                .auto_grow(1, 10)
                .submit_on_enter(true)
                .placeholder(format!("Message {label}…  @ files · / commands · Enter sends · ⇧Enter new line"))
        });
        let sub = cx.subscribe_in(&input, window, |this, _, ev: &InputEvent, window, cx| match ev {
            InputEvent::PressEnter { shift: false, .. } => {
                if this.suggestions.is_empty() {
                    this.send(window, cx);
                } else {
                    this.accept_suggestion(window, cx);
                }
            }
            InputEvent::Change => this.update_suggestions(cx),
            _ => {}
        });
        if let Some(draft) = &launch.draft {
            input.update(cx, |s, cx| s.set_value(draft.clone(), window, cx));
        }
        input.update(cx, |s, cx| s.focus(window, cx));
        let this = Self {
            launch,
            status: ChatStatus::Connecting,
            uid: new_uid(),
            session: None,
            title: None,
            client: None,
            turns: Vec::new(),
            branches: Vec::new(),
            viewing: None,
            plan: Vec::new(),
            plan_open: true,
            config: Vec::new(),
            open_config: None,
            commands: Vec::new(),
            queue: Vec::new(),
            detail: Detail::Normal,
            expanded: HashSet::new(),
            editing: None,
            prompt_list: false,
            files: None,
            suggestions: Vec::new(),
            suggestion_ix: 0,
            input,
            scroll: ScrollHandle::new(),
            focus: cx.focus_handle(),
            _sub: sub,
        };
        this.connect(cx);
        this.load_files(cx);
        this
    }

    pub fn is_waiting_permission(&self) -> bool {
        self.turns.iter().flat_map(|t| &t.items).any(|i| matches!(i, Item::Permission { answer: None, .. }))
    }

    /// Último texto de la conversación (para el título automático).
    pub fn first_prompt(&self) -> Option<&str> {
        self.turns.iter().find_map(|t| t.user.as_ref()).map(|u| u.text.as_str())
    }

    fn connect(&self, cx: &mut Context<Self>) {
        let launch = self.launch.clone();
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move {
                    let env = vec![("PATH".to_string(), kuaky_git::login_path().to_string_lossy().into_owned())];
                    let client = Arc::new(Client::spawn(&launch.program, &launch.args, &launch.cwd, &env)?);
                    let info = match &launch.resume {
                        Some(id) if client.capabilities.load_session => client.load_session(&launch.cwd, id)?,
                        _ => client.new_session_info(&launch.cwd)?,
                    };
                    anyhow::Ok((client, info))
                })
                .await;
            let (client, info) = match result {
                Ok(ok) => ok,
                Err(err) => {
                    this.update(cx, |this, cx| {
                        let text = format!("{err:#}");
                        let hint = if text.to_lowercase().contains("auth") {
                            format!("{text}\n\nSign in with the agent's own CLI in a terminal (for example `claude` → /login or `agy`), then reopen this chat.")
                        } else {
                            text
                        };
                        this.status = ChatStatus::Error(hint);
                        cx.emit(ChatEvent::Status);
                        cx.notify();
                    })
                    .ok();
                    return;
                }
            };
            let events = client.events();
            let ok = this
                .update(cx, |this, cx| {
                    this.session = Some(info.id.clone());
                    this.config = info.config;
                    this.client = Some(client);
                    this.status = ChatStatus::Ready;
                    // Lo repetido por `session/load` no cuenta como turno en curso.
                    for t in &mut this.turns {
                        t.started = None;
                    }
                    cx.emit(ChatEvent::Session(info.id));
                    cx.emit(ChatEvent::Status);
                    #[cfg(feature = "screenshot")]
                    if let Ok(prompt) = std::env::var("KUAKY_DEMO_CHAT_PROMPT") {
                        this.dispatch(prompt, cx);
                    }
                    cx.notify();
                })
                .is_ok();
            if !ok {
                return;
            }
            while let Ok(first) = events.recv_async().await {
                let mut batch = vec![first];
                batch.extend(events.try_iter());
                let alive = this
                    .update(cx, |this, cx| {
                        let at_bottom = this.at_bottom();
                        for ev in batch {
                            this.on_event(ev, cx);
                        }
                        if at_bottom {
                            this.scroll.scroll_to_bottom();
                        }
                        cx.notify();
                    })
                    .is_ok();
                if !alive {
                    break;
                }
            }
        })
        .detach();
    }

    /// Lista de archivos del proyecto para `@` (una sola vez, en segundo plano).
    fn load_files(&self, cx: &mut Context<Self>) {
        let root = self.launch.cwd.clone();
        cx.spawn(async move |this, cx| {
            let files = cx
                .background_executor()
                .spawn(async move {
                    let mut out = Vec::new();
                    for entry in ignore::WalkBuilder::new(&root).hidden(true).build().filter_map(Result::ok) {
                        if entry.file_type().is_some_and(|t| t.is_file()) {
                            if let Ok(rel) = entry.path().strip_prefix(&root) {
                                out.push(rel.to_string_lossy().replace('\\', "/"));
                            }
                        }
                        if out.len() >= 20_000 {
                            break;
                        }
                    }
                    out.sort();
                    out
                })
                .await;
            let _ = this.update(cx, |this, _| this.files = Some(Arc::new(files)));
        })
        .detach();
    }

    fn at_bottom(&self) -> bool {
        let offset = self.scroll.offset().y;
        let max = self.scroll.max_offset().y;
        max <= px(0.) || -offset >= max - px(48.)
    }

    fn current_turn(&mut self) -> &mut Turn {
        if self.turns.is_empty() {
            self.turns.push(Turn::new(None));
        }
        self.turns.last_mut().expect("there is a turn")
    }

    fn on_event(&mut self, ev: Event, cx: &mut Context<Self>) {
        match ev {
            Event::UserMessageChunk(t) => match self.turns.last_mut() {
                // Al retomar, los mensajes llegan en trozos: se juntan.
                Some(turn) if turn.user.is_some() && turn.items.is_empty() => {
                    if let Some(u) = &mut turn.user {
                        u.text.push_str(&t);
                    }
                }
                _ => {
                    let n = self.turns.iter().filter(|t| t.user.is_some()).count() + 1;
                    let mut turn = Turn::new(Some(UserMsg { text: t, n, at: jiff::Timestamp::now(), checkpoint: None }));
                    turn.started = None;
                    self.turns.push(turn);
                }
            },
            Event::MessageChunk(t) => match self.current_turn().items.last_mut() {
                Some(Item::Agent(s)) => s.push_str(&t),
                _ => self.current_turn().items.push(Item::Agent(t)),
            },
            Event::ThoughtChunk(t) => match self.current_turn().items.last_mut() {
                Some(Item::Thought(s)) => s.push_str(&t),
                _ => self.current_turn().items.push(Item::Thought(t)),
            },
            Event::ToolCall(call) => {
                let turn = self.current_turn();
                for d in &call.diffs {
                    turn.record_file(PathBuf::from(&d.path), d.old_text.clone(), d.new_text.clone());
                }
                turn.items.push(Item::Tool(call));
            }
            Event::ToolCallUpdate(update) => {
                let turn = self.current_turn();
                for d in &update.diffs {
                    turn.record_file(PathBuf::from(&d.path), d.old_text.clone(), d.new_text.clone());
                }
                let existing = turn.items.iter_mut().rev().find_map(|i| match i {
                    Item::Tool(c) if c.id == update.id => Some(c),
                    _ => None,
                });
                match existing {
                    Some(call) => {
                        if update.title.is_some() {
                            call.title = update.title;
                        }
                        if update.kind.is_some() {
                            call.kind = update.kind;
                        }
                        if update.status.is_some() {
                            call.status = update.status;
                        }
                        if !update.diffs.is_empty() {
                            call.diffs = update.diffs;
                        }
                        if update.text.is_some() {
                            call.text = update.text;
                        }
                        if !update.locations.is_empty() {
                            call.locations = update.locations;
                        }
                    }
                    None => turn.items.push(Item::Tool(update)),
                }
            }
            Event::FileWritten { path, old_text, new_text } => self.current_turn().record_file(path, old_text, new_text),
            Event::Plan(entries) => {
                self.plan = entries.clone();
                cx.emit(ChatEvent::Plan(entries));
            }
            Event::Permission(req) => {
                cx.emit(ChatEvent::NeedsYou(req.title.clone()));
                self.current_turn().items.push(Item::Permission { req, answer: None });
                cx.emit(ChatEvent::Status);
            }
            Event::Commands(c) => self.commands = c,
            Event::ModeChanged(mode) => {
                for opt in self.config.iter_mut().filter(|o| o.category == "mode" || o.id == "mode") {
                    opt.current = mode.clone();
                }
            }
            Event::ConfigOptions(opts) => {
                if !opts.is_empty() {
                    self.config = opts;
                }
            }
            Event::Title(t) => {
                self.title = Some(t.clone());
                cx.emit(ChatEvent::Title(t));
            }
            Event::TurnEnded(reason) => {
                self.status = ChatStatus::Ready;
                if let Some(turn) = self.turns.last_mut() {
                    turn.duration = turn.started.map(|s| s.elapsed());
                    if reason != "end_turn" {
                        let msg = match reason.as_str() {
                            "cancelled" => "Stopped.".to_string(),
                            "max_tokens" => "Reached the response token limit.".to_string(),
                            "max_turn_requests" => "Reached the agent's step limit for this turn.".to_string(),
                            "refusal" => "The agent declined to continue.".to_string(),
                            other => format!("Turn ended: {other}"),
                        };
                        turn.items.push(Item::Notice(msg));
                    }
                }
                cx.emit(ChatEvent::TurnDone);
                cx.emit(ChatEvent::Status);
                if !self.queue.is_empty() {
                    let next = self.queue.remove(0);
                    self.dispatch(next, cx);
                }
            }
            Event::Error(e) => {
                self.status = ChatStatus::Ready;
                self.current_turn().items.push(Item::Notice(format!("Error: {e}")));
                cx.emit(ChatEvent::Status);
            }
            Event::Exited => {
                self.status = ChatStatus::Exited;
                cx.emit(ChatEvent::Status);
            }
        }
    }

    // ── Enviar ─────────────────────────────────────────────────────────

    fn send(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let text = self.input.read(cx).value().trim().to_string();
        if text.is_empty() {
            return;
        }
        self.input.update(cx, |s, cx| s.set_value("", window, cx));
        self.suggestions.clear();
        if let Some(at) = self.editing.take() {
            self.fork_at(at, text, cx);
            return;
        }
        match self.status {
            ChatStatus::Ready => self.dispatch(text, cx),
            ChatStatus::Running | ChatStatus::Connecting => {
                self.queue.push(text);
                cx.notify();
            }
            _ => {}
        }
    }

    /// Envía un mensaje: guarda un punto de restauración y arma los bloques
    /// (texto + archivos mencionados con `@`).
    fn dispatch(&mut self, text: String, cx: &mut Context<Self>) {
        let (Some(client), Some(session)) = (self.client.clone(), self.session.clone()) else {
            self.queue.insert(0, text);
            return;
        };
        let n = self.turns.iter().filter(|t| t.user.is_some()).count() + 1;
        let mut blocks = vec![PromptBlock::Text(text.clone())];
        for mention in mentions(&text) {
            let path = self.launch.cwd.join(&mention);
            if path.is_file() {
                blocks.push(PromptBlock::ResourceLink { uri: kuaky_lsp::path_to_uri(&path), name: mention });
            }
        }
        let cwd = self.launch.cwd.clone();
        let checkpoint_name = format!("{}/{n}", self.uid);
        let checkpoint = kuaky_git::Repo::discover(&cwd).and_then(|repo| repo.checkpoint(&checkpoint_name).ok());
        self.viewing = None;
        self.turns.push(Turn::new(Some(UserMsg { text: text.clone(), n, at: jiff::Timestamp::now(), checkpoint })));
        if let Err(err) = client.prompt_blocks(&session, &blocks) {
            self.current_turn().items.push(Item::Notice(format!("Failed to send: {err:#}")));
            cx.notify();
            return;
        }
        if self.title.is_none() {
            cx.emit(ChatEvent::Title(crate::lines::short(&text, 48)));
        }
        cx.emit(ChatEvent::Prompt(text));
        self.status = ChatStatus::Running;
        self.scroll.scroll_to_bottom();
        cx.emit(ChatEvent::Status);
        cx.notify();
    }

    /// Editar el mensaje `#at`: la conversación desde ahí pasa a ser una
    /// versión anterior y se sigue en una sesión nueva con el contexto previo.
    fn fork_at(&mut self, at: usize, text: String, cx: &mut Context<Self>) {
        let Some(pos) = self.turns.iter().position(|t| t.user.as_ref().is_some_and(|u| u.n == at)) else {
            self.dispatch(text, cx);
            return;
        };
        let tail: Vec<Turn> = self.turns.drain(pos..).collect();
        self.branches.push(Branch { at, turns: tail });
        let context = transcript(&self.turns, 6000);
        let Some(client) = self.client.clone() else { return };
        let cwd = self.launch.cwd.clone();
        self.status = ChatStatus::Connecting;
        cx.notify();
        cx.spawn(async move |this, cx| {
            let result = cx.background_executor().spawn(async move { client.new_session_info(&cwd) }).await;
            let _ = this.update(cx, |this, cx| match result {
                Ok(info) => {
                    this.session = Some(info.id.clone());
                    if !info.config.is_empty() {
                        this.config = info.config;
                    }
                    this.status = ChatStatus::Ready;
                    cx.emit(ChatEvent::Session(info.id));
                    let full = if context.is_empty() {
                        text
                    } else {
                        format!("Context from the earlier part of this conversation:\n{context}\n\n---\n\n{text}")
                    };
                    let shown = text_only(&full);
                    this.dispatch_as(at, full, shown, cx);
                }
                Err(err) => {
                    this.status = ChatStatus::Ready;
                    this.current_turn().items.push(Item::Notice(format!("Could not start a new version: {err:#}")));
                    cx.notify();
                }
            });
        })
        .detach();
    }

    /// Envía `full` al agente mostrando `shown` como mensaje `#at`.
    fn dispatch_as(&mut self, at: usize, full: String, shown: String, cx: &mut Context<Self>) {
        self.dispatch(full, cx);
        if let Some(turn) = self.turns.last_mut() {
            if let Some(u) = &mut turn.user {
                u.n = at;
                u.text = shown;
            }
        }
    }

    fn stop(&mut self, cx: &mut Context<Self>) {
        if let (Some(client), Some(session)) = (&self.client, &self.session) {
            let _ = client.cancel(session);
        }
        let client = self.client.clone();
        for item in self.turns.iter_mut().flat_map(|t| t.items.iter_mut()) {
            if let Item::Permission { req, answer: answer @ None } = item {
                if let Some(c) = &client {
                    let _ = c.answer_permission(&req.request_id, None);
                }
                *answer = Some("cancelled".into());
            }
        }
        cx.notify();
    }

    fn answer(&mut self, turn: usize, index: usize, option: Option<(String, String)>, cx: &mut Context<Self>) {
        let Some(client) = self.client.clone() else { return };
        if let Some(Item::Permission { req, answer }) = self.turns.get_mut(turn).and_then(|t| t.items.get_mut(index)) {
            if answer.is_some() {
                return;
            }
            let _ = client.answer_permission(&req.request_id, option.as_ref().map(|(id, _)| id.as_str()));
            *answer = Some(option.map(|(_, name)| name).unwrap_or_else(|| "cancelled".into()));
        }
        cx.emit(ChatEvent::Status);
        cx.notify();
    }

    fn set_config(&mut self, id: String, value: String, cx: &mut Context<Self>) {
        self.open_config = None;
        for opt in self.config.iter_mut().filter(|o| o.id == id) {
            opt.current = value.clone();
        }
        let (Some(client), Some(session)) = (self.client.clone(), self.session.clone()) else { return };
        cx.spawn(async move |this, cx| {
            let result = cx.background_executor().spawn(async move { client.set_config_option(&session, &id, &value) }).await;
            let _ = this.update(cx, |this, cx| {
                match result {
                    Ok(opts) if !opts.is_empty() => this.config = opts,
                    Ok(_) => {}
                    Err(err) => this.current_turn().items.push(Item::Notice(format!("Could not change setting: {err:#}"))),
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    fn restore(&mut self, sha: String, n: usize, cx: &mut Context<Self>) {
        let cwd = self.launch.cwd.clone();
        let result = kuaky_git::Repo::discover(&cwd).map(|r| r.restore_checkpoint(&sha));
        let msg = match result {
            Some(Ok(())) => format!("Files restored to before message #{n}. New files created later were kept."),
            Some(Err(err)) => format!("Could not restore: {err:#}"),
            None => "This folder is not a git repository.".into(),
        };
        self.current_turn().items.push(Item::Notice(msg));
        cx.notify();
    }

    fn edit(&mut self, n: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(text) = self.turns.iter().filter_map(|t| t.user.as_ref()).find(|u| u.n == n).map(|u| u.text.clone()) else { return };
        self.editing = Some(n);
        self.input.update(cx, |s, cx| {
            s.set_value(text, window, cx);
            s.focus(window, cx);
        });
        cx.notify();
    }

    // ── Navegación entre tus mensajes ─────────────────────────────────

    /// Índices (en la lista mostrada) de los turnos con mensaje tuyo.
    fn prompt_indices(&self) -> Vec<usize> {
        self.shown_turns().iter().enumerate().filter(|(_, t)| t.user.is_some()).map(|(i, _)| i).collect()
    }

    fn jump(&mut self, delta: i32, cx: &mut Context<Self>) {
        let prompts = self.prompt_indices();
        if prompts.is_empty() {
            return;
        }
        let (top, offset) = self.scroll.logical_scroll_top();
        let target = if delta < 0 {
            // Si ya estás dentro de un turno, primero vuelve a su inicio.
            prompts.iter().rev().find(|&&i| i < top || (i == top && offset < px(-4.))).copied().unwrap_or(prompts[0])
        } else {
            prompts.iter().find(|&&i| i > top).copied().unwrap_or(*prompts.last().unwrap())
        };
        self.scroll.scroll_to_top_of_item(target);
        cx.notify();
    }

    fn go_to(&mut self, index: usize, cx: &mut Context<Self>) {
        self.prompt_list = false;
        self.scroll.scroll_to_top_of_item(index);
        cx.notify();
    }

    /// Turnos que se ven: los actuales, o una versión anterior.
    fn shown_turns(&self) -> Vec<&Turn> {
        match self.viewing.and_then(|(b, _)| self.branches.get(b)) {
            Some(branch) => {
                let cut = self.turns.iter().position(|t| t.user.as_ref().is_some_and(|u| u.n == branch.at)).unwrap_or(self.turns.len());
                self.turns[..cut].iter().chain(branch.turns.iter()).collect()
            }
            None => self.turns.iter().collect(),
        }
    }

    /// Versiones del mensaje `#n`: (actual, total), o `None` si no se editó.
    fn versions_of(&self, n: usize) -> Option<(usize, usize)> {
        let branches: Vec<usize> = self.branches.iter().enumerate().filter(|(_, b)| b.at == n).map(|(i, _)| i).collect();
        if branches.is_empty() {
            return None;
        }
        let total = branches.len() + 1;
        let current = match self.viewing {
            Some((b, _)) => branches.iter().position(|&x| x == b).map(|p| p + 1).unwrap_or(total),
            None => total,
        };
        Some((current, total))
    }

    fn switch_version(&mut self, n: usize, delta: i32, cx: &mut Context<Self>) {
        let branches: Vec<usize> = self.branches.iter().enumerate().filter(|(_, b)| b.at == n).map(|(i, _)| i).collect();
        let Some((current, total)) = self.versions_of(n) else { return };
        let next = (current as i32 + delta).clamp(1, total as i32) as usize;
        self.viewing = if next == total { None } else { Some((branches[next - 1], n)) };
        cx.notify();
    }

    // ── Sugerencias (@ archivos, / comandos) ────────────────────────────

    fn update_suggestions(&mut self, cx: &mut Context<Self>) {
        let text = self.input.read(cx).value().to_string();
        self.suggestions.clear();
        self.suggestion_ix = 0;
        if let Some(rest) = text.strip_prefix('/') {
            if !rest.contains(char::is_whitespace) {
                let q = rest.to_lowercase();
                self.suggestions = self
                    .commands
                    .iter()
                    .filter(|c| c.name.to_lowercase().starts_with(&q))
                    .take(8)
                    .cloned()
                    .map(Suggestion::Command)
                    .collect();
            }
        } else if let Some(query) = trailing_mention(&text) {
            if let Some(files) = &self.files {
                self.suggestions = rank_files(files, query, 8).into_iter().map(Suggestion::File).collect();
            }
        }
        cx.notify();
    }

    fn accept_suggestion(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(choice) = self.suggestions.get(self.suggestion_ix).cloned() else { return };
        let text = self.input.read(cx).value().to_string();
        let new = match choice {
            Suggestion::Command(c) => format!("/{} ", c.name),
            Suggestion::File(path) => {
                let cut = text.rfind('@').unwrap_or(text.len());
                format!("{}@{} ", &text[..cut], path)
            }
        };
        self.input.update(cx, |s, cx| s.set_value(new, window, cx));
        self.suggestions.clear();
        cx.notify();
    }

    fn on_key(&mut self, ev: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let key = ev.keystroke.key.as_str();
        let plain = !ev.keystroke.modifiers.modified();
        if !self.suggestions.is_empty() && plain {
            match key {
                "up" => self.suggestion_ix = self.suggestion_ix.saturating_sub(1),
                "down" => self.suggestion_ix = (self.suggestion_ix + 1).min(self.suggestions.len() - 1),
                "tab" => self.accept_suggestion(window, cx),
                "escape" => self.suggestions.clear(),
                _ => return,
            }
            cx.stop_propagation();
            cx.notify();
            return;
        }
        if key == "escape" && plain {
            if self.prompt_list {
                self.prompt_list = false;
            } else if self.editing.take().is_some() {
                self.input.update(cx, |s, cx| s.set_value("", window, cx));
            } else if self.status == ChatStatus::Running {
                self.stop(cx);
            }
            cx.notify();
        }
        // ↑ con la caja vacía recupera tu último mensaje.
        if key == "up" && plain && self.input.read(cx).value().is_empty() {
            let last = self.queue.pop().or_else(|| self.turns.iter().rev().find_map(|t| t.user.as_ref()).map(|u| u.text.clone()));
            if let Some(text) = last {
                self.input.update(cx, |s, cx| s.set_value(text, window, cx));
                cx.stop_propagation();
            }
        }
    }

    // ── Render ─────────────────────────────────────────────────────────

    fn render_turn(&self, ti: usize, turn: &Turn, live: bool, cx: &mut Context<Self>) -> AnyElement {
        let mut col = v_flex().gap_2().pb_4();
        if let Some(user) = &turn.user {
            col = col.child(self.user_card(user, live, cx));
        }
        let mut agent_started = false;
        for (ii, item) in turn.items.iter().enumerate() {
            let first_agent = !agent_started && matches!(item, Item::Agent(_) | Item::Tool(_) | Item::Thought(_));
            if first_agent {
                agent_started = true;
                col = col.child(
                    h_flex()
                        .gap_1p5()
                        .items_center()
                        .text_xs()
                        .text_color(style::muted())
                        .child(div().size(px(8.)).rounded_full().bg(style::output()))
                        .child(self.launch.label),
                );
            }
            col = col.child(self.render_item(ti, ii, item, live, cx));
        }
        if turn.duration.is_some() || !turn.files.is_empty() {
            let (added, removed) = turn.stats();
            let files = turn.files.len();
            let patch: String = turn
                .files
                .iter()
                .map(|(p, (old, new))| unified_patch(&FileDiff { path: p.to_string_lossy().into_owned(), old_text: old.clone(), new_text: new.clone() }))
                .collect::<Vec<_>>()
                .join("\n");
            let n = turn.user.as_ref().map(|u| u.n).unwrap_or(0);
            col = col.child(
                h_flex()
                    .pt_1()
                    .gap_2()
                    .items_center()
                    .border_t_1()
                    .border_color(style::border())
                    .text_xs()
                    .text_color(style::faint())
                    .when_some(turn.duration, |d, dur| d.child(format!("Turn · {}", fmt_duration(dur))))
                    .when(files > 0, |d| {
                        d.child(format!("· {files} file{}", if files == 1 { "" } else { "s" }))
                            .child(div().text_color(style::good()).child(format!("+{added}")))
                            .child(div().text_color(style::bad()).child(format!("−{removed}")))
                    })
                    .child(div().flex_1())
                    .when(files > 0, |d| {
                        d.child(
                            div()
                                .id(("review-turn", ti))
                                .text_color(style::accent())
                                .cursor_pointer()
                                .hover(|d| d.text_color(style::text()))
                                .child("Review changes")
                                .on_click(cx.listener(move |_, _, _, cx| {
                                    cx.emit(ChatEvent::OpenDiff { title: format!("Changes · message #{n}"), patch: patch.clone() });
                                })),
                        )
                    }),
            );
        }
        if live && !turn.files.is_empty() && turn.duration.is_some() {
            col = col.children(turn.files.iter().enumerate().map(|(fi, (path, (old, _)))| {
                let name = path.strip_prefix(&self.launch.cwd).unwrap_or(path).to_string_lossy().into_owned();
                let (path, old) = (path.clone(), old.clone());
                h_flex()
                    .pl_3()
                    .gap_2()
                    .text_xs()
                    .text_color(style::faint())
                    .child(crate::icons::ui("file", px(11.), style::faint()))
                    .child(div().flex_1().min_w_0().truncate().child(name.clone()))
                    .child(
                        div()
                            .id(("revert-file", ti * 1000 + fi))
                            .cursor_pointer()
                            .hover(|d| d.text_color(style::bad()))
                            .tooltip(|window, cx| Tooltip::new("Undo this turn's change to the file").build(window, cx))
                            .child("Revert")
                            .on_click(cx.listener(move |this, _, _, cx| this.revert_file(path.clone(), old.clone(), name.clone(), cx))),
                    )
            }));
        }
        col.into_any_element()
    }

    /// Deshace el cambio de un turno a un archivo (o lo borra si era nuevo).
    fn revert_file(&mut self, path: PathBuf, old: Option<String>, name: String, cx: &mut Context<Self>) {
        let result = match &old {
            Some(text) => std::fs::write(&path, text),
            None => std::fs::remove_file(&path),
        };
        let msg = match result {
            Ok(()) => format!("Reverted {name}."),
            Err(err) => format!("Could not revert {name}: {err}"),
        };
        self.current_turn().items.push(Item::Notice(msg));
        cx.notify();
    }

    fn user_card(&self, user: &UserMsg, live: bool, cx: &mut Context<Self>) -> AnyElement {
        let n = user.n;
        let time = jiff::Zoned::new(user.at, jiff::tz::TimeZone::system()).strftime("%H:%M").to_string();
        let versions = self.versions_of(n);
        let checkpoint = user.checkpoint.clone();
        let text = user.text.clone();
        let copy_text = user.text.clone();
        let small_button = |id: SharedString, icon: &'static str, tip: &'static str| {
            div()
                .id(id)
                .p_0p5()
                .rounded_sm()
                .cursor_pointer()
                .hover(|d| d.bg(style::panel_hover()))
                .tooltip(move |window, cx| Tooltip::new(tip).build(window, cx))
                .child(crate::icons::ui(icon, px(12.), style::muted()))
        };
        v_flex()
            .id(("user-card", n))
            .px_3()
            .py_2()
            .gap_1()
            .bg(style::accent_alpha(0.07))
            .border_1()
            .border_l_4()
            .border_color(style::accent_alpha(0.45))
            .child(
                h_flex()
                    .gap_2()
                    .items_center()
                    .text_xs()
                    .child(div().text_color(style::accent()).font_weight(FontWeight::MEDIUM).child("You"))
                    .child(div().text_color(style::muted()).child(format!("#{n} · {time}")))
                    .child(div().flex_1())
                    .when_some(versions, |d, (cur, total)| {
                        d.child(
                            h_flex()
                                .gap_1()
                                .items_center()
                                .text_color(style::muted())
                                .child(
                                    div()
                                        .id(("ver-prev", n))
                                        .px_1()
                                        .cursor_pointer()
                                        .hover(|d| d.text_color(style::text()))
                                        .child("‹")
                                        .on_click(cx.listener(move |this, _, _, cx| this.switch_version(n, -1, cx))),
                                )
                                .child(format!("{cur}/{total}"))
                                .child(
                                    div()
                                        .id(("ver-next", n))
                                        .px_1()
                                        .cursor_pointer()
                                        .hover(|d| d.text_color(style::text()))
                                        .child("›")
                                        .on_click(cx.listener(move |this, _, _, cx| this.switch_version(n, 1, cx))),
                                ),
                        )
                    })
                    .child(small_button(SharedString::from(format!("copy-{n}")), "copy", "Copy message").on_click(cx.listener(
                        move |_, _, _, cx| cx.write_to_clipboard(ClipboardItem::new_string(copy_text.clone())),
                    )))
                    .when(live, |d| {
                        d.child(
                            small_button(SharedString::from(format!("edit-{n}")), "pencil", "Edit and resend (keeps this version)")
                                .on_click(cx.listener(move |this, _, window, cx| this.edit(n, window, cx))),
                        )
                    })
                    .when_some(checkpoint.filter(|_| live), |d, sha| {
                        d.child(
                            small_button(SharedString::from(format!("restore-{n}")), "rotate-ccw", "Restore files to before this message")
                                .on_click(cx.listener(move |this, _, _, cx| this.restore(sha.clone(), n, cx))),
                        )
                    }),
            )
            .child(div().text_sm().whitespace_normal().child(text))
            .into_any_element()
    }

    fn render_item(&self, ti: usize, ii: usize, item: &Item, live: bool, cx: &mut Context<Self>) -> AnyElement {
        let key = ti * 10_000 + ii;
        match item {
            Item::Agent(text) => div().pl_3().text_sm().child(TextView::markdown(("agent-msg", key), text.clone())).into_any_element(),
            Item::Thought(text) => {
                let open = self.detail != Detail::Normal || self.expanded.contains(&(ti, ii));
                v_flex()
                    .pl_3()
                    .child(
                        h_flex()
                            .id(("thought", key))
                            .gap_1()
                            .items_center()
                            .text_xs()
                            .text_color(style::faint())
                            .cursor_pointer()
                            .child(crate::icons::ui(if open { "chevron-down" } else { "chevron-right" }, px(11.), style::faint()))
                            .child("Thinking")
                            .on_click(cx.listener(move |this, _, _, cx| {
                                if !this.expanded.remove(&(ti, ii)) {
                                    this.expanded.insert((ti, ii));
                                }
                                cx.notify();
                            })),
                    )
                    .when(open, |d| {
                        d.child(div().pl_4().border_l_1().border_color(style::border()).text_xs().italic().text_color(style::muted()).child(text.clone()))
                    })
                    .into_any_element()
            }
            Item::Notice(text) => div().pl_3().text_xs().italic().text_color(style::muted()).child(text.clone()).into_any_element(),
            Item::Tool(call) => self.render_tool(key, call, cx),
            Item::Permission { req, answer } => {
                let pending = answer.is_none() && live;
                match answer {
                    Some(a) => h_flex()
                        .pl_3()
                        .gap_2()
                        .text_xs()
                        .text_color(style::muted())
                        .child(crate::icons::ui("shield-check", px(12.), style::muted()))
                        .child(format!("{} — {a}", req.title))
                        .into_any_element(),
                    None => v_flex()
                        .gap_2()
                        .p_3()
                        .rounded_lg()
                        .border_1()
                        .border_color(style::soft(rgb(0xfbbf24), if pending { 0.7 } else { 0.3 }))
                        .bg(style::soft(rgb(0xfbbf24), 0.07))
                        .child(
                            h_flex()
                                .gap_2()
                                .items_center()
                                .text_sm()
                                .child(crate::icons::ui("triangle-alert", px(14.), rgb(0xfbbf24)))
                                .child(div().flex_1().child(req.title.clone())),
                        )
                        .when(pending, |d| {
                            d.child(h_flex().gap_2().flex_wrap().children(req.options.iter().enumerate().map(|(oi, o)| {
                                let allow = o.kind.starts_with("allow");
                                let choice = (o.id.clone(), o.name.clone());
                                div()
                                    .id(("perm", key * 16 + oi))
                                    .px_3()
                                    .py_1()
                                    .rounded_md()
                                    .text_sm()
                                    .cursor_pointer()
                                    .when(allow && oi == 0, |d| d.bg(style::accent()).text_color(style::bg()))
                                    .when(!(allow && oi == 0), |d| d.border_1().border_color(style::border()).hover(|d| d.bg(style::panel_hover())))
                                    .child(o.name.clone())
                                    .on_click(cx.listener(move |this, _, _, cx| this.answer(ti, ii, Some(choice.clone()), cx)))
                            })))
                        })
                        .into_any_element(),
                }
            }
        }
    }

    fn render_tool(&self, key: usize, call: &ToolCall, cx: &mut Context<Self>) -> AnyElement {
        let status = call.status.as_deref().unwrap_or("pending");
        let (status_icon, color): (&'static str, Hsla) = match status {
            "completed" => ("check", style::good().into()),
            "failed" => ("x", style::bad().into()),
            "in_progress" => ("loader", style::accent().into()),
            _ => ("circle-dashed", style::faint().into()),
        };
        let icon = match call.kind.as_deref() {
            Some("edit") => "pencil",
            Some("read") => "file-text",
            Some("execute") => "square-terminal",
            Some("search") => "search",
            Some("fetch") => "globe",
            Some("think") => "brain",
            Some("delete") => "trash",
            Some("move") => "folder-input",
            _ => "wrench",
        };
        let location = call.locations.first().cloned();
        let verbose = self.detail == Detail::Verbose;
        v_flex()
            .pl_3()
            .gap_0p5()
            .child(
                h_flex()
                    .id(("tool", key))
                    .gap_2()
                    .px_2()
                    .py_1()
                    .items_center()
                    .rounded_md()
                    .bg(style::panel())
                    .text_xs()
                    .text_color(style::muted())
                    .child(crate::icons::ui(icon, px(12.), style::muted()))
                    .child(div().flex_1().min_w_0().truncate().child(call.title.clone().unwrap_or_else(|| "Tool".into())))
                    .when_some(location.clone(), |d, loc| {
                        let path = PathBuf::from(&loc.path);
                        let line = loc.line.unwrap_or(1);
                        let label = format!("{}{}", path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default(), loc.line.map(|l| format!(":{l}")).unwrap_or_default());
                        d.child(
                            div()
                                .id(("tool-loc", key))
                                .flex_none()
                                .text_color(style::accent())
                                .cursor_pointer()
                                .hover(|d| d.text_color(style::text()))
                                .child(label)
                                .on_click(cx.listener(move |_, _, _, cx| cx.emit(ChatEvent::OpenFile { path: path.clone(), line }))),
                        )
                    })
                    .children(call.diffs.iter().take(1).map(|d| {
                        let (added, removed) = count_changes(d);
                        h_flex().flex_none().gap_1().child(div().text_color(style::good()).child(format!("+{added}"))).child(div().text_color(style::bad()).child(format!("−{removed}")))
                    }))
                    .child(crate::icons::ui(status_icon, px(12.), color)),
            )
            .children(call.diffs.iter().enumerate().map(|(di, d)| {
                let diff = d.clone();
                let file = d.path.rsplit('/').next().unwrap_or(&d.path).to_string();
                div()
                    .id(("tool-diff", key * 32 + di))
                    .pl_6()
                    .text_xs()
                    .text_color(style::accent())
                    .cursor_pointer()
                    .hover(|d| d.text_color(style::text()))
                    .child(format!("Open diff · {file}"))
                    .on_click(cx.listener(move |_, _, _, cx| {
                        cx.emit(ChatEvent::OpenDiff { title: format!("± {}", diff.path.rsplit('/').next().unwrap_or("")), patch: unified_patch(&diff) });
                    }))
            }))
            .when_some(call.text.clone().filter(|t| verbose && !t.trim().is_empty()), |d, t| {
                let short: String = t.lines().take(20).collect::<Vec<_>>().join("\n");
                d.child(
                    div()
                        .ml_6()
                        .p_2()
                        .rounded_md()
                        .bg(style::bg())
                        .text_xs()
                        .font_family(crate::style::mono_font())
                        .text_color(style::faint())
                        .child(short),
                )
            })
            .into_any_element()
    }

    fn header(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let (status_label, status_color): (String, Hsla) = match &self.status {
            _ if self.is_waiting_permission() => ("needs you".into(), rgb(0xfbbf24).into()),
            ChatStatus::Connecting => ("connecting…".into(), style::faint().into()),
            ChatStatus::Ready => ("ready".into(), style::good().into()),
            ChatStatus::Running => ("working…".into(), style::accent().into()),
            ChatStatus::Error(_) => ("error".into(), style::bad().into()),
            ChatStatus::Exited => ("closed".into(), style::faint().into()),
        };
        let order = |c: &str| match c {
            "mode" => 0,
            "model" => 1,
            "thought_level" => 2,
            _ => 3,
        };
        let mut config: Vec<&ConfigOption> = self.config.iter().filter(|o| !o.options.is_empty()).collect();
        config.sort_by_key(|o| order(&o.category));
        let title = self.title.clone().or_else(|| self.first_prompt().map(|p| crate::lines::short(p, 60))).unwrap_or_else(|| format!("New {} chat", self.launch.label));
        v_flex()
            .border_b_1()
            .border_color(style::border())
            .child(
                h_flex()
                    .px_4()
                    .py_2()
                    .gap_2()
                    .items_center()
                    .child(div().flex_1().min_w_0().truncate().text_sm().font_weight(FontWeight::MEDIUM).child(title))
                    .child(h_flex().gap_1().items_center().text_xs().text_color(status_color).child(div().size(px(6.)).rounded_full().bg(status_color)).child(status_label))
                    .children(config.into_iter().take(4).map(|opt| {
                        let value = opt.options.iter().find(|v| v.value == opt.current).map(|v| v.name.clone()).unwrap_or_else(|| opt.current.clone());
                        let current = if opt.category == "model" || opt.category == "mode" { value } else { format!("{}: {value}", opt.name) };
                        let id = opt.id.clone();
                        let open = self.open_config.as_deref() == Some(opt.id.as_str());
                        let name: SharedString = opt.name.clone().into();
                        div()
                            .id(SharedString::from(format!("cfg-{}", opt.id)))
                            .px_2()
                            .py_0p5()
                            .rounded_md()
                            .border_1()
                            .border_color(if open { style::accent().into() } else { style::border() })
                            .text_xs()
                            .text_color(style::muted())
                            .cursor_pointer()
                            .hover(|d| d.bg(style::panel_hover()))
                            .tooltip(move |window, cx| Tooltip::new(name.clone()).build(window, cx))
                            .child(format!("{current} ▾"))
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.open_config = if this.open_config.as_deref() == Some(id.as_str()) { None } else { Some(id.clone()) };
                                cx.notify();
                            }))
                    }))
                    .child(
                        div()
                            .id("detail-level")
                            .px_2()
                            .py_0p5()
                            .rounded_md()
                            .border_1()
                            .border_color(style::border())
                            .text_xs()
                            .text_color(style::muted())
                            .cursor_pointer()
                            .hover(|d| d.bg(style::panel_hover()))
                            .tooltip(|window, cx| Tooltip::new("Detail: Normal → Thinking → Verbose").build(window, cx))
                            .child(self.detail.label())
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.detail = this.detail.next();
                                cx.notify();
                            })),
                    ),
            )
            .when_some(self.open_config.as_ref().and_then(|id| self.config.iter().find(|o| &o.id == id)).cloned(), |d, opt| {
                d.child(
                    h_flex()
                        .px_4()
                        .pb_2()
                        .gap_1()
                        .flex_wrap()
                        .children(opt.options.iter().map(|v| {
                            let selected = v.value == opt.current;
                            let (id, value) = (opt.id.clone(), v.value.clone());
                            let tip: SharedString = v.description.clone().unwrap_or_else(|| v.name.clone()).into();
                            div()
                                .id(SharedString::from(format!("cfgv-{}-{}", opt.id, v.value)))
                                .px_2()
                                .py_0p5()
                                .rounded_md()
                                .text_xs()
                                .cursor_pointer()
                                .when(selected, |d| d.bg(style::accent_alpha(0.18)).text_color(style::accent()))
                                .when(!selected, |d| d.text_color(style::muted()).hover(|d| d.bg(style::panel_hover())))
                                .tooltip(move |window, cx| Tooltip::new(tip.clone()).build(window, cx))
                                .child(v.name.clone())
                                .on_click(cx.listener(move |this, _, _, cx| this.set_config(id.clone(), value.clone(), cx)))
                        })),
                )
            })
    }

    fn plan_panel(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        if self.plan.is_empty() {
            return None;
        }
        let done = self.plan.iter().filter(|e| e.status == "completed").count();
        let open = self.plan_open;
        Some(
            v_flex()
                .mx_4()
                .mt_2()
                .px_3()
                .py_2()
                .gap_1()
                .rounded_lg()
                .border_1()
                .border_color(style::border())
                .bg(style::panel())
                .child(
                    h_flex()
                        .id("plan-toggle")
                        .gap_1()
                        .items_center()
                        .text_xs()
                        .text_color(style::muted())
                        .cursor_pointer()
                        .child(crate::icons::ui(if open { "chevron-down" } else { "chevron-right" }, px(11.), style::muted()))
                        .child(format!("Plan · {done}/{}", self.plan.len()))
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.plan_open = !this.plan_open;
                            cx.notify();
                        })),
                )
                .when(open, |d| {
                    d.children(self.plan.iter().map(|e| {
                        let (icon, color): (&'static str, Hsla) = match e.status.as_str() {
                            "completed" => ("circle-check", style::good().into()),
                            "in_progress" => ("loader", style::accent().into()),
                            _ => ("circle", style::faint().into()),
                        };
                        h_flex()
                            .gap_2()
                            .items_center()
                            .text_xs()
                            .child(crate::icons::ui(icon, px(12.), color))
                            .child(div().when(e.status == "completed", |d| d.text_color(style::muted())).child(e.content.clone()))
                    }))
                })
                .into_any_element(),
        )
    }

    /// Riel con una marca por cada mensaje tuyo.
    fn prompt_rail(&self, turns: &[&Turn], cx: &mut Context<Self>) -> impl IntoElement {
        let first = self.scroll.bounds_for_item(0);
        let last = self.scroll.bounds_for_item(turns.len().saturating_sub(1));
        let total = match (first, last) {
            (Some(f), Some(l)) => f32::from(l.bottom() - f.top()).max(1.),
            _ => 1.,
        };
        let (top, _) = self.scroll.logical_scroll_top();
        let rail_h = f32::from(self.scroll.bounds().size.height).max(1.);
        div().w(px(14.)).h_full().flex_none().relative().children(turns.iter().enumerate().filter_map(|(i, t)| {
            let user = t.user.as_ref()?;
            let pos = match (first, self.scroll.bounds_for_item(i)) {
                (Some(f), Some(b)) => f32::from(b.top() - f.top()) / total,
                _ => i as f32 / turns.len().max(1) as f32,
            };
            let waiting = t.items.iter().any(|it| matches!(it, Item::Permission { answer: None, .. }));
            let current = i == top || (i < top && !turns[i + 1..=top.min(turns.len() - 1)].iter().any(|t| t.user.is_some()));
            let tip: SharedString = format!("#{} · {}", user.n, crate::lines::short(&user.text, 80)).into();
            Some(
                div()
                    .id(("rail", i))
                    .absolute()
                    .top(px((pos * (rail_h - 8.)).max(0.)))
                    .left(px(2.))
                    .w(px(if current { 12. } else { 9. }))
                    .h(px(3.))
                    .rounded_sm()
                    .cursor_pointer()
                    .bg(if waiting {
                        rgb(0xfbbf24).into()
                    } else if current {
                        style::accent().into()
                    } else {
                        style::soft(style::muted(), 0.6)
                    })
                    .tooltip(move |window, cx| Tooltip::new(tip.clone()).build(window, cx))
                    .on_click(cx.listener(move |this, _, _, cx| this.go_to(i, cx))),
            )
        }))
    }
}

impl Render for ChatView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let turns: Vec<Turn> = self.shown_turns().into_iter().cloned().collect();
        let live_upto = if self.viewing.is_some() { 0 } else { usize::MAX };
        let refs: Vec<&Turn> = turns.iter().collect();

        // Prompt fijo: el del turno que estás leyendo, si su tarjeta ya pasó.
        let (top, offset) = self.scroll.logical_scroll_top();
        let sticky = (offset < px(-36.))
            .then(|| (0..=top.min(turns.len().saturating_sub(1))).rev().find_map(|i| turns.get(i)?.user.as_ref().map(|u| (i, u.clone()))))
            .flatten();

        let conversation = div()
            .id("chat-scroll")
            .flex_1()
            .min_w_0()
            .overflow_y_scroll()
            .track_scroll(&self.scroll)
            .on_scroll(cx.listener(|_, _, _, cx| cx.notify()))
            .px_4()
            .pt_3()
            .when(turns.is_empty() && matches!(self.status, ChatStatus::Ready | ChatStatus::Connecting), |d| {
                d.child(
                    v_flex()
                        .pt_8()
                        .gap_1()
                        .items_center()
                        .text_color(style::faint())
                        .child(div().text_sm().child(format!("Start a conversation with {}", self.launch.label)))
                        .child(div().text_xs().child("Mention files with @, run agent commands with /, and pick mode or model above.")),
                )
            })
            .children(turns.iter().enumerate().map(|(i, t)| self.render_turn(i, t, i < live_upto, cx)));

        let body = match &self.status {
            ChatStatus::Error(e) if turns.is_empty() => div()
                .flex_1()
                .p_4()
                .child(
                    v_flex()
                        .gap_2()
                        .p_3()
                        .rounded_lg()
                        .border_1()
                        .border_color(style::soft(style::bad(), 0.5))
                        .child(div().text_sm().child(format!("Could not start {}", self.launch.label)))
                        .child(div().text_xs().text_color(style::muted()).whitespace_normal().child(e.clone())),
                )
                .into_any_element(),
            _ => div().flex().flex_row().flex_1().min_h_0().child(conversation).child(self.prompt_rail(&refs, cx)).into_any_element(),
        };

        let suggestions = (!self.suggestions.is_empty()).then(|| {
            v_flex()
                .mx_4()
                .mb_1()
                .p_1()
                .rounded_lg()
                .border_1()
                .border_color(style::border())
                .bg(style::panel())
                .children(self.suggestions.iter().enumerate().map(|(i, s)| {
                    let selected = i == self.suggestion_ix;
                    let (label, detail) = match s {
                        Suggestion::File(p) => (format!("@{p}"), String::new()),
                        Suggestion::Command(c) => (format!("/{}", c.name), c.description.clone()),
                    };
                    h_flex()
                        .id(("suggestion", i))
                        .px_2()
                        .py_1()
                        .gap_2()
                        .rounded_md()
                        .text_xs()
                        .cursor_pointer()
                        .when(selected, |d| d.bg(style::panel_hover()))
                        .child(div().font_family(crate::style::mono_font()).child(label))
                        .child(div().flex_1().truncate().text_color(style::faint()).child(detail))
                        .on_click(cx.listener(move |this, _, window, cx| {
                            this.suggestion_ix = i;
                            this.accept_suggestion(window, cx);
                        }))
                }))
        });

        let queue = (!self.queue.is_empty()).then(|| {
            v_flex().mx_4().mb_1().gap_0p5().children(self.queue.iter().enumerate().map(|(i, q)| {
                h_flex()
                    .gap_2()
                    .px_2()
                    .py_1()
                    .items_center()
                    .rounded_md()
                    .bg(style::panel())
                    .text_xs()
                    .child(crate::icons::ui("clock", px(12.), style::muted()))
                    .child(div().flex_1().min_w_0().truncate().text_color(style::muted()).child(format!("Queued: {q}")))
                    .child(
                        div()
                            .id(("queue-now", i))
                            .text_color(style::accent())
                            .cursor_pointer()
                            .child("Send now")
                            .on_click(cx.listener(move |this, _, _, cx| {
                                let text = this.queue.remove(i);
                                this.queue.insert(0, text);
                                this.stop(cx);
                            })),
                    )
                    .child(
                        div()
                            .id(("queue-edit", i))
                            .text_color(style::muted())
                            .cursor_pointer()
                            .child("Edit")
                            .on_click(cx.listener(move |this, _, window, cx| {
                                let text = this.queue.remove(i);
                                this.input.update(cx, |s, cx| s.set_value(text, window, cx));
                                cx.notify();
                            })),
                    )
                    .child(
                        div()
                            .id(("queue-drop", i))
                            .cursor_pointer()
                            .child(crate::icons::ui("x", px(11.), style::faint()))
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.queue.remove(i);
                                cx.notify();
                            })),
                    )
            }))
        });

        let running = self.status == ChatStatus::Running;
        let composer = v_flex()
            .px_4()
            .pb_3()
            .pt_1()
            .gap_1()
            .when_some(self.editing, |d, n| {
                d.child(
                    h_flex()
                        .gap_2()
                        .text_xs()
                        .text_color(style::accent())
                        .child(crate::icons::ui("pencil", px(11.), style::accent()))
                        .child(format!("Editing message #{n} — sending starts a new version (the current one is kept). Esc cancels."))
                )
            })
            .child(
                h_flex()
                    .gap_2()
                    .items_end()
                    .p_2()
                    .rounded_lg()
                    .border_1()
                    .border_color(style::border())
                    .bg(style::panel())
                    .child(div().flex_1().child(Textarea::new(&self.input)))
                    .child(
                        div()
                            .id("chat-send")
                            .px_3()
                            .py_1()
                            .rounded_md()
                            .text_sm()
                            .cursor_pointer()
                            .when(running, |d| d.border_1().border_color(style::border()).child("Stop").on_click(cx.listener(|this, _, _, cx| this.stop(cx))))
                            .when(!running, |d| d.bg(style::accent()).text_color(style::bg()).child("Send").on_click(cx.listener(|this, _, window, cx| this.send(window, cx)))),
                    ),
            )
            .child(
                h_flex()
                    .gap_3()
                    .text_xs()
                    .text_color(style::faint())
                    .child("⌥↑ ⌥↓ your messages")
                    .child("⌘⇧J list")
                    .child("↑ last message")
                    .child(div().flex_1())
                    .child(if running { "Esc stops · new messages queue" } else { "Enter sends" }),
            );

        let prompt_list = self.prompt_list.then(|| {
            let items: Vec<(usize, UserMsg)> = turns.iter().enumerate().filter_map(|(i, t)| t.user.clone().map(|u| (i, u))).collect();
            div()
                .id("prompt-list-backdrop")
                .absolute()
                .inset_0()
                .flex()
                .justify_center()
                .pt(px(48.))
                .bg(style::soft(style::bg(), 0.6))
                .on_click(cx.listener(|this, _, _, cx| {
                    this.prompt_list = false;
                    cx.notify();
                }))
                .child(
                    v_flex()
                        .id("prompt-list")
                        .w(px(520.))
                        .max_h(px(420.))
                        .overflow_y_scroll()
                        .p_2()
                        .rounded_lg()
                        .border_1()
                        .border_color(style::border())
                        .bg(style::panel())
                        .child(div().px_2().pb_1().text_xs().text_color(style::faint()).child("Your messages"))
                        .when(items.is_empty(), |d| d.child(div().px_2().text_xs().text_color(style::muted()).child("No messages yet.")))
                        .children(items.into_iter().map(|(i, u)| {
                            h_flex()
                                .id(("prompt-item", i))
                                .px_2()
                                .py_1()
                                .gap_2()
                                .rounded_md()
                                .cursor_pointer()
                                .hover(|d| d.bg(style::panel_hover()))
                                .child(div().w(px(28.)).flex_none().text_xs().text_color(style::accent()).child(format!("#{}", u.n)))
                                .child(div().flex_1().min_w_0().truncate().text_sm().child(crate::lines::short(&u.text, 120)))
                                .on_click(cx.listener(move |this, _, _, cx| this.go_to(i, cx)))
                        })),
                )
        });

        v_flex()
            .size_full()
            .relative()
            .key_context("ChatView")
            .track_focus(&self.focus)
            .on_action(cx.listener(|this, _: &PrevPrompt, _, cx| this.jump(-1, cx)))
            .on_action(cx.listener(|this, _: &NextPrompt, _, cx| this.jump(1, cx)))
            .on_action(cx.listener(|this, _: &TogglePromptList, _, cx| {
                this.prompt_list = !this.prompt_list;
                cx.notify();
            }))
            .capture_key_down(cx.listener(Self::on_key))
            .bg(style::bg())
            .child(self.header(cx))
            .when(self.viewing.is_some(), |d| {
                d.child(
                    h_flex()
                        .id("old-version")
                        .px_4()
                        .py_1()
                        .gap_2()
                        .text_xs()
                        .bg(style::soft(rgb(0xfbbf24), 0.08))
                        .text_color(rgb(0xfbbf24))
                        .child("You're viewing an earlier version (read-only).")
                        .child(
                            div()
                                .id("back-to-current")
                                .text_color(style::accent())
                                .cursor_pointer()
                                .child("Back to current")
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.viewing = None;
                                    cx.notify();
                                })),
                        ),
                )
            })
            .when_some(self.plan_panel(cx), |d, p| d.child(p))
            .when_some(sticky, |d, (i, user)| {
                d.child(
                    h_flex()
                        .id("sticky-prompt")
                        .mx_4()
                        .mt_1()
                        .px_3()
                        .py_1()
                        .gap_2()
                        .items_center()
                        .border_l_4()
                        .border_color(style::accent_alpha(0.45))
                        .bg(style::panel())
                        .text_xs()
                        .cursor_pointer()
                        .child(div().text_color(style::accent()).child(format!("#{}", user.n)))
                        .child(div().flex_1().min_w_0().truncate().text_color(style::muted()).child(crate::lines::short(&user.text, 160)))
                        .child(crate::icons::ui("arrow-up", px(11.), style::faint()))
                        .on_click(cx.listener(move |this, _, _, cx| this.go_to(i, cx))),
                )
            })
            .child(body)
            .when_some(queue, |d, q| d.child(q))
            .when_some(suggestions, |d, s| d.child(s))
            .child(composer)
            .when_some(prompt_list, |d, l| d.child(l))
    }
}

/// Archivos mencionados con `@` en un mensaje.
fn mentions(text: &str) -> Vec<String> {
    text.split_whitespace()
        .filter_map(|w| w.strip_prefix('@'))
        .map(|w| w.trim_end_matches([',', '.', ';', ':', ')', '?', '!']).to_string())
        .filter(|w| !w.is_empty())
        .collect()
}

/// `@consulta` al final del texto (lo que se está escribiendo).
fn trailing_mention(text: &str) -> Option<&str> {
    let start = text.rfind('@')?;
    let rest = &text[start + 1..];
    let before_ok = start == 0 || text[..start].ends_with(char::is_whitespace);
    (before_ok && !rest.contains(char::is_whitespace)).then_some(rest)
}

/// Los mejores archivos para una consulta: primero por nombre, luego por ruta.
fn rank_files(files: &[String], query: &str, limit: usize) -> Vec<String> {
    let q = query.to_lowercase();
    let mut scored: Vec<(u32, usize, &String)> = files
        .iter()
        .filter_map(|f| {
            let lower = f.to_lowercase();
            let name = lower.rsplit('/').next().unwrap_or(&lower);
            let score = if q.is_empty() {
                3
            } else if name.starts_with(&q) {
                0
            } else if name.contains(&q) {
                1
            } else if lower.contains(&q) {
                2
            } else {
                return None;
            };
            Some((score, f.len(), f))
        })
        .collect();
    scored.sort();
    scored.into_iter().take(limit).map(|(_, _, f)| f.clone()).collect()
}

/// Resumen en texto de la conversación (para empezar una versión nueva).
fn transcript(turns: &[Turn], max_chars: usize) -> String {
    let mut out = String::new();
    for t in turns {
        if let Some(u) = &t.user {
            out.push_str(&format!("User: {}\n", u.text));
        }
        let answer: String = t.items.iter().filter_map(|i| if let Item::Agent(s) = i { Some(s.as_str()) } else { None }).collect::<Vec<_>>().join("");
        if !answer.trim().is_empty() {
            out.push_str(&format!("Assistant: {}\n", answer.trim()));
        }
    }
    if out.chars().count() > max_chars {
        let skip = out.chars().count() - max_chars;
        out = format!("…{}", out.chars().skip(skip).collect::<String>());
    }
    out
}

/// El mensaje que ve el usuario cuando se le antepuso contexto.
fn text_only(full: &str) -> String {
    full.rsplit_once("\n\n---\n\n").map(|(_, t)| t.to_string()).unwrap_or_else(|| full.to_string())
}

fn fmt_duration(d: Duration) -> String {
    let s = d.as_secs();
    if s < 60 { format!("{s}s") } else { format!("{}m {:02}s", s / 60, s % 60) }
}

fn count_changes(d: &FileDiff) -> (usize, usize) {
    let old = d.old_text.as_deref().unwrap_or("");
    let diff = similar::TextDiff::from_lines(old, &d.new_text);
    let mut added = 0;
    let mut removed = 0;
    for change in diff.iter_all_changes() {
        match change.tag() {
            similar::ChangeTag::Insert => added += 1,
            similar::ChangeTag::Delete => removed += 1,
            similar::ChangeTag::Equal => {}
        }
    }
    (added, removed)
}

/// Patch unificado (estilo git) a partir del texto viejo y nuevo de un archivo.
pub fn unified_patch(d: &FileDiff) -> String {
    let old = d.old_text.as_deref().unwrap_or("");
    let path = d.path.trim_start_matches('/');
    let from = if d.old_text.is_some() { format!("a/{path}") } else { "/dev/null".to_string() };
    let body = similar::TextDiff::from_lines(old, &d.new_text)
        .unified_diff()
        .context_radius(3)
        .header(&from, &format!("b/{path}"))
        .to_string();
    format!("diff --git a/{path} b/{path}\n{body}")
}

#[cfg(test)]
mod tests {
    use super::{FileDiff, count_changes, mentions, rank_files, text_only, trailing_mention, unified_patch};

    #[test]
    fn builds_patch_parsed_by_diff_view() {
        let d = FileDiff { path: "/tmp/a.txt".into(), old_text: Some("uno\ndos\n".into()), new_text: "uno\ntres\n".into() };
        let patch = unified_patch(&d);
        assert!(patch.contains("-dos") && patch.contains("+tres"));
        assert_eq!(count_changes(&d), (1, 1));
        let files = gpui_kit::component::diff::DiffFile::parse(&patch).unwrap();
        assert_eq!(files.len(), 1);
    }

    #[test]
    fn mentions_and_suggestions() {
        assert_eq!(mentions("revisa @src/main.rs, y @Cargo.toml."), ["src/main.rs", "Cargo.toml"]);
        assert_eq!(trailing_mention("mira @src/ma"), Some("src/ma"));
        assert_eq!(trailing_mention("correo a@b.com"), None);
        assert_eq!(trailing_mention("@ y luego"), None);
        let files: Vec<String> = ["docs/main.md", "src/main.rs", "src/lib.rs"].iter().map(|s| s.to_string()).collect();
        assert_eq!(rank_files(&files, "main", 5), ["src/main.rs", "docs/main.md"]);
        assert_eq!(rank_files(&files, "lib", 5), ["src/lib.rs"]);
    }

    #[test]
    fn edited_message_hides_the_added_context() {
        assert_eq!(text_only("Context…\nUser: a\n\n---\n\nnuevo"), "nuevo");
        assert_eq!(text_only("solo"), "solo");
    }
}
