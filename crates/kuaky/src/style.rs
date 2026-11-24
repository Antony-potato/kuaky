//! Paleta y formatos de kuaky.

use gpui_kit::{Hsla, Rgba, rgb, rgba};
use kuaky_usage::{Agent, Provider};

/// Fuente monoespaciada del sistema.
pub fn mono_font() -> &'static str {
    if cfg!(target_os = "macos") {
        "Menlo"
    } else if cfg!(windows) {
        "Consolas"
    } else {
        "monospace"
    }
}

pub fn bg() -> Rgba {
    rgb(0x0b0c0f)
}
pub fn panel() -> Rgba {
    rgb(0x13151a)
}
pub fn panel_hover() -> Rgba {
    rgb(0x1a1d24)
}
pub fn border() -> Rgba {
    rgb(0x22262e)
}
pub fn text() -> Rgba {
    rgb(0xe8eaee)
}
pub fn muted() -> Rgba {
    rgb(0x8a909c)
}
pub fn faint() -> Rgba {
    rgb(0x555b66)
}

/// Acento de kuaky: entrada de tokens.
pub fn accent() -> Rgba {
    rgb(0x5eead4)
}
/// Salida de tokens.
pub fn output() -> Rgba {
    rgb(0xa78bfa)
}
pub fn good() -> Rgba {
    rgb(0x4ade80)
}
pub fn bad() -> Rgba {
    rgb(0xf87171)
}

/// El acento con una opacidad entre 0 y 1 (heatmap, fondos suaves).
pub fn accent_alpha(alpha: f32) -> Hsla {
    let mut c: Hsla = accent().into();
    c.a = alpha.clamp(0.0, 1.0);
    c
}

pub fn soft(color: Rgba, alpha: f32) -> Hsla {
    let mut c: Hsla = color.into();
    c.a = alpha;
    c
}

pub fn provider_color(p: Provider) -> Rgba {
    match p {
        Provider::Anthropic => rgb(0xe07a5f),
        Provider::OpenAI => rgb(0x34d399),
        Provider::Google => rgb(0x60a5fa),
        Provider::XAi => rgb(0xc4b5fd),
        Provider::Other => rgb(0x6b7280),
    }
}

pub fn agent_color(a: Agent) -> Rgba {
    match a {
        Agent::ClaudeCode => rgb(0xe07a5f),
        Agent::Codex => rgb(0x34d399),
        Agent::GeminiCli => rgb(0x60a5fa),
        Agent::Antigravity => rgb(0xfbbf24),
        Agent::OpenCode => rgb(0xd1d5db),
        Agent::Grok => rgb(0xc4b5fd),
    }
}

/// Letra que identifica al agente en su insignia.
#[allow(dead_code)]
pub fn agent_glyph(a: Agent) -> &'static str {
    match a {
        Agent::ClaudeCode => "C",
        Agent::Codex => "X",
        Agent::GeminiCli => "G",
        Agent::Antigravity => "A",
        Agent::OpenCode => "O",
        Agent::Grok => "K",
    }
}

#[allow(dead_code)]
pub fn transparent() -> Rgba {
    rgba(0x00000000)
}

/// "claude-opus-5-5" → "Opus 5.5"; otros modelos se dejan como vienen.
pub fn model_name(model: &str) -> String {
    let Some(rest) = model.strip_prefix("claude-") else {
        return model.to_string();
    };
    let mut parts = rest.split('-');
    let Some(family) = parts.next() else {
        return model.to_string();
    };
    let version: Vec<&str> = parts.take_while(|p| p.len() <= 2 && p.chars().all(|c| c.is_ascii_digit())).collect();
    let mut name = String::new();
    let mut chars = family.chars();
    if let Some(first) = chars.next() {
        name.extend(first.to_uppercase());
        name.push_str(chars.as_str());
    }
    if !version.is_empty() {
        name.push(' ');
        name.push_str(&version.join("."));
    }
    name
}

/// Dólares: "$2,647", "$12.34", "<$0.01".
pub fn money(usd: f64) -> String {
    if usd <= 0.0 {
        "$0".into()
    } else if usd < 0.01 {
        "<$0.01".into()
    } else if usd < 100.0 {
        format!("${usd:.2}")
    } else {
        format!("${}", thousands(usd.round() as u64))
    }
}

/// 11 600 000 000 → "11.6B".
pub fn human(n: u64) -> String {
    let n = n as f64;
    let (v, suffix) = if n >= 1e9 {
        (n / 1e9, "B")
    } else if n >= 1e6 {
        (n / 1e6, "M")
    } else if n >= 1e3 {
        (n / 1e3, "K")
    } else {
        return format!("{}", n as u64);
    };
    if v >= 100.0 {
        format!("{v:.0}{suffix}")
    } else {
        format!("{v:.1}{suffix}")
    }
}

/// 1 234 → "1,234".
pub fn thousands(n: u64) -> String {
    let s = n.to_string();
    let mut out = String::new();
    for (i, ch) in s.chars().enumerate() {
        if i > 0 && (s.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(ch);
    }
    out
}

/// Segundos → "347 h", "23 min".
pub fn duration(secs: u64) -> String {
    if secs >= 3600 * 10 {
        format!("{} h", secs / 3600)
    } else if secs >= 3600 {
        format!("{:.1} h", secs as f64 / 3600.0)
    } else if secs >= 60 {
        format!("{} min", secs / 60)
    } else if secs > 0 {
        format!("{secs} s")
    } else {
        "—".into()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_numbers() {
        assert_eq!(human(11_600_000_000), "11.6B");
        assert_eq!(human(61_300_000), "61.3M");
        assert_eq!(human(447_000_000), "447M");
        assert_eq!(human(999), "999");
        assert_eq!(thousands(1110), "1,110");
        assert_eq!(thousands(100), "100");
        assert_eq!(duration(347 * 3600), "347 h");
        assert_eq!(duration(23 * 60), "23 min");
        assert_eq!(model_name("claude-opus-5-5"), "Opus 5.5");
        assert_eq!(money(2647.28), "$2,647");
        assert_eq!(money(12.345), "$12.35");
        assert_eq!(money(0.001), "<$0.01");
        assert_eq!(model_name("claude-haiku-5-5-20261001"), "Haiku 5.5");
        assert_eq!(model_name("gpt-6.2-astra"), "gpt-6.2-astra");
    }
}
