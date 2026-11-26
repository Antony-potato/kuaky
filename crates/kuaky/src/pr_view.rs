//! Un pull request en una pestaña: solo **Overview** (descripción y estado) y
//! **Files changed**, con un botón: *Merge*, o *Resolve & merge* si tiene conflictos.

use crate::project::Harness;
use crate::style;
use gpui_kit::component::diff::{Diff, DiffFile, DiffMode, DiffState};
use gpui_kit::component::text::TextView;
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::component::{h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use kuaky_git::{PrDetail, PullRequest, Repo};
use std::time::{Duration, Instant};

pub enum PrEvent {
    Merge(u64),
    ResolveAndMerge(u64),
    /// Abrir la sesión ligada a este PR.
    OpenSession(String),
    /// Empezar una sesión nueva para trabajar en este PR con el agente elegido.
    StartSession { number: u64, harness: Harness, body: Option<String> },
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Tab {
    Overview,
    Files,
}

pub struct PrView {
    pub number: u64,
    pr: PullRequest,
    detail: Option<Result<PrDetail, String>>,
    files: Option<Result<usize, String>>,
    diff: Entity<DiffState>,
    tab: Tab,
    /// Sesión que hizo o trabaja en el PR: (uid, nombre).
    session: Option<(String, String)>,
    /// Agentes disponibles para "Start session…".
    agents: Vec<Harness>,
    session_menu: bool,
    session_menu_closed: Option<Instant>,
    /// "Merging…" / "Resolving…" mientras corre.
    pub busy: Option<String>,
    pub error: Option<String>,
}

impl EventEmitter<PrEvent> for PrView {}

impl PrView {
    pub fn new(
        repo: Repo,
        pr: PullRequest,
        session: Option<(String, String)>,
        agents: Vec<Harness>,
        cx: &mut Context<Self>,
    ) -> Self {
        let number = pr.number;
        let diff = cx.new(|cx| DiffState::new([], cx).with_mode(DiffMode::Unified));
        let this = PrView {
            number,
            pr,
            detail: None,
            files: None,
            diff,
            tab: Tab::Overview,
            session,
            agents,
            session_menu: false,
            session_menu_closed: None,
            busy: None,
            error: None,
        };
        this.load(repo, cx);
        this
    }

    pub fn reload(&mut self, repo: Repo, cx: &mut Context<Self>) {
        self.load(repo, cx);
    }

    pub fn set_pr(&mut self, pr: PullRequest, session: Option<(String, String)>, agents: Vec<Harness>, cx: &mut Context<Self>) {
        self.pr = pr;
        self.session = session;
        self.agents = agents;
        if self.session.is_some() {
            self.session_menu = false;
        }
        cx.notify();
    }

    fn menu_just_closed(&self) -> bool {
        self.session_menu_closed.is_some_and(|t| t.elapsed() < Duration::from_millis(300))
    }

    fn current_body(&self) -> Option<String> {
        self.detail
            .as_ref()
            .and_then(|d| d.as_ref().ok())
            .map(|d| d.body.clone())
            .filter(|b| !b.trim().is_empty())
            .or_else(|| (!self.pr.body.trim().is_empty()).then(|| self.pr.body.clone()))
    }

    fn load(&self, repo: Repo, cx: &mut Context<Self>) {
        let number = self.number;
        cx.spawn(async move |this, cx| {
            let r = repo.clone();
            let (detail, patch) = cx
                .background_executor()
                .spawn(async move {
                    let detail = r.pr_detail(number).map_err(|e| format!("{e:#}"));
                    let patch = r
                        .pr_diff(number)
                        .and_then(|p| DiffFile::parse(&p).map_err(|e| anyhow::anyhow!("{e}")))
                        .map_err(|e| format!("{e:#}"));
                    (detail, patch)
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                this.detail = Some(detail);
                this.files = Some(match patch {
                    Ok(files) => {
                        let n = files.len();
                        this.diff.update(cx, |s, cx| s.set_files(files, cx));
                        Ok(n)
                    }
                    Err(e) => Err(e),
                });
                cx.notify();
            });
        })
        .detach();
    }

    fn conflicts(&self) -> bool {
        match &self.detail {
            Some(Ok(d)) => d.has_conflicts(),
            _ => self.pr.has_conflicts(),
        }
    }

    fn status(&self) -> (String, Hsla) {
        if let Some(Ok(d)) = &self.detail {
            if d.state == "MERGED" {
                return ("Merged".into(), style::merged().into());
            }
        }
        if self.pr.is_draft {
            return ("Draft".into(), style::muted().into());
        }
        if self.conflicts() {
            return ("Has conflicts with the base branch".into(), style::bad().into());
        }
        let state = match &self.detail {
            Some(Ok(d)) => d.merge_state_status.clone(),
            _ => self.pr.merge_state_status.clone(),
        };
        match state.as_str() {
            "CLEAN" | "HAS_HOOKS" => ("Ready to merge".into(), style::good().into()),
            "BEHIND" => ("Behind the base branch (can still merge)".into(), style::warn().into()),
            "BLOCKED" => ("Blocked: needs approval or checks".into(), style::warn().into()),
            "UNSTABLE" => ("Checks failing".into(), style::warn().into()),
            _ => ("Checking…".into(), style::muted().into()),
        }
    }
}

/// Botón principal de un PR: *Merge* o *Resolve & merge*.
pub fn merge_button(id: impl Into<ElementId>, conflicts: bool, small: bool) -> Stateful<Div> {
    div()
        .id(id)
        .flex_none()
        .when(small, |d| d.px_2().py_0p5().text_xs())
        .when(!small, |d| d.px_3().py_1().text_sm())
        .rounded_md()
        .cursor_pointer()
        .font_weight(FontWeight::MEDIUM)
        .when(conflicts, |d| d.bg(style::soft(style::warn(), 0.18)).text_color(style::warn()).hover(|d| d.bg(style::soft(style::warn(), 0.28))))
        .when(!conflicts, |d| d.bg(style::accent()).text_color(style::on_accent()).hover(|d| d.opacity(0.9)))
        .child(if conflicts { "Resolve & merge" } else { "Merge" })
}

impl Render for PrView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let number = self.number;
        let conflicts = self.conflicts();
        let (status, status_color) = self.status();
        let merged = matches!(&self.detail, Some(Ok(d)) if d.state == "MERGED");
        let files_label = match &self.files {
            Some(Ok(n)) => format!("Files changed {n}"),
            _ => "Files changed".into(),
        };
        let tab = |id: &'static str, label: String, which: Tab, cx: &mut Context<Self>| {
            let selected = self.tab == which;
            div()
                .id(id)
                .px_3()
                .py_2()
                .text_sm()
                .cursor_pointer()
                .border_b_2()
                .border_color(if selected { style::accent().into() } else { style::transparent() })
                .text_color(if selected { style::text() } else { style::muted() })
                .hover(|d| d.text_color(style::text()))
                .child(label)
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.tab = which;
                    cx.notify();
                }))
        };
        let header = v_flex()
            .px_5()
            .pt_4()
            .gap_2()
            .border_b_1()
            .border_color(style::border())
            .child(
                h_flex()
                    .gap_2()
                    .items_start()
                    .child(div().flex_1().min_w_0().text_lg().font_weight(FontWeight::SEMIBOLD).whitespace_normal().child(self.pr.title.clone()))
                    .child(div().text_lg().text_color(style::faint()).child(format!("#{number}"))),
            )
            .child(
                h_flex()
                    .gap_2()
                    .flex_wrap()
                    .items_center()
                    .text_xs()
                    .text_color(style::muted())
                    .child(self.pr.author.login.clone())
                    .child(
                        div()
                            .px_1p5()
                            .rounded_sm()
                            .bg(style::panel())
                            .font_family(style::mono_font())
                            .child(format!("{} → {}", self.pr.head_ref_name, self.pr.base_ref_name)),
                    )
                    .child(div().text_color(style::good()).child(format!("+{}", self.pr.additions)))
                    .child(div().text_color(style::bad()).child(format!("−{}", self.pr.deletions)))
                    .map(|d| match self.session.clone() {
                        Some((uid, name)) => {
                            let tip: SharedString = format!("Open session: {}", crate::lines::short(&name, 60)).into();
                            d.child(
                                h_flex()
                                    .id("pr-session")
                                    .gap_1()
                                    .px_2()
                                    .py_0p5()
                                    .rounded_sm()
                                    .cursor_pointer()
                                    .border_1()
                                    .border_color(style::border())
                                    .bg(style::panel())
                                    .text_color(style::text())
                                    .hover(|d| d.bg(style::panel_hover()))
                                    .tooltip(move |window, cx| Tooltip::new(tip.clone()).build(window, cx))
                                    .child(crate::icons::ui("message-square", px(11.), style::accent()))
                                    .child("Open session")
                                    .on_click(cx.listener(move |_, _, _, cx| cx.emit(PrEvent::OpenSession(uid.clone())))),
                            )
                        }
                        None => {
                            let menu_open = self.session_menu;
                            let agents = self.agents.clone();
                            let body = self.current_body();
                            d.child(
                                h_flex()
                                    .id("pr-start-session")
                                    .relative()
                                    .gap_1()
                                    .px_2()
                                    .py_0p5()
                                    .rounded_sm()
                                    .cursor_pointer()
                                    .border_1()
                                    .border_color(style::border())
                                    .when(menu_open, |d| d.bg(style::panel_hover()).text_color(style::text()))
                                    .when(!menu_open, |d| d.bg(style::panel()).text_color(style::muted()))
                                    .hover(|d| d.bg(style::panel_hover()).text_color(style::text()))
                                    .tooltip(|window, cx| Tooltip::new("Start an agent session with this PR's context").build(window, cx))
                                    .child(crate::icons::ui("plus", px(11.), style::muted()))
                                    .child("Start session…")
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        cx.stop_propagation();
                                        this.session_menu = !this.session_menu && !this.menu_just_closed();
                                        cx.notify();
                                    }))
                                    .when(menu_open, |d| {
                                        crate::webview::cover();
                                        d.child(
                                            deferred(
                                                div().absolute().top_full().left_0().mt_1().child(
                                                    v_flex()
                                                        .id("pr-view-agent-menu")
                                                        .occlude()
                                                        .w(px(200.))
                                                        .p_1()
                                                        .rounded_lg()
                                                        .border_1()
                                                        .border_color(style::border())
                                                        .bg(style::panel())
                                                        .shadow_lg()
                                                        .text_color(style::text())
                                                        .when(agents.is_empty(), |d| {
                                                            d.child(div().px_2().py_1().text_xs().text_color(style::faint()).child("No agents found"))
                                                        })
                                                        .children(agents.into_iter().map(|h| {
                                                            let body = body.clone();
                                                            h_flex()
                                                                .id(SharedString::from(format!("pr-view-launch-{}", h.key())))
                                                                .gap_2()
                                                                .px_2()
                                                                .py_1()
                                                                .rounded_md()
                                                                .text_sm()
                                                                .cursor_pointer()
                                                                .hover(|d| d.bg(style::panel_hover()))
                                                                .child(crate::project::harness_badge(h))
                                                                .child(div().flex_1().min_w_0().truncate().child(h.label()))
                                                                .on_click(cx.listener(move |this, _, _, cx| {
                                                                    cx.stop_propagation();
                                                                    this.session_menu = false;
                                                                    cx.emit(PrEvent::StartSession { number, harness: h, body: body.clone() });
                                                                    cx.notify();
                                                                }))
                                                        }))
                                                        .on_mouse_down_out(cx.listener(|this, _, _, cx| {
                                                            if this.session_menu {
                                                                this.session_menu = false;
                                                                this.session_menu_closed = Some(Instant::now());
                                                                cx.notify();
                                                            }
                                                        })),
                                                ),
                                            )
                                            .with_priority(1),
                                        )
                                    }),
                            )
                        }
                    })
                    .child(
                        div()
                            .id("pr-open-github")
                            .cursor_pointer()
                            .hover(|d| d.text_color(style::text()))
                            .child("GitHub ↗")
                            .on_click({
                                let url = self.pr.url.clone();
                                move |_, _, cx| cx.open_url(&url)
                            }),
                    ),
            )
            .child(
                h_flex()
                    .mt_1()
                    .px_3()
                    .py_2()
                    .gap_3()
                    .items_center()
                    .rounded_lg()
                    .border_1()
                    .border_color(style::border())
                    .bg(style::panel())
                    .child(div().size(px(8.)).rounded_full().bg(status_color))
                    .child(div().flex_1().text_sm().child(status))
                    .when_some(self.error.clone(), |d, e| d.child(div().text_xs().text_color(style::bad()).max_w(px(360.)).truncate().child(e)))
                    .when_some(self.busy.clone(), |d, b| d.child(div().text_xs().text_color(style::accent()).child(b)))
                    .when(self.busy.is_none() && !merged && !self.pr.is_draft, |d| {
                        d.child(merge_button("pr-view-merge", conflicts, false).on_click(cx.listener(move |this, _, _, cx| {
                            this.error = None;
                            cx.emit(if conflicts { PrEvent::ResolveAndMerge(number) } else { PrEvent::Merge(number) });
                        })))
                    }),
            )
            .child(h_flex().mt_1().child(tab("pr-tab-overview", "Overview".into(), Tab::Overview, cx)).child(tab("pr-tab-files", files_label, Tab::Files, cx)));

        let body = match self.tab {
            Tab::Overview => {
                let content = match &self.detail {
                    None => div().text_sm().text_color(style::muted()).child("Loading…").into_any_element(),
                    Some(Err(e)) => div().text_sm().text_color(style::bad()).child(e.clone()).into_any_element(),
                    Some(Ok(d)) if d.body.trim().is_empty() => div().text_sm().text_color(style::muted()).child("No description.").into_any_element(),
                    Some(Ok(d)) => div().text_sm().child(TextView::markdown("pr-body", d.body.clone())).into_any_element(),
                };
                div()
                    .id("pr-overview")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .child(div().w_full().max_w(px(900.)).px_5().py_4().child(content))
                    .into_any_element()
            }
            Tab::Files => match &self.files {
                None => div().p_5().text_sm().text_color(style::muted()).child("Loading…").into_any_element(),
                Some(Err(e)) => div().p_5().text_sm().text_color(style::bad()).child(e.clone()).into_any_element(),
                Some(Ok(_)) => div().flex_1().min_h_0().child(Diff::new(&self.diff).size_full()).into_any_element(),
            },
        };
        v_flex().size_full().bg(style::bg()).child(header).child(body)
    }
}
