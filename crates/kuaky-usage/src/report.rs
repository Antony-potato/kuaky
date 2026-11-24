//! Agregaciones para el dashboard: KPIs, series por día, heatmap, modelos,
//! agentes y proyectos.

use crate::{Agent, Pricing, Provider, UsageEvent};
use jiff::{Timestamp, ToSpan, civil::Date, tz::TimeZone};
use std::{
    collections::{BTreeMap, HashMap, HashSet},
    sync::Arc,
};

/// Hueco máximo entre respuestas para seguir contando la sesión como activa.
const IDLE_GAP_SECS: i64 = 5 * 60;
/// Tiempo mínimo que se atribuye a una respuesta aislada.
const MIN_TURN_SECS: i64 = 30;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Range {
    Days(u32),
    All,
}

impl Range {
    pub const PRESETS: [Range; 4] = [Range::Days(7), Range::Days(30), Range::Days(90), Range::All];

    pub fn label(self) -> &'static str {
        match self {
            Range::Days(7) => "7 días",
            Range::Days(30) => "30 días",
            Range::Days(90) => "90 días",
            Range::Days(_) => "días",
            Range::All => "Todo",
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Kpis {
    pub input_total: u64,
    pub cache_read: u64,
    pub output: u64,
    pub sessions: u64,
    pub turns: u64,
    pub agent_secs: u64,
    pub max_parallel: u32,
    /// Costo equivalente en API (dólares) de las respuestas con precio conocido.
    pub cost: f64,
    /// Respuestas cuyo modelo no tiene precio conocido.
    pub unpriced: u64,
}

impl Kpis {
    pub fn cache_pct(&self) -> f64 {
        pct(self.cache_read, self.input_total)
    }

    pub fn output_per_turn(&self) -> u64 {
        if self.turns == 0 { 0 } else { self.output / self.turns }
    }
}

#[derive(Debug, Clone, Default)]
pub struct DayStat {
    pub date: Option<Date>,
    pub cost: f64,
    pub input_total: u64,
    pub output: u64,
    pub sessions: u64,
    pub turns: u64,
    pub agent_secs: u64,
}

#[derive(Debug, Clone)]
pub struct ModelRow {
    pub model: Arc<str>,
    pub provider: Provider,
    pub input_total: u64,
    pub output: u64,
    pub turns: u64,
    /// `None` si el modelo no tiene precio conocido.
    pub cost: Option<f64>,
}

#[derive(Debug, Clone)]
pub struct AgentRow {
    pub agent: Agent,
    pub input_total: u64,
    pub output: u64,
    pub agent_secs: u64,
    pub cost: f64,
}

#[derive(Debug, Clone)]
pub struct ProjectRow {
    pub path: Arc<str>,
    pub last_active: Timestamp,
    pub tokens: u64,
    pub sessions: u64,
    pub agents: Vec<Agent>,
}

#[derive(Debug, Clone, Default)]
pub struct Report {
    pub start: Option<Date>,
    pub end: Option<Date>,
    pub kpis: Kpis,
    /// KPIs del periodo anterior de igual duración (no existe para "Todo").
    pub previous: Option<Kpis>,
    pub days: Vec<DayStat>,
    pub active_days: usize,
    /// `heatmap[día][hora]`, día 0 = lunes, en hora local.
    pub heatmap: [[u64; 24]; 7],
    pub providers: Vec<(Provider, u64)>,
    pub models: Vec<ModelRow>,
    pub agents: Vec<AgentRow>,
    pub projects: Vec<ProjectRow>,
}

impl Report {
    /// `events` debe venir ordenado por fecha.
    pub fn build(
        events: &[UsageEvent],
        range: Range,
        agent: Option<Agent>,
        tz: &TimeZone,
        now: Timestamp,
        pricing: &Pricing,
    ) -> Report {
        let today = now.to_zoned(tz.clone()).date();
        let filtered: Vec<&UsageEvent> = events.iter().filter(|e| agent.is_none_or(|a| e.agent == a)).collect();
        let Some(first) = filtered.first() else {
            return Report::default();
        };
        let start = match range {
            Range::Days(n) => today.checked_sub((n as i64 - 1).days()).unwrap_or(today),
            Range::All => first.ts.to_zoned(tz.clone()).date(),
        };
        let date_of = |e: &UsageEvent| e.ts.to_zoned(tz.clone()).date();

        let current: Vec<&UsageEvent> = filtered.iter().copied().filter(|e| date_of(e) >= start).collect();
        let previous = match range {
            Range::Days(n) => {
                let prev_start = start.checked_sub((n as i64).days()).unwrap_or(start);
                let prev: Vec<&UsageEvent> = filtered
                    .iter()
                    .copied()
                    .filter(|e| {
                        let d = date_of(e);
                        d >= prev_start && d < start
                    })
                    .collect();
                Some(kpis_of(&prev, pricing))
            }
            Range::All => None,
        };

        // Series por día, incluidos los días sin actividad.
        let mut days: BTreeMap<Date, DayStat> = BTreeMap::new();
        let mut d = start;
        while d <= today {
            days.insert(d, DayStat { date: Some(d), ..DayStat::default() });
            let Ok(next) = d.tomorrow() else { break };
            d = next;
        }
        let mut day_sessions: HashMap<Date, HashSet<(Agent, &str)>> = HashMap::new();
        let mut heatmap = [[0u64; 24]; 7];
        let mut models: HashMap<Arc<str>, ModelRow> = HashMap::new();
        let mut providers: HashMap<Provider, u64> = HashMap::new();
        let mut agents: HashMap<Agent, AgentRow> = HashMap::new();
        let mut projects: HashMap<Arc<str>, (Timestamp, u64, HashSet<&str>, HashSet<Agent>)> = HashMap::new();

        for e in &current {
            let z = e.ts.to_zoned(tz.clone());
            let date = z.date();
            let tokens = e.input_total() + e.output;
            let cost = pricing.cost(e);
            if let Some(day) = days.get_mut(&date) {
                day.input_total += e.input_total();
                day.output += e.output;
                day.turns += 1;
                day.cost += cost.unwrap_or(0.0);
            }
            day_sessions.entry(date).or_default().insert((e.agent, &e.session));
            let wd = z.weekday().to_monday_zero_offset() as usize;
            heatmap[wd][z.hour() as usize] += tokens;

            let m = models.entry(e.model.clone()).or_insert_with(|| ModelRow {
                model: e.model.clone(),
                provider: e.provider(),
                input_total: 0,
                output: 0,
                turns: 0,
                cost: pricing.price(&e.model).map(|_| 0.0),
            });
            m.input_total += e.input_total();
            m.output += e.output;
            m.turns += 1;
            if let (Some(total), Some(c)) = (m.cost.as_mut(), cost) {
                *total += c;
            }
            *providers.entry(e.provider()).or_default() += tokens;

            let a = agents.entry(e.agent).or_insert_with(|| AgentRow {
                agent: e.agent,
                input_total: 0,
                output: 0,
                agent_secs: 0,
                cost: 0.0,
            });
            a.input_total += e.input_total();
            a.output += e.output;
            a.cost += cost.unwrap_or(0.0);

            if let Some(p) = &e.project {
                let entry = projects.entry(p.clone()).or_insert_with(|| (e.ts, 0, HashSet::new(), HashSet::new()));
                entry.0 = entry.0.max(e.ts);
                entry.1 += tokens;
                entry.2.insert(&e.session);
                entry.3.insert(e.agent);
            }
        }
        for (date, set) in &day_sessions {
            if let Some(day) = days.get_mut(date) {
                day.sessions = set.len() as u64;
            }
        }

        let segments = active_segments(&current);
        for seg in &segments {
            let secs = (seg.end - seg.start) as u64;
            if let Some(a) = agents.get_mut(&seg.agent) {
                a.agent_secs += secs;
            }
            if let Ok(ts) = Timestamp::from_second(seg.start) {
                if let Some(day) = days.get_mut(&ts.to_zoned(tz.clone()).date()) {
                    day.agent_secs += secs;
                }
            }
        }

        let mut kpis = kpis_of(&current, pricing);
        kpis.max_parallel = max_parallel(&segments);

        let days: Vec<DayStat> = days.into_values().collect();
        let active_days = days.iter().filter(|d| d.turns > 0).count();

        let mut models: Vec<ModelRow> = models.into_values().collect();
        models.sort_by_key(|m| std::cmp::Reverse(m.input_total + m.output));
        let mut providers: Vec<(Provider, u64)> = providers.into_iter().collect();
        providers.sort_by_key(|(_, t)| std::cmp::Reverse(*t));
        let mut agents: Vec<AgentRow> = agents.into_values().collect();
        agents.sort_by_key(|a| std::cmp::Reverse(a.input_total + a.output));
        let mut projects: Vec<ProjectRow> = projects
            .into_iter()
            .map(|(path, (last, tokens, sessions, agents))| {
                let mut agents: Vec<Agent> = agents.into_iter().collect();
                agents.sort();
                ProjectRow { path, last_active: last, tokens, sessions: sessions.len() as u64, agents }
            })
            .collect();
        projects.sort_by_key(|p| std::cmp::Reverse(p.last_active));

        Report {
            start: Some(start),
            end: Some(today),
            kpis,
            previous,
            days,
            active_days,
            heatmap,
            providers,
            models,
            agents,
            projects,
        }
    }

    /// Variación porcentual del costo contra el periodo anterior.
    pub fn cost_delta(&self) -> Option<f64> {
        let prev = self.previous.as_ref()?.cost;
        (prev > 0.0).then(|| (self.kpis.cost - prev) / prev * 100.0)
    }

    /// Variación porcentual contra el periodo anterior.
    pub fn delta(&self, pick: impl Fn(&Kpis) -> u64) -> Option<f64> {
        let prev = pick(self.previous.as_ref()?);
        if prev == 0 {
            return None;
        }
        Some((pick(&self.kpis) as f64 - prev as f64) / prev as f64 * 100.0)
    }

    /// Día de la semana (0 = lunes) y hora con más actividad.
    pub fn busiest_slot(&self) -> Option<(usize, usize)> {
        let mut best = None;
        let mut max = 0;
        for (d, row) in self.heatmap.iter().enumerate() {
            for (h, &v) in row.iter().enumerate() {
                if v > max {
                    max = v;
                    best = Some((d, h));
                }
            }
        }
        best
    }

    pub fn heatmap_max(&self) -> u64 {
        self.heatmap.iter().flatten().copied().max().unwrap_or(0)
    }
}

fn kpis_of(events: &[&UsageEvent], pricing: &Pricing) -> Kpis {
    let mut k = Kpis::default();
    let mut sessions = HashSet::new();
    for e in events {
        match pricing.cost(e) {
            Some(c) => k.cost += c,
            None => k.unpriced += 1,
        }
        k.input_total += e.input_total();
        k.cache_read += e.cache_read;
        k.output += e.output;
        k.turns += 1;
        sessions.insert((e.agent, &*e.session));
    }
    k.sessions = sessions.len() as u64;
    k.agent_secs = active_segments(events).iter().map(|s| (s.end - s.start) as u64).sum();
    k
}

struct Segment {
    agent: Agent,
    start: i64,
    end: i64,
}

/// Tramos de actividad continua por sesión: respuestas separadas por menos de
/// [`IDLE_GAP_SECS`] pertenecen al mismo tramo.
fn active_segments(events: &[&UsageEvent]) -> Vec<Segment> {
    let mut by_session: HashMap<(Agent, &str), Vec<i64>> = HashMap::new();
    for e in events {
        by_session.entry((e.agent, &e.session)).or_default().push(e.ts.as_second());
    }
    let mut out = Vec::new();
    for ((agent, _), mut times) in by_session {
        times.sort_unstable();
        let mut start = times[0];
        let mut last = times[0];
        for &t in &times[1..] {
            if t - last > IDLE_GAP_SECS {
                out.push(Segment { agent, start, end: (last).max(start + MIN_TURN_SECS) });
                start = t;
            }
            last = t;
        }
        out.push(Segment { agent, start, end: last.max(start + MIN_TURN_SECS) });
    }
    out
}

fn max_parallel(segments: &[Segment]) -> u32 {
    let mut edges: Vec<(i64, i32)> = segments.iter().flat_map(|s| [(s.start, 1), (s.end, -1)]).collect();
    // A la misma hora, cerrar antes de abrir.
    edges.sort_unstable();
    let (mut cur, mut max) = (0i32, 0i32);
    for (_, d) in edges {
        cur += d;
        max = max.max(cur);
    }
    max.max(0) as u32
}

fn pct(part: u64, whole: u64) -> f64 {
    if whole == 0 { 0.0 } else { part as f64 / whole as f64 * 100.0 }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ev(agent: Agent, session: &str, ts: &str, input: u64, output: u64) -> UsageEvent {
        UsageEvent {
            agent,
            ts: ts.parse().unwrap(),
            model: Arc::from(if agent == Agent::Codex { "gpt-6.2" } else { "claude-opus-5-5" }),
            session: Arc::from(session),
            project: Some(Arc::from("/tmp/app")),
            input,
            output,
            cache_read: input * 9,
            cache_write: 0,
            cache_write_1h: 0,
        }
    }

    #[test]
    fn builds_kpis_segments_and_parallelism() {
        let events = vec![
            ev(Agent::ClaudeCode, "a", "2026-11-20T10:00:00Z", 10, 5),
            ev(Agent::ClaudeCode, "a", "2026-11-20T10:02:00Z", 10, 5),
            ev(Agent::Codex, "b", "2026-11-20T10:01:00Z", 10, 5),
            ev(Agent::Codex, "b", "2026-11-20T10:03:00Z", 10, 5),
            // Hueco de más de 5 min: nuevo tramo.
            ev(Agent::ClaudeCode, "a", "2026-11-20T11:00:00Z", 10, 5),
        ];
        let now: Timestamp = "2026-11-21T12:00:00Z".parse().unwrap();
        let r = Report::build(&events, Range::Days(7), None, &TimeZone::UTC, now, &Pricing::embedded());
        assert_eq!(r.kpis.turns, 5);
        assert_eq!(r.kpis.sessions, 2);
        assert_eq!(r.kpis.input_total, 500);
        assert_eq!(r.kpis.cache_pct().round(), 90.0);
        // a: 120 s + 30 s; b: 120 s
        assert_eq!(r.kpis.agent_secs, 270);
        assert_eq!(r.kpis.max_parallel, 2);
        assert_eq!(r.days.len(), 7);
        assert_eq!(r.active_days, 1);
        assert_eq!(r.agents.len(), 2);
        assert_eq!(r.projects.len(), 1);
        assert_eq!(r.providers.len(), 2);
        // 20 nov 2026 es viernes (índice 4).
        assert_eq!(r.busiest_slot().map(|s| s.0), Some(4));
    }

    #[test]
    fn filters_by_agent_and_computes_delta() {
        let events = vec![
            ev(Agent::ClaudeCode, "a", "2026-11-10T10:00:00Z", 10, 5),
            ev(Agent::ClaudeCode, "a", "2026-11-20T10:00:00Z", 10, 15),
            ev(Agent::Codex, "b", "2026-11-20T10:00:00Z", 10, 5),
        ];
        let now: Timestamp = "2026-11-21T12:00:00Z".parse().unwrap();
        let r = Report::build(&events, Range::Days(7), Some(Agent::ClaudeCode), &TimeZone::UTC, now, &Pricing::embedded());
        assert_eq!(r.kpis.turns, 1);
        assert_eq!(r.delta(|k| k.output), Some(200.0));
        assert!(r.kpis.cost > 0.0, "Opus 5.5 tiene precio");
        assert_eq!(r.kpis.unpriced, 0);
    }
}
