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
    Appearance,
    Agents,
    Panel,
    Pulls,
    Worktrees,
}

impl Section {
    const ALL: [Section; 5] = [Section::Appearance, Section::Agents, Section::Panel, Section::Pulls, Section::Worktrees];

    fn label(self) -> &'static str {
        match self {
            Section::Appearance => "Appearance",
            Section::Agents => "Agents",
            Section::Panel => "Right panel",
            Section::Pulls => "Pull requests",
            Section::Worktrees => "Worktrees",
        }
    }

    fn icon(self) -> &'static str {
        match self {
            Section::Appearance => "palette",
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
    /// Instalaciones desde aquí: `None` = en curso, `Some(error)` = falló.
    installing: HashMap<Harness, Option<String>>,
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
            section: if std::env::var_os("KUAKY_DEMO_APPEARANCE").is_some() { Section::Appearance } else { Section::Agents },
            agents,
            detected: HashMap::new(),
            installing: HashMap::new(),
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

    /// Instala el agente con su instalador oficial y vuelve a buscarlo.
    fn install(&mut self, harness: Harness, cx: &mut Context<Self>) {
        let Some((program, args)) = install_command(harness) else {
            cx.open_url(ANTIGRAVITY_URL);
            return;
        };
        if kuaky_git::find_program(program).is_none() {
            let need = if program == "npm" { "Needs Node.js (nodejs.org)".into() } else { format!("Needs {program}") };
            self.installing.insert(harness, Some(need));
            cx.notify();
            return;
        }
        self.installing.insert(harness, None);
        cx.notify();
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move {
                    let out = kuaky_git::command(program).args(args).output().map_err(|e| e.to_string())?;
                    if out.status.success() {
                        return Ok(());
                    }
                    let err = String::from_utf8_lossy(&out.stderr);
                    Err(err.lines().rev().find(|l| !l.trim().is_empty()).unwrap_or("Install failed").trim().to_string())
                })
                .await;
            this.update(cx, |this, cx| {
                match result {
                    Ok(()) => {
                        this.installing.remove(&harness);
                        this.detect(cx);
                        cx.emit(SettingsEvent::Changed);
                    }
                    Err(e) => {
                        this.installing.insert(harness, Some(e));
                    }
                }
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

    /// Elige el tema para su apariencia (claro u oscuro) y lo muestra de inmediato.
    fn pick_theme(&mut self, name: String, dark: bool, window: &mut Window, cx: &mut Context<Self>) {
        let system_dark = crate::theme::system_is_dark(Some(window), cx);
        state::update(cx, |s| {
            let t = &mut s.settings.theme;
            if dark {
                t.dark = name;
            } else {
                t.light = name;
            }
            t.mode = crate::theme::mode_after_pick(&t.mode, system_dark, dark).to_string();
        });
        crate::theme::apply(Some(window), cx);
        cx.emit(SettingsEvent::Changed);
        cx.notify();
    }

    fn set_theme_mode(&mut self, mode: &'static str, window: &mut Window, cx: &mut Context<Self>) {
        state::update(cx, |s| s.settings.theme.mode = mode.to_string());
        crate::theme::apply(Some(window), cx);
        cx.emit(SettingsEvent::Changed);
        cx.notify();
    }

    fn appearance_section(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let saved = state::get(cx).settings.theme.clone();
        let choices = ThemeChoice::all(cx);
        v_flex()
            .gap_4()
            .child(self.theme_mode_picker(&saved.mode, cx))
            .child(self.theme_group(&choices, true, saved.dark_or_default(), cx))
            .child(self.theme_group(&choices, false, saved.light_or_default(), cx))
            .child(themes_folder_row(cx))
    }

    /// System / Light / Dark.
    fn theme_mode_picker(&self, mode: &str, cx: &mut Context<Self>) -> impl IntoElement {
        let mode = if mode.is_empty() { "system" } else { mode };
        let modes = [("system", "System"), ("light", "Light"), ("dark", "Dark")];
        v_flex()
            .gap_2()
            .child(div().text_xs().text_color(style::muted()).child("APPEARANCE"))
            .child(h_flex().gap_1().children(modes.into_iter().map(|(key, label)| {
                div()
                    .id(SharedString::from(format!("theme-mode-{key}")))
                    .px_3()
                    .py_1()
                    .rounded_md()
                    .text_sm()
                    .cursor_pointer()
                    .border_1()
                    .border_color(if mode == key { style::accent().into() } else { style::border() })
                    .text_color(if mode == key { style::accent() } else { style::muted() })
                    .child(label)
                    .on_click(cx.listener(move |this, _, window, cx| this.set_theme_mode(key, window, cx)))
            })))
    }

    fn theme_group(&self, choices: &[ThemeChoice], dark: bool, chosen: &str, cx: &mut Context<Self>) -> impl IntoElement {
        v_flex()
            .gap_1p5()
            .child(div().text_xs().text_color(style::faint()).child(if dark { "DARK THEMES" } else { "LIGHT THEMES" }))
            .children(choices.iter().filter(|c| c.dark == dark).map(|c| self.theme_row(c, c.name == chosen, cx)))
    }

    fn theme_row(&self, c: &ThemeChoice, selected: bool, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let (name, dark) = (c.name.clone(), c.dark);
        h_flex()
            .id(SharedString::from(format!("theme-{name}")))
            .p_2()
            .gap_3()
            .items_center()
            .rounded_md()
            .border_1()
            .border_color(if selected { style::accent().into() } else { style::border() })
            .bg(style::bg())
            .cursor_pointer()
            .hover(|d| d.bg(style::panel_hover()))
            .child(theme_swatch(&c.swatch))
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .child(
                        h_flex()
                            .gap_2()
                            .child(div().text_sm().font_weight(FontWeight::MEDIUM).child(c.name.clone()))
                            .when(c.custom, |d| d.child(div().text_xs().text_color(style::faint()).child("custom")))
                            .when(c.live, |d| d.child(div().text_xs().text_color(style::accent()).child("in use"))),
                    )
                    .child(div().text_xs().text_color(style::faint()).whitespace_normal().child(c.about.clone())),
            )
            .on_click(cx.listener(move |this, _, window, cx| this.pick_theme(name.clone(), dark, window, cx)))
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
                                        let install = self.installing.get(&row.harness);
                                        let status = match (detected, install) {
                                            (Some(p), _) => (crate::home::tilde(p), style::good()),
                                            (None, Some(None)) => ("Installing…".to_string(), style::muted()),
                                            (None, Some(Some(err))) => (err.clone(), style::warn()),
                                            (None, None) => ("Not detected".to_string(), style::faint()),
                                        };
                                        // Sin detectar ni ruta propia: se puede instalar desde aquí.
                                        let can_install = detected.is_none()
                                            && !matches!(install, Some(None))
                                            && row.path_input.read(cx).value().trim().is_empty();
                                        let harness = row.harness;
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
                                                    .when(can_install, |d| d.child(
                                                        h_flex()
                                                            .id(("install-agent", i))
                                                            .gap_1()
                                                            .px_2()
                                                            .py_0p5()
                                                            .rounded_md()
                                                            .border_1()
                                                            .border_color(style::border())
                                                            .text_xs()
                                                            .cursor_pointer()
                                                            .hover(|d| d.bg(style::panel_hover()))
                                                            .child(icons::ui("download", px(12.), style::accent()))
                                                            .child(if install_command(harness).is_some() { "Install" } else { "Download" })
                                                            .on_click(cx.listener(move |this, _, _, cx| this.install(harness, cx))),
                                                    ))
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
                            .when(self.section == Section::Appearance, |d| d.child(self.appearance_section(cx)))
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
                            .text_color(style::on_accent())
                            .child("Done")
                            .on_click(cx.listener(|_, _, _, cx| cx.emit(SettingsEvent::Close))),
                    ),
            )
    }
}

