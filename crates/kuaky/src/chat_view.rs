//! Chat propio con agentes por ACP (Claude Code, Antigravity, Codex…).
//!
//! Pensado para conversaciones largas:
//! - tus mensajes van en burbuja a la derecha (como la app de Claude) y las
//!   herramientas se agrupan en una línea ("Ran 5 commands ›");
//! - mientras el agente trabaja, una línea viva al final dice qué hace y cuánto lleva;
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
use kuaky_usage::{Agent, AgentLimits, RateWindow};
use std::{
    collections::{BTreeMap, HashMap, HashSet},
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
    /// Servidores MCP para la sesión (las herramientas de kuaky).
    pub tools: Vec<kuaky_acp::McpServer>,
    /// Se abre sin tomar el foco (la abrió otro agente mientras escribes).
    pub background: bool,
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
    /// Revisar los cambios de un turno: (ruta, antes, después).
    OpenReview { title: String, files: Vec<(PathBuf, Option<String>, String)> },
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
    /// Encender/apagar las rondas automáticas con la sesión conectada.
    RelayAuto(bool),
    /// Modo seguir: el agente está leyendo o editando este archivo.
    Follow { path: PathBuf, line: u32 },
    OpenPr(u64),
    /// Merge (o resolver y merge) del PR de esta sesión.
    MergePr(u64),
}

/// Conexión con otra sesión (revisor ↔ autor), mostrada arriba del chat.
#[derive(Clone)]
pub struct RelayBanner {
    /// "Reviewing for Claude Code chat" / "Reviewed by Antigravity".
    pub text: String,
    pub auto: bool,
    pub rounds_left: u8,
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
    // SharedString: el render pasa el texto a TextView en cada frame sin copiarlo.
    Agent(SharedString),
    Thought(SharedString),
    Tool(ToolCall),
    Permission { req: PermissionRequest, answer: Option<String> },
    Notice(String),
}

/// Imagen adjunta a un mensaje (pegada o elegida).
#[derive(Clone)]
struct Attachment {
    data: String,
    mime: String,
    image: Arc<Image>,
}

impl Attachment {
    /// El original solo viaja en base64 al agente; en pantalla queda una miniatura.
    fn new(format: ImageFormat, bytes: Vec<u8>) -> Attachment {
        use base64::Engine as _;
        let data = base64::engine::general_purpose::STANDARD.encode(&bytes);
        let mime = format.mime_type().to_string();
        let image = thumbnail(format, &bytes).unwrap_or_else(|| Image::from_bytes(format, bytes));
        Attachment { data, mime, image: Arc::new(image) }
    }
}

/// Lado mayor de la miniatura: el chat pinta las imágenes a 240×160 como
/// mucho (el doble para pantallas retina).
const THUMB_MAX: (u32, u32) = (480, 320);

/// Copia chica para mostrar. Decodificada, una captura retina ocupa
/// ancho × alto × 4 bytes (13 MB una de 2230×1478) mientras viva el chat.
/// `None` si ya es chica, si es un GIF (perdería la animación) o no se lee.
fn thumbnail(format: ImageFormat, bytes: &[u8]) -> Option<Image> {
    if matches!(format, ImageFormat::Gif) {
        return None;
    }
    let decoded = image::load_from_memory(bytes).ok()?;
    if decoded.width() <= THUMB_MAX.0 && decoded.height() <= THUMB_MAX.1 {
        return None;
    }
    let small = decoded.thumbnail(THUMB_MAX.0, THUMB_MAX.1);
    let mut png = Vec::new();
    small.write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png).ok()?;
    Some(Image::from_bytes(ImageFormat::Png, png))
}

/// Mensaje en cola mientras el agente trabaja.
#[derive(Clone)]
struct Queued {
    text: String,
    images: Vec<Attachment>,
}

