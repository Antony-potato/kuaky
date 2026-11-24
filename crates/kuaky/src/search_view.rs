//! ⌘⇧F: buscar texto en todo el proyecto (respeta `.gitignore`).

use crate::style;
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::{h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use std::path::{Path, PathBuf};
use std::sync::{
    Arc,
    atomic::{AtomicU64, Ordering},
};

const MAX_MATCHES: usize = 2_000;
const MAX_FILE_BYTES: u64 = 2 * 1024 * 1024;

pub enum SearchEvent {
    Open { path: PathBuf, line: u32, column: u32 },
}

#[derive(Clone)]
pub struct Match {
    pub line: u32,
    pub column: u32,
    pub text: String,
    /// Rango (bytes) de la coincidencia dentro de `text`.
    pub range: std::ops::Range<usize>,
}

pub struct FileMatches {
    pub path: PathBuf,
    pub matches: Vec<Match>,
}

pub struct SearchView {
    root: PathBuf,
    input: Entity<InputState>,
    case_sensitive: bool,
    regex: bool,
    results: Vec<FileMatches>,
    total: usize,
    truncated: bool,
    searching: bool,
    error: Option<String>,
    generation: Arc<AtomicU64>,
    _sub: Subscription,
}

impl EventEmitter<SearchEvent> for SearchView {}

impl SearchView {
    pub fn new(root: PathBuf, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let input = cx.new(|cx| InputState::new(window, cx).placeholder("Buscar en el proyecto…"));
        let sub = cx.subscribe_in(&input, window, |this, _, ev: &InputEvent, _, cx| {
            if matches!(ev, InputEvent::Change | InputEvent::PressEnter { .. }) {
                this.run(cx);
            }
        });
        input.update(cx, |s, cx| s.focus(window, cx));
        Self {
            root,
            input,
            case_sensitive: false,
            regex: false,
            results: Vec::new(),
            total: 0,
            truncated: false,
            searching: false,
            error: None,
            generation: Arc::new(AtomicU64::new(0)),
            _sub: sub,
        }
    }

    pub fn focus(&self, window: &mut Window, cx: &mut Context<Self>) {
        self.input.update(cx, |s, cx| s.focus(window, cx));
    }

    fn run(&mut self, cx: &mut Context<Self>) {
        let query = self.input.read(cx).value().to_string();
        let generation = self.generation.fetch_add(1, Ordering::SeqCst) + 1;
        if query.trim().is_empty() {
            self.results.clear();
            self.total = 0;
            self.error = None;
            cx.notify();
            return;
        }
        let pattern = if self.regex { query.clone() } else { regex::escape(&query) };
        let re = match regex::RegexBuilder::new(&pattern).case_insensitive(!self.case_sensitive).build() {
            Ok(re) => re,
            Err(e) => {
                self.error = Some(format!("Expresión inválida: {e}"));
                cx.notify();
                return;
            }
        };
        self.error = None;
        self.searching = true;
        cx.notify();
        let root = self.root.clone();
        let current = self.generation.clone();
        cx.spawn(async move |this, cx| {
            // Pequeña espera para no buscar en cada tecla.
            cx.background_executor().timer(std::time::Duration::from_millis(150)).await;
            if current.load(Ordering::SeqCst) != generation {
                return;
            }
            let check = current.clone();
            let (results, total, truncated) = cx
                .background_executor()
                .spawn(async move { search(&root, &re, || check.load(Ordering::SeqCst) != generation) })
                .await;
            this.update(cx, |this, cx| {
                if this.generation.load(Ordering::SeqCst) == generation {
                    this.results = results;
                    this.total = total;
                    this.truncated = truncated;
                    this.searching = false;
                    cx.notify();
                }
            })
            .ok();
        })
        .detach();
    }

    fn toggle(&mut self, regex: bool, cx: &mut Context<Self>) {
        if regex {
            self.regex = !self.regex;
        } else {
            self.case_sensitive = !self.case_sensitive;
        }
        self.run(cx);
    }
}

/// Busca `re` en los archivos de texto de `root`. Se detiene si `cancelled`.
pub fn search(root: &Path, re: &regex::Regex, cancelled: impl Fn() -> bool) -> (Vec<FileMatches>, usize, bool) {
    let mut results = Vec::new();
    let mut total = 0;
    for entry in ignore::WalkBuilder::new(root)
        .hidden(false)
        .filter_entry(|e| e.file_name() != ".git")
        .build()
        .filter_map(Result::ok)
    {
        if cancelled() {
            break;
        }
        if !entry.file_type().is_some_and(|t| t.is_file()) || entry.metadata().map(|m| m.len()).unwrap_or(0) > MAX_FILE_BYTES {
            continue;
        }
        let Ok(bytes) = std::fs::read(entry.path()) else { continue };
        if bytes.iter().take(8000).any(|&b| b == 0) {
            continue;
        }
        let text = String::from_utf8_lossy(&bytes);
        let mut matches = Vec::new();
        for (i, line) in text.lines().enumerate() {
            if let Some(m) = re.find(line) {
                let (display, range) = trim_line(line, m.start(), m.end());
                matches.push(Match {
                    line: i as u32,
                    column: line[..m.start()].chars().count() as u32,
                    text: display,
                    range,
                });
                total += 1;
                if total >= MAX_MATCHES {
                    results.push(FileMatches { path: entry.path().to_path_buf(), matches });
                    return (results, total, true);
                }
            }
        }
        if !matches.is_empty() {
            results.push(FileMatches { path: entry.path().to_path_buf(), matches });
        }
    }
    (results, total, false)
}

/// Recorta líneas largas alrededor de la coincidencia.
fn trim_line(line: &str, start: usize, end: usize) -> (String, std::ops::Range<usize>) {
    let lead = line.len() - line.trim_start().len();
    let mut from = lead.min(start);
    if start - from > 60 {
        from = start - 40;
        while !line.is_char_boundary(from) {
            from -= 1;
        }
    }
    let mut to = line.len().min(end + 160);
    while !line.is_char_boundary(to) {
        to -= 1;
    }
    (line[from..to].to_string(), start - from..end - from)
}

impl Render for SearchView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let accent: Hsla = style::accent().into();
        let summary = if let Some(e) = &self.error {
            e.clone()
        } else if self.searching {
            "buscando…".into()
        } else if self.total == 0 {
            String::new()
        } else {
            format!(
                "{}{} coincidencias en {} archivos",
                if self.truncated { "más de " } else { "" },
                self.total,
                self.results.len()
            )
        };
        let toggle = |id: &'static str, label: &'static str, on: bool| {
            div()
                .id(id)
                .px_1p5()
                .py_0p5()
                .rounded_sm()
                .text_xs()
                .cursor_pointer()
                .border_1()
                .border_color(if on { style::soft(style::accent(), 0.6) } else { style::border().into() })
                .text_color(if on { style::accent() } else { style::muted() })
                .child(label)
        };
        let root = self.root.clone();
        v_flex()
            .size_full()
            .child(
                v_flex()
                    .p_3()
                    .gap_2()
                    .border_b_1()
                    .border_color(style::border())
                    .child(Input::new(&self.input))
                    .child(
                        h_flex()
                            .gap_2()
                            .child(toggle("case", "Aa", self.case_sensitive).on_click(cx.listener(|this, _, _, cx| this.toggle(false, cx))))
                            .child(toggle("regex", ".*", self.regex).on_click(cx.listener(|this, _, _, cx| this.toggle(true, cx))))
                            .child(div().text_xs().text_color(if self.error.is_some() { style::bad() } else { style::muted() }).child(summary)),
                    ),
            )
            .child(
                div().id("search-results").flex_1().min_h_0().overflow_y_scroll().child(
                    v_flex().p_2().gap_2().children(self.results.iter().enumerate().map(|(fi, file)| {
                        let rel = file.path.strip_prefix(&root).unwrap_or(&file.path).to_string_lossy().into_owned();
                        v_flex()
                            .child(
                                h_flex()
                                    .gap_2()
                                    .px_2()
                                    .text_xs()
                                    .child(div().text_color(style::text()).child(rel))
                                    .child(div().text_color(style::faint()).child(file.matches.len().to_string())),
                            )
                            .children(file.matches.iter().enumerate().map(|(mi, m)| {
                                let path = file.path.clone();
                                let (line, column) = (m.line, m.column);
                                let highlight = HighlightStyle { color: Some(accent), font_weight: Some(FontWeight::BOLD), ..Default::default() };
                                h_flex()
                                    .id(("hit", fi * 10_000 + mi))
                                    .gap_2()
                                    .px_2()
                                    .py_0p5()
                                    .rounded_sm()
                                    .text_xs()
                                    .cursor_pointer()
                                    .hover(|d| d.bg(style::panel_hover()))
                                    .child(div().w(px(36.)).flex_none().text_right().text_color(style::faint()).child((m.line + 1).to_string()))
                                    .child(
                                        div()
                                            .flex_1()
                                            .min_w_0()
                                            .overflow_hidden()
                                            .whitespace_nowrap()
                                            .font_family(style::mono_font())
                                            .text_color(style::muted())
                                            .child(StyledText::new(m.text.clone()).with_highlights([(m.range.clone(), highlight)])),
                                    )
                                    .on_click(cx.listener(move |_, _, _, cx| cx.emit(SearchEvent::Open { path: path.clone(), line, column })))
                            }))
                    })),
                ),
            )
            .when(self.results.is_empty() && !self.searching && self.error.is_none(), |d| {
                d.child(div().p_4().text_xs().text_color(style::faint()).child("Escribe para buscar. Aa: distingue mayúsculas · .*: expresión regular"))
            })
    }
}

