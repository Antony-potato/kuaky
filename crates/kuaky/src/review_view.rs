//! Revisar lo que cambió un agente en un turno del chat:
//! - cada bloque de cambios se puede **aceptar** o **rechazar** (rechazar
//!   devuelve ese bloque a como estaba, sin tocar los demás);
//! - clic en un número de línea deja un **comentario**; "Send to agent" los
//!   manda todos juntos al chat como un mensaje.

use crate::style;
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::component::{h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use similar::{DiffOp, TextDiff};
use std::path::PathBuf;

pub enum ReviewEvent {
    /// Mensaje con los comentarios, para el chat que hizo los cambios.
    SendToAgent(String),
    OpenFile { path: PathBuf, line: u32 },
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Decision {
    Pending,
    Accepted,
    Rejected,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    Context,
    Added,
    Removed,
}

struct Line {
    kind: Kind,
    old_no: Option<usize>,
    new_no: Option<usize>,
    text: String,
}

struct Hunk {
    /// Índices (en `FileReview::ops`) de las operaciones con cambios del bloque.
    ops: Vec<usize>,
    lines: Vec<Line>,
    decision: Decision,
}

struct FileReview {
    path: PathBuf,
    name: String,
    old: String,
    new: String,
    ops: Vec<DiffOp>,
    hunks: Vec<Hunk>,
    /// Lo último que kuaky dejó en disco (para no pisar cambios posteriores).
    written: String,
    error: Option<String>,
}

struct Comment {
    file: usize,
    line: usize,
    text: String,
}

pub struct ReviewView {
    pub title: SharedString,
    files: Vec<FileReview>,
    comments: Vec<Comment>,
    /// Línea con la caja de comentario abierta: (archivo, línea nueva).
    commenting: Option<(usize, usize)>,
    input: Entity<InputState>,
    _sub: Subscription,
}

impl EventEmitter<ReviewEvent> for ReviewView {}

fn split_lines(text: &str) -> Vec<&str> {
    text.split_inclusive('\n').collect()
}

impl FileReview {
    fn new(path: PathBuf, name: String, old: Option<String>, new: String) -> FileReview {
        let old = old.unwrap_or_default();
        let diff = TextDiff::from_lines(&old, &new);
        let ops: Vec<DiffOp> = diff.ops().to_vec();
        let old_lines = split_lines(&old);
        let new_lines = split_lines(&new);
        let mut hunks = Vec::new();
        for group in diff.grouped_ops(3) {
            let mut hunk = Hunk { ops: Vec::new(), lines: Vec::new(), decision: Decision::Pending };
            for op in &group {
                if let Some(i) = ops.iter().position(|o| o == op) {
                    if !matches!(op, DiffOp::Equal { .. }) {
                        hunk.ops.push(i);
                    }
                }
                let (old_range, new_range) = (op.old_range(), op.new_range());
                match op {
                    DiffOp::Equal { .. } => {
                        for (k, o) in old_range.clone().enumerate() {
                            let n = new_range.start + k;
                            hunk.lines.push(Line { kind: Kind::Context, old_no: Some(o + 1), new_no: Some(n + 1), text: trim_nl(old_lines.get(o)) });
                        }
                    }
                    _ => {
                        for o in old_range {
                            hunk.lines.push(Line { kind: Kind::Removed, old_no: Some(o + 1), new_no: None, text: trim_nl(old_lines.get(o)) });
                        }
                        for n in new_range {
                            hunk.lines.push(Line { kind: Kind::Added, old_no: None, new_no: Some(n + 1), text: trim_nl(new_lines.get(n)) });
                        }
                    }
                }
            }
            if !hunk.ops.is_empty() {
                hunks.push(hunk);
            }
        }
        FileReview { path, name, written: new.clone(), old, new, ops, hunks, error: None }
    }

    /// Texto del archivo con los bloques rechazados devueltos a como estaban.
    fn compose(&self) -> String {
        let old_lines = split_lines(&self.old);
        let new_lines = split_lines(&self.new);
        let rejected: Vec<usize> = self.hunks.iter().filter(|h| h.decision == Decision::Rejected).flat_map(|h| h.ops.iter().copied()).collect();
        let mut out = String::new();
        for (i, op) in self.ops.iter().enumerate() {
            let use_old = matches!(op, DiffOp::Equal { .. }) || rejected.contains(&i);
            if use_old {
                for l in &old_lines[op.old_range()] {
                    out.push_str(l);
                }
            } else {
                for l in &new_lines[op.new_range()] {
                    out.push_str(l);
                }
            }
        }
        out
    }

    fn stats(&self) -> (usize, usize) {
        self.hunks.iter().flat_map(|h| &h.lines).fold((0, 0), |(a, r), l| match l.kind {
            Kind::Added => (a + 1, r),
            Kind::Removed => (a, r + 1),
            Kind::Context => (a, r),
        })
    }
}

fn trim_nl(line: Option<&&str>) -> String {
    line.map(|l| l.trim_end_matches(['\n', '\r']).to_string()).unwrap_or_default()
}

impl ReviewView {
    /// `files`: (ruta, texto antes, texto después). `root` acorta los nombres.
    pub fn new(title: String, root: PathBuf, files: Vec<(PathBuf, Option<String>, String)>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let input = cx.new(|cx| InputState::new(window, cx).placeholder("Comment for the agent… (Enter saves, Esc cancels)"));
        let sub = cx.subscribe_in(&input, window, |this, _, ev: &InputEvent, window, cx| {
            if let InputEvent::PressEnter { .. } = ev {
                this.save_comment(window, cx);
            }
        });
        let files = files
            .into_iter()
            .map(|(path, old, new)| {
                let name = path.strip_prefix(&root).unwrap_or(&path).to_string_lossy().into_owned();
                FileReview::new(path, name, old, new)
            })
            .collect();
        ReviewView { title: title.into(), files, comments: Vec::new(), commenting: None, input, _sub: sub }
    }

    fn decide(&mut self, file: usize, hunk: usize, decision: Decision, cx: &mut Context<Self>) {
        let Some(f) = self.files.get_mut(file) else { return };
        let previous = f.hunks[hunk].decision;
        f.hunks[hunk].decision = decision;
        let changes_disk = previous == Decision::Rejected || decision == Decision::Rejected;
        if changes_disk {
            let current = std::fs::read_to_string(&f.path).unwrap_or_default();
            if current != f.written {
                f.hunks[hunk].decision = previous;
                f.error = Some("The file changed after this turn; open it to edit by hand.".into());
            } else {
                let text = f.compose();
                match std::fs::write(&f.path, &text) {
                    Ok(()) => {
                        f.written = text;
                        f.error = None;
                    }
                    Err(err) => {
                        f.hunks[hunk].decision = previous;
                        f.error = Some(format!("Could not write: {err}"));
                    }
                }
            }
        }
        cx.notify();
    }

    fn decide_all(&mut self, decision: Decision, cx: &mut Context<Self>) {
        for fi in 0..self.files.len() {
            for hi in 0..self.files[fi].hunks.len() {
                if self.files[fi].hunks[hi].decision == Decision::Pending {
                    self.decide(fi, hi, decision, cx);
                }
            }
        }
    }

    fn start_comment(&mut self, file: usize, line: usize, window: &mut Window, cx: &mut Context<Self>) {
        let existing = self.comments.iter().position(|c| c.file == file && c.line == line).map(|i| self.comments.remove(i).text).unwrap_or_default();
        self.commenting = Some((file, line));
        self.input.update(cx, |s, cx| {
            s.set_value(existing, window, cx);
            s.focus(window, cx);
        });
        cx.notify();
    }

    fn save_comment(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some((file, line)) = self.commenting.take() else { return };
        let text = self.input.read(cx).value().trim().to_string();
        if !text.is_empty() {
            self.comments.push(Comment { file, line, text });
            self.comments.sort_by_key(|c| (c.file, c.line));
        }
        self.input.update(cx, |s, cx| s.set_value("", window, cx));
        cx.notify();
    }

    fn send(&mut self, cx: &mut Context<Self>) {
        let mut msg = String::from("Review comments on your changes:\n");
        for c in &self.comments {
            let f = &self.files[c.file];
            let code = f.new.lines().nth(c.line.saturating_sub(1)).unwrap_or("").trim();
            msg.push_str(&format!("\n- {}:{} `{}`\n  {}", f.name, c.line, crate::lines::short(code, 80), c.text));
        }
        let rejected: Vec<String> = self
            .files
            .iter()
            .filter(|f| f.hunks.iter().any(|h| h.decision == Decision::Rejected))
            .map(|f| f.name.clone())
            .collect();
        if !rejected.is_empty() {
            msg.push_str(&format!("\n\nI rejected (reverted) some of your changes in: {}.", rejected.join(", ")));
        }
        cx.emit(ReviewEvent::SendToAgent(msg));
        self.comments.clear();
        cx.notify();
    }

    fn render_line(&self, fi: usize, key: usize, line: &Line, cx: &mut Context<Self>) -> AnyElement {
        let (bg, sign, color): (Option<Hsla>, &str, Hsla) = match line.kind {
            Kind::Added => (Some(style::soft(style::good(), 0.10)), "+", style::good().into()),
            Kind::Removed => (Some(style::soft(style::bad(), 0.10)), "−", style::bad().into()),
            Kind::Context => (None, " ", style::faint().into()),
        };
        let commentable = line.new_no;
        let has_comment = commentable.is_some_and(|n| self.comments.iter().any(|c| c.file == fi && c.line == n));
        let num = |n: Option<usize>| div().w(px(38.)).flex_none().text_right().pr_2().child(n.map(|n| n.to_string()).unwrap_or_default());
        h_flex()
            .id(("review-line", key))
            .w_full()
            .text_xs()
            .font_family(style::mono_font())
            .when_some(bg, |d, bg| d.bg(bg))
            .child(
                h_flex()
                    .id(("review-num", key))
                    .flex_none()
                    .text_color(if has_comment { style::accent() } else { style::faint() })
                    .when(commentable.is_some(), |d| d.cursor_pointer().hover(|d| d.text_color(style::accent())))
                    .tooltip(|window, cx| Tooltip::new("Comment on this line").build(window, cx))
                    .child(num(line.old_no))
                    .child(num(line.new_no))
                    .when_some(commentable, |d, n| d.on_click(cx.listener(move |this, _, window, cx| this.start_comment(fi, n, window, cx)))),
            )
            .child(div().w(px(14.)).flex_none().text_color(color).child(sign))
            .child(div().flex_1().min_w_0().whitespace_nowrap().overflow_x_hidden().text_color(style::text()).child(line.text.clone()))
            .into_any_element()
    }

    fn comment_box(&self, fi: usize, line: usize, cx: &mut Context<Self>) -> Option<AnyElement> {
        if self.commenting == Some((fi, line)) {
            return Some(
                h_flex()
                    .ml(px(90.))
                    .my_1()
                    .mr_3()
                    .gap_2()
                    .child(div().flex_1().child(Input::new(&self.input)))
                    .child(
                        div()
                            .id(("save-comment", fi * 100_000 + line))
                            .px_2()
                            .py_1()
                            .rounded_md()
                            .text_xs()
                            .bg(style::accent())
                            .text_color(style::bg())
                            .cursor_pointer()
                            .child("Save")
                            .on_click(cx.listener(|this, _, window, cx| this.save_comment(window, cx))),
                    )
                    .into_any_element(),
            );
        }
        let c = self.comments.iter().find(|c| c.file == fi && c.line == line)?;
        Some(
            h_flex()
                .id(("comment", fi * 100_000 + line))
                .ml(px(90.))
                .my_1()
                .mr_3()
                .px_2()
                .py_1()
                .gap_2()
                .rounded_md()
                .border_1()
                .border_color(style::accent_alpha(0.4))
                .bg(style::accent_alpha(0.06))
                .text_xs()
                .cursor_pointer()
                .child(crate::icons::ui("message-square", px(12.), style::accent()))
                .child(div().flex_1().whitespace_normal().child(c.text.clone()))
                .on_click(cx.listener(move |this, _, window, cx| this.start_comment(fi, line, window, cx)))
                .into_any_element(),
        )
    }
}

impl Render for ReviewView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let pending: usize = self.files.iter().flat_map(|f| &f.hunks).filter(|h| h.decision == Decision::Pending).count();
        let total: usize = self.files.iter().map(|f| f.hunks.len()).sum();
        let comments = self.comments.len();
        let mut body = v_flex().gap_4().p_4();
        for (fi, f) in self.files.iter().enumerate() {
            let (added, removed) = f.stats();
            let path = f.path.clone();
            let mut card = v_flex()
                .rounded_lg()
                .border_1()
                .border_color(style::border())
                .overflow_hidden()
                .child(
                    h_flex()
                        .px_3()
                        .py_2()
                        .gap_2()
                        .items_center()
                        .bg(style::panel())
                        .text_sm()
                        .child(crate::icons::ui("file", px(13.), style::muted()))
                        .child(
                            div()
                                .id(("review-file", fi))
                                .cursor_pointer()
                                .hover(|d| d.text_color(style::accent()))
                                .child(f.name.clone())
                                .on_click(cx.listener(move |_, _, _, cx| cx.emit(ReviewEvent::OpenFile { path: path.clone(), line: 1 }))),
                        )
                        .child(div().text_xs().text_color(style::good()).child(format!("+{added}")))
                        .child(div().text_xs().text_color(style::bad()).child(format!("−{removed}")))
                        .child(div().flex_1())
                        .when_some(f.error.clone(), |d, e| d.child(div().text_xs().text_color(style::bad()).child(e))),
                );
            for (hi, h) in f.hunks.iter().enumerate() {
                let key = fi * 1000 + hi;
                let first = h.lines.iter().find_map(|l| l.new_no.or(l.old_no)).unwrap_or(1);
                let (label, color): (&str, Hsla) = match h.decision {
                    Decision::Pending => ("", style::faint().into()),
                    Decision::Accepted => ("Kept", style::good().into()),
                    Decision::Rejected => ("Rejected — reverted on disk", style::bad().into()),
                };
                let button = |id: (&'static str, usize), text: &'static str, primary: bool| {
                    div()
                        .id(id)
                        .px_2()
                        .py_0p5()
                        .rounded_md()
                        .text_xs()
                        .cursor_pointer()
                        .when(primary, |d| d.bg(style::accent_alpha(0.18)).text_color(style::accent()))
                        .when(!primary, |d| d.border_1().border_color(style::border()).text_color(style::muted()))
                        .hover(|d| d.bg(style::panel_hover()))
                        .child(text)
                };
                card = card.child(
                    h_flex()
                        .px_3()
                        .py_1()
                        .gap_2()
                        .items_center()
                        .border_t_1()
                        .border_color(style::border())
                        .text_xs()
                        .text_color(style::faint())
                        .child(format!("Line {first}"))
                        .child(div().text_color(color).child(label))
                        .child(div().flex_1())
                        .map(|d| match h.decision {
                            Decision::Pending => d
                                .child(button(("hunk-keep", key), "Keep", true).on_click(cx.listener(move |this, _, _, cx| this.decide(fi, hi, Decision::Accepted, cx))))
                                .child(button(("hunk-reject", key), "Reject", false).on_click(cx.listener(move |this, _, _, cx| this.decide(fi, hi, Decision::Rejected, cx)))),
                            _ => d.child(button(("hunk-undo", key), "Undo", false).on_click(cx.listener(move |this, _, _, cx| this.decide(fi, hi, Decision::Pending, cx)))),
                        }),
                );
                let dim = h.decision == Decision::Rejected;
                let mut lines = v_flex().w_full().py_1().when(dim, |d| d.opacity(0.45));
                for (li, line) in h.lines.iter().enumerate() {
                    lines = lines.child(self.render_line(fi, key * 1000 + li, line, cx));
                    if let Some(n) = line.new_no.filter(|_| line.kind != Kind::Removed) {
                        if let Some(b) = self.comment_box(fi, n, cx) {
                            lines = lines.child(b);
                        }
                    }
                }
                card = card.child(lines);
            }
            body = body.child(card);
        }
        v_flex()
            .size_full()
            .bg(style::bg())
            .child(
                h_flex()
                    .h(px(34.))
                    .px_3()
                    .gap_2()
                    .items_center()
                    .text_xs()
                    .border_b_1()
                    .border_color(style::border())
                    .child(div().text_color(style::text()).child(self.title.clone()))
                    .child(div().text_color(style::muted()).child(if pending == 0 { format!("{total} blocks reviewed") } else { format!("{pending} of {total} blocks to review") }))
                    .child(div().text_color(style::faint()).child("· click a line number to comment"))
                    .child(div().flex_1())
                    .when(pending > 0, |d| {
                        d.child(
                            div()
                                .id("keep-all")
                                .px_2()
                                .py_0p5()
                                .rounded_md()
                                .cursor_pointer()
                                .border_1()
                                .border_color(style::border())
                                .hover(|d| d.bg(style::panel_hover()))
                                .child("Keep all")
                                .on_click(cx.listener(|this, _, _, cx| this.decide_all(Decision::Accepted, cx))),
                        )
                        .child(
                            div()
                                .id("reject-all")
                                .px_2()
                                .py_0p5()
                                .rounded_md()
                                .cursor_pointer()
                                .border_1()
                                .border_color(style::border())
                                .hover(|d| d.bg(style::panel_hover()))
                                .child("Reject all")
                                .on_click(cx.listener(|this, _, _, cx| this.decide_all(Decision::Rejected, cx))),
                        )
                    })
                    .when(comments > 0, |d| {
                        d.child(
                            div()
                                .id("send-comments")
                                .px_2()
                                .py_0p5()
                                .rounded_md()
                                .cursor_pointer()
                                .bg(style::accent())
                                .text_color(style::bg())
                                .child(format!("Send {comments} comment{} to agent", if comments == 1 { "" } else { "s" }))
                                .on_click(cx.listener(|this, _, _, cx| this.send(cx))),
                        )
                    }),
            )
            .child(div().id("review-scroll").flex_1().min_h_0().overflow_y_scroll().child(body))
    }
}

#[cfg(test)]
mod tests {
    use super::{Decision, FileReview};

    #[test]
    fn rejecting_one_block_keeps_the_others() {
        let old = (1..=20).map(|i| format!("line {i}\n")).collect::<String>();
        let new = old.replace("line 2\n", "line two\n").replace("line 18\n", "line eighteen\n");
        let mut f = FileReview::new("x.txt".into(), "x.txt".into(), Some(old.clone()), new.clone());
        assert_eq!(f.hunks.len(), 2);
        f.hunks[0].decision = Decision::Rejected;
        let out = f.compose();
        assert!(out.contains("line 2\n") && out.contains("line eighteen\n"));
        f.hunks[1].decision = Decision::Rejected;
        assert_eq!(f.compose(), old);
        f.hunks[0].decision = Decision::Accepted;
        f.hunks[1].decision = Decision::Pending;
        assert_eq!(f.compose(), new);
    }

    #[test]
    fn new_file_is_one_block() {
        let f = FileReview::new("n.txt".into(), "n.txt".into(), None, "a\nb\n".into());
        assert_eq!(f.hunks.len(), 1);
        assert_eq!(f.stats(), (2, 0));
    }
}