#[derive(Clone)]
struct UserMsg {
    text: String,
    images: Vec<Arc<Image>>,
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
        self.files.values().fold((0, 0), |(a, r), (old, new)| {
            let (x, y) = count_text_changes(old.as_deref(), new);
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
    pub relay: Option<RelayBanner>,
    /// Seguir al agente: kuaky abre los archivos que lee o edita.
    pub follow: bool,
    /// PRs que el agente enlazó en sus respuestas (`…/pull/N`).
    pub mentioned_prs: Vec<u64>,
    /// PR de esta sesión: (número, tiene conflictos). Lo pone el proyecto.
    pub pr: Option<(u64, bool)>,
    /// Cuándo empezó y terminó cada subagente (por `toolCallId`).
    subagent_times: HashMap<String, (jiff::Timestamp, Option<jiff::Timestamp>)>,
    last_followed: Option<(String, Option<u32>)>,
    client: Option<Arc<Client>>,
    turns: Vec<Turn>,
    branches: Vec<Branch>,
    /// Versión que se está viendo de cada punto de edición (`None` = la actual).
    viewing: Option<(usize, usize)>,
    plan: Vec<PlanEntry>,
    plan_open: bool,
    config: Vec<ConfigOption>,
    open_config: Option<String>,
    /// Lista cerrada al pulsar fuera (para que pulsar su propio botón no la reabra).
    config_closed: Option<(String, Instant)>,
    /// Subagentes con sus pasos desplegados (id de la herramienta).
    open_steps: HashSet<String>,
    commands: Vec<SlashCommand>,
    queue: Vec<Queued>,
    /// Imágenes para el próximo mensaje.
    attachments: Vec<Attachment>,
    /// Contexto usado: (tokens, ventana, costo acumulado).
    usage: Option<(u64, u64, Option<f64>)>,
    /// Límites del plan (5 h, semana, Fable): lo último que llegó o se leyó de disco.
    limits: Option<AgentLimits>,
    /// Panel de uso abierto (se abre al pulsar el anillo de contexto).
    usage_open: bool,
    /// Panel cerrado al pulsar fuera (para que pulsar el anillo no lo reabra).
    usage_closed: Option<Instant>,
    /// Último evento del agente (para "no output for 40s").
    last_event: Instant,
    detail: Detail,
    expanded: HashSet<(usize, usize)>,
    editing: Option<usize>,
    prompt_list: bool,
    files: Option<Arc<Vec<String>>>,
    suggestions: Vec<Suggestion>,
    suggestion_ix: usize,
    input: Entity<TextareaState>,
    scroll: ScrollHandle,
    /// Seguir el final mientras llega texto. Lo apaga subir con la rueda;
    /// lo enciende volver al final, "Jump to latest" o mandar un mensaje.
    stick_bottom: bool,
    /// Reconexiones recientes (para no reintentar en bucle).
    reconnects: Vec<Instant>,
    /// Posición del scroll la última vez que llegaron eventos.
    last_offset: Pixels,
    /// Proceso del agente cerrado por inactividad: se vuelve a abrir solo al
    /// mandar el próximo mensaje (cada agente ocupa 50–250 MB).
    asleep: bool,
    /// Conexión vigente: los eventos de un proceso anterior se ignoran.
    conn: u64,
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
                .placeholder(format!("Message {label}…  (@ files, / commands)"))
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
        if !launch.background {
            input.update(cx, |s, cx| s.focus(window, cx));
        }
        #[allow(unused_mut)]
        let mut this = Self {
            launch,
            status: ChatStatus::Connecting,
            uid: new_uid(),
            session: None,
            title: None,
            relay: None,
            follow: false,
            mentioned_prs: Vec::new(),
            pr: None,
            subagent_times: HashMap::new(),
            last_followed: None,
            client: None,
            turns: Vec::new(),
            branches: Vec::new(),
            viewing: None,
            plan: Vec::new(),
            plan_open: true,
            config: Vec::new(),
            open_config: None,
            config_closed: None,
            open_steps: HashSet::new(),
            commands: Vec::new(),
            queue: Vec::new(),
            attachments: Vec::new(),
            usage: None,
            limits: None,
            usage_open: false,
            usage_closed: None,
            last_event: Instant::now(),
            detail: Detail::Normal,
            expanded: HashSet::new(),
            editing: None,
            prompt_list: false,
            files: None,
            suggestions: Vec::new(),
            suggestion_ix: 0,
            input,
            scroll: ScrollHandle::new(),
            stick_bottom: true,
            reconnects: Vec::new(),
            last_offset: px(0.),
            asleep: false,
            conn: 0,
            focus: cx.focus_handle(),
            _sub: sub,
        };
        #[cfg(feature = "screenshot")]
        {
            this.follow = std::env::var("KUAKY_DEMO_FOLLOW").is_ok();
            this.usage_open = std::env::var("KUAKY_DEMO_USAGE").is_ok();
        }
        this.connect(cx);
        this.load_files(cx);
        this.watch_idle(cx);
        this
    }

    /// Revisa cada minuto si el agente lleva [`IDLE_SLEEP`] sin hacer nada.
    fn watch_idle(&self, cx: &mut Context<Self>) {
        cx.spawn(async move |this, cx| loop {
            cx.background_executor().timer(Duration::from_secs(60)).await;
            if this.update(cx, |this, cx| this.sleep_if_idle(cx)).is_err() {
                break;
            }
        })
        .detach();
    }

    /// Cierra el proceso del agente si está esperando sin nada pendiente.
    /// La conversación queda en pantalla; el próximo mensaje la retoma.
    fn sleep_if_idle(&mut self, cx: &mut Context<Self>) {
        let idle = !self.asleep
            && self.status == ChatStatus::Ready
            && self.client.is_some()
            && self.session.is_some()
            && self.queue.is_empty()
            && !self.is_waiting_permission()
            && self.last_event.elapsed() >= IDLE_SLEEP;
        if !idle {
            return;
        }
        tracing::info!(agent = self.launch.label, "chat inactivo: se cierra el proceso del agente");
        self.asleep = true;
        // El `Exited` que manda el proceso al morir es de una conexión vieja.
        self.conn += 1;
        self.client = None;
        cx.notify();
    }

    /// Vuelve a abrir un agente dormido, sin avisos en la conversación.
    fn wake(&mut self, cx: &mut Context<Self>) {
        if !self.asleep || self.status == ChatStatus::Connecting {
            return;
        }
        self.status = ChatStatus::Connecting;
        cx.emit(ChatEvent::Status);
        cx.notify();
        self.connect_with(true, cx);
    }

    pub fn is_waiting_permission(&self) -> bool {
        self.turns.iter().flat_map(|t| &t.items).any(|i| matches!(i, Item::Permission { answer: None, .. }))
    }

    /// Último texto de la conversación (para el título automático).
    pub fn first_prompt(&self) -> Option<&str> {
        self.turns.iter().find_map(|t| t.user.as_ref()).map(|u| u.text.as_str())
    }

    fn connect(&self, cx: &mut Context<Self>) {
        self.connect_with(false, cx);
    }

    /// Vuelve a abrir el agente tras morir su proceso: retoma la misma conversación
    /// (`session/load`) sin repetir en pantalla lo que ya se ve.
    fn reconnect(&mut self, cx: &mut Context<Self>) {
        if self.status == ChatStatus::Connecting {
            return;
        }
        self.client = None;
        self.status = ChatStatus::Connecting;
        self.reconnects.push(Instant::now());
        cx.emit(ChatEvent::Status);
        cx.notify();
        self.connect_with(true, cx);
    }

    fn connect_with(&self, again: bool, cx: &mut Context<Self>) {
        let mut launch = self.launch.clone();
        if again {
            launch.resume = self.session.clone().or(launch.resume);
        }
        // Preferencias de la última vez (solo para sesiones nuevas): modelo primero,
        // porque las demás opciones (esfuerzo…) pueden depender de él.
        let mut prefs: Vec<(String, String)> = if again {
            // Al reconectar, lo elegido en esta conversación (modelo, esfuerzo…).
            self.config.iter().map(|o| (o.id.clone(), o.current.clone())).collect()
        } else if launch.resume.is_none() {
            crate::state::get(cx).settings.chat_prefs.get(launch.label).map(|m| m.iter().map(|(k, v)| (k.clone(), v.clone())).collect()).unwrap_or_default()
        } else {
            Vec::new()
        };
        prefs.sort_by_key(|(id, _)| (id != "model", id.clone()));
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move {
                    let env = vec![("PATH".to_string(), kuaky_git::login_path().to_string_lossy().into_owned())];
                    let client = Arc::new(Client::spawn(&launch.program, &launch.args, &launch.cwd, &env)?);
                    client.set_mcp_servers(launch.tools.clone());
                    let resumed = launch.resume.is_some() && client.capabilities.load_session;
                    let mut info = match &launch.resume {
                        Some(id) if client.capabilities.load_session => client.load_session(&launch.cwd, id)?,
                        _ => client.new_session_info(&launch.cwd)?,
                    };
                    if again {
                        // Lo que repite `session/load` ya está en pantalla.
                        client.events().drain();
                    }
                    for (id, value) in &prefs {
                        let applies = info.config.iter().any(|o| &o.id == id && &o.current != value && o.options.iter().any(|v| &v.value == value));
                        if !applies {
                            continue;
                        }
                        match client.set_config_option(&info.id, id, value) {
                            Ok(opts) if !opts.is_empty() => info.config = opts,
                            Ok(_) => {
                                for o in info.config.iter_mut().filter(|o| &o.id == id) {
                                    o.current = value.clone();
                                }
                            }
                            Err(_) => {}
                        }
                    }
                    anyhow::Ok((client, info, resumed))
                })
                .await;
            let (client, info, resumed) = match result {
                Ok(ok) => ok,
                Err(err) => {
                    this.update(cx, |this, cx| {
                        let text = format!("{err:#}");
                        let hint = if text.to_lowercase().contains("auth") {
                            format!("{text}\n\nSign in with the agent's own CLI in a terminal (for example `claude` → /login or `agy`), then reopen this chat.")
                        } else {
                            text
                        };
                        if again {
                            this.asleep = false;
                            this.current_turn().items.push(Item::Notice(format!("Could not reconnect: {hint}")));
                            this.status = ChatStatus::Exited;
                        } else {
                            this.status = ChatStatus::Error(hint);
                        }
                        cx.emit(ChatEvent::Status);
                        cx.notify();
                    })
                    .ok();
                    return;
                }
            };
            let events = client.events();
            let conn = this
                .update(cx, |this, cx| {
                    if again && !this.asleep {
                        this.current_turn().items.push(Item::Notice(if resumed {
                            "Reconnected to the agent; the conversation continues.".into()
                        } else {
                            "Reconnected in a new session: this agent can't resume, so it doesn't remember the messages above.".into()
                        }));
                    }
                    this.asleep = false;
                    this.conn += 1;
                    this.session = Some(info.id.clone());
                    this.config = info.config;
                    this.client = Some(client);
                    this.status = ChatStatus::Ready;
                    // Lo repetido por `session/load` no cuenta como turno en curso.
                    for t in &mut this.turns {
                        t.started = None;
                    }
                    // Mensajes que llegaron mientras conectaba.
                    if !this.queue.is_empty() {
                        let next = this.queue.remove(0);
                        this.dispatch(next.text, next.images, cx);
                    }
                    cx.emit(ChatEvent::Session(info.id));
                    cx.emit(ChatEvent::Status);
                    #[cfg(feature = "screenshot")]
                    if let Ok(id) = std::env::var("KUAKY_DEMO_CONFIG") {
                        this.open_config = Some(id);
                    }
                    #[cfg(feature = "screenshot")]
                    if let Some(prompt) = std::env::var("KUAKY_DEMO_CHAT_PROMPT").ok().filter(|_| !again) {
                        this.dispatch(prompt, Vec::new(), cx);
                    }
                    cx.notify();
                    this.conn
                })
                .ok();
            let Some(conn) = conn else { return };
            while let Ok(first) = events.recv_async().await {
                let mut batch = vec![first];
                batch.extend(events.try_iter());
                let alive = this
                    .update(cx, |this, cx| {
                        // Proceso cerrado (dormido o reemplazado): sus eventos ya no cuentan.
                        if this.conn != conn {
                            return false;
                        }
                        this.check_manual_scroll();
                        for ev in batch {
                            this.on_event(ev, cx);
                        }
                        if this.stick_bottom {
                            this.scroll.scroll_to_bottom();
                        }
                        cx.notify();
                        true
                    })
                    .unwrap_or(false);
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

    /// Arrastrar la barra de scroll no manda eventos de rueda: si el chat subió
    /// desde la última vez (seguir el final solo lo baja), fuiste tú.
    /// Llegar abajo del todo vuelve a seguir el final.
    fn check_manual_scroll(&mut self) {
        let offset = self.scroll.offset().y;
        let max = self.scroll.max_offset().y;
        if self.stick_bottom && offset > self.last_offset + px(2.) && max + offset > px(8.) {
            self.stick_bottom = false;
        } else if !self.stick_bottom && max + offset <= px(2.) && offset < self.last_offset - px(2.) {
            self.stick_bottom = true;
        }
        self.last_offset = offset;
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
        self.last_event = Instant::now();
        match ev {
            Event::Usage { used, size, cost, rate_limit } => {
                if size > 0 {
                    self.usage = Some((used, size, cost.or(self.usage.and_then(|u| u.2))));
                }
                let now = Some(jiff::Timestamp::now());
                if let Some(limits) = rate_limit.and_then(|v| kuaky_usage::limits::parse_claude_rate_info(&v, now)) {
                    self.keep_limits(limits, cx);
                }
            }
            Event::UserMessageChunk(t) => match self.turns.last_mut() {
                // Al retomar, los mensajes llegan en trozos: se juntan.
                Some(turn) if turn.user.is_some() && turn.items.is_empty() => {
                    if let Some(u) = &mut turn.user {
                        u.text.push_str(&t);
                    }
                }
                _ => {
                    let n = self.turns.iter().filter(|t| t.user.is_some()).count() + 1;
                    let mut turn = Turn::new(Some(UserMsg { text: t, images: Vec::new(), n, at: jiff::Timestamp::now(), checkpoint: None }));
                    turn.started = None;
                    self.turns.push(turn);
                }
            },
            Event::MessageChunk(t) => match self.current_turn().items.last_mut() {
                Some(Item::Agent(s)) => append(s, &t),
                _ => self.current_turn().items.push(Item::Agent(t.into())),
            },
            Event::ThoughtChunk(t) => match self.current_turn().items.last_mut() {
                Some(Item::Thought(s)) => append(s, &t),
                _ => self.current_turn().items.push(Item::Thought(t.into())),
            },
            Event::ToolCall(call) => {
                let turn = self.current_turn();
                for d in &call.diffs {
                    turn.record_file(PathBuf::from(&d.path), d.old_text.clone(), d.new_text.clone());
                }
                let id = call.id.clone();
                let mut call = call;
                if let Some(delta) = call.output_delta.take() {
                    call.output.get_or_insert_with(String::new).push_str(&delta);
                }
                turn.items.push(Item::Tool(call));
                self.track_subagent(&id);
                self.follow_tool(&id, cx);
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
                        if update.tool_name.is_some() {
                            call.tool_name = update.tool_name;
                        }
                        if update.subagent.is_some() {
                            call.subagent = update.subagent;
                        }
                        // Algunas actualizaciones pierden el padre: se conserva el primero.
                        if call.parent.is_none() {
                            call.parent = update.parent;
                        }
                        // Salida del comando: completa o por trozos.
                        if let Some(out) = update.output {
                            call.output = Some(out);
                        }
                        if let Some(delta) = update.output_delta {
                            let out = call.output.get_or_insert_with(String::new);
                            out.push_str(&delta);
                            // Tope de memoria: se queda con lo último.
                            if out.len() > 256 * 1024 {
                                let cut = out.len() - 192 * 1024;
                                let cut = (cut..out.len()).find(|&i| out.is_char_boundary(i)).unwrap_or(out.len());
                                out.drain(..cut);
                            }
                        }
                        if update.exit_code.is_some() {
                            call.exit_code = update.exit_code;
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
                    None => turn.items.push(Item::Tool(update.clone())),
                }
                self.track_subagent(&update.id);
                self.follow_tool(&update.id, cx);
            }
            Event::FileWritten { path, old_text, new_text } => self.current_turn().record_file(path, old_text, new_text),
            Event::Plan(entries) => {
                self.plan = entries.clone();
                cx.emit(ChatEvent::Plan(entries));
            }
            Event::Permission(req) => {
                cx.emit(ChatEvent::NeedsYou(req.title.clone()));
                #[cfg(feature = "screenshot")]
                let auto = std::env::var("KUAKY_DEMO_ALLOW").is_ok().then(|| req.options.first().map(|o| (o.id.clone(), o.name.clone()))).flatten();
                self.current_turn().items.push(Item::Permission { req, answer: None });
                #[cfg(feature = "screenshot")]
                if let Some(choice) = auto {
                    let (ti, ii) = (self.turns.len() - 1, self.turns.last().map(|t| t.items.len() - 1).unwrap_or(0));
                    self.answer(ti, ii, Some(choice), cx);
                }
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
                self.scan_prs();
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
                #[cfg(feature = "screenshot")]
                if std::env::var("KUAKY_DEMO_REVIEW").is_ok() {
                    if let Some(turn) = self.turns.last() {
                        let files = turn.files.iter().map(|(p, (o, n))| (p.clone(), o.clone(), n.clone())).collect();
                        cx.emit(ChatEvent::OpenReview { title: "Review · message #1".into(), files });
                    }
                }
                if !self.queue.is_empty() {
                    let next = self.queue.remove(0);
                    self.dispatch(next.text, next.images, cx);
                }
            }
            Event::Error(e) => {
                // Si el agente ya se cayó, este error es solo el turno que quedó cortado.
                if self.status != ChatStatus::Exited && self.status != ChatStatus::Connecting {
                    self.status = ChatStatus::Ready;
                    self.current_turn().items.push(Item::Notice(format!("Error: {e}")));
                }
                cx.emit(ChatEvent::Status);
            }
            Event::Exited => {
                let was_running = self.status == ChatStatus::Running;
                self.status = ChatStatus::Exited;
                self.client = None;
                if let Some(turn) = self.turns.last_mut() {
                    if was_running {
                        turn.duration = turn.started.map(|s| s.elapsed());
                    }
                }
                self.current_turn().items.push(Item::Notice(if was_running {
                    "The agent process stopped in the middle of the turn.".into()
                } else {
                    "The agent process stopped.".into()
                }));
                cx.emit(ChatEvent::Status);
                // Un reintento solo (no más de 2 en 5 minutos, por si se cae en bucle).
                self.reconnects.retain(|t| t.elapsed() < Duration::from_secs(300));
                if self.reconnects.len() < 2 && !crate::app::quitting() {
                    cx.spawn(async move |this, cx| {
                        cx.background_executor().timer(Duration::from_secs(1)).await;
                        let _ = this.update(cx, |this, cx| {
                            if this.status == ChatStatus::Exited {
                                this.reconnect(cx);
                            }
                        });
                    })
                    .detach();
                }
            }
        }
    }

    // ── Enviar ─────────────────────────────────────────────────────────

    fn send(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let text = self.input.read(cx).value().trim().to_string();
        if text.is_empty() && self.attachments.is_empty() {
            return;
        }
        self.input.update(cx, |s, cx| s.set_value("", window, cx));
        self.suggestions.clear();
        let images = std::mem::take(&mut self.attachments);
        if let Some(at) = self.editing.take() {
            self.fork_at(at, text, cx);
            return;
        }
        match self.status {
            ChatStatus::Ready => self.dispatch(text, images, cx),
            ChatStatus::Running | ChatStatus::Connecting => {
                self.queue.push(Queued { text, images });
                cx.notify();
            }
            _ => {}
        }
    }

    fn find_tool(&self, id: &str) -> Option<&ToolCall> {
        self.turns.iter().rev().flat_map(|t| t.items.iter().rev()).find_map(|i| match i {
            Item::Tool(c) if c.id == id => Some(c),
            _ => None,
        })
    }

    /// Anota cuándo empezó/terminó un subagente.
    fn track_subagent(&mut self, id: &str) {
        let Some(call) = self.find_tool(id).filter(|c| c.is_subagent()) else { return };
        let finished = matches!(call.status.as_deref(), Some("completed" | "failed"));
        let now = jiff::Timestamp::now();
        let entry = self.subagent_times.entry(id.to_string()).or_insert((now, None));
        if finished && entry.1.is_none() {
            entry.1 = Some(now);
        }
    }

    /// Subagentes de esta conversación (los de la sesión retomada no tienen hora: se omiten).
    pub fn subagents(&self) -> Vec<crate::agent_tasks::Subagent> {
        self.turns
            .iter()
            .flat_map(|t| t.items.iter().map(move |i| (t, i)))
            .filter_map(|(turn, i)| match i {
                Item::Tool(c) if c.is_subagent() => {
                    let (started, done) = *self.subagent_times.get(&c.id)?;
                    let (kind, what) = c.subagent.clone().unwrap_or_else(|| ("Agent".into(), c.title.clone().unwrap_or_default()));
                    let doing = done.is_none().then(|| steps_of(turn, &c.id).last().and_then(|s| s.title.clone())).flatten().map(|t| crate::lines::short(&t, 80));
                    Some(crate::agent_tasks::Subagent {
                        id: c.id.clone(),
                        kind,
                        what,
                        started,
                        done,
                        failed: c.status.as_deref() == Some("failed"),
                        doing,
                    })
                }
                _ => None,
            })
            .collect()
    }

    /// Salta a una herramienta del chat (p. ej. un subagente) y abre su grupo.
    pub fn reveal_tool(&mut self, id: &str, cx: &mut Context<Self>) {
        self.viewing = None;
        let Some(ti) = self.turns.iter().position(|t| t.items.iter().any(|i| matches!(i, Item::Tool(c) if c.id == id))) else { return };
        for ii in 0..self.turns[ti].items.len() {
            if matches!(self.turns[ti].items[ii], Item::Tool(_)) {
                self.expanded.insert((ti, ii));
            }
        }
        self.stick_bottom = false;
        self.scroll.scroll_to_top_of_item(ti);
        cx.notify();
    }

    /// Enlaces a PRs en las respuestas del agente.
    fn scan_prs(&mut self) {
        for turn in &self.turns {
            for item in &turn.items {
                if let Item::Agent(text) = item {
                    for n in crate::project::pr_numbers_in(text) {
                        self.mentioned_prs.retain(|m| *m != n);
                        self.mentioned_prs.push(n);
                    }
                }
            }
        }
    }

    /// Con "seguir" encendido, avisa del archivo que toca la herramienta (lectura o edición).
    fn follow_tool(&mut self, id: &str, cx: &mut Context<Self>) {
        if !self.follow {
            return;
        }
        let Some(call) = self.turns.last().and_then(|t| {
            t.items.iter().rev().find_map(|i| match i {
                Item::Tool(c) if c.id == id => Some(c),
                _ => None,
            })
        }) else {
            return;
        };
        if matches!(call.kind.as_deref(), Some("execute" | "fetch" | "think")) {
            return;
        }
        let Some(loc) = call.locations.first() else { return };
        let key = (loc.path.clone(), loc.line);
        if self.last_followed.as_ref() == Some(&key) {
            return;
        }
        self.last_followed = Some(key);
        cx.emit(ChatEvent::Follow { path: PathBuf::from(&loc.path), line: loc.line.unwrap_or(1) });
    }

    pub fn set_follow(&mut self, on: bool, cx: &mut Context<Self>) {
        self.follow = on;
        self.last_followed = None;
        cx.notify();
    }

    /// Plan del agente (para las herramientas de kuaky).
    pub fn plan_entries(&self) -> &[PlanEntry] {
        &self.plan
    }

    /// ¿Ya hubo algún mensaje en la conversación?
    pub fn has_turns(&self) -> bool {
        self.turns.iter().any(|t| t.user.is_some() || !t.items.is_empty())
    }

    /// Mensajes en cola (llegaron mientras el agente estaba ocupado).
    pub fn queued(&self) -> usize {
        self.queue.len()
    }

    /// Respuesta del agente en el último turno (para pasarla a la sesión conectada).
    pub fn last_answer(&self) -> Option<String> {
        let turn = self.turns.iter().rev().find(|t| t.items.iter().any(|i| matches!(i, Item::Agent(_))))?;
        let text: Vec<&str> = turn.items.iter().filter_map(|i| if let Item::Agent(t) = i { Some(t.as_ref()) } else { None }).collect();
        Some(text.join("\n\n").trim().to_string()).filter(|t| !t.is_empty())
    }

    pub fn push_notice(&mut self, text: String, cx: &mut Context<Self>) {
        self.current_turn().items.push(Item::Notice(text));
        cx.notify();
    }

    /// Envía un mensaje armado por kuaky (p. ej. comentarios de revisión); en cola si está ocupado.
    pub fn submit_text(&mut self, text: String, cx: &mut Context<Self>) {
        match self.status {
            ChatStatus::Ready => self.dispatch(text, Vec::new(), cx),
            ChatStatus::Running | ChatStatus::Connecting => self.queue.push(Queued { text, images: Vec::new() }),
            _ => {}
        }
        cx.notify();
    }

    /// Imágenes pegadas o elegidas para el próximo mensaje.
    fn add_images(&mut self, images: Vec<(ImageFormat, Vec<u8>)>, cx: &mut Context<Self>) {
        let supported = self.client.as_ref().is_none_or(|c| c.capabilities.image);
        if !supported {
            let msg = format!("{} doesn't accept images in chat.", self.launch.label);
            self.current_turn().items.push(Item::Notice(msg));
        } else {
            self.attachments.extend(images.into_iter().map(|(f, b)| Attachment::new(f, b)));
        }
        cx.notify();
    }

    fn pick_images(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let receiver = cx.prompt_for_paths(PathPromptOptions { files: true, directories: false, multiple: true, prompt: Some("Attach".into()) });
        cx.spawn_in(window, async move |this, cx| {
            let Ok(Ok(Some(paths))) = receiver.await else { return };
            let images: Vec<(ImageFormat, Vec<u8>)> = paths
                .iter()
                .filter_map(|p| {
                    let ext = p.extension()?.to_string_lossy().to_lowercase();
                    let format = match ext.as_str() {
                        "png" => ImageFormat::Png,
                        "jpg" | "jpeg" => ImageFormat::Jpeg,
                        "gif" => ImageFormat::Gif,
                        "webp" => ImageFormat::Webp,
                        _ => return None,
                    };
                    Some((format, std::fs::read(p).ok()?))
                })
                .collect();
            let _ = this.update(cx, |this, cx| this.add_images(images, cx));
        })
        .detach();
    }

    /// Envía un mensaje: guarda un punto de restauración y arma los bloques
    /// (texto + archivos mencionados con `@`).
    fn dispatch(&mut self, text: String, images: Vec<Attachment>, cx: &mut Context<Self>) {
        let (Some(client), Some(session)) = (self.client.clone(), self.session.clone()) else {
            self.queue.insert(0, Queued { text, images });
            // Sin agente vivo: se reconecta y el mensaje sale al conectar.
            if self.asleep {
                self.wake(cx);
            } else if self.status == ChatStatus::Exited {
                self.reconnect(cx);
            }
            return;
        };
        let n = self.turns.iter().filter(|t| t.user.is_some()).count() + 1;
        let mut blocks = Vec::new();
        if !text.is_empty() {
            blocks.push(PromptBlock::Text(text.clone()));
        }
        for img in &images {
            blocks.push(PromptBlock::Image { data: img.data.clone(), mime: img.mime.clone() });
        }
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
        let shown_images = images.iter().map(|i| i.image.clone()).collect();
        self.turns.push(Turn::new(Some(UserMsg { text: text.clone(), images: shown_images, n, at: jiff::Timestamp::now(), checkpoint })));
        self.last_event = Instant::now();
        if let Err(err) = client.prompt_blocks(&session, &blocks) {
            self.current_turn().items.push(Item::Notice(format!("Failed to send: {err:#}")));
            if client.is_closed() {
                // El proceso murió: reconectar y reenviar este mensaje.
                if let Some(turn) = self.turns.pop() {
                    if let Some(user) = turn.user {
                        self.queue.insert(0, Queued { text: user.text, images });
                    }
                }
                self.status = ChatStatus::Exited;
                self.reconnect(cx);
            }
            cx.notify();
            return;
        }
        cx.emit(ChatEvent::Prompt(text));
        self.status = ChatStatus::Running;
        self.stick_bottom = true;
        self.scroll.scroll_to_bottom();
        cx.emit(ChatEvent::Status);
        cx.notify();
    }

    /// Editar el mensaje `#at`: la conversación desde ahí pasa a ser una
    /// versión anterior y se sigue en una sesión nueva con el contexto previo.
    fn fork_at(&mut self, at: usize, text: String, cx: &mut Context<Self>) {
        // Sin agente vivo no se puede abrir la versión nueva: antes de tocar los
        // turnos se reconecta (si no, la edición se perdía).
        if self.client.is_none() {
            if self.asleep {
                self.wake(cx);
            } else if self.status == ChatStatus::Exited {
                self.reconnect(cx);
            }
            self.current_turn().items.push(Item::Notice("Reconnecting to the agent: edit the message again once it's ready.".into()));
            cx.notify();
            return;
        }
        let Some(pos) = self.turns.iter().position(|t| t.user.as_ref().is_some_and(|u| u.n == at)) else {
            self.dispatch(text, Vec::new(), cx);
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
        self.dispatch(full, Vec::new(), cx);
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
        // Se recuerda para la próxima sesión nueva de este agente.
        let label = self.launch.label.to_string();
        let (k, v) = (id.clone(), value.clone());
        crate::state::update(cx, |s| {
            s.settings.chat_prefs.entry(label).or_default().insert(k, v);
        });
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
        self.stick_bottom = false;
        self.scroll.scroll_to_top_of_item(target);
        cx.notify();
    }

    fn go_to(&mut self, index: usize, cx: &mut Context<Self>) {
        self.prompt_list = false;
        self.stick_bottom = false;
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
            let last = self.queue.pop().map(|q| q.text).or_else(|| self.turns.iter().rev().find_map(|t| t.user.as_ref()).map(|u| u.text.clone()));
            if let Some(text) = last {
                self.input.update(cx, |s, cx| s.set_value(text, window, cx));
                cx.stop_propagation();
            }
        }
    }

    // ── Render ─────────────────────────────────────────────────────────

    /// Ancho máximo de la conversación (columna centrada, como la app de Claude).
    const COLUMN: f32 = 820.;

    fn render_turn(&self, ti: usize, turn: &Turn, live: bool, last: bool, cx: &mut Context<Self>) -> AnyElement {
        let mut col = v_flex().w_full().max_w(px(Self::COLUMN)).mx_auto().gap_3().pb_6();
        if let Some(user) = &turn.user {
            col = col.child(self.user_bubble(user, live, cx));
        }
        // Herramientas seguidas (y permisos ya contestados) van en un grupo plegable.
        let mut group: Vec<usize> = Vec::new();
        let flush = |col: Div, group: &mut Vec<usize>, this: &Self, cx: &mut Context<Self>| -> Div {
            if group.is_empty() {
                return col;
            }
            let ids = std::mem::take(group);
            let el = this.tool_group(ti, turn, &ids, live && last, cx);
            col.child(Self::arrive(last, format!("arrive-{ti}-g{}", ids[0]), el))
        };
        // Pasos de subagentes: van dentro de la tarjeta de su subagente.
        let tool_ids: HashSet<&str> = turn.items.iter().filter_map(|i| match i {
            Item::Tool(c) => Some(c.id.as_str()),
            _ => None,
        }).collect();
        for (ii, item) in turn.items.iter().enumerate() {
            if matches!(item, Item::Tool(c) if c.parent.as_deref().is_some_and(|p| tool_ids.contains(p))) {
                continue;
            }
            // En detalle normal el razonamiento no corta el grupo (queda oculto).
            let groupable = matches!(item, Item::Tool(_))
                || matches!(item, Item::Permission { answer: Some(_), .. })
                || (matches!(item, Item::Thought(_)) && self.detail == Detail::Normal && !group.is_empty());
            if groupable {
                group.push(ii);
                continue;
            }
            col = flush(col, &mut group, self, cx);
            let el = self.render_item(ti, ii, item, live, cx);
            col = col.child(Self::arrive(last, format!("arrive-{ti}-{ii}"), el));
        }
        col = flush(col, &mut group, self, cx);
        if last && self.status == ChatStatus::Running {
            col = col.child(self.activity_line(turn));
        }
        if !turn.files.is_empty() && turn.duration.is_some() {
            col = col.child(self.changes_card(ti, turn, live, cx));
        }
        col.into_any_element()
    }

    /// Lo nuevo del turno en curso entra con un fundido corto; el historial no se anima.
    fn arrive(current: bool, id: String, el: impl IntoElement) -> AnyElement {
        match current {
            true => crate::motion::fade_in(div().child(el), SharedString::from(id), crate::motion::FAST).into_any_element(),
            false => el.into_any_element(),
        }
    }

    /// Línea viva mientras el agente trabaja: qué hace, cuánto lleva y si está callado.
    fn activity_line(&self, turn: &Turn) -> impl IntoElement {
        let waiting = self.is_waiting_permission();
        let doing = match turn.items.last() {
            _ if waiting => "Waiting for your answer".to_string(),
            Some(Item::Tool(c)) if matches!(c.status.as_deref(), Some("in_progress") | Some("pending") | None) => {
                match c.kind.as_deref() {
                    Some("execute") => "Running a command".into(),
                    Some("edit") => "Editing".into(),
                    Some("read") => "Reading".into(),
                    Some("search") => "Searching".into(),
                    Some("fetch") => "Fetching".into(),
                    _ => "Using a tool".into(),
                }
            }
            Some(Item::Thought(_)) => "Thinking".into(),
            Some(Item::Agent(_)) => "Writing".into(),
            _ => "Working".into(),
        };
        let elapsed = turn.started.map(|s| s.elapsed()).unwrap_or_default();
        let quiet = self.last_event.elapsed();
        let color: Hsla = if waiting { style::warn().into() } else { style::accent().into() };
        h_flex()
            .gap_2()
            .items_center()
            .text_sm()
            .child(crate::motion::spinner("chat-working", color, px(14.)))
            // Como Claude Code: un brillo recorre el verbo mientras trabaja; esperando permiso queda quieto.
            .child(match waiting {
                true => div().text_color(color).child(format!("{doing}…")).into_any_element(),
                false => crate::motion::shimmer(
                    SharedString::from(format!("chat-verb-{doing}")),
                    format!("{doing}…"),
                    style::soft(style::accent(), 0.65),
                    style::text().into(),
                )
                .into_any_element(),
            })
            .child(div().text_xs().text_color(style::faint()).child(fmt_duration(elapsed)))
            .when(quiet > Duration::from_secs(30) && !waiting, |d| {
                d.child(div().text_xs().text_color(style::faint()).child(format!("· no output for {}", fmt_duration(quiet))))
            })
            .child(div().text_xs().text_color(style::faint()).child("· Esc to stop"))
    }

    /// Resumen de archivos cambiados en el turno, con revisar y deshacer por archivo.
    fn changes_card(&self, ti: usize, turn: &Turn, live: bool, cx: &mut Context<Self>) -> impl IntoElement {
        let (added, removed) = turn.stats();
        let files = turn.files.len();
        let n = turn.user.as_ref().map(|u| u.n).unwrap_or(0);
        let open = self.expanded.contains(&(ti, usize::MAX));
        v_flex()
            .rounded_lg()
            .border_1()
            .border_color(style::border())
            .child(
                h_flex()
                    .id(("changes", ti))
                    .px_3()
                    .py_1p5()
                    .gap_2()
                    .items_center()
                    .text_xs()
                    .cursor_pointer()
                    .on_click(cx.listener(move |this, _, _, cx| {
                        if !this.expanded.remove(&(ti, usize::MAX)) {
                            this.expanded.insert((ti, usize::MAX));
                        }
                        cx.notify();
                    }))
                    .child(crate::icons::ui(if open { "chevron-down" } else { "chevron-right" }, px(12.), style::muted()))
                    .child(div().text_color(style::muted()).child(format!("{files} file{} changed", if files == 1 { "" } else { "s" })))
                    .child(div().text_color(style::good()).child(format!("+{added}")))
                    .child(div().text_color(style::bad()).child(format!("−{removed}")))
                    .when_some(turn.duration, |d, dur| d.child(div().text_color(style::faint()).child(format!("· {}", fmt_duration(dur)))))
                    .child(div().flex_1())
                    .child(
                        div()
                            .id(("review-turn", ti))
                            .px_2()
                            .py_0p5()
                            .rounded_md()
                            .border_1()
                            .border_color(style::border())
                            .text_color(style::text())
                            .hover(|d| d.bg(style::panel_hover()))
                            .child("Review")
                            .on_click(cx.listener(move |this, _, _, cx| {
                                cx.stop_propagation();
                                // Se copian los textos al pulsar, no en cada frame.
                                let Some(turn) = this.shown_turns().get(ti).copied() else { return };
                                let files = turn.files.iter().map(|(p, (old, new))| (p.clone(), old.clone(), new.clone())).collect();
                                cx.emit(ChatEvent::OpenReview { title: format!("Review · message #{n}"), files });
                            })),
                    ),
            )
            .when(open, |d| {
                d.children(turn.files.iter().enumerate().map(|(fi, (path, (old, new)))| {
                    let name = path.strip_prefix(&self.launch.cwd).unwrap_or(path).to_string_lossy().into_owned();
                    let (a, r) = count_text_changes(old.as_deref(), new);
                    let (path, old) = (path.clone(), old.clone());
                    h_flex()
                        .px_3()
                        .py_1()
                        .gap_2()
                        .border_t_1()
                        .border_color(style::border())
                        .text_xs()
                        .child(crate::icons::ui("file", px(11.), style::faint()))
                        .child(div().flex_1().min_w_0().truncate().text_color(style::muted()).child(name.clone()))
                        .child(div().text_color(style::good()).child(format!("+{a}")))
                        .child(div().text_color(style::bad()).child(format!("−{r}")))
                        .when(live, |d| {
                            d.child(
                                div()
                                    .id(("revert-file", ti * 1000 + fi))
                                    .text_color(style::faint())
                                    .cursor_pointer()
                                    .hover(|d| d.text_color(style::bad()))
                                    .tooltip(|window, cx| Tooltip::new("Undo this turn's change to the file").build(window, cx))
                                    .child("Revert")
                                    .on_click(cx.listener(move |this, _, _, cx| this.revert_file(path.clone(), old.clone(), name.clone(), cx))),
                            )
                        })
                }))
            })
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

    /// Tu mensaje: burbuja a la derecha; hora y acciones debajo al pasar el mouse.
    fn user_bubble(&self, user: &UserMsg, live: bool, cx: &mut Context<Self>) -> AnyElement {
        let n = user.n;
        let time = jiff::Zoned::new(user.at, jiff::tz::TimeZone::system()).strftime("%H:%M").to_string();
        let versions = self.versions_of(n);
        let checkpoint = user.checkpoint.clone();
        let copy_text = user.text.clone();
        let group: SharedString = format!("user-{n}").into();
        let small_button = |id: SharedString, icon: &'static str, tip: &'static str| {
            div()
                .id(id)
                .p_1()
                .rounded_md()
                .cursor_pointer()
                .hover(|d| d.bg(style::panel_hover()))
                .tooltip(move |window, cx| Tooltip::new(tip).build(window, cx))
                .child(crate::icons::ui(icon, px(13.), style::muted()))
        };
        v_flex()
            .id(("user-card", n))
            .group(group.clone())
            .w_full()
            .items_end()
            .gap_1()
            .pt_2()
            .child(
                v_flex()
                    .max_w(relative(0.82))
                    .px_4()
                    .py_2p5()
                    .gap_2()
                    .rounded(px(16.))
                    .bg(style::panel_hover())
                    .when(!user.images.is_empty(), |d| {
                        d.child(h_flex().gap_2().flex_wrap().children(user.images.iter().map(|img| {
                            img_el(img.clone()).max_h(px(160.)).max_w(px(240.)).rounded_md()
                        })))
                    })
                    .when(!user.text.is_empty(), |d| d.child(div().text_sm().whitespace_normal().child(user.text.clone()))),
            )
            .child(
                h_flex()
                    .gap_0p5()
                    .items_center()
                    .text_xs()
                    .text_color(style::faint())
                    .when_some(versions, |d, (cur, total)| {
                        d.child(
                            h_flex()
                                .gap_1()
                                .items_center()
                                .mr_1()
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
                    .child(
                        h_flex()
                            .gap_0p5()
                            .items_center()
                            .invisible()
                            .group_hover(group, |s| s.visible())
                            .child(div().mr_1().child(format!("#{n} · {time}")))
                            .when_some(checkpoint.filter(|_| live), |d, sha| {
                                d.child(
                                    small_button(SharedString::from(format!("restore-{n}")), "rotate-ccw", "Restore files to before this message")
                                        .on_click(cx.listener(move |this, _, _, cx| this.restore(sha.clone(), n, cx))),
                                )
                            })
                            .when(live, |d| {
                                d.child(
                                    small_button(SharedString::from(format!("edit-{n}")), "pencil", "Edit and resend (keeps this version)")
                                        .on_click(cx.listener(move |this, _, window, cx| this.edit(n, window, cx))),
                                )
                            })
                            .child(small_button(SharedString::from(format!("copy-{n}")), "copy", "Copy message").on_click(cx.listener(
                                move |_, _, _, cx| cx.write_to_clipboard(ClipboardItem::new_string(copy_text.clone())),
                            ))),
                    ),
            )
            .into_any_element()
    }

    fn render_item(&self, ti: usize, ii: usize, item: &Item, live: bool, cx: &mut Context<Self>) -> AnyElement {
        let key = ti * 10_000 + ii;
        match item {
            Item::Agent(text) => div().text_sm().child(TextView::markdown(("agent-msg", key), text.clone())).into_any_element(),
            Item::Thought(text) => {
                let open = self.detail != Detail::Normal || self.expanded.contains(&(ti, ii));
                v_flex()
                    .child(
                        h_flex()
                            .id(("thought", key))
                            .gap_1()
                            .items_center()
                            .text_sm()
                            .text_color(style::faint())
                            .cursor_pointer()
                            .hover(|d| d.text_color(style::muted()))
                            .child("Thought")
                            .child(crate::icons::ui(if open { "chevron-down" } else { "chevron-right" }, px(13.), style::faint()))
                            .on_click(cx.listener(move |this, _, _, cx| {
                                if !this.expanded.remove(&(ti, ii)) {
                                    this.expanded.insert((ti, ii));
                                }
                                cx.notify();
                            })),
                    )
                    .when(open, |d| {
                        let body = div().mt_1().pl_3().border_l_2().border_color(style::border()).text_xs().text_color(style::muted()).child(text.clone());
                        d.child(crate::motion::fade_in(body, SharedString::from(format!("open-{key}")), crate::motion::FAST))
                    })
                    .into_any_element()
            }
            Item::Notice(text) => div().text_xs().italic().text_color(style::muted()).child(text.clone()).into_any_element(),
            Item::Tool(call) => self.tool_card(key, call, &[], cx),
            Item::Permission { req, answer } => {
                let pending = answer.is_none() && live;
                match answer {
                    Some(a) => h_flex()
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
                        .border_color(style::soft(style::warn(), if pending { 0.7 } else { 0.3 }))
                        .bg(style::soft(style::warn(), 0.07))
                        .child(
                            h_flex()
                                .gap_2()
                                .items_center()
                                .text_sm()
                                .child(crate::icons::ui("triangle-alert", px(14.), style::warn()))
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

    /// Herramientas seguidas como una línea ("Ran 3 commands, edited a.rs +4 −1 ›").
    /// Se abre con clic; mientras corre, se ve la herramienta en curso.
    fn tool_group(&self, ti: usize, turn: &Turn, ids: &[usize], running_turn: bool, cx: &mut Context<Self>) -> AnyElement {
        let first = ids[0];
        let calls: Vec<&ToolCall> = ids
            .iter()
            .filter_map(|&i| match turn.items.get(i) {
                Some(Item::Tool(c)) => Some(c),
                _ => None,
            })
            .collect();
        let open = self.detail == Detail::Verbose || self.expanded.contains(&(ti, first));
        let in_flight = |c: &&&ToolCall| matches!(c.status.as_deref(), Some("in_progress") | Some("pending") | None);
        let current: Option<&ToolCall> = if running_turn { calls.iter().rev().find(in_flight).copied() } else { None };

        // Partes del resumen.
        let count = |kind: &str| calls.iter().filter(|c| c.kind.as_deref() == Some(kind)).count();
        let plural = |n: usize, one: &str, many: &str| if n == 1 { one.to_string() } else { format!("{n} {many}") };
        let mut parts: Vec<AnyElement> = Vec::new();
        let mut words: Vec<String> = Vec::new();
        let ran = count("execute");
        if ran > 0 {
            words.push(format!("ran {}", plural(ran, "a command", "commands")));
        }
        let read = count("read");
        if read > 0 {
            words.push(format!("read {}", plural(read, "a file", "files")));
        }
        let searched = count("search");
        if searched > 0 {
            words.push(if searched == 1 { "searched".into() } else { format!("searched {searched} times") });
        }
        let fetched = count("fetch");
        if fetched > 0 {
            words.push(format!("fetched {}", plural(fetched, "a page", "pages")));
        }
        let subagents = calls.iter().filter(|c| c.is_subagent()).count();
        if subagents > 0 {
            words.push(format!("ran {}", plural(subagents, "a subagent", "subagents")));
        }
        let other = calls
            .iter()
            .filter(|c| !c.is_subagent() && !matches!(c.kind.as_deref(), Some("execute" | "read" | "search" | "fetch" | "edit" | "delete" | "move")))
            .count();
        if other > 0 {
            words.push(format!("used {}", plural(other, "a tool", "tools")));
        }
        // Ediciones: archivo y +/−, como "creó agent_history.rs +166".
        let mut edits: BTreeMap<String, (usize, usize)> = BTreeMap::new();
        for c in calls.iter().filter(|c| matches!(c.kind.as_deref(), Some("edit" | "delete" | "move"))) {
            if c.diffs.is_empty() {
                let name = c.locations.first().map(|l| l.path.rsplit('/').next().unwrap_or(&l.path).to_string()).unwrap_or_else(|| "a file".into());
                edits.entry(name).or_default();
            }
            for d in &c.diffs {
                let (a, r) = count_changes(d);
                let e = edits.entry(d.path.rsplit('/').next().unwrap_or(&d.path).to_string()).or_default();
                e.0 += a;
                e.1 += r;
            }
        }
        let failed = calls.iter().filter(|c| c.status.as_deref() == Some("failed")).count();
        let mut lead = words.join(", ");
        if !edits.is_empty() {
            if !lead.is_empty() {
                lead.push_str(", ");
            }
            lead.push_str("edited ");
        }
        let mut chars = lead.chars();
        let lead = chars.next().map(|c| c.to_uppercase().collect::<String>() + chars.as_str()).unwrap_or_default();
        parts.push(div().child(lead).into_any_element());
        let edit_count = edits.len();
        for (i, (name, (a, r))) in edits.into_iter().enumerate() {
            if i > 0 {
                parts.push(div().child(",").into_any_element());
            }
            parts.push(div().text_color(style::text()).child(name).into_any_element());
            if a > 0 {
                parts.push(div().text_color(style::good()).child(format!("+{a}")).into_any_element());
            }
            if r > 0 {
                parts.push(div().text_color(style::bad()).child(format!("−{r}")).into_any_element());
            }
            if i >= 2 && edit_count > 3 {
                parts.push(div().child(format!("and {} more", edit_count - 3)).into_any_element());
                break;
            }
        }
        if failed > 0 {
            parts.push(div().text_color(style::bad()).child(format!("({failed} failed)")).into_any_element());
        }
        let answered: Vec<(usize, &Item)> = ids.iter().filter_map(|&i| turn.items.get(i).map(|it| (i, it))).filter(|(_, it)| matches!(it, Item::Permission { .. })).collect();

        v_flex()
            .gap_1p5()
            .child(
                h_flex()
                    .id(("tools", ti * 10_000 + first))
                    .gap_1()
                    .items_center()
                    .flex_wrap()
                    .text_sm()
                    .text_color(style::faint())
                    .cursor_pointer()
                    .hover(|d| d.text_color(style::muted()))
                    .children(parts)
                    .child(crate::icons::ui(if open { "chevron-down" } else { "chevron-right" }, px(13.), style::faint()))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        if !this.expanded.remove(&(ti, first)) {
                            this.expanded.insert((ti, first));
                        }
                        cx.notify();
                    })),
            )
            .when(open, |d| {
                let body = v_flex()
                    .gap_1p5()
                    .children(ids.iter().filter_map(|&i| match turn.items.get(i) {
                        Some(Item::Tool(c)) => Some(self.tool_card(ti * 10_000 + i, c, &steps_of(turn, &c.id), cx)),
                        Some(it @ Item::Permission { .. }) => Some(self.render_item(ti, i, it, false, cx)),
                        _ => None,
                    }));
                d.child(crate::motion::fade_in(body, SharedString::from(format!("group-open-{ti}-{first}")), crate::motion::FAST))
            })
            .when(!open, |d| {
                d.when_some(current, |d, c| d.child(self.tool_card(ti * 10_000 + 9_999, c, &steps_of(turn, &c.id), cx)))
                    .when(answered.is_empty(), |d| d)
            })
            .into_any_element()
    }

    /// Una herramienta, estilo Zed: etiqueta arriba y el comando o título en monoespaciado.
    fn tool_card(&self, key: usize, call: &ToolCall, steps: &[&ToolCall], cx: &mut Context<Self>) -> AnyElement {
        let status = call.status.as_deref().unwrap_or("pending");
        let (status_icon, color): (&'static str, Hsla) = match status {
            "completed" => ("check", style::good().into()),
            "failed" => ("x", style::bad().into()),
            "in_progress" => ("loader-circle", style::accent().into()),
            _ => ("circle-dashed", style::faint().into()),
        };
        let subagent_label = call.subagent.as_ref().map(|(kind, _)| format!("Subagent · {kind}"));
        let label = match call.kind.as_deref() {
            Some("edit") => "Edit",
            Some("read") => "Read",
            Some("execute") => "Command",
            Some("search") => "Search",
            Some("fetch") => "Fetch",
            Some("think") => "Think",
            Some("delete") => "Delete",
            Some("move") => "Move",
            _ => "Tool",
        };
        let location = call.locations.first().cloned();
        let verbose = self.detail == Detail::Verbose;
        let title = call.title.clone().unwrap_or_else(|| "Tool".into());
        let title: String = title.lines().take(6).collect::<Vec<_>>().join("\n");
        v_flex()
            .rounded_md()
            .border_1()
            .border_color(style::border())
            .bg(style::panel())
            .child(
                h_flex()
                    .px_3()
                    .pt_1p5()
                    .gap_2()
                    .items_center()
                    .text_xs()
                    .text_color(style::faint())
                    .child(subagent_label.unwrap_or_else(|| label.to_string()))
                    .when_some(location, |d, loc| {
                        let path = PathBuf::from(&loc.path);
                        let line = loc.line.unwrap_or(1);
                        let label = format!(
                            "{}{}",
                            path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default(),
                            loc.line.map(|l| format!(":{l}")).unwrap_or_default()
                        );
                        d.child(
                            div()
                                .id(("tool-loc", key))
                                .text_color(style::accent())
                                .cursor_pointer()
                                .hover(|d| d.text_color(style::text()))
                                .child(label)
                                .on_click(cx.listener(move |_, _, _, cx| cx.emit(ChatEvent::OpenFile { path: path.clone(), line }))),
                        )
                    })
                    .child(div().flex_1())
                    .children(call.diffs.iter().take(1).map(|d| {
                        let (added, removed) = count_changes(d);
                        h_flex()
                            .gap_1()
                            .child(div().text_color(style::good()).child(format!("+{added}")))
                            .child(div().text_color(style::bad()).child(format!("−{removed}")))
                    }))
                    .child(match status {
                        // En curso: el ícono parpadea lento, como el punto de Claude Code.
                        "in_progress" => crate::motion::blink(
                            div().child(crate::icons::ui(status_icon, px(12.), color)),
                            SharedString::from(format!("tool-blink-{}", call.id)),
                        )
                        .into_any_element(),
                        _ => crate::icons::ui(status_icon, px(12.), color).into_any_element(),
                    }),
            )
            .child(
                div()
                    .px_3()
                    .pt_0p5()
                    .pb_2()
                    .text_xs()
                    .font_family(style::mono_font())
                    .text_color(style::muted())
                    .whitespace_normal()
                    .child(title),
            )
            .children(call.diffs.iter().enumerate().map(|(di, d)| {
                let diff = d.clone();
                let file = d.path.rsplit('/').next().unwrap_or(&d.path).to_string();
                div()
                    .id(("tool-diff", key * 32 + di))
                    .px_3()
                    .py_1()
                    .border_t_1()
                    .border_color(style::border())
                    .text_xs()
                    .text_color(style::accent())
                    .cursor_pointer()
                    .hover(|d| d.text_color(style::text()))
                    .child(format!("Open diff · {file}"))
                    .on_click(cx.listener(move |_, _, _, cx| {
                        cx.emit(ChatEvent::OpenDiff { title: format!("± {}", diff.path.rsplit('/').next().unwrap_or("")), patch: unified_patch(&diff) });
                    }))
            }))
            .when_some(call.output.clone().filter(|o| !o.trim().is_empty()), |d, out| d.child(command_output(&out, call.exit_code, verbose)))
            .when(!steps.is_empty(), |d| d.child(self.subagent_steps(key, call, steps, cx)))
            .when_some(call.text.clone().filter(|t| verbose && !t.trim().is_empty()), |d, t| {
                let short: String = t.lines().take(20).collect::<Vec<_>>().join("\n");
                d.child(
                    div()
                        .px_3()
                        .py_2()
                        .border_t_1()
                        .border_color(style::border())
                        .text_xs()
                        .font_family(style::mono_font())
                        .text_color(style::faint())
                        .child(short),
                )
            })
            .into_any_element()
    }

    /// Pasos de un subagente dentro de su tarjeta: plegados en una línea
    /// ("12 steps · now: Read lib.rs"); al abrir, uno por fila.
    fn subagent_steps(&self, key: usize, call: &ToolCall, steps: &[&ToolCall], cx: &mut Context<Self>) -> AnyElement {
        let open = self.detail == Detail::Verbose
            || self.open_steps.contains(&call.id)
            || (cfg!(feature = "screenshot") && std::env::var("KUAKY_DEMO_STEPS").is_ok());
        let running = matches!(call.status.as_deref(), Some("in_progress") | Some("pending") | None);
        let count = steps.len();
        let failed = steps.iter().filter(|s| s.status.as_deref() == Some("failed")).count();
        let last = steps.last().and_then(|s| s.title.clone()).unwrap_or_default();
        let mut summary = format!("{count} step{}", if count == 1 { "" } else { "s" });
        if failed > 0 {
            summary.push_str(&format!(" · {failed} failed"));
        }
        if running && !last.is_empty() {
            summary.push_str(&format!(" · now: {}", crate::lines::short(last.lines().next().unwrap_or(""), 70)));
        }
        let id = call.id.clone();
        const MAX: usize = 30;
        let hidden = count.saturating_sub(MAX);
        v_flex()
            .border_t_1()
            .border_color(style::border())
            .child(
                h_flex()
                    .id(("sub-steps", key))
                    .px_3()
                    .py_1()
                    .gap_1()
                    .items_center()
                    .text_xs()
                    .text_color(style::faint())
                    .cursor_pointer()
                    .hover(|d| d.text_color(style::muted()))
                    .child(crate::icons::ui(if open { "chevron-down" } else { "chevron-right" }, px(11.), style::faint()))
                    .child(div().flex_1().min_w_0().truncate().child(summary))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        if !this.open_steps.remove(&id) {
                            this.open_steps.insert(id.clone());
                        }
                        cx.notify();
                    })),
            )
            .when(open, |d| {
                d.when(hidden > 0, |d| d.child(div().px_3().pl(px(28.)).text_xs().text_color(style::faint()).child(format!("{hidden} earlier steps"))))
                    .children(steps.iter().skip(hidden).map(|s| {
                        let (icon, color): (&'static str, Hsla) = match s.status.as_deref() {
                            Some("completed") => ("check", style::faint().into()),
                            Some("failed") => ("x", style::bad().into()),
                            _ => ("loader-circle", style::accent().into()),
                        };
                        let title = s.title.clone().unwrap_or_else(|| s.tool_name.clone().unwrap_or_else(|| "Tool".into()));
                        h_flex()
                            .px_3()
                            .pl(px(28.))
                            .py_0p5()
                            .gap_2()
                            .items_center()
                            .text_xs()
                            .child(crate::icons::ui(icon, px(11.), color))
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .truncate()
                                    .font_family(style::mono_font())
                                    .text_color(style::muted())
                                    .child(title.lines().next().unwrap_or("").to_string()),
                            )
                    }))
                    .child(div().h_1())
            })
            .into_any_element()
    }

    /// Tu mensaje del turno que lees, fijo arriba del chat (clic: ir a él).
    fn sticky_prompt(&self, i: usize, user: &UserMsg, cx: &mut Context<Self>) -> AnyElement {
        h_flex()
            .id("sticky-prompt")
            .occlude()
            .w_full()
            .max_w(px(Self::COLUMN))
            .mx_auto()
            .px_3()
            .py_1()
            .gap_2()
            .items_center()
            .rounded_lg()
            .border_1()
            .border_color(style::border())
            .bg(style::panel())
            .shadow_md()
            .text_xs()
            .cursor_pointer()
            .child(div().text_color(style::faint()).child(format!("#{}", user.n)))
            .child(div().flex_1().min_w_0().truncate().text_color(style::muted()).child(crate::lines::short(&user.text, 160)))
            .child(crate::icons::ui("arrow-up", px(11.), style::faint()))
            .on_click(cx.listener(move |this, _, _, cx| this.go_to(i, cx)))
            .into_any_element()
    }

    fn header(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let (status_label, status_color): (String, Hsla) = match &self.status {
            _ if self.is_waiting_permission() => ("needs you".into(), style::warn().into()),
            ChatStatus::Connecting => ("connecting…".into(), style::faint().into()),
            ChatStatus::Ready => ("ready".into(), style::good().into()),
            ChatStatus::Running => ("working".into(), style::accent().into()),
            ChatStatus::Error(_) => ("error".into(), style::bad().into()),
            ChatStatus::Exited => ("closed".into(), style::faint().into()),
        };
        let title = self.title.clone().or_else(|| self.first_prompt().map(|p| crate::lines::short(p, 60))).unwrap_or_else(|| format!("New {} chat", self.launch.label));
        let list_tip: SharedString = if cfg!(target_os = "macos") { "Your messages (⌘⇧J) · ⌥↑ ⌥↓ jump".into() } else { "Your messages (Ctrl+Shift+J) · Alt+↑ Alt+↓ jump".into() };
        h_flex()
            .px_4()
            .py_2()
            .gap_2()
            .items_center()
            .border_b_1()
            .border_color(style::border())
            .child(div().flex_1().min_w_0().truncate().text_sm().font_weight(FontWeight::MEDIUM).child(title))
            .child(h_flex().gap_1().items_center().text_xs().text_color(status_color).child(div().size(px(6.)).rounded_full().bg(status_color)).child(status_label))
            .when_some(self.pr, |d, (n, conflicts)| {
                d.child(
                    h_flex()
                        .gap_1()
                        .items_center()
                        .child(
                            h_flex()
                                .id("chat-pr")
                                .gap_1()
                                .px_1p5()
                                .py_0p5()
                                .rounded_md()
                                .text_xs()
                                .cursor_pointer()
                                .text_color(style::muted())
                                .hover(|d| d.bg(style::panel_hover()).text_color(style::text()))
                                .tooltip(|window, cx| Tooltip::new("This session's pull request").build(window, cx))
                                .child(crate::icons::ui("git-pull-request", px(12.), style::muted()))
                                .child(format!("#{n}"))
                                .on_click(cx.listener(move |_, _, _, cx| cx.emit(ChatEvent::OpenPr(n)))),
                        )
                        .child(
                            crate::pr_view::merge_button("chat-pr-merge", conflicts, true)
                                .on_click(cx.listener(move |_, _, _, cx| cx.emit(ChatEvent::MergePr(n)))),
                        ),
                )
            })
            .child(
                div()
                    .id("follow-agent")
                    .p_1()
                    .rounded_md()
                    .cursor_pointer()
                    .when(self.follow, |d| d.bg(style::accent_alpha(0.15)))
                    .hover(|d| d.bg(style::panel_hover()))
                    .tooltip(|window, cx| Tooltip::new("Follow the agent: open the files it reads and edits").build(window, cx))
                    .child(crate::icons::ui("locate-fixed", px(14.), if self.follow { style::accent() } else { style::muted() }))
                    .on_click(cx.listener(|this, _, _, cx| {
                        let on = !this.follow;
                        this.set_follow(on, cx);
                    })),
            )
            .child(
                div()
                    .id("prompt-list-button")
                    .p_1()
                    .rounded_md()
                    .cursor_pointer()
                    .hover(|d| d.bg(style::panel_hover()))
                    .tooltip(move |window, cx| Tooltip::new(list_tip.clone()).build(window, cx))
                    .child(crate::icons::ui("list", px(14.), style::muted()))
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.prompt_list = !this.prompt_list;
                        cx.notify();
                    })),
            )
    }

    fn plan_panel(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        if self.plan.is_empty() {
            return None;
        }
        let done = self.plan.iter().filter(|e| e.status == "completed").count();
        let open = self.plan_open;
        Some(
            v_flex()
                .w_full()
                .max_w(px(Self::COLUMN))
                .mx_auto()
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
                            "in_progress" => ("loader-circle", style::accent().into()),
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
                        style::warn().into()
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

    /// Selectores del agente dentro de la caja (estilo Zed): listas y on/off.
    fn config_controls(&self, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let order = |c: &str| match c {
            "mode" => 0,
            "model" => 1,
            "thought_level" => 2,
            _ => 3,
        };
        let mut config: Vec<&ConfigOption> = self.config.iter().filter(|o| !o.options.is_empty()).collect();
        config.sort_by_key(|o| order(&o.category));
        config
            .into_iter()
            .take(5)
            .map(|opt| {
                let id = opt.id.clone();
                let name: SharedString = opt.name.clone().into();
                // Opción de dos valores tipo on/off → interruptor.
                let toggle = (opt.options.len() == 2).then(|| {
                    let on = opt.options.iter().find(|v| matches!(v.value.to_lowercase().as_str(), "on" | "true" | "enabled" | "yes"))?;
                    let off = opt.options.iter().find(|v| v.value != on.value)?;
                    Some((on.value.clone(), off.value.clone()))
                });
                if let Some(Some((on, off))) = toggle {
                    let is_on = opt.current == on;
                    let next = if is_on { off } else { on };
                    return h_flex()
                        .id(SharedString::from(format!("cfg-{}", opt.id)))
                        .gap_1p5()
                        .px_1p5()
                        .py_0p5()
                        .items_center()
                        .rounded_md()
                        .text_xs()
                        .text_color(style::muted())
                        .cursor_pointer()
                        .hover(|d| d.bg(style::panel_hover()))
                        .child(opt.name.clone())
                        .child(
                            div()
                                .w(px(24.))
                                .h(px(14.))
                                .p(px(2.))
                                .rounded_full()
                                .bg(if is_on { style::accent().into() } else { style::border() })
                                .child(div().size(px(10.)).rounded_full().bg(style::text()).when(is_on, |d| d.ml(px(10.)))),
                        )
                        .on_click(cx.listener(move |this, _, _, cx| this.set_config(id.clone(), next.clone(), cx)))
                        .into_any_element();
                }
                let value = opt.options.iter().find(|v| v.value == opt.current).map(|v| v.name.clone()).unwrap_or_else(|| opt.current.clone());
                let open = self.open_config.as_deref() == Some(opt.id.as_str());
                let menu = if open { self.config_menu(cx) } else { None };
                h_flex()
                    .id(SharedString::from(format!("cfg-{}", opt.id)))
                    .relative()
                    .gap_0p5()
                    .px_1p5()
                    .py_0p5()
                    .items_center()
                    .rounded_md()
                    .text_xs()
                    .text_color(if open { style::text() } else { style::muted() })
                    .when(open, |d| d.bg(style::panel_hover()))
                    .cursor_pointer()
                    .hover(|d| d.bg(style::panel_hover()))
                    .tooltip(move |window, cx| Tooltip::new(name.clone()).build(window, cx))
                    .child(value)
                    .child(crate::icons::ui("chevron-down", px(11.), style::faint()))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        let just_closed = matches!(&this.config_closed, Some((c, at)) if c == &id && at.elapsed() < Duration::from_millis(400));
                        this.open_config = if just_closed || this.open_config.as_deref() == Some(id.as_str()) { None } else { Some(id.clone()) };
                        this.config_closed = None;
                        cx.notify();
                    }))
                    // La lista sale encima de su botón, flotando sobre el chat.
                    .when_some(menu, |d, menu| {
                        d.child(deferred(div().absolute().bottom_full().left_0().mb_1().child(menu)).with_priority(1))
                    })
                    .into_any_element()
            })
            .collect()
    }