#[cfg(test)]
mod tests {
    use super::{search, trim_line};

    #[test]
    fn finds_matches_respecting_gitignore() {
        let dir = std::env::temp_dir().join(format!("kuaky-search-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("src")).unwrap();
        std::fs::create_dir_all(dir.join("target")).unwrap();
        std::fs::write(dir.join(".gitignore"), "target/\n").unwrap();
        std::fs::write(dir.join("src/a.rs"), "fn main() {\n    let Hola = 1;\n}\n").unwrap();
        std::fs::write(dir.join("target/b.rs"), "hola ignorada\n").unwrap();
        std::fs::write(dir.join("bin.dat"), [0u8, 1, 2, b'h', b'o', b'l', b'a']).unwrap();
        // `ignore` aplica .gitignore solo dentro de un repo git.
        std::process::Command::new("git").args(["init", "-q"]).current_dir(&dir).status().unwrap();
        let re = regex::RegexBuilder::new("hola").case_insensitive(true).build().unwrap();
        let (results, total, truncated) = search(&dir, &re, || false);
        std::fs::remove_dir_all(&dir).ok();
        assert_eq!(total, 1);
        assert!(!truncated);
        assert!(results[0].path.ends_with("src/a.rs"));
        assert_eq!(results[0].matches[0].line, 1);
        assert_eq!(results[0].matches[0].column, 8);
        assert_eq!(&results[0].matches[0].text[results[0].matches[0].range.clone()], "Hola");
    }

    #[test]
    fn trims_long_lines_around_match() {
        let line = format!("{}needle{}", "x".repeat(200), "y".repeat(300));
        let (text, range) = trim_line(&line, 200, 206);
        assert_eq!(&text[range], "needle");
        assert!(text.len() < 250);
    }
}
