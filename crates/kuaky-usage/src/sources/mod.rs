//! Lectores de logs, uno por agente.

mod claude;
mod codex;
mod gemini;

use crate::{Agent, UsageEvent};
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::Arc,
};

pub use claude::ClaudeCode;
pub use codex::Codex;
pub use gemini::GeminiCli;

pub trait Source: Send + Sync {
    fn agent(&self) -> Agent;
    fn load(&self) -> anyhow::Result<Vec<UsageEvent>>;
}

pub fn all() -> Vec<Box<dyn Source>> {
    vec![
        Box::new(ClaudeCode::from_env()),
        Box::new(Codex::from_env()),
        Box::new(GeminiCli::from_env()),
    ]
}

/// Raíces de configuración: la variable de entorno (separada por comas) o las
/// rutas por defecto relativas a `$HOME`.
pub(crate) fn roots(env_var: &str, defaults: &[&str]) -> Vec<PathBuf> {
    if let Ok(value) = std::env::var(env_var) {
        let list: Vec<PathBuf> = value
            .split(',')
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(PathBuf::from)
            .collect();
        if !list.is_empty() {
            return list;
        }
    }
    let Some(home) = dirs::home_dir() else {
        return Vec::new();
    };
    defaults.iter().map(|d| home.join(d)).collect()
}

/// Archivos con la extensión dada bajo `dir`, sin seguir enlaces.
pub(crate) fn files_with_ext(dir: &Path, ext: &str) -> Vec<PathBuf> {
    if !dir.is_dir() {
        return Vec::new();
    }
    walkdir::WalkDir::new(dir)
        .into_iter()
        .filter_map(Result::ok)
        .filter(|e| e.file_type().is_file())
        .filter(|e| e.path().extension().is_some_and(|x| x == ext))
        .map(|e| e.into_path())
        .collect()
}

/// Reutiliza un mismo `Arc<str>` para cadenas repetidas (modelos, sesiones).
#[derive(Default)]
pub(crate) struct Interner(HashMap<String, Arc<str>>);

impl Interner {
    pub fn get(&mut self, s: &str) -> Arc<str> {
        if let Some(a) = self.0.get(s) {
            return a.clone();
        }
        let a: Arc<str> = Arc::from(s);
        self.0.insert(s.to_owned(), a.clone());
        a
    }
}