    /// Lista de valores de la opción abierta (sale encima de la caja).
    fn config_menu(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let opt = self.open_config.as_ref().and_then(|id| self.config.iter().find(|o| &o.id == id))?.clone();
        Some(
            v_flex()
                .id("config-menu")
                .occlude()
                .w(px(300.))
                .max_h(px(420.))
                .overflow_y_scroll()
                .p_1()
                .shadow_lg()
                .rounded_lg()
                .border_1()
                .border_color(style::border())
                .bg(style::panel())
                .child(div().px_2().py_1().text_xs().text_color(style::faint()).child(opt.name.clone()))
                .children(opt.options.iter().map(|v| {
                    let selected = v.value == opt.current;
                    let (id, value) = (opt.id.clone(), v.value.clone());
                    h_flex()
                        .id(SharedString::from(format!("cfgv-{}-{}", opt.id, v.value)))
                        .px_2()
                        .py_1()
                        .gap_2()
                        .items_start()
                        .rounded_md()
                        .cursor_pointer()
                        .hover(|d| d.bg(style::panel_hover()))
                        .child(div().w(px(14.)).pt_0p5().when(selected, |d| d.child(crate::icons::ui("check", px(12.), style::accent()))))
                        .child(
                            v_flex()
                                .flex_1()
                                .min_w_0()
                                .child(div().text_sm().child(v.name.clone()))
                                .when_some(v.description.clone(), |d, desc| d.child(div().text_xs().text_color(style::faint()).whitespace_normal().child(desc))),
                        )
                        .on_click(cx.listener(move |this, _, _, cx| this.set_config(id.clone(), value.clone(), cx)))
                }))
                .on_mouse_down_out(cx.listener(|this, _, _, cx| {
                    this.config_closed = this.open_config.take().map(|id| (id, Instant::now()));
                    cx.notify();
                }))
                .into_any_element(),
        )
    }

