//! Settings modal (⌘,): visible agents, binary paths, extra arguments, and worktrees directory.

use crate::icons;
use crate::project::{Harness, SidePanel};
use crate::state::{self, AgentConfig};
use crate::style;
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::{h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use std::collections::HashMap;
use std::path::PathBuf;

pub enum SettingsEvent {
    Close,
    Changed,
}

struct AgentRowState {
    harness: Harness,
    visible: bool,
    path_input: Entity<InputState>,
    args_input: Entity<InputState>,
    /// `chat` / `terminal` (vacío = chat).
    open_as: String,
    /// `folder` / `worktree` (vacío = esta carpeta).
    workspace: String,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Section {
    Agents,
    Panel,
    Pulls,
    Worktrees,
}

impl Section {
    const ALL: [Section; 4] = [Section::Agents, Section::Panel, Section::Pulls, Section::Worktrees];

    fn label(self) -> &'static str {
        match self {
            Section::Agents => "Agents",
            Section::Panel => "Right panel",
            Section::Pulls => "Pull requests",
            Section::Worktrees => "Worktrees",
        }
    }

    fn icon(self) -> &'static str {
        match self {
            Section::Agents => "bot",
            Section::Panel => "panel-right",
            Section::Pulls => "git-pull-request",
            Section::Worktrees => "git-branch",
        }
    }
}

pub struct SettingsView {
    section: Section,
    agents: Vec<AgentRowState>,
    detected: HashMap<Harness, PathBuf>,
    worktrees_input: Entity<InputState>,
    hidden_panels: Vec<String>,
    merge_method: String,
    _subs: Vec<Subscription>,
}

impl EventEmitter<SettingsEvent> for SettingsView {}

impl SettingsView {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let saved = state::get(cx).settings.clone();
        let mut subs = Vec::new();
        let mut agents = Vec::new();

        for harness in Harness::AGENTS {
            let key = harness.key();
            let cfg = saved.agent(key);
            let default_cmd = harness.command().unwrap_or_default().to_string();
            let path_input = cx.new(|cx| {
                let mut s = InputState::new(window, cx).placeholder(format!("Auto-detect ({default_cmd})"));
                if !cfg.binary_path.is_empty() {
                    s.set_value(cfg.binary_path.clone(), window, cx);
                }
                s
            });
            let args_input = cx.new(|cx| {
                let mut s = InputState::new(window, cx).placeholder("Extra CLI arguments (optional)");
                if !cfg.extra_args.is_empty() {
                    s.set_value(cfg.extra_args.clone(), window, cx);
                }
                s
            });
            subs.push(cx.subscribe(&path_input, |this: &mut Self, _, ev: &InputEvent, cx| {
                if matches!(ev, InputEvent::Change) {
                    this.save(cx);
                }
            }));
            subs.push(cx.subscribe(&args_input, |this: &mut Self, _, ev: &InputEvent, cx| {
                if matches!(ev, InputEvent::Change) {
                    this.save(cx);
                }
            }));
            agents.push(AgentRowState { harness, visible: cfg.visible, path_input, args_input, open_as: cfg.open_as.clone(), workspace: cfg.workspace.clone() });
        }

        let worktrees_input = cx.new(|cx| {
            let mut s = InputState::new(window, cx).placeholder("~/.kuaky/worktrees");
            if !saved.worktrees_dir.is_empty() {
                s.set_value(saved.worktrees_dir.clone(), window, cx);
            }
            s
        });
        subs.push(cx.subscribe(&worktrees_input, |this: &mut Self, _, ev: &InputEvent, cx| {
            if matches!(ev, InputEvent::Change) {
                this.save(cx);
            }
        }));

