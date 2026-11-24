use jiff::Timestamp;
use std::sync::Arc;

/// Agente (harness) que generó el consumo.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Agent {
    ClaudeCode,
    Codex,
    GeminiCli,
    Antigravity,
    OpenCode,
    Grok,
}

impl Agent {
    pub const ALL: [Agent; 6] = [
        Agent::ClaudeCode,
        Agent::Codex,
        Agent::GeminiCli,
        Agent::Antigravity,
        Agent::OpenCode,
        Agent::Grok,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Agent::ClaudeCode => "Claude Code",
            Agent::Codex => "Codex",
            Agent::GeminiCli => "Gemini CLI",
            Agent::Antigravity => "Antigravity",
            Agent::OpenCode => "OpenCode",
            Agent::Grok => "Grok CLI",
        }
    }
}

/// Empresa detrás del modelo, deducida de su nombre.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Provider {
    Anthropic,
    OpenAI,
    Google,
    XAi,
    Other,
}

impl Provider {
    pub fn from_model(model: &str) -> Provider {
        let m = model.to_ascii_lowercase();
        if ["claude", "opus", "sonnet", "haiku", "fable"]
            .iter()
            .any(|k| m.contains(k))
        {
            Provider::Anthropic
        } else if m.contains("gemini") || m.contains("gemma") {
            Provider::Google
        } else if m.starts_with("gpt") || m.starts_with("o3") || m.starts_with("o4") || m.contains("codex") {
            Provider::OpenAI
        } else if m.contains("grok") {
            Provider::XAi
        } else {
            Provider::Other
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Provider::Anthropic => "Anthropic",
            Provider::OpenAI => "OpenAI",
            Provider::Google => "Google",
            Provider::XAi => "xAI",
            Provider::Other => "Otros",
        }
    }
}

/// Una respuesta del modelo con su consumo.
#[derive(Debug, Clone)]
pub struct UsageEvent {
    pub agent: Agent,
    pub ts: Timestamp,
    /// Nombre del modelo tal como lo reporta el agente.
    pub model: Arc<str>,
    pub session: Arc<str>,
    /// Carpeta donde corrió el agente, si se conoce.
    pub project: Option<Arc<str>>,
    /// Tokens de entrada que no vinieron de caché.
    pub input: u64,
    pub output: u64,
    pub cache_read: u64,
    pub cache_write: u64,
    /// Parte de `cache_write` que fue caché de 1 hora (más cara).
    pub cache_write_1h: u64,
}

impl UsageEvent {
    /// Toda la entrada: nueva + leída de caché + escrita en caché.
    pub fn input_total(&self) -> u64 {
        self.input + self.cache_read + self.cache_write
    }

    pub fn provider(&self) -> Provider {
        Provider::from_model(&self.model)
    }
}
