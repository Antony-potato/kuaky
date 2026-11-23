//! Terminal embebida: abre un PTY con el shell del usuario y mantiene el
//! estado de la pantalla con `alacritty_terminal` (Apache-2.0).
//!
//! Este crate no sabe nada de la interfaz: expone una [`Snapshot`] con las
//! celdas a pintar y recibe bytes de entrada.

mod keys;
mod palette;

pub use alacritty_terminal::event::Event as RawEvent;
pub use keys::{Key, key_to_bytes};
pub use palette::Rgb;

use alacritty_terminal::{
    event::{Event, EventListener, Notify, OnResize, WindowSize},
    event_loop::{EventLoop, Notifier},
    grid::{Dimensions, Scroll},
    index::{Column, Line, Point, Side},
    selection::{Selection, SelectionType},
    sync::FairMutex,
    term::{Config, Term, TermMode, cell::Flags},
    tty::{self, Options, Shell},
    vte::ansi::{Color, NamedColor},
};
use std::{
    collections::HashMap,
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
};

/// Lo que pasa en la terminal y la interfaz debe saber.
#[derive(Debug, Clone)]
pub enum TermEvent {
    /// Hay contenido nuevo que pintar.
    Wakeup,
    Title(String),
    Bell,
    /// El programa pidió copiar texto al portapapeles (OSC 52).
    Copy(String),
    /// El proceso terminó.
    Exited,
}

/// Qué ejecutar en la terminal.
#[derive(Debug, Clone)]
pub struct Launch {
    pub cwd: PathBuf,
    /// Comando que se escribe en el shell al abrirse (p. ej. `claude`).
    /// Se escribe en el shell —y no se ejecuta directo— para que el agente
    /// herede el mismo entorno que tendrías en tu terminal (PATH, login, etc.).
    pub command: Option<String>,
    /// Texto que se deja escrito en el shell sin ejecutarlo (p. ej. el comando
    /// para instalar un agente que falta).
    pub prefill: Option<String>,
}

#[derive(Clone)]
struct Listener {
    tx: flume::Sender<Event>,
}