    /// Agente de este chat, si es uno cuyos límites de plan conoce kuaky.
    fn plan_agent(&self) -> Option<Agent> {
        Agent::ALL.into_iter().find(|a| a.name() == self.launch.label && matches!(a, Agent::ClaudeCode | Agent::Codex))
    }

    /// Límites que trajo una respuesta. Se guardan en disco para el inicio, la barra
    /// de estado y los demás chats (son de la cuenta, no de esta sesión).
    fn keep_limits(&mut self, limits: AgentLimits, cx: &mut Context<Self>) {
        self.limits = Some(limits.clone());
        cx.background_executor()
            .spawn(async move {
                if let Err(err) = kuaky_usage::limits::save_claude_limits(&limits) {
                    tracing::warn!(%err, "no se pudieron guardar los límites");
                }
            })
            .detach();
    }

    fn toggle_usage(&mut self, cx: &mut Context<Self>) {
        let just_closed = self.usage_closed.take().is_some_and(|at| at.elapsed() < Duration::from_millis(400));
        self.usage_open = !self.usage_open && !just_closed;
        if self.usage_open {
            self.load_limits(cx);
        }
        cx.notify();
    }

    /// Lee los límites guardados (terminales y otros chats): sirven si este chat
    /// aún no recibió ninguno o si son más nuevos.
    fn load_limits(&self, cx: &mut Context<Self>) {
        let load = match self.plan_agent() {
            Some(Agent::ClaudeCode) => kuaky_usage::limits::load_claude_limits,
            Some(Agent::Codex) => kuaky_usage::limits::load_codex_limits,
            _ => return,
        };
        cx.spawn(async move |this, cx| {
            let Some(found) = cx.background_executor().spawn(async move { load() }).await else { return };
            let _ = this.update(cx, |this, cx| {
                if this.limits.as_ref().is_none_or(|l| l.updated_at < found.updated_at) {
                    this.limits = Some(found);
                    cx.notify();
                }
            });
        })
        .detach();
    }

