//! Settings modal (⌘,): visible agents, binary paths, extra arguments, and worktrees directory.

use crate::icons;
use crate::project::Harness;
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
}

pub struct SettingsView {
    agents: Vec<AgentRowState>,
    detected: HashMap<Harness, PathBuf>,
    worktrees_input: Entity<InputState>,
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
            agents.push(AgentRowState { harness, visible: cfg.visible, path_input, args_input });
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

        let this = Self { agents, detected: HashMap::new(), worktrees_input, _subs: subs };
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
                },
            );
        }
        let worktrees_dir = self.worktrees_input.read(cx).value().trim().to_string();
        state::update(cx, |s| {
            s.settings.agents = map;
            s.settings.worktrees_dir = worktrees_dir;
        });
        cx.emit(SettingsEvent::Changed);
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
            .w(px(640.))
            .max_h(px(620.))
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
                div()
                    .id("settings-scroll")
                    .flex_1()
                    .overflow_y_scroll()
                    .child(
                        v_flex()
                            .p_4()
                            .gap_5()
                            .child(
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
                                    })),
                            )
                            .child(
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
                            ),
                    ),
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
