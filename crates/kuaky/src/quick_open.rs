//! ⌘P: buscar un archivo del proyecto por nombre.

use crate::style;
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::{h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use std::{path::PathBuf, sync::Arc};

const MAX_FILES: usize = 50_000;
const MAX_RESULTS: usize = 40;

pub enum QuickOpenEvent {
    Open(PathBuf),
    Dismiss,
}

pub struct QuickOpen {
    root: PathBuf,
    input: Entity<InputState>,
    files: Arc<Vec<String>>,
    results: Vec<(usize, Vec<usize>)>,
    selected: usize,
    loading: bool,
    _sub: Subscription,
}

impl EventEmitter<QuickOpenEvent> for QuickOpen {}

impl QuickOpen {
    pub fn new(root: PathBuf, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let input = cx.new(|cx| InputState::new(window, cx).placeholder("Buscar archivo por nombre…"));
        let sub = cx.subscribe_in(&input, window, |this, _, ev: &InputEvent, _, cx| match ev {
            InputEvent::Change => this.refilter(cx),
            InputEvent::PressEnter { .. } => this.confirm(cx),
            _ => {}
        });
        input.update(cx, |s, cx| s.focus(window, cx));
        let r = root.clone();
        cx.spawn(async move |this, cx| {
            let files = cx.background_executor().spawn(async move { list_files(&r) }).await;
            this.update(cx, |this, cx| {
                this.files = Arc::new(files);
                this.loading = false;
                this.refilter(cx);
            })
            .ok();
        })
        .detach();
        Self { root, input, files: Arc::new(Vec::new()), results: Vec::new(), selected: 0, loading: true, _sub: sub }
    }

    fn refilter(&mut self, cx: &mut Context<Self>) {
        let query = self.input.read(cx).value().to_string();
        self.results = search(&self.files, &query, MAX_RESULTS);
        self.selected = 0;
        cx.notify();
    }

    fn confirm(&mut self, cx: &mut Context<Self>) {
        if let Some((i, _)) = self.results.get(self.selected) {
            cx.emit(QuickOpenEvent::Open(self.root.join(&self.files[*i])));
        }
    }

    fn on_key(&mut self, ev: &KeyDownEvent, _: &mut Window, cx: &mut Context<Self>) {
        match ev.keystroke.key.as_str() {
            "down" => self.selected = (self.selected + 1).min(self.results.len().saturating_sub(1)),
            "up" => self.selected = self.selected.saturating_sub(1),
            "escape" => cx.emit(QuickOpenEvent::Dismiss),
            _ => return,
        }
        cx.stop_propagation();
        cx.notify();
    }
}

impl Render for QuickOpen {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let empty = if self.loading {
            Some("Leyendo archivos…")
        } else if self.results.is_empty() {
            Some("Sin resultados")
        } else {
            None
        };
        v_flex()
            .w(px(560.))
            .max_h(px(460.))
            .rounded_xl()
            .border_1()
            .border_color(style::border())
            .bg(style::panel())
            .shadow_lg()
            .overflow_hidden()
            .capture_key_down(cx.listener(Self::on_key))
            .child(div().p_2().border_b_1().border_color(style::border()).child(Input::new(&self.input)))
            .child(
                div().id("quick-open-results").flex_1().overflow_y_scroll().p_1().children(
                    self.results.iter().enumerate().map(|(row, (i, hits))| {
                        let path = &self.files[*i];
                        let (dir, file) = match path.rsplit_once('/') {
                            Some((d, f)) => (d.to_string(), f.to_string()),
                            None => (String::new(), path.clone()),
                        };
                        let file_start = path.len() - file.len();
                        let file_hits: Vec<usize> = hits.iter().filter(|&&h| h >= file_start).map(|h| h - file_start).collect();
                        let full = self.root.join(path);
                        h_flex()
                            .id(("qo", row))
                            .gap_2()
                            .px_2()
                            .py_1()
                            .rounded_md()
                            .text_sm()
                            .cursor_pointer()
                            .when(row == self.selected, |d| d.bg(style::soft(style::accent(), 0.14)))
                            .hover(|d| d.bg(style::panel_hover()))
                            .child(highlighted(&file, &file_hits))
                            .child(div().truncate().text_xs().text_color(style::faint()).child(dir))
                            .on_click(cx.listener(move |_, _, _, cx| cx.emit(QuickOpenEvent::Open(full.clone()))))
                    }),
                ),
            )
            .when_some(empty, |d, t| d.child(div().p_3().text_xs().text_color(style::muted()).child(t)))
    }
}