    /// Contexto usado: anillo y porcentaje. Al pulsarlo abre el panel de uso.
    fn context_meter(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let (used, size, _) = self.usage?;
        let pct = (used as f64 / size as f64 * 100.).clamp(0., 100.);
        let color = level_color(pct, 65., 85.);
        let frac = (pct / 100.) as f32;
        let open = self.usage_open;
        Some(
            h_flex()
                .id("context-meter")
                .relative()
                .gap_1()
                .px_1p5()
                .py_0p5()
                .items_center()
                .rounded_md()
                .text_xs()
                .text_color(color)
                .cursor_pointer()
                .hover(|d| d.bg(style::panel_hover()))
                .when(open, |d| d.bg(style::panel_hover()))
                .when(!open, |d| d.tooltip(|window, cx| Tooltip::new("Context and plan limits").build(window, cx)))
                .on_click(cx.listener(|this, _, _, cx| this.toggle_usage(cx)))
                .child(
                    canvas(
                        |_, _, _| {},
                        move |bounds, _, window, _| {
                            let c = bounds.center();
                            let r = px(5.5);
                            let ring = |from: f32, to: f32, color: Hsla, window: &mut Window| {
                                let steps = 32;
                                let mut b = PathBuilder::stroke(px(2.));
                                for i in 0..=steps {
                                    let a = (from + (to - from) * i as f32 / steps as f32) * std::f32::consts::TAU - std::f32::consts::FRAC_PI_2;
                                    let p = point(c.x + r * a.cos(), c.y + r * a.sin());
                                    if i == 0 {
                                        b.move_to(p);
                                    } else {
                                        b.line_to(p);
                                    }
                                }
                                if let Ok(path) = b.build() {
                                    window.paint_path(path, color);
                                }
                            };
                            ring(0., 1., style::border().into(), window);
                            if frac > 0.005 {
                                ring(0., frac, color, window);
                            }
                        },
                    )
                    .size(px(14.)),
                )
                .child(format!("{pct:.0}%"))
                // Sale encima del anillo, alineado a su derecha (el anillo está al borde de la caja).
                .when(open, |d| {
                    d.child(deferred(div().absolute().bottom_full().right_0().mb_2().child(self.usage_panel(cx))).with_priority(1))
                })
                .into_any_element(),
        )
    }

