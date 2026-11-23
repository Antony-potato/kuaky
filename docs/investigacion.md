# Investigación: dashboard de uso de agentes en terminal (Rust)

Fecha: 2026-11-23 · Objetivo: herramienta de terminal en Rust, multiplataforma (macOS + Windows), cuya pantalla principal se parezca al screenshot de referencia ("Agent usage").

---

## 1. Anatomía de la pantalla de referencia

| Zona | Qué muestra | Cómo se construye en TUI |
|---|---|---|
| Encabezado | Título + rango de fechas, selector `7 days / 30 days / 90 days / All`, `esc` | `Tabs` o párrafo con estilo resaltado; teclas `1-4` / `←→` |
| 4 tarjetas KPI | Input tokens (11.6B, 98% de caché), Output tokens, Tasks/turns, Agent time ("up to 10 at once") + delta % vs periodo anterior + mini barras | `Block` con bordes redondeados, número grande (`tui-big-text` o texto en negrita), `Sparkline` abajo, badge verde con el delta |
| Tokens by day | Barras espejo: input hacia arriba (gris), output hacia abajo (azul), eje con fechas | Widget propio: `BarChart` no soporta valores negativos, se dibuja con caracteres de bloque (`▀▄█▁▂▃…`) o dos `BarChart` uno invertido |
| When they work | Heatmap día de la semana × hora (7×24), escala de grises | Widget propio: cada celda es un `Span` de 2 caracteres con color de fondo por intensidad |
| Models | Barra apilada por proveedor (Anthropic/Google/OpenAI) + tabla modelo → input/output/tasks | `Gauge`/barra propia con segmentos de color + `Table` con barra proporcional en una columna |
| Agents | Tabla agente → input/output/tiempo, "click one to see only its usage" | `Table` con selección (teclado y ratón) que filtra todo el dashboard |

Conclusión: todo se puede lograr con **ratatui**; tres piezas serán widgets propios (barras espejo, heatmap, barra apilada). Nada de esto requiere gráficos de imagen.

---

## 2. Proyectos open source de referencia

