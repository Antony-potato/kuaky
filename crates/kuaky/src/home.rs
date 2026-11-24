//! Ventana de inicio: consumo de tokens de todos los agentes.

use crate::icons;
use crate::style::{self, human, thousands};
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::component::{h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use jiff::{Timestamp, civil::Date, tz::TimeZone};
use kuaky_usage::{Agent, AgentLimits, Pricing, Range, Report, UsageEvent};
use std::{path::PathBuf, sync::Arc};

pub enum HomeEvent {
    OpenProject(PathBuf),
    PickFolder,
}

pub struct HomeView {
    events: Option<Arc<Vec<UsageEvent>>>,
    limits: Vec<AgentLimits>,
    range: Range,
    agent: Option<Agent>,
    report: Report,
    tz: TimeZone,
    now: Timestamp,
    pricing: Arc<Pricing>,
}

impl EventEmitter<HomeEvent> for HomeView {}

impl HomeView {
    pub fn new(cx: &mut Context<Self>) -> Self {
        let this = Self {
            events: None,
            limits: Vec::new(),
            range: Range::Days(30),
            agent: None,
            report: Report::default(),
            tz: TimeZone::system(),
            now: Timestamp::now(),
            pricing: Arc::new(Pricing::embedded()),
        };
        this.reload(cx);
        this.refresh_prices(cx);
        this
    }

    /// Carga los precios guardados y, si tienen más de un día, descarga los de
    /// LiteLLM en segundo plano.
    fn refresh_prices(&self, cx: &mut Context<Self>) {
        cx.spawn(async move |this, cx| {
            let pricing = cx.background_executor().spawn(async { Pricing::load() }).await;
            this.update(cx, |this, cx| {
                this.pricing = Arc::new(pricing);
                this.rebuild();
                cx.notify();
            })
            .ok();
            let updated = cx.background_executor().spawn(async { kuaky_usage::pricing::refresh_if_stale() }).await;
            if updated {
                let pricing = cx.background_executor().spawn(async { Pricing::load() }).await;
                this.update(cx, |this, cx| {
                    this.pricing = Arc::new(pricing);
                    this.rebuild();
                    cx.notify();
                })
                .ok();
            }
        })
        .detach();
    }

    /// Eventos de consumo ya cargados (para la actividad de cada proyecto).
    pub fn events(&self) -> Option<Arc<Vec<UsageEvent>>> {
        self.events.clone()
    }

    /// Vuelve a leer los logs en segundo plano.
    pub fn reload(&self, cx: &mut Context<Self>) {
        cx.spawn(async move |this, cx| {
            let (events, limits) = cx
                .background_executor()
                .spawn(async {
                    let events = kuaky_usage::load_all();
                    let limits = kuaky_usage::load_all_limits();
                    (events, limits)
                })
                .await;
            this.update(cx, |this, cx| {
                this.events = Some(Arc::new(events));
                this.limits = limits;
                #[cfg(feature = "screenshot")]
                if this.limits.is_empty() && std::env::var("KUAKY_DEMO_LIMITS").is_ok() {
                    this.limits = demo_limits();
                }
                this.rebuild();
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn rebuild(&mut self) {
        self.now = Timestamp::now();
        if let Some(events) = &self.events {
            self.report = Report::build(events, self.range, self.agent, &self.tz, self.now, &self.pricing);
        }
    }

    fn set_range(&mut self, range: Range, cx: &mut Context<Self>) {
        self.range = range;
        self.rebuild();
        cx.notify();
    }

    fn toggle_agent(&mut self, agent: Agent, cx: &mut Context<Self>) {
        self.agent = if self.agent == Some(agent) { None } else { Some(agent) };
        self.rebuild();
        cx.notify();
    }

    fn header(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let subtitle = match (self.report.start, self.report.end) {
            (Some(s), Some(_)) => format!("{} – today", short_date(s)),
            _ => "Reading local logs…".to_string(),
        };
        let title = match self.agent {
            Some(a) => format!("Usage · {}", a.name()),
            None => "Agent usage".to_string(),
        };
        h_flex()
            .justify_between()
            .items_start()
            .child(
                v_flex()
                    .gap_0p5()
                    .child(div().text_lg().font_weight(FontWeight::SEMIBOLD).child(title))
                    .child(div().text_sm().text_color(style::muted()).child(subtitle)),
            )
            .child(
                h_flex()
                    .gap_3()
                    .child(
                        h_flex()
                            .p_0p5()
                            .gap_0p5()
                            .rounded_lg()
                            .bg(style::panel())
                            .border_1()
                            .border_color(style::border())
                            .children(Range::PRESETS.into_iter().map(|r| {
                                let selected = r == self.range;
                                div()
                                    .id(SharedString::from(format!("range-{}", r.label())))
                                    .px_3()
                                    .py_1()
                                    .rounded_md()
                                    .text_sm()
                                    .cursor_pointer()
                                    .text_color(if selected { style::text() } else { style::muted() })
                                    .when(selected, |d| d.bg(style::panel_hover()))
                                    .hover(|d| d.text_color(style::text()))
                                    .child(r.label())
                                    .on_click(cx.listener(move |this, _, _, cx| this.set_range(r, cx)))
                            })),
                    )
                    .child(
                        div()
                            .id("open-folder")
                            .px_3()
                            .py_1()
                            .rounded_lg()
                            .text_sm()
                            .cursor_pointer()
                            .bg(style::accent())
                            .text_color(style::bg())
                            .font_weight(FontWeight::MEDIUM)
                            .child("Open folder")
                            .on_click(cx.listener(|_, _, _, cx| cx.emit(HomeEvent::PickFolder))),
                    ),
            )
    }

    fn limits_section(&self) -> impl IntoElement {
        let now = self.now;
        let tz = &self.tz;
        section(
            "Remaining limits",
            Some("live session & weekly quotas".into()),
            h_flex().gap_4().flex_wrap().children(self.limits.iter().map(|al| {
                let updated = al.updated_at.map(|ts| ago(ts, now)).unwrap_or_else(|| "active".into());
                v_flex()
                    .flex_1()
                    .min_w(px(280.))
                    .p_4()
                    .gap_3()
                    .rounded_xl()
                    .bg(style::panel())
                    .border_1()
                    .border_color(style::border())
                    .child(
                        h_flex()
                            .gap_2()
                            .items_center()
                            .child(agent_badge(al.agent))
                            .child(div().text_sm().font_weight(FontWeight::MEDIUM).child(al.agent.name()))
                            .child(div().flex_1())
                            .child(div().text_xs().text_color(style::faint()).child(updated)),
                    )
                    .children(al.windows().map(|w| {
                        let rem = w.remaining_pct;
                        let bar_color = if rem <= 10.0 {
                            style::bad()
                        } else if rem <= 25.0 {
                            rgb(0xfbbf24)
                        } else {
                            style::accent()
                        };
                        let reset_text = w
                            .resets_at
                            .and_then(|ts| kuaky_usage::limits::format_reset(ts, now, tz))
                            .map(|t| format!("resets {t}"));
                        v_flex()
                            .gap_1()
                            .child(
                                h_flex()
                                    .gap_2()
                                    .text_xs()
                                    .child(div().text_color(style::text()).font_weight(FontWeight::MEDIUM).child(w.label.clone()))
                                    .child(div().text_color(bar_color).child(format!("{:.0}% left", rem)))
                                    .child(div().text_color(style::faint()).child(format!("({:.0}% used)", w.used_pct)))
                                    .child(div().flex_1())
                                    .when_some(reset_text, |d, r| d.child(div().text_color(style::muted()).child(r))),
                            )
                            .child(
                                div()
                                    .h(px(6.))
                                    .w_full()
                                    .rounded_full()
                                    .bg(style::panel_hover())
                                    .overflow_hidden()
                                    .child(
                                        div()
                                            .h_full()
                                            .w(relative((rem as f32 / 100.0).clamp(0.0, 1.0)))
                                            .rounded_full()
                                            .bg(bar_color),
                                    ),
                            )
                    }))
            })),
        )
    }

    fn kpi_cards(&self) -> impl IntoElement {
        let r = &self.report;
        let k = r.kpis;
        let series = |f: fn(&kuaky_usage::report::DayStat) -> u64| r.days.iter().map(f).collect::<Vec<_>>();
        let parallel = if k.max_parallel > 1 {
            format!("up to {} in parallel", k.max_parallel)
        } else {
            "one session at a time".into()
        };
        h_flex()
            .gap_4()
            .child(kpi_card(
                "Input tokens",
                human(k.input_total),
                format!("{:.0}% from cache", k.cache_pct()),
                r.delta(|k| k.input_total),
                series(|d| d.input_total),
                style::accent(),
            ))
            .child(kpi_card(
                "Output tokens",
                human(k.output),
                format!("{} per response", human(k.output_per_turn())),
                r.delta(|k| k.output),
                series(|d| d.output),
                style::output(),
            ))
            .child(kpi_card(
                "Sessions",
                thousands(k.sessions),
                format!("{} responses", thousands(k.turns)),
                r.delta(|k| k.sessions),
                series(|d| d.sessions),
                style::accent(),
            ))
            .child(kpi_card(
                "Agent time",
                style::duration(k.agent_secs),
                parallel,
                r.delta(|k| k.agent_secs),
                series(|d| d.agent_secs),
                style::accent(),
            ))
            .child(kpi_card(
                "API cost",
                style::money(k.cost),
                if k.unpriced > 0 {
                    format!("{} unpriced responses", thousands(k.unpriced))
                } else {
                    "API-equivalent pricing".into()
                },
                r.cost_delta(),
                r.days.iter().map(|d| (d.cost * 100.0) as u64).collect(),
                style::output(),
            ))
    }

    fn daily_chart(&self) -> impl IntoElement {
        let r = &self.report;
        let days = &r.days;
        let max_in = days.iter().map(|d| d.input_total).max().unwrap_or(0).max(1);
        let max_out = days.iter().map(|d| d.output).max().unwrap_or(0).max(1);
        let total: u64 = days.iter().map(|d| d.input_total + d.output).sum();
        let per_day = if r.active_days > 0 { total / r.active_days as u64 } else { 0 };
        let right = format!("{} of {} active days · {}/day", r.active_days, days.len(), human(per_day));
        let first = days.first().and_then(|d| d.date).map(short_date).unwrap_or_default();
        let middle = days.get(days.len() / 2).and_then(|d| d.date).map(short_date).unwrap_or_default();
        let gap = if days.len() > 60 { px(1.) } else { px(3.) };

        section(
            "Tokens per day",
            Some(right),
            h_flex()
                .gap_4()
                .child(
                    v_flex()
                        .w(px(64.))
                        .h(px(196.))
                        .justify_between()
                        .child(axis_label(human(max_in), "input", style::accent()))
                        .child(axis_label(human(max_out), "output", style::output())),
                )
                .child(
                    v_flex()
                        .flex_1()
                        .child(
                            h_flex()
                                .h(px(120.))
                                .items_end()
                                .gap(gap)
                                .children(days.iter().enumerate().map(|(i, d)| {
                                    let tip = format!(
                                        "{} · in {} · out {} · {}",
                                        d.date.map(short_date).unwrap_or_default(),
                                        human(d.input_total),
                                        human(d.output),
                                        style::money(d.cost)
                                    );
                                    div()
                                        .id(("in", i))
                                        .flex_1()
                                        .h(relative(d.input_total as f32 / max_in as f32))
                                        .min_h(px(1.))
                                        .rounded_t(px(2.))
                                        .bg(style::soft(style::accent(), 0.85))
                                        .hover(|s| s.bg(style::accent()))
                                        .tooltip(move |window, cx| Tooltip::new(tip.clone()).build(window, cx))
                                })),
                        )
                        .child(div().h(px(1.)).bg(style::border()))
                        .child(
                            h_flex()
                                .h(px(75.))
                                .items_start()
                                .gap(gap)
                                .children(days.iter().map(|d| {
                                    div()
                                        .flex_1()
                                        .h(relative(d.output as f32 / max_out as f32))
                                        .rounded_b(px(2.))
                                        .bg(style::soft(style::output(), 0.85))
                                })),
                        )
                        .child(
                            h_flex()
                                .mt_2()
                                .justify_between()
                                .text_xs()
                                .text_color(style::muted())
                                .child(first)
                                .child(middle)
                                .child("today"),
                        ),
                ),
        )
    }

    fn heatmap(&self) -> impl IntoElement {
        const DAYS: [&str; 7] = ["M", "T", "W", "T", "F", "S", "S"];
        const DAY_NAMES: [&str; 7] = ["Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun"];
        let r = &self.report;
        let max = r.heatmap_max().max(1) as f32;
        let right = r
            .busiest_slot()
            .map(|(d, h)| format!("busiest {} {}:00", DAY_NAMES[d], h))
            .unwrap_or_default();
        section(
            "When agents work",
            Some(right),
            v_flex()
                .gap_1()
                .children(r.heatmap.iter().enumerate().map(|(d, row)| {
                    h_flex()
                        .gap_1()
                        .child(div().w(px(14.)).text_xs().text_color(style::muted()).child(DAYS[d]))
                        .children(row.iter().enumerate().map(move |(h, &v)| {
                            let color = if v == 0 {
                                style::soft(style::panel_hover(), 1.0)
                            } else {
                                style::accent_alpha(0.12 + 0.88 * (v as f32 / max).powf(0.55))
                            };
                            let tip = format!("{} {}:00 · {} tokens", DAY_NAMES[d], h, human(v));
                            div()
                                .id(("heat", d * 24 + h))
                                .flex_1()
                                .h(px(18.))
                                .rounded_sm()
                                .bg(color)
                                .tooltip(move |window, cx| Tooltip::new(tip.clone()).build(window, cx))
                        }))
                }))
                .child(
                    h_flex()
                        .pl(px(18.))
                        .mt_1()
                        .text_xs()
                        .text_color(style::muted())
                        .children([0, 6, 12, 18].map(|h| div().flex_1().child(h.to_string()))),
                )
                .child(
                    h_flex()
                        .justify_end()
                        .gap_1()
                        .text_xs()
                        .text_color(style::muted())
                        .child("less")
                        .children([0.15f32, 0.4, 0.7, 1.0].map(|a| div().size(px(10.)).rounded_sm().bg(style::accent_alpha(a))))
                        .child("more"),
                ),
        )
    }

    fn models(&self) -> impl IntoElement {
        let r = &self.report;
        let total: u64 = r.providers.iter().map(|(_, t)| t).sum::<u64>().max(1);
        let max = r.models.first().map(|m| m.input_total + m.output).unwrap_or(1).max(1);
        const SHOWN: usize = 6;
        let rest = r.models.iter().skip(SHOWN);
        let rest_count = r.models.len().saturating_sub(SHOWN);
        let (rest_in, rest_out, rest_turns, rest_cost) = rest.fold((0, 0, 0, 0.0), |a, m| {
            (a.0 + m.input_total, a.1 + m.output, a.2 + m.turns, a.3 + m.cost.unwrap_or(0.0))
        });

        section(
            "Models",
            Some(format!("{} models", r.models.len())),
            v_flex()
                .gap_3()
                .child(
                    h_flex()
                        .h(px(6.))
                        .w_full()
                        .gap(px(2.))
                        .children(r.providers.iter().map(|(p, t)| {
                            div()
                                .h_full()
                                .w(relative(*t as f32 / total as f32))
                                .rounded_full()
                                .bg(style::provider_color(*p))
                        })),
                )
                .child(
                    h_flex().gap_4().text_sm().children(r.providers.iter().map(|(p, t)| {
                        h_flex()
                            .gap_1p5()
                            .child(provider_badge(*p, 14.))
                            .child(div().text_color(style::muted()).child(format!(
                                "{} {:.0}%",
                                p.name(),
                                *t as f64 / total as f64 * 100.0
                            )))
                    })),
                )
                .child(table_header(&["input", "output", "responses", "cost"]))
                .children(r.models.iter().take(SHOWN).map(|m| {
                    let share = (m.input_total + m.output) as f32 / max as f32;
                    table_row(
                        provider_badge(m.provider, 16.),
                        style::model_name(&m.model),
                        share,
                        style::provider_color(m.provider),
                        vec![human(m.input_total), human(m.output), thousands(m.turns), m.cost.map(style::money).unwrap_or_else(|| "—".into())],
                    )
                }))
                .when(rest_count > 0, |d| {
                    d.child(table_row(
                        div().size(px(8.)).into_any_element(),
                        format!("{rest_count} more"),
                        0.0,
                        style::faint(),
                        vec![human(rest_in), human(rest_out), thousands(rest_turns), style::money(rest_cost)],
                    ))
                }),
        )
    }

    fn agents(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let r = &self.report;
        let max = r.agents.first().map(|a| a.input_total + a.output).unwrap_or(1).max(1);
        let right = match self.agent {
            Some(a) => format!("only {} · click to show all", a.name()),
            None => "click one to filter".into(),
        };
        section(
            "Agents",
            Some(right),
            v_flex()
                .gap_1()
                .child(table_header(&["input", "output", "time", "cost"]))
                .children(r.agents.iter().map(|a| {
                    let agent = a.agent;
                    let selected = self.agent == Some(agent);
                    div()
                        .id(SharedString::from(format!("agent-{}", agent.name())))
                        .rounded_md()
                        .cursor_pointer()
                        .when(selected, |d| d.bg(style::panel_hover()))
                        .hover(|d| d.bg(style::panel_hover()))
                        .on_click(cx.listener(move |this, _, _, cx| this.toggle_agent(agent, cx)))
                        .child(table_row(
                            agent_badge(agent),
                            agent.name().to_string(),
                            (a.input_total + a.output) as f32 / max as f32,
                            style::agent_color(agent),
                            vec![human(a.input_total), human(a.output), style::duration(a.agent_secs), style::money(a.cost)],
                        ))
                })),
        )
    }

    fn projects(&self, cx: &mut Context<Self>) -> impl IntoElement {
        // Proyectos con actividad de agentes + carpetas abiertas en kuaky,
        // del más reciente al más antiguo.
        let mut rows: Vec<kuaky_usage::report::ProjectRow> = self
            .report
            .projects
            .iter()
            .filter(|p| !p.path.contains("/scratch-workspaces/") && std::path::Path::new(&*p.path).is_dir())
            .cloned()
            .collect();
        for recent in &crate::state::get(cx).recent {
            let opened = jiff::Timestamp::from_second(recent.opened).unwrap_or(self.now);
            match rows.iter_mut().find(|r| &*r.path == recent.path) {
                Some(row) => row.last_active = row.last_active.max(opened),
                None if std::path::Path::new(&recent.path).is_dir() => rows.push(kuaky_usage::report::ProjectRow {
                    path: Arc::from(recent.path.as_str()),
                    last_active: opened,
                    tokens: 0,
                    sessions: 0,
                    agents: Vec::new(),
                }),
                None => {}
            }
        }
        // Agrupar subcarpetas bajo la raíz de su repositorio (p. ej. `kuaky/crates/x` → `kuaky`).
        let mut grouped: Vec<kuaky_usage::report::ProjectRow> = Vec::new();
        for mut row in rows {
            let root = repo_root(std::path::Path::new(&*row.path));
            row.path = Arc::from(root.to_string_lossy().as_ref());
            match grouped.iter_mut().find(|g| g.path == row.path) {
                Some(g) => {
                    g.last_active = g.last_active.max(row.last_active);
                    g.tokens += row.tokens;
                    g.sessions += row.sessions;
                    for a in row.agents {
                        if !g.agents.contains(&a) {
                            g.agents.push(a);
                        }
                    }
                }
                None => grouped.push(row),
            }
        }
        let mut rows = grouped;
        rows.sort_by_key(|r| std::cmp::Reverse(r.last_active));
        rows.truncate(8);
        let now = self.now;
        section(
            "Recent projects",
            Some("click to open".into()),
            v_flex()
                .gap_1()
                .when(rows.is_empty(), |d| {
                    d.child(div().text_sm().text_color(style::muted()).child("No projects with agent activity yet."))
                })
                .children(rows.into_iter().enumerate().map(|(i, p)| {
                    let path = PathBuf::from(&*p.path);
                    let name = path
                        .file_name()
                        .map(|n| n.to_string_lossy().into_owned())
                        .unwrap_or_else(|| p.path.to_string());
                    let parent = path.parent().map(|p| tilde(p)).unwrap_or_default();
                    h_flex()
                        .id(("project", i))
                        .gap_3()
                        .px_2()
                        .py_1p5()
                        .rounded_md()
                        .cursor_pointer()
                        .hover(|d| d.bg(style::panel_hover()))
                        .on_click(cx.listener(move |_, _, _, cx| cx.emit(HomeEvent::OpenProject(path.clone()))))
                        .child(
                            v_flex()
                                .flex_1()
                                .min_w_0()
                                .child(div().text_sm().font_weight(FontWeight::MEDIUM).child(name))
                                .child(div().text_xs().text_color(style::faint()).truncate().child(parent)),
                        )
                        .child(h_flex().gap_1().children(p.agents.iter().map(|a| agent_badge(*a))))
                        .child(
                            div()
                                .w(px(90.))
                                .text_right()
                                .text_sm()
                                .text_color(style::muted())
                                .child(if p.tokens > 0 { human(p.tokens) } else { "—".into() }),
                        )
                        .child(
                            div()
                                .w(px(90.))
                                .text_right()
                                .text_xs()
                                .text_color(style::faint())
                                .child(ago(p.last_active, now)),
                        )
                })),
        )
    }
}

impl Render for HomeView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let loading = self.events.is_none();
        let empty = !loading && self.report.kpis.turns == 0;
        div()
            .id("home")
            .size_full()
            .overflow_y_scroll()
            .bg(style::bg())
            .text_color(style::text())
            .child(
                v_flex()
                    .p_8()
                    .gap_8()
                    .child(self.header(cx))
                    .when(loading, |d| d.child(placeholder("Reading your agent logs…")))
                    .when(empty, |d| d.child(placeholder("No agent activity in this period.")))
                    .when(!loading && !empty, |d| {
                        d.child(self.kpi_cards())
                            .when(!self.limits.is_empty(), |d| d.child(self.limits_section()))
                            .child(
                                h_flex()
                                    .gap_8()
                                    .items_start()
                                    .child(div().flex_1().flex_basis(relative(0.58)).child(self.daily_chart()))
                                    .child(div().flex_1().flex_basis(relative(0.42)).child(self.heatmap())),
                            )
                            .child(
                                h_flex()
                                    .gap_8()
                                    .items_start()
                                    .child(div().flex_1().child(self.models()))
                                    .child(div().flex_1().child(self.agents(cx))),
                            )
                    })
                    .when(!loading, |d| d.child(self.projects(cx))),
            )
    }
}

// ── piezas ───────────────────────────────────────────────────────────────

fn kpi_card(
    label: &'static str,
    value: String,
    sub: String,
    delta: Option<f64>,
    series: Vec<u64>,
    color: Rgba,
) -> impl IntoElement {
    let max = series.iter().copied().max().unwrap_or(0).max(1) as f32;
    v_flex()
        .flex_1()
        .min_w_0()
        .p_4()
        .gap_1()
        .rounded_xl()
        .bg(style::panel())
        .border_1()
        .border_color(style::border())
        .child(
            h_flex()
                .justify_between()
                .child(div().text_xs().text_color(style::muted()).child(label.to_uppercase()))
                .when_some(delta, |d, delta| d.child(delta_pill(delta))),
        )
        .child(div().text_3xl().font_weight(FontWeight::SEMIBOLD).child(value))
        .child(div().text_sm().text_color(style::muted()).child(sub))
        .child(
            h_flex()
                .mt_3()
                .h(px(32.))
                .items_end()
                .gap(px(2.))
                .children(series.into_iter().map(move |v| {
                    div()
                        .flex_1()
                        .h(relative(v as f32 / max))
                        .min_h(px(2.))
                        .rounded_t(px(1.))
                        .bg(if v == 0 { style::soft(style::faint(), 0.5) } else { style::soft(color, 0.7) })
                })),
        )
}

fn delta_pill(delta: f64) -> impl IntoElement {
    let up = delta >= 0.0;
    let color = if up { style::good() } else { style::bad() };
    let text = if delta.abs() >= 1000.0 {
        format!("{}999%", if up { "+" } else { "−" })
    } else {
        format!("{}{:.0}%", if up { "+" } else { "−" }, delta.abs())
    };
    div()
        .px_2()
        .rounded_full()
        .text_xs()
        .bg(style::soft(color, 0.12))
        .text_color(color)
        .child(text)
}

fn section(title: &'static str, right: Option<String>, body: impl IntoElement) -> impl IntoElement {
    v_flex()
        .gap_4()
        .child(
            h_flex()
                .gap_3()
                .text_xs()
                .text_color(style::muted())
                .child(title.to_uppercase())
                .child(div().flex_1().h(px(1.)).bg(style::border()))
                .when_some(right, |d, r| d.child(r)),
        )
        .child(body)
}

fn axis_label(value: String, label: &'static str, color: Rgba) -> impl IntoElement {
    v_flex()
        .child(div().text_sm().child(value))
        .child(div().text_xs().text_color(color).child(label))
}

fn table_header(cols: &[&'static str]) -> impl IntoElement {
    h_flex()
        .px_2()
        .text_xs()
        .text_color(style::faint())
        .child(div().flex_1())
        .children(cols.iter().map(|c| div().w(px(76.)).text_right().child(*c)))
}

fn table_row(icon: AnyElement, name: String, share: f32, color: Rgba, values: Vec<String>) -> impl IntoElement {
    h_flex()
        .px_2()
        .py_1()
        .gap_3()
        .text_sm()
        .child(h_flex().w(px(180.)).gap_2().child(icon).child(div().truncate().child(name)))
        .child(
            div().flex_1().child(
                div()
                    .h(px(6.))
                    .w(relative(share.clamp(0.0, 1.0)))
                    .min_w(px(if share > 0.0 { 4. } else { 0. }))
                    .rounded_full()
                    .bg(color),
            ),
        )
        .children(values.into_iter().map(|v| div().w(px(76.)).text_right().child(v)))
}

fn agent_badge(agent: Agent) -> AnyElement {
    icons::logo(icons::agent(agent), px(18.)).into_any_element()
}

/// Logo del proveedor o, si no hay, un punto de su color.
fn provider_badge(p: kuaky_usage::Provider, size: f32) -> AnyElement {
    match icons::provider(p) {
        Some(image) => icons::logo(image, px(size)).into_any_element(),
        None => div().size(px(8.)).rounded_full().bg(style::provider_color(p)).into_any_element(),
    }
}

fn placeholder(text: &'static str) -> impl IntoElement {
    div()
        .h(px(240.))
        .flex()
        .items_center()
        .justify_center()
        .rounded_xl()
        .border_1()
        .border_color(style::border())
        .text_color(style::muted())
        .child(text)
}

const MONTHS: [&str; 12] = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];

fn short_date(d: Date) -> String {
    format!("{} {}", MONTHS[(d.month() - 1) as usize], d.day())
}

fn ago(ts: Timestamp, now: Timestamp) -> String {
    let secs = (now.as_second() - ts.as_second()).max(0);
    match secs {
        s if s < 60 => "just now".into(),
        s if s < 3600 => format!("{} min ago", s / 60),
        s if s < 86_400 => format!("{} h ago", s / 3600),
        s => format!("{} d ago", s / 86_400),
    }
}

/// Carpeta más cercana hacia arriba que contiene `.git`; si no hay, la misma.
fn repo_root(path: &std::path::Path) -> std::path::PathBuf {
    path.ancestors()
        .find(|p| p.join(".git").exists())
        .unwrap_or(path)
        .to_path_buf()
}

/// `/Users/x/proyectos` → `~/proyectos`.
pub fn tilde(path: &std::path::Path) -> String {
    if let Some(home) = dirs::home_dir() {
        if let Ok(rest) = path.strip_prefix(&home) {
            return format!("~/{}", rest.display());
        }
    }
    path.display().to_string()
}

#[cfg(feature = "screenshot")]
pub fn demo_limits() -> Vec<AgentLimits> {
    let now = Timestamp::now();
    let five_h = Timestamp::from_second(now.as_second() + 2 * 3600 + 40 * 60).ok();
    let week = Timestamp::from_second(now.as_second() + 4 * 86_400).ok();
    vec![
        AgentLimits {
            agent: Agent::ClaudeCode,
            primary: Some(kuaky_usage::RateWindow::from_used("5h", 42.0, five_h)),
            secondary: Some(kuaky_usage::RateWindow::from_used("week", 18.0, week)),
            updated_at: Some(now),
        },
        AgentLimits {
            agent: Agent::Codex,
            primary: Some(kuaky_usage::RateWindow::from_used("5h", 28.0, five_h)),
            secondary: Some(kuaky_usage::RateWindow::from_used("week", 11.0, week)),
            updated_at: Some(now),
        },
    ]
}
