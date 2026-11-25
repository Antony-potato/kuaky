//! Vista GPUI de una terminal de `kuaky-term`.

use crate::agent_status::{self, AgentState};
use crate::style;
use gpui_kit::component::ActiveTheme as _;
use gpui_kit::*;
use kuaky_term::{Key, Launch, Rgb, Size, TermEvent, Terminal, key_to_bytes};
use std::{
    cell::Cell,
    rc::Rc,
    time::{Duration, Instant},
};

const PADDING: f32 = 10.;
const RESIZE_DEBOUNCE: Duration = Duration::from_millis(120);

pub enum TerminalViewEvent {
    TitleChanged,
    /// The user sent a prompt (full text, from agent hooks).
    Prompt(String),
    Exited,
    /// El agente cambió de estado (según sus hooks).
    AgentState,
}

pub struct TerminalView {
    term: Option<Terminal>,
    error: Option<String>,
    pub label: SharedString,
    pub title: Option<SharedString>,
    /// First user prompt reported by agent hooks (fallback auto-title).
    pub first_prompt: Option<SharedString>,
    pub exited: bool,
    pub last_output: Option<Instant>,
    pub started_at: jiff::Timestamp,
    /// The agent's conversation ID (from hooks, or the one being resumed).
    pub agent_session: Option<String>,
    /// Stable kuaky session ID (links the session to its metro line).
    pub uid: String,
    pub state_changed_at: Option<jiff::Timestamp>,
    /// Estado reportado por los hooks del agente, si los tiene.
    pub agent_state: Option<AgentState>,
    pub previous_state: Option<AgentState>,
    focus: FocusHandle,
    bounds: Rc<Cell<Option<Bounds<Pixels>>>>,
    cell: Option<(Pixels, Pixels)>,
    sized: bool,
    pending_size: Option<Size>,
    resize_task: Option<Task<()>>,
    selecting: bool,
    /// El botón está presionado y se está enviando al programa.
    mouse_reporting: bool,
    scroll_acc: f32,
    font_size: Pixels,
}

impl EventEmitter<TerminalViewEvent> for TerminalView {}