| Proyecto | Lenguaje / TUI | Agentes | Notas útiles | Licencia |
|---|---|---|---|---|
| [Piebald-AI/splitrail](https://github.com/Piebald-AI/splitrail) | Rust 2024, ratatui 0.30 + crossterm 0.29 | Claude Code, Codex, Gemini CLI, Copilot, OpenCode, Cline/Roo/Kilo, Grok, Qwen, Pi… | **El más parecido en arquitectura.** Watch de archivos con `notify`, SQLite (`rusqlite` bundled), `simd-json`, `rayon`, servidor MCP, dashboard web con heatmap día×hora. Builds para Windows y macOS. | MIT |
| [junhoyeo/tokscale](https://github.com/junhoyeo/tokscale) | Núcleo Rust + CLI/web en TypeScript (Bun) | ~60 agentes | **La mejor "tabla de rutas"** de dónde guarda datos cada agente. Precios desde LiteLLM con caché + fallback OpenRouter. 10 vistas TUI, gráfica estilo contribuciones de GitHub, temas. | MIT (verificar, el badge dice MIT + AGPL) |
| [tokenusage (`tu`)](https://docs.rs/crate/tokenusage/1.5.2) | Rust, ratatui + crossterm; GUI con iced | Claude Code, Codex, Antigravity | Lee Antigravity consultando el *language server* local del IDE (no hay archivos con tokens). Modo `live`, `top`, imágenes para compartir. | MIT |
| [ccusage](https://www.mintlify.com/ryoppippi/ccusage/family/overview) | TypeScript | Claude Code (+ familia Codex, OpenCode, Amp) | El origen del género; buena referencia de reglas de deduplicación y bloques de 5 h. | MIT |
| [vct-cli](https://docs.rs/crate/vct-cli/2.2.4) | Rust | Claude Code, Codex, Gemini, otros | TUI interactiva por defecto + salida tabla/JSON. | — |

**Recomendación:** empezar desde cero (para tener nuestro diseño y código limpio), usando **splitrail** como referencia de arquitectura y **tokscale** como referencia de rutas/formatos por agente. Ambos son MIT, así que podemos estudiar y adaptar código citándolos.

---

## 3. Stack recomendado

### Núcleo TUI
| Crate | Para qué | Por qué |
|---|---|---|
| `ratatui` 0.30 | Layout y widgets (`Block`, `Table`, `Sparkline`, `Tabs`, `Gauge`) | Estándar de facto; lo usan los 3 proyectos de referencia |
| `crossterm` 0.29 | Backend de terminal | Funciona nativo en Windows (Windows Terminal y conhost) y macOS, sin ncurses |
| `tui-big-text` | Números grandes en las tarjetas (opcional) | Alternativa: negrita + tamaño normal, que se ve más fiel al screenshot |

### Datos y parseo
| Crate | Para qué |
|---|---|
| `serde` + `serde_json` | Parseo de JSONL/JSON (más adelante `simd-json` si hace falta velocidad) |
| `rusqlite` (feature `bundled`) | Leer bases SQLite de agentes (OpenCode, Codex nuevo, Copilot) **y** nuestra propia caché. `bundled` evita depender del SQLite del sistema en Windows |
| `rayon` | Parsear miles de archivos en paralelo |
| `walkdir` o `ignore` | Recorrer directorios |
| `notify` 8 | Modo "live": refrescar cuando un agente escribe |
| `zstd` | Sesiones archivadas de Codex (`.jsonl.zst`) |
| `jiff` | Fechas y zonas horarias (incluye tzdb, funciona igual en Windows). Alternativa: `chrono` + `chrono-tz` |

### Infraestructura
| Crate / herramienta | Para qué |
|---|---|
| `clap` 4 (derive) | Subcomandos y flags (`kuaky`, `kuaky --days 7`, `kuaky json`) |
| `etcetera` o `dirs` | Rutas de home/config/cache correctas en cada SO |
| `anyhow` / `color-eyre` | Manejo de errores |
| `toml` | Archivo de configuración |
| `ureq` (rustls) | Descargar tabla de precios (LiteLLM) sin necesitar `tokio` |
| `insta` + `ratatui::backend::TestBackend` | Tests de snapshot de la pantalla |
| [`dist`](https://axodotdev.github.io/cargo-dist/book/install.html) (antes cargo-dist) | Releases en GitHub con binarios para macOS (x86_64/arm64) y Windows, instalador shell/PowerShell, tap de Homebrew, `cargo binstall` |

**Sin `tokio` al inicio:** el trabajo es leer archivos y pintar; con hilos + `crossbeam-channel` (o `std::sync::mpsc`) basta y el binario queda más pequeño. Se puede agregar después si hacemos servidor MCP/web.

---

## 4. Fuentes de datos por agente

| Agente | Ubicación | Formato | Estado |
|---|---|---|---|
| **Claude Code** | `~/.claude/projects/<proyecto>/<sesión>.jsonl` (respetar `CLAUDE_CONFIG_DIR`; en Windows `%USERPROFILE%\.claude`) | JSONL. Cada respuesta trae `message.model` y `message.usage` con `input_tokens`, `output_tokens`, `cache_read_input_tokens`, `cache_creation_input_tokens`, más `timestamp`, `sessionId`, `cwd`, `gitBranch`, `isSidechain` | **Verificado en esta Mac.** Ojo: un mismo mensaje puede aparecer en varias líneas, hay que deduplicar por `message.id` + `requestId` |
| **Codex CLI** | `~/.codex/sessions/YYYY/MM/DD/rollout-*.jsonl` y `~/.codex/archived_sessions/` (respetar `CODEX_HOME`) | JSONL; eventos `token_count` con `total_token_usage` (acumulado) y `last_token_usage` (por turno) | En esta Mac `~/.codex` solo tiene bases SQLite (`logs_2`, `state_5`), sin `sessions/`. Hay que revisar una sesión real antes de escribir el parser |
| **Gemini CLI** | `~/.gemini/tmp/<hash>/chats/*.json` (respetar `GEMINI_CLI_HOME`) | JSON por sesión; tokens en mensajes tipo `gemini` | Formato cambia entre versiones |
| **OpenCode** | `~/.local/share/opencode/opencode.db` (+ `storage/message/` legado) | SQLite | Estable |
| **Antigravity** | `~/.gemini/antigravity/` | Los archivos no tienen tokens; las herramientas existentes consultan el *language server* local del IDE mientras corre | **Difícil**, dejar para después |
| **Copilot CLI** | `~/.copilot/session-store.db`, `~/.copilot/otel/*.jsonl` | SQLite / JSONL | Opcional |
| **Grok CLI, Junie** | — | — | Sin documentación clara; investigar con datos reales |

### Métricas derivadas (las del screenshot)
- **Input tokens / % de caché**: `input + cache_read + cache_creation`; % caché = `cache_read / total_input`.
- **Tasks / turns**: task = sesión (o prompt del usuario), turn = respuesta del modelo. Definirlo igual para todos los agentes.
- **Agent time**: suma de intervalos activos por sesión, cortando huecos mayores a N minutos (p. ej. 5).
- **"Up to 10 at once"**: máximo de intervalos activos que se solapan (barrido de eventos ordenados por tiempo).
- **Delta %**: mismo cálculo sobre el periodo anterior de igual duración.
- **Heatmap**: conteo de tokens (o turnos) por `(día_semana, hora)` en hora local.

---

## 5. Consideraciones Windows + macOS

- **Teclas duplicadas en Windows:** crossterm envía `Press` y `Release`; filtrar `KeyEventKind::Press`.
- **Colores:** Windows Terminal soporta truecolor; el conhost antiguo no. Detectar `COLORTERM` y caer a paleta de 256 colores.
- **Caracteres de bloque** (`█▄▀▁…▇`): se ven bien en Windows Terminal, iTerm2, Ghostty y Terminal.app. Evitar emojis para los íconos de agentes (anchos inconsistentes); usar letras o símbolos de un carácter con color.
- **Rutas:** nunca concatenar con `/`; usar `PathBuf` y `etcetera`/`dirs`. Respetar las variables de entorno de cada agente.
- **SQLite en uso:** abrir siempre en solo lectura (`SQLITE_OPEN_READ_ONLY`); si el agente tiene la base bloqueada, copiar a temporal y leer la copia.
- **Fin de línea:** tolerar `\r\n` en JSONL.
- **CI:** GitHub Actions con matriz `macos-latest` + `windows-latest` desde el día uno.

---

## 6. Arquitectura propuesta

```
kuaky/                       (workspace de Cargo)
├── crates/
│   ├── core/       UsageEvent, agregaciones (por día, modelo, agente, heatmap, concurrencia)
│   ├── sources/    un módulo por agente que implementa el trait Source
│   ├── store/      caché incremental en SQLite (archivo + mtime + tamaño → eventos ya parseados)
│   ├── pricing/    precios LiteLLM descargados + tabla embebida de respaldo (opcional, fase 2)
│   └── tui/        app ratatui: estado, vistas, widgets propios (mirror_bars, heatmap, stacked_bar)
└── src/main.rs     CLI con clap
```

```rust
pub struct UsageEvent {
    pub agent: AgentKind,          // ClaudeCode, Codex, Gemini, OpenCode…
    pub model: String,             // "claude-opus-5-5"
    pub session_id: String,
    pub project: Option<String>,
    pub ts: jiff::Timestamp,
    pub input: u64,
    pub output: u64,
    pub cache_read: u64,
    pub cache_write: u64,
}

pub trait Source {
    fn kind(&self) -> AgentKind;
    fn discover(&self) -> Vec<PathBuf>;                          // archivos/bases a leer
    fn parse(&self, path: &Path) -> anyhow::Result<Vec<UsageEvent>>;
}
```

Flujo: `discover` → filtrar contra la caché (mtime/tamaño) → `parse` en paralelo con rayon → guardar en caché → agregar según rango y filtro → pintar. En modo live, `notify` dispara un re-parse solo del archivo que cambió.

---

## 7. Plan por fases

1. **Esqueleto**: workspace, `clap`, pantalla ratatui vacía con el layout de 4 zonas y selector de rango. CI macOS + Windows.
2. **Claude Code**: parser + deduplicación + tarjetas KPI y tabla de modelos con datos reales.
3. **Widgets propios**: barras espejo, heatmap, barra apilada por proveedor, filtro por agente.
4. **Caché incremental** en SQLite para que abrir sea instantáneo.
5. **Más agentes**: Codex, Gemini CLI, OpenCode.
6. **Live + distribución**: `notify`, releases con `dist`, Homebrew y PowerShell installer.
7. **Extras**: costos (LiteLLM), salida JSON, Antigravity, temas.

---

## 8. Decisiones abiertas

- Nombre del binario (¿`kuaky`?).
- ¿Mostrar costos en dólares además de tokens? (el screenshot solo muestra tokens).
- Qué agentes son prioridad después de Claude Code.
- ¿Ratón además de teclado? (crossterm lo soporta en ambos SO).

---

## Fuentes

- splitrail: https://github.com/Piebald-AI/splitrail · Cargo.toml: https://raw.githubusercontent.com/Piebald-AI/splitrail/main/Cargo.toml
- tokscale: https://github.com/junhoyeo/tokscale
- tokenusage: https://docs.rs/crate/tokenusage/1.5.2
- vct-cli: https://docs.rs/crate/vct-cli/2.2.4
- ccusage: https://www.mintlify.com/ryoppippi/ccusage/family/overview
- ratatui: https://docs.rs/crate/ratatui/latest · https://lib.rs/crates/ratatui-crossterm
- Gemini CLI sesiones: https://geminicli.com/docs/cli/session-management/
- Codex formato JSONL: https://codex.danielvaughan.com/2026/03/30/codex-cli-session-analytics-jsonl-rollout · https://lin-guanguo.github.io/llm-memory-research/agent-cli/codex-session-file-schema/
- Antigravity: https://raw.githubusercontent.com/getagentseal/codeburn/main/docs/providers/antigravity.md
- dist (cargo-dist): https://axodotdev.github.io/cargo-dist/book/install.html