    /// Panel del anillo: contexto de esta sesión (con compactar) y límites del plan.
    fn usage_panel(&self, cx: &mut Context<Self>) -> AnyElement {
        v_flex()
            .id("usage-panel")
            .occlude()
            .w(px(300.))
            .p_3()
            .gap_3()
            .shadow_lg()
            .rounded_lg()
            .border_1()
            .border_color(style::border())
            .bg(style::panel())
            .when_some(self.context_section(cx), |d, s| d.child(s))
            .when(self.plan_agent().is_some(), |d| d.child(div().h(px(1.)).bg(style::border())).child(self.limits_section()))
            .on_mouse_down_out(cx.listener(|this, _, _, cx| {
                this.usage_open = false;
                this.usage_closed = Some(Instant::now());
                cx.notify();
            }))
            .into_any_element()
    }

    /// Tokens usados de la ventana, costo y el botón para compactar.
    fn context_section(&self, cx: &mut Context<Self>) -> Option<Div> {
        let (used, size, cost) = self.usage?;
        let pct = (used as f64 / size as f64 * 100.).clamp(0., 100.);
        let color = level_color(pct, 65., 85.);
        let can_compact = self.commands.iter().any(|c| c.name == "compact");
        Some(
            v_flex()
                .gap_1p5()
                .child(
                    h_flex()
                        .items_center()
                        .child(eyebrow("CONTEXT · THIS SESSION"))
                        .child(div().flex_1())
                        .child(div().text_xs().text_color(color).child(format!("{pct:.0}%"))),
                )
                .child(meter_bar(pct, color))
                .child(
                    h_flex()
                        .text_xs()
                        .text_color(style::muted())
                        .child(format!("{} of {} tokens", human(used), human(size)))
                        .child(div().flex_1())
                        .when_some(cost, |d, c| d.child(format!("cost {}", style::money(c)))),
                )
                .when(can_compact, |d| d.child(self.compact_row(pct, cx))),
        )
    }

