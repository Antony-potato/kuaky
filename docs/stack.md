# Propuesta de stack: editor ligero centrado en agentes (Rust)

Fecha: 2026-11-23 · Complementa a [investigacion.md](investigacion.md) (seguimiento de uso).

## 1. Qué queremos construir

Una app de escritorio nativa (macOS + Windows), más ligera que Zed, que al abrir una carpeta muestre:

```
┌──────────────────────────────────────────────────────────────────────┐
│ kuaky · ~/proyectos/app        main ▾   ● claude  ● codex  ○ agy     │ ← barra: rama, sesiones activas
├───────────────┬──────────────────────────────────────┬───────────────┤
│ SESIONES      │  editor / diff / chat de agente       │ ARCHIVOS      │
│ ● claude #3   │  (pestañas)                          │ ▸ src         │
│   "fix auth"  │                                      │ ▸ docs        │
│ ● codex #1    │                                      │   Cargo.toml  │
│ ○ agy #2      ├──────────────────────────────────────┤───────────────┤
│               │  terminal (PTY): claude / codex / agy │ GIT           │
│ USO HOY       │  corriendo con TU sesión ya iniciada  │ ramas · diffs │
│ 402M tokens   │                                      │ PRs abiertos  │
└───────────────┴──────────────────────────────────────┴───────────────┘
```

- **Agentes (Claude Code, Codex, Antigravity)** usando las sesiones que ya tienes iniciadas (`claude`, `codex`, `agy`): nunca pedimos API keys.
- **Árbol de carpetas a la derecha**, editor con resaltado, **diffs por rama**, **PRs abiertos**, **actividad de sesiones** y el **dashboard de uso**.

---

## 2. Tres caminos para la interfaz

| | **A. GPUI + GPUI Kit** ⭐ | B. Tauri 2 + React + shadcn | C. Ratatui (TUI) |
|---|---|---|---|
| Qué es | La misma tecnología de UI que Zed (GPU: Metal en Mac, DirectX 11 en Windows), con una librería de componentes estilo shadcn | Backend Rust + interfaz web en un WebView | Interfaz dentro de tu terminal |
| Ligereza | Alta: un solo proceso nativo, sin motor web | Media: WebView (en Windows es WebView2 = Chromium) + JS | Máxima |
| Editor de código | Incluido (rope, Tree-sitter, LSP, 200K líneas) | Monaco o CodeMirror (pesados en JS) | Muy limitado |
| Terminal embebida | `alacritty_terminal` + `portable-pty` (lo que usa Zed) | xterm.js | Ya estás en una terminal |
| Diffs, árbol, tablas, gráficas | Incluidos (`Diff`, `Tree`, `DataTable`, `Chart`, `Dock`) | shadcn + librerías | Widgets propios |
| React Bits / shadcn MCP | **No aplica** (son componentes React) | Sí, encajan directo | No aplica |
| Riesgo | GPUI es pre-1.0 y su API cambia; hay que fijar versiones | Bajo, ecosistema enorme | Bajo, pero se queda corto para un editor |

### Recomendación: **A. GPUI + GPUI Kit**

