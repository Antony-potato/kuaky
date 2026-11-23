//! Logos de agentes y proveedores.
//!
//! SVG de LobeHub Icons (MIT, `@lobehub/icons-static-svg` 1.106.0); los
//! monocromáticos se recolorearon para el tema oscuro. Ver `THIRD_PARTY.md`.

use gpui_kit::*;
use kuaky_usage::{Agent, Provider};
use std::sync::{Arc, OnceLock};

macro_rules! icon {
    ($name:ident, $file:literal) => {
        fn $name() -> Arc<Image> {
            static CELL: OnceLock<Arc<Image>> = OnceLock::new();
            CELL.get_or_init(|| {
                Arc::new(Image::from_bytes(
                    ImageFormat::Svg,
                    include_bytes!(concat!("../assets/icons/", $file)).to_vec(),
                ))
            })
            .clone()
        }
    };
}

icon!(claude_code, "claudecode-color.svg");
icon!(codex, "codex-color.svg");
icon!(antigravity, "antigravity-color.svg");
icon!(gemini_cli, "geminicli-color.svg");
icon!(opencode, "opencode.svg");
icon!(anthropic, "anthropic.svg");
icon!(openai, "openai.svg");
icon!(gemini, "gemini-color.svg");
icon!(grok, "grok.svg");

pub fn agent(a: Agent) -> Arc<Image> {
    match a {
        Agent::ClaudeCode => claude_code(),
        Agent::Codex => codex(),
        Agent::Antigravity => antigravity(),
        Agent::GeminiCli => gemini_cli(),
        Agent::OpenCode => opencode(),
    }
}

pub fn provider(p: Provider) -> Option<Arc<Image>> {
    match p {
        Provider::Anthropic => Some(anthropic()),
        Provider::OpenAI => Some(openai()),
        Provider::Google => Some(gemini()),
        Provider::XAi => Some(grok()),
        Provider::Other => None,
    }
}

/// Logo cuadrado del tamaño dado.
pub fn logo(image: Arc<Image>, size: Pixels) -> Img {
    img(image).size(size).flex_none()
}