        let this = Self {
            section: Section::Agents,
            agents,
            detected: HashMap::new(),
            worktrees_input,
            hidden_panels: saved.hidden_panels.clone(),
            merge_method: if saved.pr_merge_method.is_empty() { "merge".into() } else { saved.pr_merge_method.clone() },
            _subs: subs,
        };
        this.detect(cx);
        this
    }

    fn detect(&self, cx: &mut Context<Self>) {
        cx.spawn(async move |this, cx| {
            let found = cx
                .background_executor()
                .spawn(async {
                    Harness::AGENTS
                        .into_iter()
                        .filter_map(|h| Some((h, kuaky_git::find_agent(h.command()?)?)))
                        .collect::<HashMap<_, _>>()
                })
                .await;
            this.update(cx, |this, cx| {
                this.detected = found;
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn set_agent_pref(&mut self, index: usize, open_as: Option<&str>, workspace: Option<&str>, cx: &mut Context<Self>) {
        if let Some(row) = self.agents.get_mut(index) {
            if let Some(v) = open_as {
                row.open_as = v.to_string();
            }
            if let Some(v) = workspace {
                row.workspace = v.to_string();
            }
            self.save(cx);
            cx.notify();
        }
    }

    /// Dos opciones lado a lado (la elegida resaltada).
    fn choice(id: impl Into<ElementId>, label: &'static str, on: bool) -> Stateful<Div> {
        div()
            .id(id)
            .px_2()
            .py_0p5()
            .rounded_md()
            .text_xs()
            .cursor_pointer()
            .when(on, |d| d.bg(style::panel_hover()).text_color(style::text()))
            .when(!on, |d| d.text_color(style::muted()))
            .hover(|d| d.bg(style::panel_hover()))
            .child(label)
    }

    fn toggle_visible(&mut self, index: usize, cx: &mut Context<Self>) {
        if let Some(row) = self.agents.get_mut(index) {
            row.visible = !row.visible;
            self.save(cx);
            cx.notify();
        }
    }

    fn save(&self, cx: &mut Context<Self>) {
        let mut map = HashMap::new();
        for row in &self.agents {
            map.insert(
                row.harness.key().to_string(),
                AgentConfig {
                    visible: row.visible,
                    binary_path: row.path_input.read(cx).value().trim().to_string(),
                    extra_args: row.args_input.read(cx).value().trim().to_string(),
                    open_as: row.open_as.clone(),
                    workspace: row.workspace.clone(),
                },
            );
        }
        let worktrees_dir = self.worktrees_input.read(cx).value().trim().to_string();
        let (hidden, method) = (self.hidden_panels.clone(), self.merge_method.clone());
        state::update(cx, |s| {
            s.settings.agents = map;
            s.settings.worktrees_dir = worktrees_dir;
            s.settings.hidden_panels = hidden;
            s.settings.pr_merge_method = method;
        });
        cx.emit(SettingsEvent::Changed);
    }

    /// Pestañas de Settings (izquierda).
    fn nav(&self, cx: &mut Context<Self>) -> impl IntoElement {
        v_flex()
            .w(px(180.))
            .h_full()
            .flex_none()
            .p_2()
            .gap_0p5()
            .border_r_1()
            .border_color(style::border())
            .children(Section::ALL.into_iter().map(|sec| {
                let selected = self.section == sec;
                h_flex()
                    .id(SharedString::from(format!("settings-nav-{}", sec.label())))
                    .px_2()
                    .py_1p5()
                    .gap_2()
                    .items_center()
                    .rounded_md()
                    .text_sm()
                    .cursor_pointer()
                    .when(selected, |d| d.bg(style::panel_hover()).text_color(style::text()))
                    .when(!selected, |d| d.text_color(style::muted()))
                    .hover(|d| d.bg(style::panel_hover()))
                    .child(icons::ui(sec.icon(), px(14.), if selected { style::accent() } else { style::muted() }))
                    .child(sec.label())
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.section = sec;
                        cx.notify();
                    }))
            }))
    }

    fn toggle_panel(&mut self, key: &str, cx: &mut Context<Self>) {
        if let Some(i) = self.hidden_panels.iter().position(|h| h == key) {
            self.hidden_panels.remove(i);
        } else if SidePanel::ALL.len() - self.hidden_panels.len() > 1 {
            self.hidden_panels.push(key.to_string());
        }
        self.save(cx);
        cx.notify();
    }

    fn panels_section(&self, cx: &mut Context<Self>) -> impl IntoElement {
        v_flex()
            .gap_2()
            .child(div().text_xs().text_color(style::muted()).child("RIGHT PANEL TABS"))
            .child(
                v_flex()
                    .p_1()
                    .rounded_lg()
                    .bg(style::bg())
                    .border_1()
                    .border_color(style::border())
                    .children(SidePanel::ALL.into_iter().map(|p| {
                        let on = !self.hidden_panels.iter().any(|h| h == p.key());
                        let key = p.key();
                        h_flex()
                            .id(SharedString::from(format!("panel-toggle-{key}")))
                            .px_2()
                            .py_1p5()
                            .gap_3()
                            .items_center()
                            .rounded_md()
                            .cursor_pointer()
                            .hover(|d| d.bg(style::panel_hover()))
                            .child(switch(on))
                            .child(
                                v_flex()
                                    .flex_1()
                                    .min_w_0()
                                    .child(div().text_sm().child(p.label()))
                                    .child(div().text_xs().text_color(style::faint()).whitespace_normal().child(p.about())),
                            )
                            .on_click(cx.listener(move |this, _, _, cx| this.toggle_panel(key, cx)))
                    })),
            )
    }

    fn merge_section(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let options = [("merge", "Merge commit"), ("squash", "Squash"), ("rebase", "Rebase")];
        v_flex()
            .gap_2()
            .child(div().text_xs().text_color(style::muted()).child("PULL REQUESTS"))
            .child(
                v_flex()
                    .p_3()
                    .gap_2()
                    .rounded_lg()
                    .bg(style::bg())
                    .border_1()
                    .border_color(style::border())
                    .child(div().text_xs().text_color(style::faint()).child("The Merge button merges with"))
                    .child(h_flex().gap_1().children(options.into_iter().map(|(key, label)| {
                        let selected = self.merge_method == key;
                        div()
                            .id(SharedString::from(format!("merge-method-{key}")))
                            .px_3()
                            .py_1()
                            .rounded_md()
                            .text_sm()
                            .cursor_pointer()
                            .border_1()
                            .border_color(if selected { style::accent().into() } else { style::border() })
                            .text_color(if selected { style::accent() } else { style::muted() })
                            .child(label)
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.merge_method = key.to_string();
                                this.save(cx);
                                cx.notify();
                            }))
                    }))),
            )
    }

    fn on_key(&mut self, ev: &KeyDownEvent, _: &mut Window, cx: &mut Context<Self>) {
        if ev.keystroke.key == "escape" {
            cx.stop_propagation();
            cx.emit(SettingsEvent::Close);
        }
    }
}

