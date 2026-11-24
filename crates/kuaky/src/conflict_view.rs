//! Interactive merge conflict resolver tab: shows each conflicted file with
//! Ours / Theirs blocks, Accept ours / theirs / both per block, optional
//! "Resolve with agent" launcher, and Complete / Abort merge actions.

use crate::icons;
use crate::project::Harness;
use crate::style;
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::component::{h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use kuaky_git::{ConflictChoice, ConflictFile, Repo, conflicts::Segment};
use std::collections::HashMap;

pub enum ConflictEvent {
    /// User clicked "Resolve with agent" for the chosen harness with the current conflicted file list.
    ResolveWithAgent { harness: Harness, files: Vec<String> },
    /// Merge was completed or aborted cleanly.
    MergeFinished,
}

struct FileEntry {
    path: String,
    parsed: ConflictFile,
    choices: HashMap<usize, ConflictChoice>,
    remaining_markers: usize,
    error: Option<String>,
}

pub struct ConflictView {
    repo: Repo,
    files: Vec<FileEntry>,
    selected: usize,
    agents: Vec<Harness>,
    agent_picker_open: bool,
    status_message: Option<(String, bool)>,
    busy: bool,
}

impl EventEmitter<ConflictEvent> for ConflictView {}

impl ConflictView {
    pub fn new(repo: Repo, initial_files: Vec<String>, agents: Vec<Harness>, cx: &mut Context<Self>) -> Self {
        let mut this = Self {
            repo,
            files: Vec::new(),
            selected: 0,
            agents,
            agent_picker_open: false,
            status_message: None,
            busy: false,
        };
        this.load_files(initial_files, cx);
        this
    }

    pub fn set_agents(&mut self, agents: Vec<Harness>, cx: &mut Context<Self>) {
        self.agents = agents;
        cx.notify();
    }

    pub fn reload(&mut self, cx: &mut Context<Self>) {
        let files = self.repo.conflicted_files().unwrap_or_default();
        self.load_files(files, cx);
    }

    fn load_files(&mut self, paths: Vec<String>, cx: &mut Context<Self>) {
        let mut entries = Vec::new();
        for path in paths {
            let full = self.repo.root().join(&path);
            match std::fs::read_to_string(&full) {
                Ok(content) => {
                    let parsed = ConflictFile::parse(&content);
                    let remaining_markers = parsed.conflict_count();
                    entries.push(FileEntry {
                        path,
                        parsed,
                        choices: HashMap::new(),
                        remaining_markers,
                        error: None,
                    });
                }
                Err(err) => entries.push(FileEntry {
                    path,
                    parsed: ConflictFile { segments: Vec::new() },
                    choices: HashMap::new(),
                    remaining_markers: 1,
                    error: Some(format!("Could not read file: {err}")),
                }),
            }
        }
        self.files = entries;
        if self.selected >= self.files.len() {
            self.selected = 0;
        }
        cx.notify();
    }

    fn choose(&mut self, file_idx: usize, block_id: usize, choice: ConflictChoice, cx: &mut Context<Self>) {
        let Some(entry) = self.files.get_mut(file_idx) else { return };
        entry.choices.insert(block_id, choice);
        let rendered = entry.parsed.render_with(&entry.choices);
        let full = self.repo.root().join(&entry.path);
        match std::fs::write(&full, &rendered) {
            Ok(()) => {
                let total_blocks = entry.parsed.conflict_count();
                entry.remaining_markers = total_blocks.saturating_sub(entry.choices.len());
                entry.error = None;
                if entry.remaining_markers == 0 {
                    let _ = self.repo.stage_file(&entry.path);
                }
            }
            Err(err) => {
                entry.error = Some(format!("Failed to write file: {err}"));
            }
        }
        cx.notify();
    }

    fn undo_choice(&mut self, file_idx: usize, block_id: usize, cx: &mut Context<Self>) {
        let Some(entry) = self.files.get_mut(file_idx) else { return };
        entry.choices.remove(&block_id);
        let rendered = entry.parsed.render_with(&entry.choices);
        let full = self.repo.root().join(&entry.path);
        if std::fs::write(&full, &rendered).is_ok() {
            let total_blocks = entry.parsed.conflict_count();
            entry.remaining_markers = total_blocks.saturating_sub(entry.choices.len());
        }
        cx.notify();
    }

    pub fn unresolved_files(&self) -> Vec<String> {
        self.files
            .iter()
            .filter(|f| f.remaining_markers > 0)
            .map(|f| f.path.clone())
            .collect()
    }

    fn all_resolved(&self) -> bool {
        self.files.iter().all(|f| f.remaining_markers == 0)
    }

    fn complete_merge(&mut self, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }
        self.busy = true;
        self.status_message = None;
        let repo = self.repo.clone();
        cx.spawn(async move |this, cx| {
            let res = cx.background_executor().spawn(async move { repo.complete_merge(None) }).await;
            this.update(cx, |this, cx| {
                this.busy = false;
                match res {
                    Ok(()) => {
                        this.status_message = Some(("Merge completed cleanly.".into(), false));
                        cx.emit(ConflictEvent::MergeFinished);
                    }
                    Err(err) => {
                        this.status_message = Some((format!("{err:#}"), true));
                    }
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn abort_merge(&mut self, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }
        self.busy = true;
        let repo = self.repo.clone();
        cx.spawn(async move |this, cx| {
            let res = cx.background_executor().spawn(async move { repo.abort_merge() }).await;
            this.update(cx, |this, cx| {
                this.busy = false;
                match res {
                    Ok(()) => cx.emit(ConflictEvent::MergeFinished),
                    Err(err) => this.status_message = Some((format!("{err:#}"), true)),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }
}

impl Render for ConflictView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let unresolved_count = self.files.iter().filter(|f| f.remaining_markers > 0).count();
        let can_complete = self.all_resolved() && !self.busy;
        let has_agents = !self.agents.is_empty();

        v_flex()
            .size_full()
            .bg(style::bg())
            .child(
                h_flex()
                    .px_3()
                    .py_2()
                    .gap_2()
                    .items_center()
                    .border_b_1()
                    .border_color(style::border())
                    .bg(style::panel())
                    .child(icons::ui("git-merge-conflict", px(15.), if unresolved_count > 0 { style::bad() } else { style::good() }))
                    .child(div().text_sm().font_weight(FontWeight::SEMIBOLD).child("Merge Conflicts"))
                    .child(div().text_xs().text_color(style::muted()).child(if unresolved_count == 0 {
                        "All conflicts resolved".to_string()
                    } else if unresolved_count == 1 {
                        "1 file remaining".to_string()
                    } else {
                        format!("{unresolved_count} files remaining")
                    }))
                    .child(div().flex_1())
                    .when(has_agents && unresolved_count > 0, |d| {
                        d.child(
                            h_flex()
                                .id("resolve-with-agent-btn")
                                .px_2p5()
                                .py_1()
                                .gap_1p5()
                                .rounded_md()
                                .text_xs()
                                .cursor_pointer()
                                .border_1()
                                .border_color(style::soft(style::accent(), 0.5))
                                .bg(style::soft(style::accent(), 0.1))
                                .text_color(style::accent())
                                .hover(|d| d.bg(style::soft(style::accent(), 0.2)))
                                .tooltip(|window, cx| {
                                    Tooltip::new("Launch an agent to resolve conflicts and run tests without committing (uses tokens)")
                                        .build(window, cx)
                                })
                                .child("Resolve with agent")
                                .on_click(cx.listener(|this, _, _, cx| {
                                    if this.agents.len() == 1 {
                                        let h = this.agents[0];
                                        let files = this.unresolved_files();
                                        cx.emit(ConflictEvent::ResolveWithAgent { harness: h, files });
                                    } else {
                                        this.agent_picker_open = !this.agent_picker_open;
                                        cx.notify();
                                    }
                                })),
                        )
                    })
                    .child(
                        div()
                            .id("conflict-reload")
                            .px_2()
                            .py_1()
                            .rounded_md()
                            .text_xs()
                            .cursor_pointer()
                            .border_1()
                            .border_color(style::border())
                            .text_color(style::muted())
                            .hover(|d| d.bg(style::panel_hover()).text_color(style::text()))
                            .child("Reload")
                            .on_click(cx.listener(|this, _, _, cx| this.reload(cx))),
                    )
                    .child(
                        div()
                            .id("conflict-abort")
                            .px_2()
                            .py_1()
                            .rounded_md()
                            .text_xs()
                            .cursor_pointer()
                            .border_1()
                            .border_color(style::soft(style::bad(), 0.4))
                            .text_color(style::bad())
                            .hover(|d| d.bg(style::soft(style::bad(), 0.12)))
                            .child("Abort merge")
                            .on_click(cx.listener(|this, _, _, cx| this.abort_merge(cx))),
                    )
                    .child(
                        div()
                            .id("conflict-complete")
                            .px_2p5()
                            .py_1()
                            .rounded_md()
                            .text_xs()
                            .font_weight(FontWeight::MEDIUM)
                            .cursor_pointer()
                            .when(can_complete, |d| d.bg(style::good()).text_color(style::bg()))
                            .when(!can_complete, |d| {
                                d.bg(style::panel_hover()).text_color(style::faint()).opacity(0.6)
                            })
                            .child("Complete merge")
                            .on_click(cx.listener(move |this, _, _, cx| {
                                if can_complete {
                                    this.complete_merge(cx);
                                }
                            })),
                    ),
            )
            .when(self.agent_picker_open, |d| {
                let agents = self.agents.clone();
                d.child(
                    h_flex()
                        .px_3()
                        .py_2()
                        .gap_2()
                        .items_center()
                        .border_b_1()
                        .border_color(style::border())
                        .bg(style::panel())
                        .child(div().text_xs().text_color(style::muted()).child("Choose agent to resolve conflicts:"))
                        .children(agents.into_iter().map(|h| {
                            h_flex()
                                .id(SharedString::from(format!("resolve-agent-{}", h.key())))
                                .px_2()
                                .py_1()
                                .gap_1p5()
                                .rounded_md()
                                .text_xs()
                                .cursor_pointer()
                                .border_1()
                                .border_color(style::border())
                                .bg(style::bg())
                                .hover(|d| d.bg(style::panel_hover()))
                                .when_some(h.agent(), |d, a| d.child(icons::logo(icons::agent(a), px(14.))))
                                .child(h.label())
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.agent_picker_open = false;
                                    let files = this.unresolved_files();
                                    cx.emit(ConflictEvent::ResolveWithAgent { harness: h, files });
                                    cx.notify();
                                }))
                        })),
                )
            })
            .when_some(self.status_message.clone(), |d, (msg, is_err)| {
                d.child(
                    div()
                        .px_3()
                        .py_1p5()
                        .text_xs()
                        .border_b_1()
                        .border_color(style::border())
                        .text_color(if is_err { style::bad() } else { style::good() })
                        .child(msg),
                )
            })
            .child(
                h_flex()
                    .px_3()
                    .py_1p5()
                    .gap_1p5()
                    .flex_wrap()
                    .border_b_1()
                    .border_color(style::border())
                    .children(self.files.iter().enumerate().map(|(i, f)| {
                        let active = i == self.selected;
                        let done = f.remaining_markers == 0;
                        h_flex()
                            .id(("cfile", i))
                            .px_2()
                            .py_1()
                            .gap_1p5()
                            .rounded_md()
                            .text_xs()
                            .cursor_pointer()
                            .border_1()
                            .border_color(if active { style::accent() } else { style::border() })
                            .when(active, |d| d.bg(style::panel_hover()))
                            .child(div().size(px(6.)).rounded_full().bg(if done { style::good() } else { style::bad() }))
                            .child(f.path.clone())
                            .when(!done, |d| d.child(div().text_color(style::faint()).child(format!("({})", f.remaining_markers))))
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.selected = i;
                                cx.notify();
                            }))
                    })),
            )
            .child(div().id("conflict-body").flex_1().min_h_0().overflow_y_scroll().child(self.render_selected_file(cx)))
    }
}

impl ConflictView {
    fn render_selected_file(&self, cx: &mut Context<Self>) -> AnyElement {
        let Some(entry) = self.files.get(self.selected) else {
            return div().p_4().text_sm().text_color(style::muted()).child("No conflicted files.").into_any_element();
        };
        if let Some(err) = &entry.error {
            return div().p_4().text_sm().text_color(style::bad()).child(err.clone()).into_any_element();
        }
        let file_idx = self.selected;
        let blocks: Vec<AnyElement> = entry
            .parsed
            .segments
            .iter()
            .enumerate()
            .map(|(si, seg)| match seg {
                Segment::Text(t) => {
                    let preview: String = {
                        let lines: Vec<&str> = t.lines().collect();
                        if lines.len() > 8 {
                            let head = lines[..3].join("\n");
                            let tail = lines[lines.len() - 3..].join("\n");
                            format!("{head}\n  ⋯ {} unchanged lines ⋯\n{tail}", lines.len() - 6)
                        } else {
                            t.trim_end_matches('\n').to_string()
                        }
                    };
                    if preview.is_empty() {
                        div().into_any_element()
                    } else {
                        div()
                            .px_3()
                            .py_1()
                            .font_family(style::mono_font())
                            .text_xs()
                            .text_color(style::faint())
                            .child(preview)
                            .into_any_element()
                    }
                }
                Segment::Conflict(b) => {
                    let choice = entry.choices.get(&b.id).copied();
                    let bid = b.id;
                    let btn = |id: (&'static str, usize), label: &'static str, selected: bool, color: Rgba| {
                        div()
                            .id(id)
                            .px_2()
                            .py_0p5()
                            .rounded_sm()
                            .text_xs()
                            .cursor_pointer()
                            .border_1()
                            .border_color(if selected { color } else { style::border() })
                            .when(selected, |d| d.bg(style::soft(color, 0.2)).text_color(color))
                            .when(!selected, |d| d.text_color(style::muted()).hover(|d| d.bg(style::panel_hover()).text_color(style::text())))
                            .child(label)
                    };
                    v_flex()
                        .mx_3()
                        .my_2()
                        .rounded_lg()
                        .border_1()
                        .border_color(if choice.is_some() { style::soft(style::good(), 0.45) } else { style::soft(style::bad(), 0.5) })
                        .overflow_hidden()
                        .child(
                            h_flex()
                                .px_3()
                                .py_1p5()
                                .gap_2()
                                .items_center()
                                .bg(style::panel())
                                .border_b_1()
                                .border_color(style::border())
                                .child(div().text_xs().text_color(style::muted()).child(format!("Conflict #{} (line {})", bid + 1, b.start_line)))
                                .child(div().flex_1())
                                .child(btn(("ours", si), "Accept ours", choice == Some(ConflictChoice::Ours), style::accent()).on_click(
                                    cx.listener(move |this, _, _, cx| this.choose(file_idx, bid, ConflictChoice::Ours, cx)),
                                ))
                                .child(btn(("theirs", si), "Accept theirs", choice == Some(ConflictChoice::Theirs), style::output()).on_click(
                                    cx.listener(move |this, _, _, cx| this.choose(file_idx, bid, ConflictChoice::Theirs, cx)),
                                ))
                                .child(btn(("both", si), "Accept both", choice == Some(ConflictChoice::Both), style::good()).on_click(
                                    cx.listener(move |this, _, _, cx| this.choose(file_idx, bid, ConflictChoice::Both, cx)),
                                ))
                                .when(choice.is_some(), |d| {
                                    d.child(
                                        div()
                                            .id(("undo", si))
                                            .px_1p5()
                                            .py_0p5()
                                            .text_xs()
                                            .cursor_pointer()
                                            .text_color(style::faint())
                                            .hover(|d| d.text_color(style::text()))
                                            .child("Undo")
                                            .on_click(cx.listener(move |this, _, _, cx| this.undo_choice(file_idx, bid, cx))),
                                    )
                                }),
                        )
                        .child(
                            h_flex()
                                .items_start()
                                .child(
                                    v_flex()
                                        .flex_1()
                                        .min_w_0()
                                        .border_r_1()
                                        .border_color(style::border())
                                        .bg(style::soft(style::accent(), 0.05))
                                        .child(
                                            div()
                                                .px_3()
                                                .py_1()
                                                .text_xs()
                                                .text_color(style::accent())
                                                .child(if b.ours_label.is_empty() {
                                                    "Ours (current)".to_string()
                                                } else {
                                                    format!("Ours · {}", b.ours_label)
                                                }),
                                        )
                                        .child(
                                            div()
                                                .px_3()
                                                .pb_2()
                                                .font_family(style::mono_font())
                                                .text_xs()
                                                .text_color(style::text())
                                                .child(b.ours.trim_end_matches('\n').to_string()),
                                        ),
                                )
                                .child(
                                    v_flex()
                                        .flex_1()
                                        .min_w_0()
                                        .bg(style::soft(style::output(), 0.05))
                                        .child(
                                            div()
                                                .px_3()
                                                .py_1()
                                                .text_xs()
                                                .text_color(style::output())
                                                .child(if b.theirs_label.is_empty() {
                                                    "Theirs (incoming)".to_string()
                                                } else {
                                                    format!("Theirs · {}", b.theirs_label)
                                                }),
                                        )
                                        .child(
                                            div()
                                                .px_3()
                                                .pb_2()
                                                .font_family(style::mono_font())
                                                .text_xs()
                                                .text_color(style::text())
                                                .child(b.theirs.trim_end_matches('\n').to_string()),
                                        ),
                                ),
                        )
                        .into_any_element()
                }
            })
            .collect();

        v_flex().py_2().children(blocks).into_any_element()
    }
}
