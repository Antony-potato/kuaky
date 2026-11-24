//! Precios por token de cada modelo y costo equivalente en API.
//!
//! Fuente: tabla de precios de LiteLLM (MIT,
//! `model_prices_and_context_window.json`). kuaky trae una copia reducida
//! (`data/prices.json`) para funcionar sin red y la actualiza una vez al día en
//! `<cache>/kuaky/litellm-prices.json`.

use crate::UsageEvent;
use serde_json::Value;
use std::{
    collections::HashMap,
    path::PathBuf,
    time::{Duration, SystemTime},
};

const LITELLM_URL: &str = "https://raw.githubusercontent.com/BerriAI/litellm/main/model_prices_and_context_window.json";
const MAX_AGE: Duration = Duration::from_secs(24 * 3600);
const EMBEDDED: &str = include_str!("../data/prices.json");

/// Precio en dólares por token.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Price {
    pub input: f64,
    pub output: f64,
    pub cache_read: f64,
    /// Escritura en caché de 5 minutos.
    pub cache_write: f64,
    /// Escritura en caché de 1 hora (la que usa Claude Code).
    pub cache_write_1h: f64,
}

#[derive(Debug, Clone, Default)]
pub struct Pricing {
    prices: HashMap<String, Price>,
}

impl Pricing {
    /// Precios del caché descargado si existe; si no, los que trae kuaky.
    pub fn load() -> Pricing {
        if let Some(text) = cache_path().and_then(|p| std::fs::read_to_string(p).ok()) {
            if let Some(p) = Self::from_litellm(&text).filter(|p| !p.prices.is_empty()) {
                return p;
            }
        }
        Self::embedded()
    }

    pub fn embedded() -> Pricing {
        let raw: HashMap<String, HashMap<String, f64>> = serde_json::from_str(EMBEDDED).unwrap_or_default();
        let prices = raw
            .into_iter()
            .filter_map(|(k, f)| {
                let input = *f.get("i")?;
                Some((k, complete(input, f.get("o").copied()?, f.get("r").copied(), f.get("w").copied(), f.get("w1").copied())))
            })
            .collect();
        Pricing { prices }
    }

    /// Lee el formato completo de LiteLLM.
    pub fn from_litellm(text: &str) -> Option<Pricing> {
        let v: HashMap<String, Value> = serde_json::from_str(text).ok()?;
        let num = |o: &Value, k: &str| o.get(k).and_then(Value::as_f64);
        let prices = v
            .into_iter()
            .filter_map(|(k, o)| {
                let input = num(&o, "input_cost_per_token")?;
                let output = num(&o, "output_cost_per_token")?;
                Some((
                    k,
                    complete(
                        input,
                        output,
                        num(&o, "cache_read_input_token_cost"),
                        num(&o, "cache_creation_input_token_cost"),
                        num(&o, "cache_creation_input_token_cost_above_1hr"),
                    ),
                ))
            })
            .collect();
        Some(Pricing { prices })
    }

    /// Precio de un modelo tal como lo reporta el agente (`claude-opus-5-5`,
    /// `gemini-4-argon-high`, `grok-4.7`…).
    pub fn price(&self, model: &str) -> Option<&Price> {
        let model = model.trim().to_ascii_lowercase();
        let mut candidate = model.as_str().to_string();
        for _ in 0..3 {
            if let Some(p) = self.lookup(&candidate) {
                return Some(p);
            }
            // Quitar un sufijo: fecha (`-20251001`), nivel de razonamiento (`-high`)…
            match candidate.rsplit_once('-') {
                Some((head, _)) if !head.is_empty() => candidate = head.to_string(),
                _ => break,
            }
        }
        None
    }

    fn lookup(&self, name: &str) -> Option<&Price> {
        if let Some(p) = self.prices.get(name) {
            return Some(p);
        }
        ["anthropic/", "openai/", "gemini/", "xai/"]
            .iter()
            .find_map(|prefix| self.prices.get(&format!("{prefix}{name}")))
    }