impl Focusable for TerminalView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl TerminalView {
    pub fn new(
        label: impl Into<SharedString>,
        launch: Launch,
        status_file: Option<std::path::PathBuf>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        if let Some(path) = status_file {
            Self::watch_status(path, cx);
        }
        // Tamaño provisional; se ajusta en el primer pintado.
        let size = Size { cols: 100, rows: 30, cell_width: 8, cell_height: 18 };
        let (term, error) = match Terminal::spawn(&launch, size) {
            Ok(t) => (Some(t), None),
            Err(err) => (None, Some(format!("Failed to open terminal: {err:#}"))),
        };
        if let Some(term) = &term {
            let rx = term.events();
            cx.spawn(async move |this, cx| {
                while let Ok(first) = rx.recv_async().await {
                    let mut batch = vec![first];
                    batch.extend(rx.try_iter());
                    let alive = this
                        .update(cx, |this, cx| {
                            for ev in batch {
                                this.on_term_event(ev, cx);
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
        let focus = cx.focus_handle();
        window.focus(&focus, cx);
        Self {
            term,
            error,
            label: label.into(),
            title: None,
            first_prompt: None,
            exited: false,
            last_output: None,
            started_at: jiff::Timestamp::now(),
            agent_session: None,
            uid: new_uid(),
            state_changed_at: None,
            agent_state: None,
            previous_state: None,
            focus,
            bounds: Rc::new(Cell::new(None)),
            cell: None,
            sized: false,
            pending_size: None,
            resize_task: None,
            selecting: false,
            mouse_reporting: false,
            scroll_acc: 0.,
            font_size: px(13.),
        }
    }

    /// Automatic display title: terminal title (OSC) → first user prompt → harness label.
    pub fn auto_title(&self) -> SharedString {
        if let Some(t) = &self.title {
            if !t.is_empty() {
                return t.clone();
            }
        }
        if let Some(p) = &self.first_prompt {
            if !p.is_empty() {
                return p.clone();
            }
        }
        self.label.clone()
    }

    /// Direcciones locales que se ven en la terminal (un servidor de desarrollo, por ejemplo).
    pub fn local_urls(&self) -> Vec<String> {
        static URL: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
        let re = URL.get_or_init(|| regex::Regex::new(r#"https?://(?:localhost|127\.0\.0\.1|0\.0\.0\.0|\[::1\])(?::\d+)?[^\s"'<>)]*"#).unwrap());
        let Some(term) = &self.term else { return Vec::new() };
        let text: String = term.snapshot().lines.iter().map(|l| l.iter().map(|c| c.ch).chain(['\n']).collect::<String>()).collect();
        let mut out: Vec<String> = Vec::new();
        for m in re.find_iter(&text) {
            let url = m.as_str().trim_end_matches(['.', ',', ';', ':']).replace("0.0.0.0", "localhost");
            if !out.contains(&url) {
                out.push(url);
            }
        }
        out
    }

    /// Writes text into the program's input without pressing Enter.
    pub fn paste_text(&self, text: &str) {
        if let Some(term) = &self.term {
            term.paste(text);
        }
    }

    /// Pastes `text` and presses Enter (sends it to the agent).
    pub fn submit_text(&self, text: &str) {
        if let Some(term) = &self.term {
            term.paste(text);
            term.write(b"\r".to_vec());
        }
    }

    /// Pastes `text` once the program has started and its output settled
    /// (agents ignore input while they boot). Gives up after 30 s.
    pub fn paste_when_ready(&mut self, text: String, cx: &mut Context<Self>) {
        cx.spawn(async move |this, cx| {
            let start = Instant::now();
            loop {
                cx.background_executor().timer(std::time::Duration::from_millis(300)).await;
                let ready = this
                    .update(cx, |t, _| {
                        t.last_output.is_some_and(|at| at.elapsed() > std::time::Duration::from_millis(1200))
                            && start.elapsed() > std::time::Duration::from_millis(1500)
                    })
                    .unwrap_or(true);
                if ready || start.elapsed() > std::time::Duration::from_secs(30) {
                    let _ = this.update(cx, |t, cx| {
                        t.paste_text(&text);
                        cx.notify();
                    });
                    break;
                }
            }
        })
        .detach();
    }

    pub fn last_activity_at(&self) -> jiff::Timestamp {
        self.state_changed_at.unwrap_or(self.started_at)
    }

    /// Lee las líneas nuevas del archivo de estado cada 300 ms.
    fn watch_status(path: std::path::PathBuf, cx: &mut Context<Self>) {
        cx.spawn(async move |this, cx| {
            let mut offset = 0usize;
            loop {
                cx.background_executor().timer(std::time::Duration::from_millis(300)).await;
                let text = std::fs::read(&path).unwrap_or_default();
                if text.len() > offset {
                    let new = String::from_utf8_lossy(&text[offset..]).into_owned();
                    // Solo líneas completas.
                    let consumed = new.rfind('\n').map(|i| i + 1).unwrap_or(0);
                    offset += consumed;
                    let chunk = &new[..consumed];
                    let prompt = chunk.lines().find_map(agent_status::parse_prompt);
                    let prompts: Vec<String> = chunk.lines().filter_map(agent_status::parse_prompt_text).collect();
                    let session = chunk.lines().filter_map(agent_status::parse_session_id).last();
                    let latest = chunk.lines().filter_map(agent_status::parse_line).last();
                    if prompt.is_some() || latest.is_some() || session.is_some() || !prompts.is_empty() {
                        let alive = this
                            .update(cx, |this, cx| {
                                let mut changed = false;
                                if session.is_some() && this.agent_session != session {
                                    this.agent_session = session;
                                    cx.emit(TerminalViewEvent::TitleChanged);
                                    changed = true;
                                }
                                for text in prompts {
                                    cx.emit(TerminalViewEvent::Prompt(text));
                                }
                                if let Some(p) = prompt {
                                    if this.first_prompt.is_none() {
                                        this.first_prompt = Some(p.into());
                                        cx.emit(TerminalViewEvent::TitleChanged);
                                        changed = true;
                                    }
                                }
                                if let Some(state) = latest {
                                    if this.agent_state.as_ref() != Some(&state) {
                                        this.previous_state = this.agent_state.replace(state);
                                        this.state_changed_at = Some(jiff::Timestamp::now());
                                        cx.emit(TerminalViewEvent::AgentState);
                                        changed = true;
                                    }
                                }
                                if changed {
                                    cx.notify();
                                }
                            })
                            .is_ok();
                        if !alive {
                            break;
                        }
                    }
                }
                if this.upgrade().is_none() {
                    break;
                }
            }
            std::fs::remove_file(&path).ok();
        })
        .detach();
    }

    fn on_term_event(&mut self, ev: kuaky_term::RawEvent, cx: &mut Context<Self>) {
        let Some(term) = &self.term else { return };
        match term.handle(ev) {
            Some(TermEvent::Wakeup) => {
                self.last_output = Some(Instant::now());
                if self.agent_state.is_none() {
                    self.state_changed_at = Some(jiff::Timestamp::now());
                }
            }
            Some(TermEvent::Title(t)) => {
                let cleaned = clean_term_title(&t);
                self.title = cleaned.map(Into::into);
                cx.emit(TerminalViewEvent::TitleChanged);
            }
            Some(TermEvent::Copy(text)) => cx.write_to_clipboard(ClipboardItem::new_string(text)),
            Some(TermEvent::Exited) => {
                self.exited = true;
                self.state_changed_at = Some(jiff::Timestamp::now());
                cx.emit(TerminalViewEvent::Exited);
            }
            Some(TermEvent::Bell) | None => {}
        }
    }

    fn cell_at(&self, position: Point<Pixels>) -> Option<(usize, usize)> {
        let bounds = self.bounds.get()?;
        let (cw, lh) = self.cell?;
        let x = (position.x - bounds.origin.x - px(PADDING)).max(px(0.));
        let y = (position.y - bounds.origin.y - px(PADDING)).max(px(0.));
        Some(((y / lh) as usize, (x / cw) as usize))
    }

    fn on_key(&mut self, ev: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let Some(term) = &self.term else { return };
        let ks = &ev.keystroke;
        let m = ks.modifiers;
        let copy_combo = (m.platform && ks.key == "c") || (m.control && m.shift && ks.key == "c");
        let paste_combo = (m.platform && ks.key == "v") || (m.control && m.shift && ks.key == "v");
        if copy_combo {
            if let Some(text) = term.selected_text() {
                cx.write_to_clipboard(ClipboardItem::new_string(text));
                cx.stop_propagation();
            }
            return;
        }
        if paste_combo {
            if let Some(text) = cx.read_from_clipboard().and_then(|c| c.text()) {
                term.paste(&text);
            }
            cx.stop_propagation();
            return;
        }
        if m.platform {
            // Atajos de la app (cmd-w, cmd-t, …).
            return;
        }
        let key = Key {
            name: &ks.key,
            text: ks.key_char.as_deref(),
            ctrl: m.control,
            alt: m.alt,
            shift: m.shift,
        };
        if let Some(bytes) = key_to_bytes(&key, term.app_cursor()) {
            term.clear_selection();
            term.scroll_to_bottom();
            term.write(bytes);
            cx.stop_propagation();
            cx.notify();
        }
        let _ = window;
    }

    fn measure_cell(&mut self, window: &mut Window, cx: &mut Context<Self>) -> (Pixels, Pixels) {
        let font = font(cx.theme().mono_font_family.clone());
        let font_id = window.text_system().resolve_font(&font);
        let cw = window
            .text_system()
            .advance(font_id, self.font_size, 'm')
            .map(|s| s.width)
            .unwrap_or(px(8.));
        let lh = (self.font_size * 1.4).round();
        self.cell = Some((cw, lh));
        (cw, lh)
    }

    fn target_size(&self) -> Option<Size> {
        let bounds = self.bounds.get()?;
        let (cw, lh) = self.cell?;
        size_for_bounds(bounds.size.width, bounds.size.height, cw, lh)
    }

    fn apply_resize(&mut self, size: Size, cx: &mut Context<Self>) {
        let Some(term) = self.term.as_mut() else { return };
        let at_bottom = !term.scrolled_back();
        term.resize(size);
        if at_bottom && !term.alt_screen() {
            term.scroll_to_bottom();
        }
        self.sized = true;
        cx.notify();
    }

    /// Aplica el tamaño inicial de inmediato y amortigua los redimensionados
    /// posteriores (~120 ms) para evitar que las TUIs brinquen al arrastrar.
    fn schedule_resize(&mut self, cx: &mut Context<Self>) {
        let Some(target) = self.target_size() else { return };
        let Some(current) = self.term.as_ref().map(|t| t.size()) else { return };
        if target == current {
            self.sized = true;
            self.pending_size = None;
            self.resize_task = None;
            return;
        }
        if !self.sized {
            self.apply_resize(target, cx);
            return;
        }
        if self.pending_size == Some(target) {
            return;
        }
        self.pending_size = Some(target);
        self.resize_task = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(RESIZE_DEBOUNCE).await;
            this.update(cx, |this, cx| {
                this.pending_size = None;
                if let Some(latest) = this.target_size() {
                    this.apply_resize(latest, cx);
                }
            })
            .ok();
        }));
    }
}

fn size_for_bounds(width: Pixels, height: Pixels, cw: Pixels, lh: Pixels) -> Option<Size> {
    let w = width - px(PADDING * 2.);
    let h = height - px(PADDING * 2.);
    if w <= px(1.) || h <= px(1.) || cw <= px(0.) || lh <= px(0.) {
        return None;
    }
    let cols = (w / cw).floor() as u16;
    let rows = (h / lh).floor() as u16;
    if cols < 2 || rows < 1 {
        return None;
    }
    Some(Size {
        cols,
        rows,
        cell_width: f32::from(cw) as u16,
        cell_height: f32::from(lh) as u16,
    })
}

fn clean_term_title(raw: &str) -> Option<String> {
    let trimmed = raw
        .trim()
        .trim_start_matches(|c: char| matches!(c, '✳' | '⠋' | '⠙' | '⠹' | '⠸' | '⠼' | '⠴' | '⠦' | '⠧' | '⠇' | '⠏' | '⠐' | '⠂' | '⠁' | '⠈' | '⠠' | '⠄' | '•' | '·'))
        .trim();
    (!trimmed.is_empty()).then(|| trimmed.to_string())
}

fn hsla(c: Rgb) -> Hsla {
    rgb(((c.r as u32) << 16) | ((c.g as u32) << 8) | c.b as u32).into()
}

impl Render for TerminalView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let (_, lh) = self.measure_cell(window, cx);
        let focused = self.focus.is_focused(window);
        let font_family = cx.theme().mono_font_family.clone();
        let base_font = font(font_family.clone());

        let mut rows: Vec<AnyElement> = Vec::new();
        if let Some(term) = &self.term {
            let snap = term.snapshot();
            for (r, line) in snap.lines.iter().enumerate() {
                let mut text = String::new();
                let mut runs: Vec<TextRun> = Vec::new();
                for (c, cell) in line.iter().enumerate() {
                    if cell.spacer {
                        continue;
                    }
                    let is_cursor = snap.cursor == Some((r, c)) && !snap.scrolled_back;
                    let (mut fg, mut bg) = (hsla(cell.fg), cell.bg.map(hsla));
                    if cell.dim {
                        fg.a = 0.66;
                    }
                    if is_cursor {
                        if focused {
                            bg = Some(style::accent().into());
                            fg = style::bg().into();
                        } else {
                            bg = Some(style::soft(style::accent(), 0.35));
                        }
                    }
                    let ch = if cell.ch == '\0' { ' ' } else { cell.ch };
                    let start = text.len();
                    text.push(ch);
                    let len = text.len() - start;
                    let mut f = base_font.clone();
                    if cell.bold {
                        f.weight = FontWeight::BOLD;
                    }
                    if cell.italic {
                        f.style = FontStyle::Italic;
                    }
                    let underline = cell.underline.then(|| UnderlineStyle {
                        thickness: px(1.),
                        color: Some(fg),
                        wavy: false,
                    });
                    match runs.last_mut() {
                        Some(last)
                            if last.color == fg
                                && last.background_color == bg
                                && last.font == f
                                && last.underline == underline =>
                        {
                            last.len += len;
                        }
                        _ => runs.push(TextRun {
                            len,
                            font: f,
                            color: fg,
                            background_color: bg,
                            underline,
                            strikethrough: None,
                        }),
                    }
                }
                rows.push(
                    div()
                        .h(lh)
                        .whitespace_nowrap()
                        .overflow_hidden()
                        .child(StyledText::new(text).with_runs(runs))
                        .into_any_element(),
                );
            }
        }

        let bounds = self.bounds.clone();
        let entity = cx.entity().downgrade();
        div()
            .id("terminal")
            .key_context("Terminal")
            .track_focus(&self.focus)
            .size_full()
            .relative()
            .bg(style::bg())
            .p(px(PADDING))
            .font_family(font_family)
            .text_size(self.font_size)
            .line_height(lh)
            .text_color(style::text())
            .cursor(CursorStyle::IBeam)
            .on_key_down(cx.listener(Self::on_key))
            .on_action(cx.listener(|this, _: &gpui_kit::component::input::Copy, _, cx| {
                if let Some(text) = this.term.as_ref().and_then(|t| t.selected_text()) {
                    cx.write_to_clipboard(ClipboardItem::new_string(text));
                }
            }))
            .on_action(cx.listener(|this, _: &gpui_kit::component::input::Paste, _, cx| {
                if let (Some(term), Some(text)) = (&this.term, cx.read_from_clipboard().and_then(|c| c.text())) {
                    term.paste(&text);
                }
            }))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, ev: &MouseDownEvent, window, cx| {
                    this.focus.focus(window, cx);
                    if let (Some((row, col)), Some(term)) = (this.cell_at(ev.position), &this.term) {
                        // Con Shift se selecciona texto aunque el programa use el ratón.
                        if term.mouse_mode() && !ev.modifiers.shift {
                            term.send_mouse(0, row, col, true);
                            this.mouse_reporting = true;
                            return;
                        }
                        if ev.click_count >= 2 {
                            term.start_selection(row, col, true);
                        } else {
                            term.clear_selection();
                            term.start_selection(row, col, false);
                        }
                        this.selecting = true;
                    }
                    cx.notify();
                }),
            )
            .on_mouse_move(cx.listener(|this, ev: &MouseMoveEvent, _, cx| {
                if this.mouse_reporting && ev.dragging() {
                    if let (Some((row, col)), Some(term)) = (this.cell_at(ev.position), &this.term) {
                        term.send_mouse(32, row, col, true);
                    }
                    return;
                }
                if this.selecting && ev.dragging() {
                    if let (Some((row, col)), Some(term)) = (this.cell_at(ev.position), &this.term) {
                        term.update_selection(row, col);
                        cx.notify();
                    }
                }
            }))
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|this, ev: &MouseUpEvent, _, cx| {
                    if this.mouse_reporting {
                        this.mouse_reporting = false;
                        if let (Some((row, col)), Some(term)) = (this.cell_at(ev.position), &this.term) {
                            term.send_mouse(0, row, col, false);
                        }
                        return;
                    }
                    this.selecting = false;
                    if let Some(term) = &this.term {
                        if term.selected_text().is_none() {
                            term.clear_selection();
                        }
                    }
                    cx.notify();
                }),
            )
            .on_scroll(cx.listener(move |this, ev: &ScrollEvent, _, cx| {
                let Some(term) = &this.term else { return };
                let dy = f32::from(ev.delta.pixel_delta(lh).y);
                this.scroll_acc += dy / f32::from(lh);
                let lines = this.scroll_acc.trunc() as i32;
                if lines == 0 {
                    return;
                }
                this.scroll_acc -= lines as f32;
                if term.mouse_mode() {
                    let (row, col) = this.cell_at(ev.position).unwrap_or((0, 0));
                    let button = if lines > 0 { 64 } else { 65 };
                    for _ in 0..lines.unsigned_abs() {
                        term.send_mouse(button, row, col, true);
                    }
                } else if term.alt_screen() {
                    let key = if lines > 0 { "\x1b[A" } else { "\x1b[B" };
                    term.write(key.repeat(lines.unsigned_abs() as usize).into_bytes());
                } else {
                    term.scroll(lines);
                }
                cx.notify();
            }))
            .child(
                canvas(
                    move |b, _, cx| {
                        if bounds.get() != Some(b) {
                            bounds.set(Some(b));
                            let entity = entity.clone();
                            cx.defer(move |cx| {
                                let _ = entity.update(cx, |this, cx| this.schedule_resize(cx));
                            });
                        }
                    },
                    |_, _, _, _| {},
                )
                .absolute()
                .inset_0(),
            )
            .children(self.error.clone().map(|e| div().text_color(style::bad()).child(e)))
            .children(rows)
    }
}

#[cfg(test)]
mod tests {
    use super::size_for_bounds;
    use gpui_kit::px;

    #[test]
    fn ignores_zero_and_hidden_bounds() {
        assert!(size_for_bounds(px(0.), px(0.), px(8.), px(18.)).is_none());
        assert!(size_for_bounds(px(20.), px(500.), px(8.), px(18.)).is_none());
        assert!(size_for_bounds(px(500.), px(10.), px(8.), px(18.)).is_none());
        let s = size_for_bounds(px(820.), px(380.), px(8.), px(18.)).unwrap();
        assert_eq!((s.cols, s.rows), (100, 20));
    }
}

/// Short unique ID for a session (`s` + milliseconds + counter).
pub fn new_uid() -> String {
    use std::sync::atomic::{AtomicU32, Ordering};
    static COUNTER: AtomicU32 = AtomicU32::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    format!("s{}{n:03}", jiff::Timestamp::now().as_millisecond())
}