/// Nombre con las letras que coinciden resaltadas.
fn highlighted(text: &str, hits: &[usize]) -> StyledText {
    let accent: Hsla = style::accent().into();
    let ranges = hits
        .iter()
        .filter_map(|&h| text.get(h..).and_then(|rest| rest.chars().next()).map(|c| h..h + c.len_utf8()))
        .map(|r| (r, HighlightStyle { color: Some(accent), font_weight: Some(FontWeight::SEMIBOLD), ..Default::default() }));
    StyledText::new(text.to_string()).with_highlights(ranges)
}

fn list_files(root: &std::path::Path) -> Vec<String> {
    let mut files: Vec<String> = ignore::WalkBuilder::new(root)
        .hidden(false)
        .filter_entry(|e| e.file_name() != ".git")
        .build()
        .filter_map(Result::ok)
        .filter(|e| e.file_type().is_some_and(|t| t.is_file()))
        .filter_map(|e| e.path().strip_prefix(root).ok().map(|p| p.to_string_lossy().replace('\\', "/")))
        .take(MAX_FILES)
        .collect();
    files.sort_by_key(|f| (f.matches('/').count(), f.len()));
    files
}

/// Búsqueda difusa: las letras de la consulta deben aparecer en orden. Puntúa
/// mejor las coincidencias seguidas y las que caen en el nombre del archivo.
/// Devuelve índice del archivo y posiciones (bytes) que coinciden.
pub fn search(files: &[String], query: &str, limit: usize) -> Vec<(usize, Vec<usize>)> {
    let query: Vec<char> = query.to_lowercase().chars().filter(|c| !c.is_whitespace()).collect();
    if query.is_empty() {
        return (0..files.len().min(limit)).map(|i| (i, Vec::new())).collect();
    }
    let mut scored: Vec<(i64, usize, Vec<usize>)> = files
        .iter()
        .enumerate()
        .filter_map(|(i, f)| score(f, &query).map(|(s, hits)| (s, i, hits)))
        .collect();
    scored.sort_by(|a, b| b.0.cmp(&a.0).then(files[a.1].len().cmp(&files[b.1].len())));
    scored.into_iter().take(limit).map(|(_, i, hits)| (i, hits)).collect()
}

fn score(path: &str, query: &[char]) -> Option<(i64, Vec<usize>)> {
    let file_start = path.rfind('/').map(|i| i + 1).unwrap_or(0);
    let mut hits = Vec::with_capacity(query.len());
    let mut qi = 0;
    let mut score = 0i64;
    let mut last: Option<usize> = None;
    for (bi, c) in path.char_indices() {
        if qi == query.len() {
            break;
        }
        if c.to_lowercase().next() == Some(query[qi]) {
            score += 1;
            if bi >= file_start {
                score += 3;
            }
            if last.is_some_and(|l| path[l..bi].chars().count() == 1) {
                score += 5;
            }
            if bi == file_start || path[..bi].ends_with(['/', '_', '-', '.']) {
                score += 4;
            }
            hits.push(bi);
            last = Some(bi);
            qi += 1;
        }
    }
    (qi == query.len()).then_some((score, hits))
}

#[cfg(test)]
mod tests {
    use super::search;

    #[test]
    fn prefers_file_name_matches() {
        let files: Vec<String> = ["crates/kuaky/src/home.rs", "docs/plan.md", "crates/kuaky-usage/src/report.rs", "README.md"]
            .map(String::from)
            .to_vec();
        let r = search(&files, "home", 10);
        assert_eq!(files[r[0].0], "crates/kuaky/src/home.rs");
        let r = search(&files, "rep", 10);
        assert_eq!(files[r[0].0], "crates/kuaky-usage/src/report.rs");
        assert!(search(&files, "zzz", 10).is_empty());
        assert_eq!(search(&files, "", 2).len(), 2);
    }
}