    /// Aviso según lo lleno y botón que manda `/compact` (en cola si el agente trabaja).
    fn compact_row(&self, pct: f64, cx: &mut Context<Self>) -> impl IntoElement {
        let (hint, hint_color) = if pct >= 85. {
            ("Almost full. Compact now to keep working without losing the thread.", style::bad())
        } else if pct >= 65. {
            ("Filling up. Compacting summarizes the conversation to free space.", style::warn())
        } else {
            ("Compacting summarizes the conversation to free space.", style::faint())
        };
        let urgent = pct >= 85.;
        let tip = if self.status == ChatStatus::Running { "Runs when the agent finishes this turn" } else { "Sends /compact" };
        h_flex()
            .gap_2()
            .pt_1()
            .items_center()
            .child(div().flex_1().min_w_0().text_xs().text_color(hint_color).whitespace_normal().child(hint))
            .child(
                div()
                    .id("compact-now")
                    .px_2()
                    .py_1()
                    .rounded_md()
                    .border_1()
                    .text_xs()
                    .cursor_pointer()
                    .map(|d| {
                        if urgent {
                            d.border_color(style::accent()).bg(style::accent()).text_color(style::on_accent())
                        } else {
                            d.border_color(style::border()).text_color(style::text()).hover(|d| d.bg(style::panel_hover()))
                        }
                    })
                    .tooltip(move |window, cx| Tooltip::new(tip).build(window, cx))
                    .child("Compact")
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.usage_open = false;
                        this.submit_text("/compact".into(), cx);
                    })),
            )
    }

    /// Ventanas del plan (5 h, semana, Fable) con cuándo se reinician.
    fn limits_section(&self) -> Div {
        let now = jiff::Timestamp::now();
        let tz = jiff::tz::TimeZone::system();
        let limits = self.limits.as_ref().filter(|l| l.has_any());
        let updated = limits.and_then(|l| l.updated_at).map(|ts| crate::home::ago(ts, now));
        v_flex()
            .gap_2()
            .child(
                h_flex()
                    .items_center()
                    .child(eyebrow("PLAN LIMITS"))
                    .child(div().flex_1())
                    .when_some(updated, |d, u| d.child(div().text_xs().text_color(style::faint()).child(u))),
            )
            .map(|d| match limits {
                Some(l) => d.children(l.windows().map(|w| limit_row(w, now, &tz))),
                None => d.child(div().text_xs().text_color(style::faint()).whitespace_normal().child("They show up after the agent's next reply.")),
            })
    }
}

/// Una ventana del plan: nombre, % usado, cuándo se reinicia y su barra.
fn limit_row(w: &RateWindow, now: jiff::Timestamp, tz: &jiff::tz::TimeZone) -> Div {
    let label = match w.label.as_str() {
        "5h" => "5-hour",
        "week" => "Weekly",
        "Fable" => "Fable · weekly",
        other => other,
    };
    // Si ya pasó su reinicio, la ventana empezó de cero (se actualiza con la próxima respuesta).
    let reset_passed = w.resets_at.is_some_and(|t| t <= now);
    let used = if reset_passed { 0. } else { w.used_pct };
    let note = if reset_passed {
        Some("reset".to_string())
    } else {
        w.resets_at.and_then(|t| kuaky_usage::limits::format_reset(t, now, tz)).map(|r| format!("resets {r}"))
    };
    let color = level_color(used, 75., 90.);
    v_flex()
        .gap_1()
        .child(
            h_flex()
                .gap_2()
                .text_xs()
                .child(div().text_color(style::text()).child(label.to_string()))
                .child(div().text_color(color).child(format!("{used:.0}%")))
                .child(div().flex_1())
                .when_some(note, |d, n| d.child(div().text_color(style::faint()).child(n))),
        )
        .child(meter_bar(used, color))
}

/// Etiqueta de sección: mono, mayúsculas y tenue (como en una etiqueta industrial).
fn eyebrow(text: &'static str) -> Div {
    div().text_xs().font_family(style::mono_font()).text_color(style::faint()).child(text)
}

/// Barra fina de 0 a 100 %.
fn meter_bar(pct: f64, color: Hsla) -> Div {
    let frac = (pct / 100.).clamp(0., 1.) as f32;
    div()
        .h(px(4.))
        .w_full()
        .rounded_full()
        .bg(style::border())
        .overflow_hidden()
        .child(div().h_full().rounded_full().bg(color).w(relative(frac)))
}

/// Color según lo lleno: normal, aviso a partir de `warn` y alerta a partir de `bad`.
fn level_color(pct: f64, warn: f64, bad: f64) -> Hsla {
    if pct >= bad {
        style::bad().into()
    } else if pct >= warn {
        style::warn().into()
    } else {
        style::muted().into()
    }
}

/// Salida de un comando dentro de su tarjeta: las últimas líneas (todas en Verbose,
/// hasta 200) y el código de salida si no fue 0.
fn command_output(out: &str, exit_code: Option<i64>, verbose: bool) -> AnyElement {
    let out = strip_ansi(out);
    let lines: Vec<&str> = out.trim_end().lines().collect();
    let keep = if verbose { 200 } else { 8 };
    let hidden = lines.len().saturating_sub(keep);
    let shown = lines[hidden..].join("\n");
    v_flex()
        .px_3()
        .py_2()
        .gap_1()
        .border_t_1()
        .border_color(style::border())
        .when(hidden > 0, |d| d.child(div().text_xs().text_color(style::faint()).child(format!("… {hidden} earlier lines"))))
        .child(div().text_xs().font_family(style::mono_font()).text_color(style::muted()).whitespace_normal().child(shown))
        .when_some(exit_code.filter(|c| *c != 0), |d, c| d.child(div().text_xs().text_color(style::bad()).child(format!("exit {c}"))))
        .into_any_element()
}

/// Quita los códigos de color de la terminal (`ESC[…m`).
fn strip_ansi(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\u{1b}' {
            if chars.peek() == Some(&'[') {
                chars.next();
                while let Some(&n) = chars.peek() {
                    chars.next();
                    if n.is_ascii_alphabetic() {
                        break;
                    }
                }
            }
            continue;
        }
        if c != '\r' {
            out.push(c);
        }
    }
    out
}

/// Pasos de un subagente en un turno (herramientas cuyo padre es `id`).
fn steps_of<'a>(turn: &'a Turn, id: &str) -> Vec<&'a ToolCall> {
    turn.items
        .iter()
        .filter_map(|i| match i {
            Item::Tool(c) if c.parent.as_deref() == Some(id) => Some(c),
            _ => None,
        })
        .collect()
}

fn human(n: u64) -> String {
    style::human(n)
}

