//! Vista GPUI de una terminal de `kuaky-term`.

use crate::style;
use gpui_kit::component::ActiveTheme as _;
use gpui_kit::*;
use kuaky_term::{Key, Launch, Rgb, Size, TermEvent, Terminal, key_to_bytes};
use std::{cell::Cell, rc::Rc, time::Instant};

const PADDING: f32 = 10.;

pub enum TerminalViewEvent {
    TitleChanged,
    Exited,
}

pub struct TerminalView {
    term: Option<Terminal>,
    error: Option<String>,
    pub label: SharedString,
    pub title: Option<SharedString>,
    pub exited: bool,
    pub last_output: Option<Instant>,
    focus: FocusHandle,
    bounds: Rc<Cell<Option<Bounds<Pixels>>>>,
    cell: Option<(Pixels, Pixels)>,
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
    pub fn new(label: impl Into<SharedString>, launch: Launch, window: &mut Window, cx: &mut Context<Self>) -> Self {
        // Tamaño provisional; se ajusta en el primer pintado.
        let size = Size { cols: 100, rows: 30, cell_width: 8, cell_height: 18 };
        let (term, error) = match Terminal::spawn(&launch, size) {
            Ok(t) => (Some(t), None),
            Err(err) => (None, Some(format!("No se pudo abrir la terminal: {err:#}"))),
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
            exited: false,
            last_output: None,
            focus,
            bounds: Rc::new(Cell::new(None)),
            cell: None,
            selecting: false,
            mouse_reporting: false,
            scroll_acc: 0.,
            font_size: px(13.),
        }
    }

    fn on_term_event(&mut self, ev: kuaky_term::RawEvent, cx: &mut Context<Self>) {
        let Some(term) = &self.term else { return };
        match term.handle(ev) {
            Some(TermEvent::Wakeup) => self.last_output = Some(Instant::now()),
            Some(TermEvent::Title(t)) => {
                self.title = (!t.trim().is_empty()).then(|| t.trim().to_string().into());
                cx.emit(TerminalViewEvent::TitleChanged);
            }
            Some(TermEvent::Copy(text)) => cx.write_to_clipboard(ClipboardItem::new_string(text)),
            Some(TermEvent::Exited) => {
                self.exited = true;
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

    fn sync_size(&mut self, window: &mut Window, cx: &mut Context<Self>) -> (Pixels, Pixels) {
        let font = font(cx.theme().mono_font_family.clone());
        let font_id = window.text_system().resolve_font(&font);
        let cw = window
            .text_system()
            .advance(font_id, self.font_size, 'm')
            .map(|s| s.width)
            .unwrap_or(px(8.));
        let lh = (self.font_size * 1.4).round();
        self.cell = Some((cw, lh));
        if let (Some(bounds), Some(term)) = (self.bounds.get(), self.term.as_mut()) {
            let w = (bounds.size.width - px(PADDING * 2.)).max(px(0.));
            let h = (bounds.size.height - px(PADDING * 2.)).max(px(0.));
            let size = Size {
                cols: ((w / cw).floor() as u16).max(2),
                rows: ((h / lh).floor() as u16).max(1),
                cell_width: f32::from(cw) as u16,
                cell_height: f32::from(lh) as u16,
            };
            term.resize(size);
        }
        (cw, lh)
    }
}

fn hsla(c: Rgb) -> Hsla {
    rgb(((c.r as u32) << 16) | ((c.g as u32) << 8) | c.b as u32).into()
}

impl Render for TerminalView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let (_, lh) = self.sync_size(window, cx);
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
                    move |b, _, _| bounds.set(Some(b)),
                    |_, _, _, _| {},
                )
                .absolute()
                .inset_0(),
            )
            .children(self.error.clone().map(|e| div().text_color(style::bad()).child(e)))
            .children(rows)
    }
}