    /// Costo en dólares de un evento, si se conoce el precio del modelo.
    pub fn cost(&self, e: &UsageEvent) -> Option<f64> {
        let p = self.price(&e.model)?;
        let write_5m = e.cache_write.saturating_sub(e.cache_write_1h);
        Some(
            e.input as f64 * p.input
                + e.output as f64 * p.output
                + e.cache_read as f64 * p.cache_read
                + write_5m as f64 * p.cache_write
                + e.cache_write_1h as f64 * p.cache_write_1h,
        )
    }
}

/// Completa los precios de caché que falten con las proporciones habituales.
fn complete(input: f64, output: f64, read: Option<f64>, write: Option<f64>, write_1h: Option<f64>) -> Price {
    let cache_write = write.unwrap_or(input * 1.25);
    Price {
        input,
        output,
        cache_read: read.unwrap_or(input * 0.1),
        cache_write,
        cache_write_1h: write_1h.unwrap_or(input * 2.0),
    }
}

fn cache_path() -> Option<PathBuf> {
    Some(dirs::cache_dir()?.join("kuaky").join("litellm-prices.json"))
}

/// Descarga la tabla de LiteLLM si la copia local tiene más de un día. Usa la
/// CLI `curl` (incluida en macOS y Windows 10+). Llamar fuera del hilo de la
/// interfaz. Devuelve `true` si descargó una versión nueva.
pub fn refresh_if_stale() -> bool {
    let Some(path) = cache_path() else { return false };
    let fresh = std::fs::metadata(&path)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| SystemTime::now().duration_since(t).ok())
        .is_some_and(|age| age < MAX_AGE);
    if fresh {
        return false;
    }
    let Some(dir) = path.parent() else { return false };
    if std::fs::create_dir_all(dir).is_err() {
        return false;
    }
    let tmp = path.with_extension("json.tmp");
    let ok = std::process::Command::new("curl")
        .args(["-sfL", "--max-time", "60", "-o"])
        .arg(&tmp)
        .arg(LITELLM_URL)
        .status()
        .is_ok_and(|s| s.success());
    let valid = ok
        && std::fs::read_to_string(&tmp)
            .ok()
            .and_then(|t| Pricing::from_litellm(&t))
            .is_some_and(|p| p.prices.len() > 100);
    if valid {
        std::fs::rename(&tmp, &path).is_ok()
    } else {
        let _ = std::fs::remove_file(&tmp);
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Agent, UsageEvent};
    use std::sync::Arc;

    fn ev(model: &str) -> UsageEvent {
        UsageEvent {
            agent: Agent::ClaudeCode,
            ts: "2026-11-20T10:00:00Z".parse().unwrap(),
            model: Arc::from(model),
            session: Arc::from("s"),
            project: None,
            input: 1_000_000,
            output: 1_000_000,
            cache_read: 1_000_000,
            cache_write: 2_000_000,
            cache_write_1h: 1_000_000,
        }
    }

    #[test]
    fn embedded_prices_cover_current_models() {
        let p = Pricing::embedded();
        assert!(p.price("claude-opus-5-5").is_some());
        assert!(p.price("claude-haiku-5-5-20261001").is_some(), "quita la fecha");
        assert!(p.price("gemini-4-argon-high").is_some(), "quita el nivel");
        assert!(p.price("gpt-6.2-astra").is_some());
        assert!(p.price("modelo-inventado-9").is_none());
    }

    #[test]
    fn computes_cost_with_both_cache_kinds() {
        let p = Pricing::embedded();
        // Opus 5.5: entrada $4, salida $20, lectura $0.20, escritura 5 m $5, 1 h $8 por millón.
        let cost = p.cost(&ev("claude-opus-5-5")).unwrap();
        assert!((cost - (4.0 + 20.0 + 0.2 + 5.0 + 8.0)).abs() < 1e-6, "{cost}");
        assert!(p.cost(&ev("desconocido")).is_none());
    }
}
