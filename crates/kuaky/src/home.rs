//! Ventana de inicio: consumo de tokens de todos los agentes.

use crate::icons;
use crate::style::{self, human, thousands};
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::component::{h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use jiff::{Timestamp, civil::Date, tz::TimeZone};
use kuaky_usage::{Agent, Range, Report, UsageEvent};
use std::{path::PathBuf, sync::Arc};

pub enum HomeEvent {
    OpenProject(PathBuf),
    PickFolder,
}

pub struct HomeView {
    events: Option<Arc<Vec<UsageEvent>>>,
    range: Range,
    agent: Option<Agent>,
    report: Report,
    tz: TimeZone,
    now: Timestamp,
}

impl EventEmitter<HomeEvent> for HomeView {}

impl HomeView {
    pub fn new(cx: &mut Context<Self>) -> Self {
        let this = Self {
            events: None,
            range: Range::Days(30),
            agent: None,
            report: Report::default(),
            tz: TimeZone::system(),
            now: Timestamp::now(),
        };
        this.reload(cx);
        this
    }

    /// Eventos de consumo ya cargados (para la actividad de cada proyecto).
    pub fn events(&self) -> Option<Arc<Vec<UsageEvent>>> {
        self.events.clone()
    }

    /// Vuelve a leer los logs en segundo plano.
    pub fn reload(&self, cx: &mut Context<Self>) {
        cx.spawn(async move |this, cx| {
            let events = cx
                .background_executor()
                .spawn(async { kuaky_usage::load_all() })
                .await;
            this.update(cx, |this, cx| {
                this.events = Some(Arc::new(events));
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
            self.report = Report::build(events, self.range, self.agent, &self.tz, self.now);
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
            (Some(s), Some(_)) => format!("{} – hoy", short_date(s)),
            _ => "Leyendo logs locales…".to_string(),
        };
        let title = match self.agent {
            Some(a) => format!("Consumo · {}", a.name()),
            None => "Consumo de agentes".to_string(),
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
                            .child("Abrir carpeta")
                            .on_click(cx.listener(|_, _, _, cx| cx.emit(HomeEvent::PickFolder))),
                    ),
            )
    }

    fn kpi_cards(&self) -> impl IntoElement {
        let r = &self.report;
        let k = r.kpis;
        let series = |f: fn(&kuaky_usage::report::DayStat) -> u64| r.days.iter().map(f).collect::<Vec<_>>();
        let parallel = if k.max_parallel > 1 {
            format!("hasta {} a la vez", k.max_parallel)
        } else {
            "una sesión a la vez".into()
        };
        h_flex()
            .gap_4()
            .child(kpi_card(
                "Tokens de entrada",
                human(k.input_total),
                format!("{:.0}% desde caché", k.cache_pct()),
                r.delta(|k| k.input_total),
                series(|d| d.input_total),
                style::accent(),
            ))
            .child(kpi_card(
                "Tokens de salida",
                human(k.output),
                format!("{} por respuesta", human(k.output_per_turn())),
                r.delta(|k| k.output),
                series(|d| d.output),
                style::output(),
            ))
            .child(kpi_card(
                "Sesiones",
                thousands(k.sessions),
                format!("{} respuestas", thousands(k.turns)),
                r.delta(|k| k.sessions),
                series(|d| d.sessions),
                style::accent(),
            ))
            .child(kpi_card(
                "Tiempo de agentes",
                style::duration(k.agent_secs),
                parallel,
                r.delta(|k| k.agent_secs),
                series(|d| d.agent_secs),
                style::accent(),
            ))
    }

    fn daily_chart(&self) -> impl IntoElement {
        let r = &self.report;
        let days = &r.days;
        let max_in = days.iter().map(|d| d.input_total).max().unwrap_or(0).max(1);
        let max_out = days.iter().map(|d| d.output).max().unwrap_or(0).max(1);
        let total: u64 = days.iter().map(|d| d.input_total + d.output).sum();
        let per_day = if r.active_days > 0 { total / r.active_days as u64 } else { 0 };
        let right = format!("{} de {} días con agentes · {}/día", r.active_days, days.len(), human(per_day));
        let first = days.first().and_then(|d| d.date).map(short_date).unwrap_or_default();
        let middle = days.get(days.len() / 2).and_then(|d| d.date).map(short_date).unwrap_or_default();
        let gap = if days.len() > 60 { px(1.) } else { px(3.) };

        section(
            "Tokens por día",
            Some(right),
            h_flex()
                .gap_4()
                .child(
                    v_flex()
                        .w(px(64.))
                        .h(px(196.))
                        .justify_between()
                        .child(axis_label(human(max_in), "entrada", style::accent()))
                        .child(axis_label(human(max_out), "salida", style::output())),
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
                                        "{} · entrada {} · salida {}",
                                        d.date.map(short_date).unwrap_or_default(),
                                        human(d.input_total),
                                        human(d.output)
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
                                .child("hoy"),
                        ),
                ),
        )
    }

    fn heatmap(&self) -> impl IntoElement {
        const DAYS: [&str; 7] = ["L", "M", "M", "J", "V", "S", "D"];
        const DAY_NAMES: [&str; 7] = ["lun", "mar", "mié", "jue", "vie", "sáb", "dom"];
        let r = &self.report;
        let max = r.heatmap_max().max(1) as f32;
        let right = r
            .busiest_slot()
            .map(|(d, h)| format!("más activo {} {} h", DAY_NAMES[d], h))
            .unwrap_or_default();
        section(
            "Cuándo trabajan",
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
                        .child("menos")
                        .children([0.15f32, 0.4, 0.7, 1.0].map(|a| div().size(px(10.)).rounded_sm().bg(style::accent_alpha(a))))
                        .child("más"),
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
        let (rest_in, rest_out, rest_turns) = rest.fold((0, 0, 0), |a, m| (a.0 + m.input_total, a.1 + m.output, a.2 + m.turns));

        section(
            "Modelos",
            Some(format!("{} modelos", r.models.len())),
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
                .child(table_header(&["entrada", "salida", "respuestas"]))
                .children(r.models.iter().take(SHOWN).map(|m| {
                    let share = (m.input_total + m.output) as f32 / max as f32;
                    table_row(
                        provider_badge(m.provider, 16.),
                        style::model_name(&m.model),
                        share,
                        style::provider_color(m.provider),
                        [human(m.input_total), human(m.output), thousands(m.turns)],
                    )
                }))
                .when(rest_count > 0, |d| {
                    d.child(table_row(
                        div().size(px(8.)).into_any_element(),
                        format!("{rest_count} más"),
                        0.0,
                        style::faint(),
                        [human(rest_in), human(rest_out), thousands(rest_turns)],
                    ))
                }),
        )
    }

    fn agents(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let r = &self.report;
        let max = r.agents.first().map(|a| a.input_total + a.output).unwrap_or(1).max(1);
        let right = match self.agent {
            Some(a) => format!("solo {} · clic para ver todos", a.name()),
            None => "clic en uno para ver solo su consumo".into(),
        };
        section(
            "Agentes",
            Some(right),
            v_flex()
                .gap_1()
                .child(table_header(&["entrada", "salida", "tiempo"]))
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
                            [human(a.input_total), human(a.output), style::duration(a.agent_secs)],
                        ))
                })),
        )
    }

    fn projects(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let rows: Vec<_> = self
            .report
            .projects
            .iter()
            .filter(|p| !p.path.contains("/scratch-workspaces/") && std::path::Path::new(&*p.path).is_dir())
            .take(8)
            .cloned()
            .collect();
        let now = self.now;
        section(
            "Proyectos recientes",
            Some("clic para abrir".into()),
            v_flex()
                .gap_1()
                .when(rows.is_empty(), |d| {
                    d.child(div().text_sm().text_color(style::muted()).child("Aún no hay proyectos con actividad de agentes."))
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
                                .child(human(p.tokens)),
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
                    .when(loading, |d| d.child(placeholder("Leyendo los logs de tus agentes…")))
                    .when(empty, |d| d.child(placeholder("No hay actividad de agentes en este periodo.")))
                    .when(!loading && !empty, |d| {
                        d.child(self.kpi_cards())
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

fn table_header(cols: &[&'static str; 3]) -> impl IntoElement {
    h_flex()
        .px_2()
        .text_xs()
        .text_color(style::faint())
        .child(div().flex_1())
        .children(cols.iter().map(|c| div().w(px(84.)).text_right().child(*c)))
}

fn table_row(icon: AnyElement, name: String, share: f32, color: Rgba, values: [String; 3]) -> impl IntoElement {
    h_flex()
        .px_2()
        .py_1()
        .gap_3()
        .text_sm()
        .child(h_flex().w(px(220.)).gap_2().child(icon).child(div().truncate().child(name)))
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
        .children(values.into_iter().map(|v| div().w(px(84.)).text_right().child(v)))
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

const MONTHS: [&str; 12] = ["ene", "feb", "mar", "abr", "may", "jun", "jul", "ago", "sep", "oct", "nov", "dic"];

fn short_date(d: Date) -> String {
    format!("{} {}", d.day(), MONTHS[(d.month() - 1) as usize])
}

fn ago(ts: Timestamp, now: Timestamp) -> String {
    let secs = (now.as_second() - ts.as_second()).max(0);
    match secs {
        s if s < 60 => "ahora".into(),
        s if s < 3600 => format!("hace {} min", s / 60),
        s if s < 86_400 => format!("hace {} h", s / 3600),
        s => format!("hace {} d", s / 86_400),
    }
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