- Es lo **más rápido y ligero con interfaz gráfica real**. No hay navegador dentro.
- **Zed no es pesado por GPUI.** Lo que consume RAM son los language servers (rust-analyzer solo puede pasar de 1 GB), las extensiones, la indexación y el colaborativo. Nosotros decidimos qué se carga: los LSP se encienden bajo demanda, sin extensiones y sin colaborativo.
- [GPUI Kit](https://gpui-kit.com/) (antes `gpui-component`, de Longbridge, Apache-2.0, v0.11) trae ya casi todo lo de la pantalla:
  - `Dock` (paneles arrastrables con estado guardado), `Tree`, `Editor`, `Diff`, `DataTable`, `Chart`
  - `Command` (paleta de comandos), `Sidebar`, `Tabs`, `Resizable`, `TextView` (markdown)
  - 38 temas
- Corre en macOS, Windows y Linux. Fija una versión publicada de GPUI (`gpui-pre =0.3.14`), así que no dependemos del repo de Zed.

**Lo que perdemos frente a Tauri:** React Bits y shadcn. GPUI Kit imita el estilo de shadcn, así que podemos traducir diseños, pero no copiar los componentes.

---

## 3. Stack propuesto (camino A)

### Interfaz
| Pieza | Crate / herramienta | Notas |
|---|---|---|
| Framework UI | `gpui` (vía `gpui-kit`) | GPU nativa; su propio executor async |
| Componentes | `gpui-kit` 0.11 | Dock, Tree, Editor, Diff, Tabla, Charts, Command, Temas |
| Iconos | Lucide (incluidos en GPUI Kit) | |

### Terminal embebida (para correr `claude`, `codex`, `agy` tal cual)
| Pieza | Crate | Notas |
|---|---|---|
| Emulador VT | `alacritty_terminal` | Apache-2.0, es el que usa Zed |
| PTY | `portable-pty` | MIT, de WezTerm; usa ConPTY en Windows |
| Referencia | [Termua](https://github.com/iamazy/termua) (GPUI + Alacritty/WezTerm) | Solo para leer: es **AGPL**, no copiar código |

### Agentes: dos modos
1. **Modo terminal (PTY)**: abre la CLI real en una pestaña. Funciona desde el día uno con cualquier agente y usa tu login.
2. **Modo chat estructurado (ACP)**: [Agent Client Protocol](https://agentclientprotocol.com), el estándar creado por Zed (JSON-RPC por stdin/stdout). Nos da mensajes, tool calls, permisos (permitir/denegar), diffs propuestos y sesiones que se pueden reanudar, para dibujarlos con nuestra propia UI.

| Agente | Cómo se conecta por ACP | Sesión |
|---|---|---|
| Antigravity | **Nativo:** `agy acp` | Reusa el login de `agy` (o `agy acp --login`) |
| Claude Code | Adaptador `claude-agent-acp` (de Zed, requiere Node) | Reusa el login de `claude` |
| Codex | Adaptador `codex-acp` (de Zed) | Reusa el login de `codex` |
| Gemini CLI | **Nativo:** `gemini --acp` | Reusa el login |

- Crate: `agent-client-protocol` (SDK oficial en Rust).
- Lanzar procesos: `async-process` (compatible con el executor de GPUI, sin necesidad de `tokio`).

### Git y PRs
| Pieza | Crate / herramienta | Para qué |
|---|---|---|
| Lectura rápida | `gix` (gitoxide) | Ramas, status, árbol, blobs. Rust puro, sin libgit2 |
| Diffs | `imara-diff` (lo usa gix) | Diff entre ramas y archivos para el componente `Diff` |
| Escritura | CLI `git` | Commit, checkout, worktrees: lo más confiable y usa tu configuración |
| PRs | CLI `gh` (`gh pr list --json …`) | Reusa tu login de GitHub, coherente con "usar mis sesiones". `octocrab` queda como opción futura |

### Archivos y proyecto
| Pieza | Crate |
|---|---|
| Árbol que respeta `.gitignore` | `ignore` |
| Vigilar cambios | `notify` |
| Texto del editor | `ropey` (o el rope de GPUI Kit) |
| Resaltado | `tree-sitter` + gramáticas por lenguaje (feature de GPUI Kit) |
| LSP (bajo demanda) | `lsp-types` + cliente de GPUI Kit |

### Seguimiento de uso (de la investigación anterior)
- Parsers por agente: `serde_json`, `rusqlite` (bundled), `rayon`, `jiff`.
- Caché en SQLite.
- Se muestra con `Chart` y `DataTable` de GPUI Kit en lugar de ratatui.

### Base
| Pieza | Crate / herramienta |
|---|---|
| Errores | `anyhow` |
| Config | `serde` + `toml` |
| Logs | `tracing` |
| Asignador de memoria | `mimalloc` |
| Empaquetado (.app/.dmg/.msi) | `cargo-packager` |
| Releases | `dist` + GitHub Actions (matriz macOS + Windows) |

---

## 4. Ideas a tomar de otros proyectos

| Proyecto | Idea |
|---|---|
| **Warp** (cliente abierto en 2026: UI `warpui` MIT, resto AGPL) | Salida de terminal en "bloques"; los agentes CLI viven dentro de la terminal; modo GUI + TUI sobre el mismo núcleo |
| **Zed** | ACP, paneles con Dock, terminal con alacritty, todo en GPUI |
| **Conductor / Emdash / Maestro** | Una sesión de agente por **git worktree**, revisar el diff antes de mergear, crear el PR desde la app |
| **splitrail / tokscale** | Dashboard de uso (ver investigacion.md) |

---

## 5. Fases sugeridas

1. **Ventana base**: GPUI Kit con Dock (sesiones a la izquierda, centro, árbol a la derecha, terminal abajo) y abrir carpeta.
2. **Terminal PTY**: pestañas que lanzan `claude`, `codex` y `agy` con tu sesión; lista de sesiones activas.
3. **Git**: rama actual, cambiar de rama, diff de archivos y entre ramas.
4. **PRs**: lista de PRs abiertos con `gh` y su diff.
5. **ACP**: chat estructurado con permisos y diffs propuestos, empezando por `agy acp`, que es nativo.
6. **Uso**: dashboard (KPIs, heatmap, modelos, agentes) leyendo los logs locales.
7. **Editor completo**: LSP bajo demanda, búsqueda, paleta de comandos.
8. **Worktrees por sesión** al estilo Conductor.

---

## 6. Por verificar antes de escribir código

- API exacta de GPUI Kit 0.11 y cómo integra un `Element` propio (para la terminal).
- Que el crate `agent-client-protocol` funcione con el executor de GPUI sin `tokio`.
- Consumo real de RAM: medir una ventana vacía de GPUI Kit en Mac y en Windows en build release.
- Licencias de los adaptadores `claude-agent-acp` y `codex-acp`.

## Fuentes

- GPUI Kit: https://gpui-kit.com/ · https://github.com/longbridge/gpui-kit
- Zed en Windows (DirectX 11): https://zed.dev/blog/zed-for-windows-is-here
- ACP: https://agentclientprotocol.com/overview/clients · https://zed.dev/acp · https://zed.dev/blog/codex-is-live-in-zed
- Antigravity ACP: https://antigravity.google/docs/cli/acp/
- Termua: https://github.com/iamazy/termua
- Warp: https://github.com/warpdotdev/warp · https://www.warp.dev/blog/why-is-building-a-ui-in-rust-so-hard
- Alternativas a Conductor: https://openalternative.co/alternatives/conductor · https://github.com/KarimUOB/Maestro
- gitoxide: https://github.com/Byron/gitoxide · octocrab: https://docs.rs/octocrab
