//! Modo chat: conversación con un agente por ACP, con nuestra propia interfaz
//! (mensajes en Markdown, herramientas, permisos con botones y diffs).

use crate::style;
use gpui_kit::component::input::{InputEvent, Textarea, TextareaState};
use gpui_kit::component::text::TextView;
use gpui_kit::component::{h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use kuaky_acp::{Client, Event, FileDiff, PermissionRequest, PlanEntry, ToolCall};
use std::{path::PathBuf, sync::Arc};

/// Cómo arrancar el agente en modo ACP.
#[derive(Clone)]
pub struct ChatLaunch {
    pub label: &'static str,
    pub program: PathBuf,
    pub args: Vec<String>,
    pub cwd: PathBuf,
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
    /// Cambió el estado (para la barra de sesiones y los avisos).
    Status,
    NeedsYou(String),
    TurnDone,
}

enum Item {
    User(String),
    Agent(String),
    Thought(String),
    Tool(ToolCall),
    Plan(Vec<PlanEntry>),
    Permission { req: PermissionRequest, answer: Option<String> },
    Notice(String),
}

pub struct ChatView {
    pub launch: ChatLaunch,
    pub status: ChatStatus,
    client: Option<Arc<Client>>,
    session: Option<String>,
    items: Vec<Item>,
    input: Entity<TextareaState>,
    scroll: ScrollHandle,
    _sub: Subscription,
}

impl EventEmitter<ChatEvent> for ChatView {}

impl ChatView {
    pub fn new(launch: ChatLaunch, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let input = cx.new(|cx| {
            TextareaState::new(window, cx)
                .auto_grow(1, 8)
                .submit_on_enter(true)
                .placeholder(format!("Escribe a {}…  (Enter envía · ⇧Enter nueva línea)", launch.label))
        });
        let sub = cx.subscribe_in(&input, window, |this, _, ev: &InputEvent, window, cx| {
            if let InputEvent::PressEnter { shift: false, .. } = ev {
                this.send(window, cx);
            }
        });
        input.update(cx, |s, cx| s.focus(window, cx));
        let this = Self {
            launch,
            status: ChatStatus::Connecting,
            client: None,
            session: None,
            items: Vec::new(),
            input,
            scroll: ScrollHandle::new(),
            _sub: sub,
        };
        this.connect(cx);
        this
    }

    pub fn is_waiting_permission(&self) -> bool {
        self.items.iter().any(|i| matches!(i, Item::Permission { answer: None, .. }))
    }

    fn connect(&self, cx: &mut Context<Self>) {
        let launch = self.launch.clone();
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move {
                    let env = vec![("PATH".to_string(), kuaky_git::login_path().to_string_lossy().into_owned())];
                    let client = Client::spawn(&launch.program, &launch.args, &launch.cwd, &env)?;
                    let session = client.new_session(&launch.cwd)?;
                    anyhow::Ok((Arc::new(client), session))
                })
                .await;
            let (client, session) = match result {
                Ok(ok) => ok,
                Err(err) => {
                    this.update(cx, |this, cx| {
                        this.status = ChatStatus::Error(format!("{err:#}"));
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
                    this.client = Some(client);
                    this.session = Some(session);
                    this.status = ChatStatus::Ready;
                    cx.emit(ChatEvent::Status);
                    #[cfg(feature = "screenshot")]
                    if let Ok(prompt) = std::env::var("KUAKY_DEMO_CHAT_PROMPT") {
                        if let (Some(c), Some(s)) = (&this.client, &this.session) {
                            let _ = c.prompt(s, &prompt);
                            this.items.push(Item::User(prompt));
                            this.status = ChatStatus::Running;
                        }
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
                        for ev in batch {
                            this.on_event(ev, cx);
                        }
                        this.scroll.scroll_to_bottom();
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

    fn on_event(&mut self, ev: Event, cx: &mut Context<Self>) {
        match ev {
            Event::MessageChunk(t) => match self.items.last_mut() {
                Some(Item::Agent(s)) => s.push_str(&t),
                _ => self.items.push(Item::Agent(t)),
            },
            Event::ThoughtChunk(t) => match self.items.last_mut() {
                Some(Item::Thought(s)) => s.push_str(&t),
                _ => self.items.push(Item::Thought(t)),
            },
            Event::ToolCall(call) => self.items.push(Item::Tool(call)),
            Event::ToolCallUpdate(update) => {
                let existing = self.items.iter_mut().rev().find_map(|i| match i {
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
                    }
                    None => self.items.push(Item::Tool(update)),
                }
            }
            Event::Plan(entries) => match self.items.iter_mut().rev().find(|i| matches!(i, Item::Plan(_))) {
                Some(Item::Plan(p)) => *p = entries,
                _ => self.items.push(Item::Plan(entries)),
            },
            Event::Permission(req) => {
                cx.emit(ChatEvent::NeedsYou(req.title.clone()));
                self.items.push(Item::Permission { req, answer: None });
                cx.emit(ChatEvent::Status);
            }
            Event::TurnEnded(reason) => {
                self.status = ChatStatus::Ready;
                if reason != "end_turn" {
                    let msg = match reason.as_str() {
                        "cancelled" => "Detenido.".to_string(),
                        "max_tokens" => "Se alcanzó el límite de tokens de la respuesta.".to_string(),
                        "refusal" => "El agente se negó a continuar.".to_string(),
                        other => format!("Turno terminado: {other}"),
                    };
                    self.items.push(Item::Notice(msg));
                }
                cx.emit(ChatEvent::TurnDone);
                cx.emit(ChatEvent::Status);
            }
            Event::Error(e) => {
                self.status = ChatStatus::Ready;
                self.items.push(Item::Notice(format!("Error: {e}")));
                cx.emit(ChatEvent::Status);
            }
            Event::Exited => {
                self.status = ChatStatus::Exited;
                cx.emit(ChatEvent::Status);
            }
        }
    }

    fn send(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let text = self.input.read(cx).value().trim().to_string();
        if text.is_empty() || self.status != ChatStatus::Ready {
            return;
        }
        let (Some(client), Some(session)) = (&self.client, &self.session) else { return };
        if let Err(err) = client.prompt(session, &text) {
            self.items.push(Item::Notice(format!("No se pudo enviar: {err:#}")));
            cx.notify();
            return;
        }
        self.input.update(cx, |s, cx| s.set_value("", window, cx));
        self.items.push(Item::User(text));
        self.status = ChatStatus::Running;
        self.scroll.scroll_to_bottom();
        cx.emit(ChatEvent::Status);
        cx.notify();
    }

    fn stop(&mut self, cx: &mut Context<Self>) {
        if let (Some(client), Some(session)) = (&self.client, &self.session) {
            let _ = client.cancel(session);
        }
        // Cancelar también los permisos pendientes.
        for item in &mut self.items {
            if let Item::Permission { req, answer: answer @ None } = item {
                if let Some(c) = &self.client {
                    let _ = c.answer_permission(&req.request_id, None);
                }
                *answer = Some("cancelado".into());
            }
        }
        cx.notify();
    }

    fn answer(&mut self, index: usize, option: Option<(String, String)>, cx: &mut Context<Self>) {
        let Some(client) = self.client.clone() else { return };
        if let Some(Item::Permission { req, answer }) = self.items.get_mut(index) {
            if answer.is_some() {
                return;
            }
            let _ = client.answer_permission(&req.request_id, option.as_ref().map(|(id, _)| id.as_str()));
            *answer = Some(option.map(|(_, name)| name).unwrap_or_else(|| "cancelado".into()));
        }
        cx.emit(ChatEvent::Status);
        cx.notify();
    }

    fn render_item(&self, index: usize, item: &Item, cx: &mut Context<Self>) -> AnyElement {
        match item {
            Item::User(text) => h_flex()
                .justify_end()
                .child(
                    div()
                        .max_w(relative(0.8))
                        .px_3()
                        .py_2()
                        .rounded_lg()
                        .bg(style::panel_hover())
                        .text_sm()
                        .child(text.clone()),
                )
                .into_any_element(),
            Item::Agent(text) => div()
                .text_sm()
                .child(TextView::markdown(("agent-msg", index), text.clone()))
                .into_any_element(),
            Item::Thought(text) => div()
                .pl_3()
                .border_l_2()
                .border_color(style::border())
                .text_xs()
                .italic()
                .text_color(style::faint())
                .child(text.clone())
                .into_any_element(),
            Item::Notice(text) => div().text_xs().text_color(style::muted()).child(text.clone()).into_any_element(),
            Item::Plan(entries) => v_flex()
                .gap_0p5()
                .p_2()
                .rounded_md()
                .border_1()
                .border_color(style::border())
                .child(div().text_xs().text_color(style::muted()).child("PLAN"))
                .children(entries.iter().map(|e| {
                    let (mark, color): (&str, Hsla) = match e.status.as_str() {
                        "completed" => ("✓", style::good().into()),
                        "in_progress" => ("◐", style::accent().into()),
                        _ => ("○", style::faint().into()),
                    };
                    h_flex().gap_2().text_sm().child(div().text_color(color).child(mark)).child(e.content.clone())
                }))
                .into_any_element(),
            Item::Tool(call) => self.render_tool(index, call, cx),
            Item::Permission { req, answer } => {
                let pending = answer.is_none();
                v_flex()
                    .gap_2()
                    .p_3()
                    .rounded_lg()
                    .border_1()
                    .border_color(if pending { style::soft(style::bad(), 0.6) } else { style::border().into() })
                    .bg(if pending { style::soft(style::bad(), 0.06) } else { style::panel().into() })
                    .child(div().text_xs().text_color(style::muted()).child("PERMISO"))
                    .child(div().text_sm().child(req.title.clone()))
                    .map(|d| match answer {
                        Some(a) => d.child(div().text_xs().text_color(style::muted()).child(format!("Respondiste: {a}"))),
                        None => d.child(h_flex().gap_2().flex_wrap().children(req.options.iter().enumerate().map(|(oi, o)| {
                            let allow = o.kind.starts_with("allow");
                            let choice = (o.id.clone(), o.name.clone());
                            div()
                                .id(("perm", index * 16 + oi))
                                .px_3()
                                .py_1()
                                .rounded_md()
                                .text_sm()
                                .cursor_pointer()
                                .when(allow, |d| d.bg(style::accent()).text_color(style::bg()))
                                .when(!allow, |d| d.border_1().border_color(style::border()).hover(|d| d.bg(style::panel_hover())))
                                .child(o.name.clone())
                                .on_click(cx.listener(move |this, _, _, cx| this.answer(index, Some(choice.clone()), cx)))
                        }))),
                    })
                    .into_any_element()
            }
        }
    }

    fn render_tool(&self, index: usize, call: &ToolCall, cx: &mut Context<Self>) -> AnyElement {
        let status = call.status.as_deref().unwrap_or("pending");
        let (mark, color): (&str, Hsla) = match status {
            "completed" => ("✓", style::good().into()),
            "failed" => ("✕", style::bad().into()),
            "in_progress" => ("◐", style::accent().into()),
            _ => ("○", style::faint().into()),
        };
        let icon = match call.kind.as_deref() {
            Some("edit") => "✎",
            Some("read") => "◇",
            Some("execute") => "›_",
            Some("search") => "⌕",
            Some("fetch") => "⇣",
            Some("think") => "…",
            _ => "•",
        };
        v_flex()
            .gap_1()
            .px_3()
            .py_2()
            .rounded_md()
            .bg(style::panel())
            .border_1()
            .border_color(style::border())
            .child(
                h_flex()
                    .gap_2()
                    .text_sm()
                    .child(div().text_color(style::muted()).child(icon))
                    .child(div().flex_1().truncate().child(call.title.clone().unwrap_or_else(|| "Herramienta".into())))
                    .child(div().text_xs().text_color(color).child(mark)),
            )
            .children(call.diffs.iter().enumerate().map(|(di, d)| {
                let (added, removed) = count_changes(d);
                let file = d.path.rsplit('/').next().unwrap_or(&d.path).to_string();
                let diff = d.clone();
                h_flex()
                    .id(("tool-diff", index * 32 + di))
                    .gap_2()
                    .text_xs()
                    .cursor_pointer()
                    .text_color(style::muted())
                    .hover(|d| d.text_color(style::text()))
                    .child(format!("± {file}"))
                    .child(div().text_color(style::good()).child(format!("+{added}")))
                    .child(div().text_color(style::bad()).child(format!("−{removed}")))
                    .child(div().text_color(style::accent()).child("ver diff"))
                    .on_click(cx.listener(move |_, _, _, cx| {
                        cx.emit(ChatEvent::OpenDiff { title: format!("± {}", diff.path.rsplit('/').next().unwrap_or("")), patch: unified_patch(&diff) });
                    }))
            }))
            .when_some(call.text.clone().filter(|t| !t.trim().is_empty()), |d, t| {
                let short: String = t.lines().take(6).collect::<Vec<_>>().join("\n");
                d.child(div().text_xs().font_family(crate::style::mono_font()).text_color(style::faint()).child(short))
            })
            .into_any_element()
    }
}

impl Render for ChatView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let status_text = match &self.status {
            ChatStatus::Connecting => "conectando…".to_string(),
            ChatStatus::Ready if self.is_waiting_permission() => "necesita tu permiso".into(),
            ChatStatus::Ready => "listo".into(),
            ChatStatus::Running if self.is_waiting_permission() => "necesita tu permiso".into(),
            ChatStatus::Running => "trabajando…".into(),
            ChatStatus::Error(e) => format!("error: {e}"),
            ChatStatus::Exited => "el agente terminó".into(),
        };
        let running = self.status == ChatStatus::Running;
        let items: Vec<AnyElement> = self
            .items
            .iter()
            .enumerate()
            .map(|(i, item)| self.render_item(i, item, cx))
            .collect();
        v_flex()
            .size_full()
            .child(
                h_flex()
                    .h(px(30.))
                    .px_3()
                    .gap_2()
                    .text_xs()
                    .border_b_1()
                    .border_color(style::border())
                    .child(div().text_color(style::text()).child(format!("{} · chat (ACP)", self.launch.label)))
                    .child(div().text_color(style::muted()).child(status_text))
                    .child(div().flex_1())
                    .child(div().text_color(style::faint()).child(crate::home::tilde(&self.launch.cwd))),
            )
            .child(
                div()
                    .id("chat-scroll")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .track_scroll(&self.scroll)
                    .child(
                        v_flex()
                            .max_w(px(860.))
                            .mx_auto()
                            .w_full()
                            .p_4()
                            .gap_3()
                            .when(self.items.is_empty(), |d| {
                                d.child(div().pt_8().text_sm().text_color(style::muted()).child(match &self.status {
                                    ChatStatus::Connecting => format!("Conectando con {}…", self.launch.label),
                                    ChatStatus::Error(e) => format!("No se pudo iniciar el chat: {e}"),
                                    _ => format!("Escríbele a {} abajo. Trabaja en {}.", self.launch.label, crate::home::tilde(&self.launch.cwd)),
                                }))
                            })
                            .children(items),
                    ),
            )
            .child(
                h_flex()
                    .p_3()
                    .gap_2()
                    .items_end()
                    .border_t_1()
                    .border_color(style::border())
                    .child(div().flex_1().child(Textarea::new(&self.input)))
                    .child(
                        div()
                            .id("chat-action")
                            .px_3()
                            .py_1p5()
                            .rounded_md()
                            .text_sm()
                            .cursor_pointer()
                            .map(|d| {
                                if running {
                                    d.border_1().border_color(style::border()).child("Detener").on_click(cx.listener(|this, _, _, cx| this.stop(cx)))
                                } else {
                                    d.bg(style::accent())
                                        .text_color(style::bg())
                                        .child("Enviar")
                                        .on_click(cx.listener(|this, _, window, cx| this.send(window, cx)))
                                }
                            }),
                    ),
            )
    }
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
    use super::{FileDiff, count_changes, unified_patch};

    #[test]
    fn builds_patch_parsed_by_diff_view() {
        let d = FileDiff { path: "/tmp/a.txt".into(), old_text: Some("uno\ndos\n".into()), new_text: "uno\ntres\n".into() };
        let patch = unified_patch(&d);
        assert!(patch.contains("-dos") && patch.contains("+tres"));
        assert_eq!(count_changes(&d), (1, 1));
        let files = gpui_kit::component::diff::DiffFile::parse(&patch).unwrap();
        assert_eq!(files.len(), 1);
    }
}