impl Render for SettingsView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        v_flex()
            .w(px(760.))
            .h(px(560.))
            .rounded_xl()
            .border_1()
            .border_color(style::border())
            .bg(style::panel())
            .shadow_lg()
            .overflow_hidden()
            .capture_key_down(cx.listener(Self::on_key))
            .child(
                h_flex()
                    .px_4()
                    .py_3()
                    .justify_between()
                    .border_b_1()
                    .border_color(style::border())
                    .child(
                        h_flex()
                            .gap_2()
                            .child(icons::ui("settings", px(16.), style::accent()))
                            .child(div().text_sm().font_weight(FontWeight::SEMIBOLD).child("Settings")),
                    )
                    .child(
                        div()
                            .id("settings-close")
                            .p_1()
                            .rounded_md()
                            .cursor_pointer()
                            .hover(|d| d.bg(style::panel_hover()))
                            .child(icons::ui("x", px(14.), style::muted()))
                            .on_click(cx.listener(|_, _, _, cx| cx.emit(SettingsEvent::Close))),
                    ),
            )
            .child(
                h_flex()
                    .flex_1()
                    .min_h_0()
                    .items_start()
                    .child(self.nav(cx))
                    .child(div()
                    .id("settings-scroll")
                    .min_w_0()
                    .flex_1()
                    .overflow_y_scroll()
                    .child(
                        v_flex()
                            .p_4()
                            .gap_5()
                            .when(self.section == Section::Agents, |d| d.child(
                                v_flex()
                                    .gap_3()
                                    .child(div().text_xs().text_color(style::muted()).child("AGENTS"))
                                    .children(self.agents.iter().enumerate().map(|(i, row)| {
                                        let detected = self.detected.get(&row.harness);
                                        let status = match detected {
                                            Some(p) => (crate::home::tilde(p), style::good()),
                                            None => ("Not detected".to_string(), style::faint()),
                                        };
                                        let on = row.visible;
                                        v_flex()
                                            .p_3()
                                            .gap_2()
                                            .rounded_lg()
                                            .bg(style::bg())
                                            .border_1()
                                            .border_color(style::border())
                                            .child(
                                                h_flex()
                                                    .gap_2()
                                                    .items_center()
                                                    .when_some(row.harness.agent(), |d, a| {
                                                        d.child(icons::logo(icons::agent(a), px(18.)))
                                                    })
                                                    .child(div().text_sm().font_weight(FontWeight::MEDIUM).child(row.harness.label()))
                                                    .child(
                                                        div()
                                                            .flex_1()
                                                            .min_w_0()
                                                            .truncate()
                                                            .text_xs()
                                                            .text_color(status.1)
                                                            .child(status.0),
                                                    )
                                                    .child(
                                                        h_flex()
                                                            .id(("toggle-agent", i))
                                                            .gap_1p5()
                                                            .items_center()
                                                            .cursor_pointer()
                                                            .text_xs()
                                                            .text_color(if on { style::text() } else { style::faint() })
                                                            .child(if on { "Visible" } else { "Hidden" })
                                                            .child(
                                                                div()
                                                                    .w(px(28.))
                                                                    .h(px(16.))
                                                                    .p(px(2.))
                                                                    .rounded_full()
                                                                    .flex()
                                                                    .when(on, |d| d.justify_end())
                                                                    .bg(if on {
                                                                        style::soft(style::accent(), 0.85)
                                                                    } else {
                                                                        style::soft(style::faint(), 0.5)
                                                                    })
                                                                    .child(div().size(px(12.)).rounded_full().bg(style::text())),
                                                            )
                                                            .on_click(cx.listener(move |this, _, _, cx| this.toggle_visible(i, cx))),
                                                    ),
                                            )
                                            .child(
                                                h_flex()
                                                    .gap_2()
                                                    .child(
                                                        v_flex()
                                                            .flex_1()
                                                            .gap_1()
                                                            .child(div().text_xs().text_color(style::faint()).child("Executable path"))
                                                            .child(Input::new(&row.path_input)),
                                                    )
                                                    .child(
                                                        v_flex()
                                                            .flex_1()
                                                            .gap_1()
                                                            .child(div().text_xs().text_color(style::faint()).child("Extra arguments"))
                                                            .child(Input::new(&row.args_input)),
                                                    ),
                                            )
                                            // Cómo lo abre "New session".
                                            .child({
                                                let chat = row.open_as != "terminal";
                                                let worktree = row.workspace == "worktree";
                                                h_flex()
                                                    .gap_4()
                                                    .flex_wrap()
                                                    .items_center()
                                                    .child(
                                                        h_flex()
                                                            .gap_1()
                                                            .items_center()
                                                            .child(div().mr_1().text_xs().text_color(style::faint()).child("Open as"))
                                                            .child(Self::choice(("agent-chat", i), "Chat", chat).on_click(cx.listener(move |this, _, _, cx| this.set_agent_pref(i, Some("chat"), None, cx))))
                                                            .child(Self::choice(("agent-term", i), "Terminal", !chat).on_click(cx.listener(move |this, _, _, cx| this.set_agent_pref(i, Some("terminal"), None, cx)))),
                                                    )
                                                    .child(
                                                        h_flex()
                                                            .gap_1()
                                                            .items_center()
                                                            .child(div().mr_1().text_xs().text_color(style::faint()).child("Works in"))
                                                            .child(Self::choice(("agent-folder", i), "This folder", !worktree).on_click(cx.listener(move |this, _, _, cx| this.set_agent_pref(i, None, Some("folder"), cx))))
                                                            .child(Self::choice(("agent-wt", i), "New branch", worktree).on_click(cx.listener(move |this, _, _, cx| this.set_agent_pref(i, None, Some("worktree"), cx)))),
                                                    )
                                            })
                                    })),
                            ))
                            .when(self.section == Section::Panel, |d| d.child(self.panels_section(cx)))
                            .when(self.section == Section::Pulls, |d| d.child(self.merge_section(cx)))
                            .when(self.section == Section::Worktrees, |d| d.child(
                                v_flex()
                                    .gap_2()
                                    .child(div().text_xs().text_color(style::muted()).child("GIT WORKTREES"))
                                    .child(
                                        v_flex()
                                            .p_3()
                                            .gap_1p5()
                                            .rounded_lg()
                                            .bg(style::bg())
                                            .border_1()
                                            .border_color(style::border())
                                            .child(div().text_xs().text_color(style::faint()).child("Worktrees folder"))
                                            .child(Input::new(&self.worktrees_input))
                                            .child(
                                                div()
                                                    .text_xs()
                                                    .text_color(style::faint())
                                                    .child("Separate branch copies are created inside <folder>/<repo>/<name>."),
                                            ),
                                    ),
                            )),
                    )
                    .h_full()),
            )
            .child(
                h_flex()
                    .px_4()
                    .py_2p5()
                    .justify_between()
                    .items_center()
                    .border_t_1()
                    .border_color(style::border())
                    .child(div().text_xs().text_color(style::faint()).child("Saved automatically to state.json"))
                    .child(
                        div()
                            .id("settings-done")
                            .px_3()
                            .py_1()
                            .rounded_md()
                            .text_xs()
                            .font_weight(FontWeight::MEDIUM)
                            .cursor_pointer()
                            .bg(style::accent())
                            .text_color(style::bg())
                            .child("Done")
                            .on_click(cx.listener(|_, _, _, cx| cx.emit(SettingsEvent::Close))),
                    ),
            )
    }
}

fn switch(on: bool) -> impl IntoElement {
    div()
        .w(px(26.))
        .h(px(15.))
        .p(px(2.))
        .flex_none()
        .rounded_full()
        .bg(if on { style::accent().into() } else { style::border() })
        .child(div().size(px(11.)).rounded_full().bg(style::text()).when(on, |d| d.ml(px(11.))))
}
