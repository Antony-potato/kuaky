//! Panel derecho **Actions**: los workflows de GitHub Actions del repo, lanzar uno
//! a mano (rama e inputs) y las últimas ejecuciones con re-run y cancelar.

use crate::icons;
use crate::style;
use gpui_kit::component::input::{Input, InputState};
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::component::{h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use kuaky_git::{Repo, Workflow, WorkflowInput, WorkflowRun};
use std::time::Duration;

enum Field {
    Text(Entity<InputState>),
    Choice(String),
    Bool(bool),
}

struct RunForm {
    workflow: Workflow,
    branch: Entity<InputState>,
    fields: Vec<(WorkflowInput, Field)>,
}

pub struct ActionsPanel {
    repo: Option<Repo>,
    workflows: Option<Result<Vec<Workflow>, String>>,
    runs: Option<Result<Vec<WorkflowRun>, String>>,
    loading: bool,
    started: bool,
    /// Solo las ejecuciones de este workflow.
    filter: Option<u64>,
    form: Option<RunForm>,
    busy: Option<String>,
    error: Option<String>,
    notice: Option<String>,
    branch: Option<String>,
    /// Pedir de nuevo pronto (tras lanzar algo, las ejecuciones tardan en aparecer).
    soon: bool,
}

impl ActionsPanel {
    pub fn new(repo: Option<Repo>) -> ActionsPanel {
        ActionsPanel {
            repo,
            workflows: None,
            runs: None,
            loading: false,
            started: false,
            filter: None,
            form: None,
            busy: None,
            error: None,
            notice: None,
            branch: None,
            soon: false,
        }
    }

    pub fn set_branch(&mut self, branch: Option<String>) {
        self.branch = branch;
    }

    /// Empieza a cargar (la primera vez que se abre el panel) y se refresca solo:
    /// cada 8 s si algo corre, cada minuto si no.
    pub fn ensure_started(&mut self, cx: &mut Context<Self>) {
        if self.started || self.repo.is_none() {
            return;
        }
        self.started = true;
        cx.spawn(async move |this, cx| loop {
            let Ok(Some(job)) = this.update(cx, |this, cx| this.load_job(cx)) else { break };
            job.await;
            let fast = this.update(cx, |this, _| std::mem::take(&mut this.soon) || this.any_running()).unwrap_or(false);
            cx.background_executor().timer(Duration::from_secs(if fast { 8 } else { 60 })).await;
        })
        .detach();
    }

    fn any_running(&self) -> bool {
        matches!(&self.runs, Some(Ok(runs)) if runs.iter().any(WorkflowRun::running))
    }

    fn refresh(&mut self, cx: &mut Context<Self>) {
        if let Some(job) = self.load_job(cx) {
            cx.spawn(async move |_, _| job.await).detach();
        }
    }

    fn load_job(&mut self, cx: &mut Context<Self>) -> Option<Task<()>> {
        let repo = self.repo.clone()?;
        self.loading = true;
        cx.notify();
        Some(cx.spawn(async move |this, cx| {
            let (workflows, runs) = cx
                .background_executor()
                .spawn(async move { (repo.workflows().map_err(|e| format!("{e:#}")), repo.workflow_runs(30).map_err(|e| format!("{e:#}"))) })
                .await;
            let _ = this.update(cx, |this, cx| {
                this.loading = false;
                this.workflows = Some(workflows);
                this.runs = Some(runs);
                cx.notify();
            });
        }))
    }

    fn open_form(&mut self, workflow: Workflow, window: &mut Window, cx: &mut Context<Self>) {
        let branch_name = self.branch.clone().unwrap_or_else(|| "main".into());
        let branch = cx.new(|cx| InputState::new(window, cx).default_value(branch_name));
        let fields = workflow
            .dispatch
            .clone()
            .unwrap_or_default()
            .into_iter()
            .map(|input| {
                let field = match input.kind.as_str() {
                    "boolean" => Field::Bool(input.default == "true"),
                    "choice" if !input.options.is_empty() => {
                        Field::Choice(if input.default.is_empty() { input.options[0].clone() } else { input.default.clone() })
                    }
                    _ => {
                        let (default, hint) = (input.default.clone(), input.description.clone());
                        Field::Text(cx.new(|cx| InputState::new(window, cx).default_value(default).placeholder(hint)))
                    }
                };
                (input, field)
            })
            .collect();
        self.error = None;
        self.notice = None;
        self.form = Some(RunForm { workflow, branch, fields });
        cx.notify();
    }

    fn dispatch(&mut self, cx: &mut Context<Self>) {
        let (Some(repo), Some(form)) = (self.repo.clone(), self.form.as_ref()) else { return };
        let branch = form.branch.read(cx).value().trim().to_string();
        if branch.is_empty() {
            self.error = Some("Pick a branch to run on.".into());
            cx.notify();
            return;
        }
        let mut inputs = Vec::new();
        for (input, field) in &form.fields {
            let value = match field {
                Field::Text(state) => state.read(cx).value().to_string(),
                Field::Choice(v) => v.clone(),
                Field::Bool(b) => b.to_string(),
            };
            if input.required && value.trim().is_empty() {
                self.error = Some(format!("“{}” is required.", input.name));
                cx.notify();
                return;
            }
            if !value.is_empty() {
                inputs.push((input.name.clone(), value));
            }
        }
        let workflow = form.workflow.clone();
        self.busy = Some(format!("Starting {}…", workflow.name));
        self.error = None;
        cx.notify();
        cx.spawn(async move |this, cx| {
            let name = workflow.name.clone();
            let b = branch.clone();
            let result = cx.background_executor().spawn(async move { repo.run_workflow(&workflow, &b, &inputs) }).await;
            let _ = this.update(cx, |this, cx| {
                this.busy = None;
                match result {
                    Ok(()) => {
                        this.form = None;
                        this.notice = Some(format!("Started {name} on {branch}. It shows up below in a few seconds."));
                        this.soon = true;
                    }
                    Err(e) => this.error = Some(format!("{e:#}")),
                }
                cx.notify();
            });
            // GitHub tarda unos segundos en listar la ejecución nueva.
            cx.background_executor().timer(Duration::from_secs(4)).await;
            let _ = this.update(cx, |this, cx| this.refresh(cx));
        })
        .detach();
    }

    fn run_action(&mut self, run: u64, what: RunAction, cx: &mut Context<Self>) {
        let Some(repo) = self.repo.clone() else { return };
        self.error = None;
        self.notice = None;
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move {
                    match what {
                        RunAction::Cancel => repo.cancel_run(run),
                        RunAction::Rerun => repo.rerun(run, false),
                        RunAction::RerunFailed => repo.rerun(run, true),
                    }
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                match result {
                    Ok(()) => {
                        this.notice = Some(match what {
                            RunAction::Cancel => "Cancelling…".into(),
                            _ => "Re-run started.".into(),
                        });
                        this.soon = true;
                    }
                    Err(e) => this.error = Some(format!("{e:#}")),
                }
                cx.notify();
            });
            cx.background_executor().timer(Duration::from_secs(3)).await;
            let _ = this.update(cx, |this, cx| this.refresh(cx));
        })
        .detach();
    }

    fn workflow_row(&self, w: &Workflow, cx: &mut Context<Self>) -> AnyElement {
        let id = w.id;
        let selected = self.filter == Some(id);
        let manual = w.dispatch.is_some();
        let disabled = w.state != "active";
        let last = match &self.runs {
            Some(Ok(runs)) => runs.iter().find(|r| r.workflow_database_id == id),
            _ => None,
        };
        let tip: SharedString = format!("{} · click to show only its runs", w.path).into();
        let wf = w.clone();
        h_flex()
            .id(("workflow", id as usize))
            .px_2()
            .py_1()
            .gap_2()
            .items_center()
            .rounded_md()
            .cursor_pointer()
            .when(selected, |d| d.bg(style::panel_hover()))
            .hover(|d| d.bg(style::panel_hover()))
            .tooltip(move |window, cx| Tooltip::new(tip.clone()).build(window, cx))
            .child(match last {
                Some(r) => run_icon(r, ("wf-spin", id as usize)),
                None => icons::ui("workflow", px(13.), style::faint()).into_any_element(),
            })
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_sm()
                    .text_color(if disabled { style::faint() } else { style::text() })
                    .child(w.name.clone()),
            )
            .when(disabled, |d| d.child(div().text_xs().text_color(style::faint()).child("disabled")))
            .when(manual && !disabled, |d| {
                d.child(
                    h_flex()
                        .id(("workflow-run", id as usize))
                        .gap_1()
                        .px_2()
                        .py_0p5()
                        .items_center()
                        .rounded_md()
                        .border_1()
                        .border_color(style::border())
                        .text_xs()
                        .text_color(style::text())
                        .hover(|d| d.bg(style::border()))
                        .child(icons::ui("play", px(11.), style::text()))
                        .child("Run")
                        .on_click(cx.listener(move |this, _, window, cx| {
                            cx.stop_propagation();
                            this.open_form(wf.clone(), window, cx);
                        })),
                )
            })
            .when(!manual && !disabled, |d| {
                d.child(
                    div()
                        .id(("workflow-auto", id as usize))
                        .text_xs()
                        .text_color(style::faint())
                        .tooltip(|window, cx| {
                            Tooltip::new("Runs on its own triggers (push, PR…). Add `workflow_dispatch:` under `on:` to launch it from here.").build(window, cx)
                        })
                        .child("auto"),
                )
            })
            .on_click(cx.listener(move |this, _, _, cx| {
                this.filter = if this.filter == Some(id) { None } else { Some(id) };
                cx.notify();
            }))
            .into_any_element()
    }

    fn form_view(&self, form: &RunForm, cx: &mut Context<Self>) -> AnyElement {
        let label = |text: String| div().text_xs().text_color(style::muted()).child(text);
        v_flex()
            .mx_2()
            .mb_2()
            .p_2()
            .gap_2()
            .rounded_lg()
            .border_1()
            .border_color(style::border())
            .bg(style::panel())
            .child(
                h_flex()
                    .gap_2()
                    .items_center()
                    .child(icons::ui("play", px(12.), style::text()))
                    .child(div().flex_1().min_w_0().truncate().text_sm().font_weight(FontWeight::MEDIUM).child(format!("Run {}", form.workflow.name))),
            )
            .child(v_flex().gap_1().child(label("Branch".into())).child(Input::new(&form.branch)))
            .children(form.fields.iter().enumerate().map(|(i, (input, field))| {
                let title = if input.required { format!("{} *", input.name) } else { input.name.clone() };
                let control = match field {
                    Field::Text(state) => Input::new(state).into_any_element(),
                    Field::Bool(on) => {
                        let on = *on;
                        h_flex()
                            .id(("wf-bool", i))
                            .gap_2()
                            .items_center()
                            .cursor_pointer()
                            .text_xs()
                            .child(
                                div()
                                    .w(px(24.))
                                    .h(px(14.))
                                    .p(px(2.))
                                    .rounded_full()
                                    .bg(if on { style::accent().into() } else { style::border() })
                                    .child(div().size(px(10.)).rounded_full().bg(style::text()).when(on, |d| d.ml(px(10.)))),
                            )
                            .child(if on { "true" } else { "false" })
                            .on_click(cx.listener(move |this, _, _, cx| {
                                if let Some(Field::Bool(b)) = this.form.as_mut().and_then(|f| f.fields.get_mut(i)).map(|f| &mut f.1) {
                                    *b = !*b;
                                }
                                cx.notify();
                            }))
                            .into_any_element()
                    }
                    Field::Choice(current) => h_flex()
                        .gap_1()
                        .flex_wrap()
                        .children(input.options.iter().enumerate().map(|(k, opt)| {
                            let selected = opt == current;
                            let value = opt.clone();
                            div()
                                .id(("wf-choice", i * 100 + k))
                                .px_2()
                                .py_0p5()
                                .rounded_md()
                                .border_1()
                                .border_color(if selected { style::text().into() } else { style::border() })
                                .text_xs()
                                .text_color(if selected { style::text() } else { style::muted() })
                                .cursor_pointer()
                                .hover(|d| d.bg(style::panel_hover()))
                                .child(opt.clone())
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    if let Some(Field::Choice(v)) = this.form.as_mut().and_then(|f| f.fields.get_mut(i)).map(|f| &mut f.1) {
                                        *v = value.clone();
                                    }
                                    cx.notify();
                                }))
                        }))
                        .into_any_element(),
                };
                v_flex()
                    .gap_1()
                    .child(label(title))
                    .when(!input.description.is_empty() && !matches!(field, Field::Text(_)), |d| {
                        d.child(div().text_xs().text_color(style::faint()).whitespace_normal().child(input.description.clone()))
                    })
                    .child(control)
            }))
            .child(
                h_flex()
                    .gap_2()
                    .justify_end()
                    .child(
                        div()
                            .id("wf-cancel")
                            .px_2()
                            .py_0p5()
                            .rounded_md()
                            .text_xs()
                            .text_color(style::muted())
                            .cursor_pointer()
                            .hover(|d| d.bg(style::panel_hover()))
                            .child("Cancel")
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.form = None;
                                this.error = None;
                                cx.notify();
                            })),
                    )
                    .child(
                        div()
                            .id("wf-start")
                            .px_3()
                            .py_0p5()
                            .rounded_md()
                            .text_xs()
                            .font_weight(FontWeight::MEDIUM)
                            .bg(style::text())
                            .text_color(style::bg())
                            .cursor_pointer()
                            .hover(|d| d.opacity(0.9))
                            .child(if self.busy.is_some() { "Starting…" } else { "Run workflow" })
                            .on_click(cx.listener(|this, _, _, cx| {
                                if this.busy.is_none() {
                                    this.dispatch(cx);
                                }
                            })),
                    ),
            )
            .into_any_element()
    }

    fn run_row(&self, run: &WorkflowRun, cx: &mut Context<Self>) -> AnyElement {
        let id = run.database_id;
        let url = run.url.clone();
        let group: SharedString = format!("run-{id}").into();
        let when = run.created_at.parse::<jiff::Timestamp>().ok().map(ago).unwrap_or_default();
        let detail = [run.workflow_name.as_str(), run.head_branch.as_str(), run.event.as_str(), when.as_str()]
            .iter()
            .filter(|s| !s.is_empty())
            .copied()
            .collect::<Vec<_>>()
            .join(" · ");
        let small = |id: SharedString, icon: &'static str, tip: &'static str| {
            div()
                .id(id)
                .p_1()
                .rounded_sm()
                .cursor_pointer()
                .hover(|d| d.bg(style::border()))
                .tooltip(move |window, cx| Tooltip::new(tip).build(window, cx))
                .child(icons::ui(icon, px(12.), style::muted()))
        };
        let failed = run.conclusion == "failure";
        h_flex()
            .id(("run", id as usize))
            .group(group.clone())
            .px_2()
            .py_1()
            .gap_2()
            .items_start()
            .rounded_md()
            .cursor_pointer()
            .hover(|d| d.bg(style::panel_hover()))
            .child(div().pt_0p5().child(run_icon(run, ("run-spin", id as usize))))
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .child(div().truncate().text_sm().child(run.display_title.clone()))
                    .child(div().truncate().text_xs().text_color(style::faint()).child(format!("#{} · {detail}", run.number))),
            )
            .child(
                h_flex()
                    .gap_0p5()
                    .invisible()
                    .group_hover(group, |s| s.visible())
                    .when(run.running(), |d| {
                        d.child(small(format!("cancel-{id}").into(), "square", "Cancel run").on_click(cx.listener(move |this, _, _, cx| {
                            cx.stop_propagation();
                            this.run_action(id, RunAction::Cancel, cx);
                        })))
                    })
                    .when(failed, |d| {
                        d.child(small(format!("rerun-failed-{id}").into(), "rotate-cw", "Re-run failed jobs").on_click(cx.listener(
                            move |this, _, _, cx| {
                                cx.stop_propagation();
                                this.run_action(id, RunAction::RerunFailed, cx);
                            },
                        )))
                    })
                    .when(!run.running() && !failed, |d| {
                        d.child(small(format!("rerun-{id}").into(), "rotate-cw", "Re-run").on_click(cx.listener(move |this, _, _, cx| {
                            cx.stop_propagation();
                            this.run_action(id, RunAction::Rerun, cx);
                        })))
                    }),
            )
            .tooltip(|window, cx| Tooltip::new("Open on GitHub (logs)").build(window, cx))
            .on_click(move |_, _, cx| cx.open_url(&url))
            .into_any_element()
    }
}