impl Render for ChatView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // Referencias, no copias: clonar los turnos copiaba toda la conversación
        // (textos, salidas, archivos antes/después) en cada frame.
        let refs: Vec<&Turn> = self.shown_turns();
        let turns = &refs;
        let live_upto = if self.viewing.is_some() { 0 } else { usize::MAX };

        // Prompt fijo: el del turno que estás leyendo, si su burbuja ya pasó.
        let (top, offset) = self.scroll.logical_scroll_top();
        let sticky = (offset < px(-36.))
            .then(|| (0..=top.min(turns.len().saturating_sub(1))).rev().find_map(|i| turns.get(i)?.user.as_ref().map(|u| (i, u.clone()))))
            .flatten();

        let count = turns.len();
        let conversation = div()
            .id("chat-scroll")
            .flex_1()
            .min_w_0()
            .overflow_y_scroll()
            .track_scroll(&self.scroll)
            .on_scroll(cx.listener(|this, ev: &ScrollEvent, _, cx| {
                let dy = ev.delta.pixel_delta(px(16.)).y;
                if dy > px(0.) {
                    // Subiste: deja de seguir el final aunque estés cerca.
                    this.stick_bottom = false;
                } else if dy < px(0.) {
                    // Bajando: vuelve a seguir al llegar al final.
                    let max = this.scroll.max_offset().y;
                    let below = max + this.scroll.offset().y + dy;
                    if max <= px(0.) || below <= px(8.) {
                        this.stick_bottom = true;
                    }
                }
                cx.notify();
            }))
            .px_6()
            .pt_4()
            .when(turns.is_empty() && matches!(self.status, ChatStatus::Ready | ChatStatus::Connecting), |d| {
                d.child(
                    v_flex()
                        .pt_16()
                        .gap_1()
                        .items_center()
                        .text_color(style::faint())
                        .child(div().text_base().text_color(style::muted()).child(format!("Start a conversation with {}", self.launch.label)))
                        .child(div().text_xs().child("Mention files with @, run agent commands with /, paste images with ⌘V.")),
                )
            })
            .children(turns.iter().enumerate().map(|(i, t)| self.render_turn(i, t, i < live_upto, i + 1 == count, cx)));

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
            _ => {
                let away = !turns.is_empty() && !self.at_bottom();
                let sticky_prompt = sticky.map(|(i, user)| self.sticky_prompt(i, &user, cx));
                let running = self.status == ChatStatus::Running;
                div()
                    .flex()
                    .flex_row()
                    .flex_1()
                    .min_h_0()
                    .relative()
                    .child(conversation)
                    .child(self.prompt_rail(&refs, cx))
                    // Prompt fijo flotando arriba: no cambia el alto del chat (si no, brinca).
                    .when_some(sticky_prompt, |d, p| d.child(div().absolute().top_0().left_0().right(px(14.)).px_6().pt_1().child(p)))
                    // Flecha para bajar al final (con punto si el agente sigue trabajando).
                    .when(away, |d| {
                        d.child(
                            div().absolute().bottom(px(12.)).left_0().right_0().flex().justify_center().child(
                                div()
                                    .id("scroll-to-bottom")
                                    .relative()
                                    .size(px(32.))
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .rounded_full()
                                    .bg(style::panel_hover())
                                    .border_1()
                                    .border_color(style::border())
                                    .shadow_md()
                                    .cursor_pointer()
                                    .hover(|d| d.bg(style::border()))
                                    .tooltip(|window, cx| Tooltip::new("Jump to latest").build(window, cx))
                                    .child(crate::icons::ui("arrow-down", px(16.), style::text()))
                                    .when(running, |d| {
                                        d.child(div().absolute().top(px(-2.)).right(px(-2.)).size(px(9.)).rounded_full().bg(style::accent()))
                                    })
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.stick_bottom = true;
                                        this.scroll.scroll_to_bottom();
                                        cx.notify();
                                    })),
                            ),
                        )
                    })
                    .into_any_element()
            }
        };

        let suggestions = (!self.suggestions.is_empty()).then(|| {
            v_flex()
                .w_full()
                .max_w(px(Self::COLUMN))
                .mx_auto()
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
                        .child(div().font_family(style::mono_font()).child(label))
                        .child(div().flex_1().truncate().text_color(style::faint()).child(detail))
                        .on_click(cx.listener(move |this, _, window, cx| {
                            this.suggestion_ix = i;
                            this.accept_suggestion(window, cx);
                        }))
                }))
        });

        let queue = (!self.queue.is_empty()).then(|| {
            v_flex().w_full().max_w(px(Self::COLUMN)).mx_auto().mb_1().gap_0p5().children(self.queue.iter().enumerate().map(|(i, q)| {
                let shown = if q.images.is_empty() { q.text.clone() } else { format!("{} [+{} image]", q.text, q.images.len()) };
                h_flex()
                    .gap_2()
                    .px_2()
                    .py_1()
                    .items_center()
                    .rounded_md()
                    .bg(style::panel())
                    .text_xs()
                    .child(crate::icons::ui("clock", px(12.), style::muted()))
                    .child(div().flex_1().min_w_0().truncate().text_color(style::muted()).child(format!("Queued: {shown}")))
                    .child(
                        div()
                            .id(("queue-now", i))
                            .text_color(style::accent())
                            .cursor_pointer()
                            .child("Send now")
                            .on_click(cx.listener(move |this, _, _, cx| {
                                let q = this.queue.remove(i);
                                this.queue.insert(0, q);
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
                                let q = this.queue.remove(i);
                                this.attachments.extend(q.images);
                                this.input.update(cx, |s, cx| s.set_value(q.text, window, cx));
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
        let has_text = !self.input.read(cx).value().trim().is_empty() || !self.attachments.is_empty();
        let weak = cx.entity().downgrade();
        let textarea = Textarea::new(&self.input).appearance(false).on_paste(move |item, _, cx| {
            let images: Vec<(ImageFormat, Vec<u8>)> = item
                .entries()
                .iter()
                .filter_map(|e| match e {
                    ClipboardEntry::Image(img) => Some((img.format, img.bytes.clone())),
                    _ => None,
                })
                .collect();
            if images.is_empty() {
                return false;
            }
            let _ = weak.update(cx, |this, cx| this.add_images(images, cx));
            true
        });
        let controls = self.config_controls(cx);
        let detail_tip: SharedString = format!("Detail: {} (Normal → Thinking → Verbose)", self.detail.label()).into();
        let composer = v_flex()
            .px_6()
            .pb_3()
            .pt_1()
            .gap_1()
            .when_some(self.editing, |d, n| {
                d.child(
                    h_flex()
                        .w_full()
                        .max_w(px(Self::COLUMN))
                        .mx_auto()
                        .gap_2()
                        .text_xs()
                        .text_color(style::accent())
                        .child(crate::icons::ui("pencil", px(11.), style::accent()))
                        .child(format!("Editing message #{n} — sending starts a new version (the current one is kept). Esc cancels.")),
                )
            })
            // El agente se cayó: avisar y dejar reconectar (escribir y enviar también reconecta).
            .when(self.status == ChatStatus::Exited, |d| {
                d.child(
                    h_flex()
                        .w_full()
                        .max_w(px(Self::COLUMN))
                        .mx_auto()
                        .px_3()
                        .py_1p5()
                        .gap_2()
                        .items_center()
                        .rounded_lg()
                        .border_1()
                        .border_color(style::border())
                        .text_xs()
                        .child(div().size(px(6.)).rounded_full().bg(style::faint()))
                        .child(div().flex_1().text_color(style::muted()).child("The agent isn't running. Sending a message reconnects it."))
                        .child(
                            div()
                                .id("chat-reconnect")
                                .px_2()
                                .py_0p5()
                                .rounded_md()
                                .border_1()
                                .border_color(style::border())
                                .text_color(style::text())
                                .cursor_pointer()
                                .hover(|d| d.bg(style::panel_hover()))
                                .child("Reconnect")
                                .on_click(cx.listener(|this, _, _, cx| this.reconnect(cx))),
                        ),
                )
            })
            .when(self.status == ChatStatus::Connecting && !self.reconnects.is_empty(), |d| {
                d.child(
                    div()
                        .w_full()
                        .max_w(px(Self::COLUMN))
                        .mx_auto()
                        .text_xs()
                        .text_color(style::faint())
                        .child("Reconnecting to the agent…"),
                )
            })
            .child(
                v_flex()
                    .w_full()
                    .max_w(px(Self::COLUMN))
                    .mx_auto()
                    .rounded_xl()
                    .border_1()
                    .border_color(style::border())
                    .bg(style::panel())
                    .when(!self.attachments.is_empty(), |d| {
                        d.child(h_flex().px_3().pt_3().gap_2().flex_wrap().children(self.attachments.iter().enumerate().map(|(i, a)| {
                            div()
                                .relative()
                                .child(img_el(a.image.clone()).h(px(56.)).max_w(px(96.)).rounded_md())
                                .child(
                                    div()
                                        .id(("drop-image", i))
                                        .absolute()
                                        .top(px(-6.))
                                        .right(px(-6.))
                                        .p_0p5()
                                        .rounded_full()
                                        .bg(style::panel_hover())
                                        .border_1()
                                        .border_color(style::border())
                                        .cursor_pointer()
                                        .child(crate::icons::ui("x", px(10.), style::muted()))
                                        .on_click(cx.listener(move |this, _, _, cx| {
                                            this.attachments.remove(i);
                                            cx.notify();
                                        })),
                                )
                        })))
                    })
                    .child(div().px_3().pt_2().child(textarea))
                    .child(
                        h_flex()
                            .px_2()
                            .pb_2()
                            .gap_0p5()
                            .items_center()
                            .child(
                                div()
                                    .id("attach-image")
                                    .p_1()
                                    .rounded_md()
                                    .cursor_pointer()
                                    .hover(|d| d.bg(style::panel_hover()))
                                    .tooltip(|window, cx| Tooltip::new("Attach images (or paste with ⌘V)").build(window, cx))
                                    .child(crate::icons::ui("plus", px(14.), style::muted()))
                                    .on_click(cx.listener(|this, _, window, cx| this.pick_images(window, cx))),
                            )
                            .children(controls)
                            .child(
                                div()
                                    .id("detail-level")
                                    .p_1()
                                    .rounded_md()
                                    .cursor_pointer()
                                    .hover(|d| d.bg(style::panel_hover()))
                                    .tooltip(move |window, cx| Tooltip::new(detail_tip.clone()).build(window, cx))
                                    .child(crate::icons::ui(
                                        match self.detail {
                                            Detail::Normal => "eye-off",
                                            Detail::Thinking => "eye",
                                            Detail::Verbose => "scan-eye",
                                        },
                                        px(14.),
                                        if self.detail == Detail::Normal { style::faint() } else { style::accent() },
                                    ))
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.detail = this.detail.next();
                                        cx.notify();
                                    })),
                            )
                            .child(div().flex_1())
                            .when_some(self.context_meter(cx), |d, m| d.child(m))
                            .child(
                                div()
                                    .id("chat-send")
                                    .ml_1()
                                    .size(px(28.))
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .rounded_full()
                                    .cursor_pointer()
                                    .map(|d| {
                                        if running && !has_text {
                                            d.bg(style::text())
                                                .tooltip(|window, cx| Tooltip::new("Stop (Esc)").build(window, cx))
                                                .child(div().size(px(9.)).rounded_sm().bg(style::bg()))
                                                .on_click(cx.listener(|this, _, _, cx| this.stop(cx)))
                                        } else {
                                            d.when(has_text, |d| d.bg(style::accent()))
                                                .when(!has_text, |d| d.bg(style::panel_hover()))
                                                .tooltip(move |window, cx| {
                                                    Tooltip::new(if running { "Queue message (sent when the agent finishes)" } else { "Send (Enter)" }).build(window, cx)
                                                })
                                                .child(crate::icons::ui("arrow-up", px(15.), if has_text { style::bg() } else { style::faint() }))
                                                .on_click(cx.listener(|this, _, window, cx| this.send(window, cx)))
                                        }
                                    }),
                            ),
                    ),
            );

        let prompt_list = self.prompt_list.then(|| {
            let items: Vec<(usize, UserMsg)> = turns.iter().enumerate().filter_map(|(i, t)| t.user.clone().map(|u| (i, u))).collect();
            div()
                .id("prompt-list-backdrop")
                .absolute()
                .inset_0()
                .occlude()
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
            .when_some(self.relay.clone(), |d, relay| {
                d.child(
                    h_flex()
                        .px_4()
                        .py_1()
                        .gap_2()
                        .items_center()
                        .text_xs()
                        .border_b_1()
                        .border_color(style::border())
                        .bg(style::accent_alpha(0.05))
                        .child(crate::icons::ui("arrow-left-right", px(12.), style::accent()))
                        .child(div().text_color(style::muted()).child(relay.text.clone()))
                        .child(div().flex_1())
                        .when(relay.auto, |d| d.child(div().text_color(style::faint()).child(format!("{} round{} left", relay.rounds_left, if relay.rounds_left == 1 { "" } else { "s" }))))
                        .child(
                            h_flex()
                                .id("relay-auto")
                                .gap_1p5()
                                .items_center()
                                .cursor_pointer()
                                .text_color(style::muted())
                                .tooltip(|window, cx| {
                                    Tooltip::new("Auto rounds: when one side finishes, its answer is sent to the other (up to 3 rounds; stops when the reviewer finds nothing more)")
                                        .build(window, cx)
                                })
                                .child("Auto rounds")
                                .child(
                                    div()
                                        .w(px(24.))
                                        .h(px(14.))
                                        .p(px(2.))
                                        .rounded_full()
                                        .bg(if relay.auto { style::accent().into() } else { style::border() })
                                        .child(div().size(px(10.)).rounded_full().bg(style::text()).when(relay.auto, |d| d.ml(px(10.)))),
                                )
                                .on_click(cx.listener(move |_, _, _, cx| cx.emit(ChatEvent::RelayAuto(!relay.auto)))),
                        ),
                )
            })
            .when(self.viewing.is_some(), |d| {
                d.child(
                    h_flex()
                        .id("old-version")
                        .px_4()
                        .py_1()
                        .gap_2()
                        .text_xs()
                        .bg(style::soft(style::warn(), 0.08))
                        .text_color(style::warn())
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
            .when_some(self.plan_panel(cx), |d, p| d.child(div().px_6().child(p)))
            .child(body)
            .when_some(queue, |d, q| d.child(div().px_6().child(q)))
            .when_some(suggestions, |d, s| d.child(div().px_6().child(s)))
            .child(composer)
            .when_some(prompt_list, |d, l| d.child(l))
    }
}

/// `img()` de gpui para una imagen en memoria.
fn img_el(image: Arc<Image>) -> Img {
    img(image)
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
        let answer: String = t.items.iter().filter_map(|i| if let Item::Agent(s) = i { Some(s.as_ref()) } else { None }).collect::<Vec<_>>().join("");
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

/// Tiempo sin eventos tras el que se cierra el proceso de un agente que espera.
const IDLE_SLEEP: Duration = Duration::from_secs(30 * 60);

fn fmt_duration(d: Duration) -> String {
    let s = d.as_secs();
    if s < 60 { format!("{s}s") } else { format!("{}m {:02}s", s / 60, s % 60) }
}

/// Agrega un trozo que llegó por streaming a un texto compartido.
fn append(s: &mut SharedString, chunk: &str) {
    let mut text = String::with_capacity(s.len() + chunk.len());
    text.push_str(s);
    text.push_str(chunk);
    *s = text.into();
}

fn count_changes(d: &FileDiff) -> (usize, usize) {
    count_text_changes(d.old_text.as_deref(), &d.new_text)
}

/// Líneas agregadas y quitadas. Se pide en cada frame por cada tarjeta, así
/// que el resultado se recuerda por contenido: hashear es mucho más barato que
/// volver a calcular el diff.
fn count_text_changes(old: Option<&str>, new: &str) -> (usize, usize) {
    use std::hash::{Hash, Hasher};
    thread_local! {
        static MEMO: std::cell::RefCell<HashMap<u64, (usize, usize)>> = std::cell::RefCell::new(HashMap::new());
    }
    let mut h = std::collections::hash_map::DefaultHasher::new();
    old.hash(&mut h);
    new.hash(&mut h);
    let key = h.finish();
    if let Some(counts) = MEMO.with(|m| m.borrow().get(&key).copied()) {
        return counts;
    }
    let diff = similar::TextDiff::from_lines(old.unwrap_or(""), new);
    let mut added = 0;
    let mut removed = 0;
    for change in diff.iter_all_changes() {
        match change.tag() {
            similar::ChangeTag::Insert => added += 1,
            similar::ChangeTag::Delete => removed += 1,
            similar::ChangeTag::Equal => {}
        }
    }
    MEMO.with(|m| {
        let mut m = m.borrow_mut();
        if m.len() >= 4096 {
            m.clear();
        }
        m.insert(key, (added, removed));
    });
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
