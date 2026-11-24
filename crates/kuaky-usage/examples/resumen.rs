//! `cargo run --release -p kuaky-usage --example resumen` — imprime el consumo local.
use kuaky_usage::{Range, Report};

fn main() {
    let t = std::time::Instant::now();
    let events = kuaky_usage::load_all();
    println!("{} eventos en {:?}", events.len(), t.elapsed());
    let tz = jiff::tz::TimeZone::system();
    let r = Report::build(&events, Range::Days(30), None, &tz, jiff::Timestamp::now(), &kuaky_usage::Pricing::load());
    let k = r.kpis;
    println!(
        "30 días: entrada {} ({:.0}% caché) · salida {} · {} sesiones · {} respuestas · {:.1} h · máx {} en paralelo · {} días activos",
        k.input_total, k.cache_pct(), k.output, k.sessions, k.turns, k.agent_secs as f64 / 3600.0, k.max_parallel, r.active_days
    );
    println!("  costo equivalente en API: ${:.2} ({} respuestas sin precio)", k.cost, k.unpriced);
    for m in r.models.iter().take(5) {
        println!("  modelo {:<24} {:>14} {:>10} {:>6}", m.model, m.input_total, m.output, m.turns);
    }
    for a in &r.agents {
        println!("  agente {:<12} {:>14} {:>10} {:.1} h", a.agent.name(), a.input_total, a.output, a.agent_secs as f64 / 3600.0);
    }
    for p in r.projects.iter().take(5) {
        println!("  proyecto {} ({} sesiones)", p.path, p.sessions);
    }
}