#[derive(Clone, Copy)]
enum RunAction {
    Cancel,
    Rerun,
    RerunFailed,
}

/// Estado de una ejecución: girando, ✓, ✗, cancelada…
fn run_icon(run: &WorkflowRun, id: (&'static str, usize)) -> AnyElement {
    if run.running() {
        let color: Hsla = if run.status == "in_progress" { style::accent().into() } else { style::muted().into() };
        return icons::ui("loader-circle", px(13.), color)
            .with_animation(SharedString::from(format!("{}-{}", id.0, id.1)), Animation::new(Duration::from_millis(1100)).repeat(), |svg, d| {
                svg.with_transformation(Transformation::rotate(percentage(d)))
            })
            .into_any_element();
    }
    let (icon, color): (&'static str, Hsla) = match run.conclusion.as_str() {
        "success" => ("circle-check", style::good().into()),
        "failure" | "startup_failure" | "timed_out" => ("circle-x", style::bad().into()),
        "cancelled" => ("ban", style::faint().into()),
        "skipped" | "neutral" => ("circle-slash", style::faint().into()),
        "action_required" => ("circle-dot", rgb(0xfbbf24).into()),
        _ => ("circle-dot", style::faint().into()),
    };
    icons::ui(icon, px(13.), color).into_any_element()
}

fn ago(ts: jiff::Timestamp) -> String {
    let secs = jiff::Timestamp::now().duration_since(ts).as_secs().max(0) as u64;
    if secs < 60 {
        "just now".into()
    } else {
        format!("{} ago", style::duration(secs))
    }
}

impl Render for ActionsPanel {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        #[cfg(feature = "screenshot")]
        if std::env::var("KUAKY_DEMO_ACTIONS").is_ok() && self.form.is_none() {
            if let Some(Ok(list)) = &mut self.workflows {
                if !list.iter().any(|w| w.id == 1) {
                    let yaml = "on:\n  workflow_dispatch:\n    inputs:\n      environment:\n        type: choice\n        options: [staging, production]\n      dry_run:\n        type: boolean\n        default: true\n      note:\n        description: Shown in the release notes\n";
                    let demo = Workflow { id: 1, name: "Deploy".into(), path: ".github/workflows/deploy.yml".into(), state: "active".into(), dispatch: kuaky_git::actions::dispatch_inputs(yaml) };
                    list.insert(0, demo.clone());
                    self.open_form(demo, _window, cx);
                }
            }
        }
        if self.repo.is_none() {
            return div().p_3().text_xs().text_color(style::muted()).child("This folder is not a git repository.").into_any_element();
        }
        let section = |title: String| div().px_3().pt_3().pb_1().text_xs().text_color(style::faint()).child(title);
        let workflows = match &self.workflows {
            None => div().px_3().py_2().text_xs().text_color(style::muted()).child("Loading workflows…").into_any_element(),
            Some(Err(e)) => div().px_3().py_2().text_xs().text_color(style::bad()).whitespace_normal().child(e.clone()).into_any_element(),
            Some(Ok(list)) if list.is_empty() => div()
                .px_3()
                .py_2()
                .text_xs()
                .text_color(style::muted())
                .whitespace_normal()
                .child("No workflows in .github/workflows.")
                .into_any_element(),
            Some(Ok(list)) => v_flex().px_1().children(list.clone().iter().map(|w| self.workflow_row(w, cx))).into_any_element(),
        };
        let filter_name = self.filter.and_then(|id| match &self.workflows {
            Some(Ok(list)) => list.iter().find(|w| w.id == id).map(|w| w.name.clone()),
            _ => None,
        });
        let runs = match &self.runs {
            None => div().px_3().py_2().text_xs().text_color(style::muted()).child("Loading runs…").into_any_element(),
            Some(Err(e)) => div().px_3().py_2().text_xs().text_color(style::bad()).whitespace_normal().child(e.clone()).into_any_element(),
            Some(Ok(list)) => {
                let shown: Vec<WorkflowRun> = list.iter().filter(|r| self.filter.is_none_or(|f| r.workflow_database_id == f)).take(20).cloned().collect();
                if shown.is_empty() {
                    div().px_3().py_2().text_xs().text_color(style::muted()).child("No runs yet.").into_any_element()
                } else {
                    v_flex().px_1().children(shown.iter().map(|r| self.run_row(r, cx))).into_any_element()
                }
            }
        };
        let form = self.form.as_ref().map(|f| self.form_view(f, cx));
        div()
            .id("actions-panel")
            .size_full()
            .overflow_y_scroll()
            .child(
                v_flex()
                    .pb_3()
                    .child(
                        h_flex()
                            .pr_2()
                            .items_center()
                            .child(div().flex_1().child(section("WORKFLOWS".into())))
                            .child(
                                div()
                                    .id("actions-refresh")
                                    .mt_2()
                                    .p_1()
                                    .rounded_sm()
                                    .cursor_pointer()
                                    .hover(|d| d.bg(style::panel_hover()))
                                    .tooltip(|window, cx| Tooltip::new("Refresh").build(window, cx))
                                    .child(icons::ui("refresh-cw", px(12.), if self.loading { style::faint() } else { style::muted() }))
                                    .on_click(cx.listener(|this, _, _, cx| this.refresh(cx))),
                            ),
                    )
                    .child(workflows)
                    .when_some(form, |d, f| d.child(div().pt_1().child(f)))
                    .when_some(self.error.clone(), |d, e| d.child(div().px_3().py_1().text_xs().text_color(style::bad()).whitespace_normal().child(e)))
                    .when_some(self.notice.clone(), |d, n| d.child(div().px_3().py_1().text_xs().text_color(style::muted()).whitespace_normal().child(n)))
                    .child(
                        h_flex()
                            .pr_3()
                            .items_center()
                            .child(div().flex_1().child(section(match &filter_name {
                                Some(name) => format!("RUNS · {}", name.to_uppercase()),
                                None => "RECENT RUNS".into(),
                            })))
                            .when(filter_name.is_some(), |d| {
                                d.child(
                                    div()
                                        .id("actions-all")
                                        .mt_2()
                                        .text_xs()
                                        .text_color(style::muted())
                                        .cursor_pointer()
                                        .hover(|d| d.text_color(style::text()))
                                        .child("Show all")
                                        .on_click(cx.listener(|this, _, _, cx| {
                                            this.filter = None;
                                            cx.notify();
                                        })),
                                )
                            }),
                    )
                    .child(runs),
            )
            .into_any_element()
    }
}
