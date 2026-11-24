//! Consumo de tokens de agentes de código, leído de sus logs locales.
//!
//! Cada agente tiene un módulo en [`sources`] que convierte sus archivos en
//! [`UsageEvent`]s. [`report`] agrega esos eventos para un rango de fechas.

pub mod limits;
pub mod model;
pub mod pricing;
pub mod report;
pub mod sources;

pub use limits::{AgentLimits, RateWindow, load_all_limits};
pub use model::{Agent, Provider, UsageEvent};
pub use pricing::Pricing;
pub use report::{Range, Report};

use std::time::Instant;

/// Lee todos los agentes conocidos de este equipo.
///
/// Es trabajo pesado de disco: llamarlo fuera del hilo de la interfaz.
pub fn load_all() -> Vec<UsageEvent> {
    let started = Instant::now();
    let mut events = Vec::new();
    for source in sources::all() {
        let t = Instant::now();
        match source.load() {
            Ok(mut found) => {
                tracing::info!(
                    agent = source.agent().name(),
                    events = found.len(),
                    ms = t.elapsed().as_millis() as u64,
                    "fuente leída"
                );
                events.append(&mut found);
            }
            Err(err) => tracing::warn!(agent = source.agent().name(), %err, "no se pudo leer"),
        }
    }
    events.sort_by_key(|e| e.ts);
    tracing::info!(
        total = events.len(),
        ms = started.elapsed().as_millis() as u64,
        "consumo cargado"
    );
    events
}