impl EventListener for Listener {
    fn send_event(&self, event: Event) {
        let _ = self.tx.send(event);
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Size {
    pub cols: u16,
    pub rows: u16,
    pub cell_width: u16,
    pub cell_height: u16,
}

impl Dimensions for Size {
    fn total_lines(&self) -> usize {
        self.rows as usize
    }
    fn screen_lines(&self) -> usize {
        self.rows as usize
    }
    fn columns(&self) -> usize {
        self.cols as usize
    }
}

impl Size {
    fn window_size(self) -> WindowSize {
        WindowSize {
            num_lines: self.rows,
            num_cols: self.cols,
            cell_width: self.cell_width,
            cell_height: self.cell_height,
        }
    }
}

pub struct Terminal {
    term: Arc<FairMutex<Term<Listener>>>,
    notifier: Notifier,
    events: flume::Receiver<Event>,
    size: Size,
}

static NEXT_ID: AtomicU64 = AtomicU64::new(1);

impl Terminal {
    pub fn spawn(launch: &Launch, size: Size) -> anyhow::Result<Self> {
        let (tx, rx) = flume::unbounded();
        let listener = Listener { tx };

        let mut env = HashMap::new();
        env.insert("TERM".into(), "xterm-256color".into());
        env.insert("COLORTERM".into(), "truecolor".into());
        env.insert("TERM_PROGRAM".into(), "kuaky".into());
        let options = Options {
            shell: Some(shell_for(launch.command.as_deref())),
            working_directory: Some(launch.cwd.clone()),
            drain_on_exit: true,
            env,
            #[cfg(target_os = "windows")]
            escape_args: true,
        };
        let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
        let pty = tty::new(&options, size.window_size(), id)?;

        let config = Config {
            scrolling_history: 10_000,
            ..Config::default()
        };
        let term = Arc::new(FairMutex::new(Term::new(config, &size, listener.clone())));
        let event_loop = EventLoop::new(term.clone(), listener, pty, true, false)?;
        let notifier = Notifier(event_loop.channel());
        event_loop.spawn();

        let terminal = Self { term, notifier, events: rx, size };
        if launch.command.is_none() && let Some(text) = &launch.prefill {
            terminal.write(text.clone().into_bytes());
        }
        Ok(terminal)
    }

    /// Canal de eventos crudos; la interfaz lo consume con [`Terminal::handle`].
    pub fn events(&self) -> flume::Receiver<Event> {
        self.events.clone()
    }

    /// Procesa un evento interno y devuelve lo que la interfaz debe hacer.
    pub fn handle(&self, event: Event) -> Option<TermEvent> {
        match event {
            Event::Wakeup | Event::MouseCursorDirty | Event::CursorBlinkingChange => Some(TermEvent::Wakeup),
            Event::Title(t) => Some(TermEvent::Title(t)),
            Event::ResetTitle => Some(TermEvent::Title(String::new())),
            Event::Bell => Some(TermEvent::Bell),
            Event::ClipboardStore(_, text) => Some(TermEvent::Copy(text)),
            Event::PtyWrite(text) => {
                self.write(text.into_bytes());
                None
            }
            Event::TextAreaSizeRequest(format) => {
                self.write(format(self.size.window_size()).into_bytes());
                None
            }
            Event::ColorRequest(index, format) => {
                let rgb = palette::color_for_index(index);
                self.write(
                    format(alacritty_terminal::vte::ansi::Rgb { r: rgb.r, g: rgb.g, b: rgb.b }).into_bytes(),
                );
                None
            }
            Event::ClipboardLoad(..) => None,
            Event::Exit | Event::ChildExit(_) => Some(TermEvent::Exited),
        }
    }

    pub fn write(&self, bytes: Vec<u8>) {
        self.notifier.notify(bytes);
    }

    /// Pega texto respetando el modo "bracketed paste" si el programa lo pidió.
    pub fn paste(&self, text: &str) {
        let bracketed = self.term.lock().mode().contains(TermMode::BRACKETED_PASTE);
        let text = text.replace("\r\n", "\r").replace('\n', "\r");
        if bracketed {
            self.write(format!("\x1b[200~{}\x1b[201~", text.replace('\x1b', "")).into_bytes());
        } else {
            self.write(text.into_bytes());
        }
        self.scroll_to_bottom();
    }

    pub fn resize(&mut self, size: Size) {
        if size == self.size || size.cols < 2 || size.rows < 1 {
            return;
        }
        self.size = size;
        self.notifier.on_resize(size.window_size());
        self.term.lock().resize(size);
    }

    pub fn size(&self) -> Size {
        self.size
    }

    pub fn app_cursor(&self) -> bool {
        self.term.lock().mode().contains(TermMode::APP_CURSOR)
    }

    /// El programa pidió recibir eventos del ratón (Claude Code, vim, htop…).
    pub fn mouse_mode(&self) -> bool {
        self.term.lock().mode().intersects(TermMode::MOUSE_MODE)
    }

    /// Envía un evento de ratón en formato SGR (1006) o el clásico X10.
    /// `button`: 0 izquierdo, 1 medio, 2 derecho, 64 rueda arriba, 65 rueda abajo,
    /// +32 si es arrastre.
    pub fn send_mouse(&self, button: u8, row: usize, col: usize, pressed: bool) {
        let sgr = self.term.lock().mode().contains(TermMode::SGR_MOUSE);
        let (x, y) = (col + 1, row + 1);
        let bytes = if sgr {
            format!("\x1b[<{button};{x};{y}{}", if pressed { 'M' } else { 'm' }).into_bytes()
        } else {
            let b = if pressed { button } else { 3 };
            if x > 222 || y > 222 {
                return;
            }
            vec![0x1b, b'[', b'M', 32 + b, 32 + x as u8, 32 + y as u8]
        };
        self.write(bytes);
    }

    /// Programas de pantalla completa (vim, less, htop) usan la pantalla alterna.
    pub fn alt_screen(&self) -> bool {
        self.term.lock().mode().contains(TermMode::ALT_SCREEN)
    }

    pub fn scroll(&self, lines: i32) {
        self.term.lock().scroll_display(Scroll::Delta(lines));
    }

    pub fn scroll_to_bottom(&self) {
        self.term.lock().scroll_display(Scroll::Bottom);
    }

    // ── selección ───────────────────────────────────────────────────────

    fn point(&self, term: &Term<Listener>, row: usize, col: usize) -> Point {
        let offset = term.grid().display_offset() as i32;
        let col = col.min(self.size.cols.saturating_sub(1) as usize);
        Point::new(Line(row as i32 - offset), Column(col))
    }

    pub fn start_selection(&self, row: usize, col: usize, words: bool) {
        let mut term = self.term.lock();
        let point = self.point(&term, row, col);
        let ty = if words { SelectionType::Semantic } else { SelectionType::Simple };
        term.selection = Some(Selection::new(ty, point, Side::Left));
    }

    pub fn update_selection(&self, row: usize, col: usize) {
        let mut term = self.term.lock();
        let point = self.point(&term, row, col);
        if let Some(sel) = term.selection.as_mut() {
            sel.update(point, Side::Right);
        }
    }

    pub fn clear_selection(&self) {
        self.term.lock().selection = None;
    }

    pub fn selected_text(&self) -> Option<String> {
        self.term.lock().selection_to_string().filter(|s| !s.is_empty())
    }

    /// Copia de la pantalla visible para pintarla.
    pub fn snapshot(&self) -> Snapshot {
        let term = self.term.lock();
        let content = term.renderable_content();
        let rows = self.size.rows as usize;
        let cols = self.size.cols as usize;
        let mut lines = vec![vec![Cell::default(); cols]; rows];
        let offset = content.display_offset as i32;
        let selection = content.selection;

        for indexed in content.display_iter {
            let row = indexed.point.line.0 + offset;
            let col = indexed.point.column.0;
            if row < 0 || row as usize >= rows || col >= cols {
                continue;
            }
            let cell = &indexed.cell;
            if cell.flags.contains(Flags::WIDE_CHAR_SPACER) {
                lines[row as usize][col].spacer = true;
                continue;
            }
            let mut fg = resolve(cell.fg, content.colors, true);
            let mut bg = match cell.bg {
                Color::Named(NamedColor::Background) => None,
                other => Some(resolve(other, content.colors, false)),
            };
            if cell.flags.contains(Flags::BOLD) {
                if let Color::Named(n) = cell.fg {
                    if (n as usize) < 8 {
                        fg = resolve(Color::Indexed(n as u8 + 8), content.colors, true);
                    }
                }
            }
            if cell.flags.contains(Flags::INVERSE) {
                let old_bg = bg.unwrap_or(palette::BACKGROUND);
                bg = Some(fg);
                fg = old_bg;
            }
            let selected = selection.is_some_and(|s| s.contains(indexed.point));
            if selected {
                bg = Some(palette::SELECTION);
            }
            lines[row as usize][col] = Cell {
                ch: if cell.flags.contains(Flags::HIDDEN) { ' ' } else { cell.c },
                fg,
                bg,
                bold: cell.flags.contains(Flags::BOLD),
                italic: cell.flags.contains(Flags::ITALIC),
                underline: cell.flags.intersects(Flags::ALL_UNDERLINES),
                dim: cell.flags.contains(Flags::DIM),
                wide: cell.flags.contains(Flags::WIDE_CHAR),
                spacer: false,
            };
        }

        let cursor = if content.mode.contains(TermMode::SHOW_CURSOR) {
            let row = content.cursor.point.line.0 + offset;
            (row >= 0 && (row as usize) < rows).then(|| (row as usize, content.cursor.point.column.0))
        } else {
            None
        };
        Snapshot {
            lines,
            cursor,
            scrolled_back: content.display_offset > 0,
        }
    }
}

impl Drop for Terminal {
    fn drop(&mut self) {
        let _ = self
            .notifier
            .0
            .send(alacritty_terminal::event_loop::Msg::Shutdown);
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Cell {
    pub ch: char,
    pub fg: Rgb,
    pub bg: Option<Rgb>,
    pub bold: bool,
    pub italic: bool,
    pub underline: bool,
    pub dim: bool,
    /// Ocupa dos columnas.
    pub wide: bool,
    /// Segunda mitad de un carácter ancho: no se pinta.
    pub spacer: bool,
}

impl Default for Cell {
    fn default() -> Self {
        Self {
            ch: ' ',
            fg: palette::FOREGROUND,
            bg: None,
            bold: false,
            italic: false,
            underline: false,
            dim: false,
            wide: false,
            spacer: false,
        }
    }
}

pub struct Snapshot {
    pub lines: Vec<Vec<Cell>>,
    /// Fila y columna del cursor si está visible.
    pub cursor: Option<(usize, usize)>,
    pub scrolled_back: bool,
}

fn resolve(color: Color, colors: &alacritty_terminal::term::color::Colors, is_fg: bool) -> Rgb {
    match color {
        Color::Spec(rgb) => Rgb { r: rgb.r, g: rgb.g, b: rgb.b },
        Color::Indexed(i) => colors[i as usize]
            .map(|c| Rgb { r: c.r, g: c.g, b: c.b })
            .unwrap_or_else(|| palette::color_for_index(i as usize)),
        Color::Named(n) => {
            let idx = n as usize;
            if let Some(c) = colors[idx] {
                return Rgb { r: c.r, g: c.g, b: c.b };
            }
            match n {
                NamedColor::Foreground | NamedColor::BrightForeground => palette::FOREGROUND,
                NamedColor::Background => palette::BACKGROUND,
                NamedColor::Cursor => palette::CURSOR,
                NamedColor::DimForeground => palette::DIM_FOREGROUND,
                _ if idx < 16 => palette::color_for_index(idx),
                _ if (NamedColor::DimBlack as usize..=NamedColor::DimWhite as usize).contains(&idx) => {
                    palette::color_for_index(idx - NamedColor::DimBlack as usize).dim()
                }
                _ if is_fg => palette::FOREGROUND,
                _ => palette::BACKGROUND,
            }
        }
    }
}

/// Shell de login del usuario: con él se cargan el PATH y la sesión que tienes
/// en tu terminal (importante en macOS, donde las apps del Dock no heredan PATH).
///
/// Con `command`, el shell lo ejecuta directamente (sin teclearlo, así no queda
/// eco en pantalla) y al terminar deja un shell interactivo en la misma pestaña.
fn shell_for(command: Option<&str>) -> Shell {
    #[cfg(unix)]
    {
        let program = std::env::var("SHELL").ok().filter(|s| !s.is_empty()).unwrap_or_else(|| {
            if cfg!(target_os = "macos") { "/bin/zsh".into() } else { "/bin/bash".into() }
        });
        match command {
            Some(cmd) => {
                let script = format!("{cmd}; exec '{}' -l", program.replace('\'', "'\\''"));
                Shell::new(program, vec!["-l".into(), "-i".into(), "-c".into(), script])
            }
            None => Shell::new(program, vec!["-l".into()]),
        }
    }
    #[cfg(windows)]
    {
        let pwsh = std::env::var_os("PATH")
            .map(|p| std::env::split_paths(&p).any(|d| d.join("pwsh.exe").is_file()))
            .unwrap_or(false);
        let program = if pwsh { "pwsh.exe" } else { "powershell.exe" };
        let mut args = vec!["-NoLogo".to_string()];
        if let Some(cmd) = command {
            args.extend(["-NoExit".into(), "-Command".into(), cmd.to_string()]);
        }
        Shell::new(program.into(), args)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    #[cfg(unix)]
    #[test]
    fn runs_a_command_and_renders_its_output() {
        let size = Size { cols: 80, rows: 24, cell_width: 8, cell_height: 16 };
        let launch = Launch {
            cwd: std::env::temp_dir(),
            command: Some("echo kuaky-$((40+2))".into()),
            prefill: None,
        };
        let term = Terminal::spawn(&launch, size).unwrap();
        let events = term.events();
        let deadline = Instant::now() + Duration::from_secs(15);
        let mut found = false;
        while Instant::now() < deadline && !found {
            if let Ok(ev) = events.recv_timeout(Duration::from_millis(200)) {
                term.handle(ev);
            }
            let snap = term.snapshot();
            found = snap
                .lines
                .iter()
                .any(|l| l.iter().map(|c| c.ch).collect::<String>().contains("kuaky-42"));
        }
        assert!(found, "la salida del comando no apareció en pantalla");
    }
}