/// Un tema tal como se muestra en la lista de Appearance.
struct ThemeChoice {
    name: String,
    about: String,
    dark: bool,
    swatch: Vec<Rgba>,
    custom: bool,
    live: bool,
}

impl ThemeChoice {
    fn all(cx: &App) -> Vec<Self> {
        let registry = cx.global::<crate::theme::Registry>();
        registry
            .themes
            .iter()
            .map(|t| {
                let p = &t.palette;
                Self {
                    name: t.name.clone(),
                    about: t.description.clone(),
                    dark: t.is_dark(),
                    swatch: vec![p.background, p.surface, p.text, p.accent, p.data_output, p.warning],
                    custom: t.source.is_some(),
                    live: t.name == registry.active,
                }
            })
            .collect()
    }
}

fn theme_swatch(colors: &[Rgba]) -> impl IntoElement {
    h_flex()
        .flex_none()
        .rounded_sm()
        .overflow_hidden()
        .border_1()
        .border_color(style::border())
        .children(colors.iter().map(|c| div().w(px(14.)).h(px(28.)).bg(*c)))
}

/// Carpeta de temas propios y botón para volver a leerlos.
fn themes_folder_row(cx: &mut Context<SettingsView>) -> impl IntoElement {
    let folder = crate::theme::user_dir().map(|p| crate::home::tilde(&p)).unwrap_or_default();
    h_flex()
        .gap_2()
        .items_center()
        .child(
            div()
                .flex_1()
                .min_w_0()
                .text_xs()
                .text_color(style::faint())
                .whitespace_normal()
                .child(format!("Your own themes: JSON files in {folder}")),
        )
        .child(
            div()
                .id("themes-reload")
                .px_2()
                .py_1()
                .rounded_md()
                .text_xs()
                .cursor_pointer()
                .border_1()
                .border_color(style::border())
                .text_color(style::muted())
                .hover(|d| d.bg(style::panel_hover()))
                .child("Reload themes")
                .on_click(cx.listener(|_, _, _, cx| {
                    crate::theme::reload(cx);
                    cx.notify();
                })),
        )
}

/// Antigravity no tiene instalador por terminal: se descarga de su página.
const ANTIGRAVITY_URL: &str = "https://antigravity.google";

/// Instalador oficial de cada agente (sin pedir nada en la terminal).
fn install_command(harness: Harness) -> Option<(&'static str, &'static [&'static str])> {
    Some(match harness {
        Harness::Claude if cfg!(windows) => ("powershell", &["-NoProfile", "-ExecutionPolicy", "Bypass", "-Command", "irm https://claude.ai/install.ps1 | iex"]),
        Harness::Claude => ("bash", &["-c", "curl -fsSL https://claude.ai/install.sh | bash"]),
        Harness::Codex => ("npm", &["install", "-g", "@openai/codex"]),
        Harness::Gemini => ("npm", &["install", "-g", "@google/gemini-cli"]),
        Harness::OpenCode => ("npm", &["install", "-g", "opencode-ai"]),
        Harness::Antigravity | Harness::Shell => return None,
    })
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
