# Plan de kuaky

Fecha: 2026-11-23 · Decisión tomada: **GPUI + GPUI Kit** (ver [stack.md](stack.md) y [competencia.md](competencia.md)).

kuaky es un editor de escritorio ligero (macOS + Windows) centrado en agentes. Al abrir una carpeta muestra:
- sesiones de Claude Code, Codex y Antigravity usando tu login,
- árbol de archivos a la derecha,
- diffs entre ramas, PRs abiertos, actividad de sesiones y consumo de tokens.

---

## Estado (2026-11-23)

| Fase | Estado | Notas |
|---|---|---|
| 0. Fundación | ✅ | Workspace, CI macOS + Windows, repo privado en GitHub |
| 6. Dashboard de consumo (inicio) | ✅ | Claude Code, Codex y Gemini CLI; KPIs, días, heatmap, modelos, agentes, proyectos recientes |
| 1. Proyecto y layout | ✅ | Sesiones (izq.), pestañas (centro), archivos / cambios / ramas / PRs (der.) |
| 2. Terminal | ✅ | `kuaky-term`: alacritty_terminal + shell de login; selección, copiar/pegar, scroll |
| 3. Sesiones | ✅ | Lanzadores por agente, actividad por proyecto. Claude Code: estado exacto con hooks (`--settings`, sin tocar tus ajustes) y avisos cuando termina o pide permiso. Codex/Gemini: estado aproximado |
| 4. Git | ✅ | Cambios locales con diff, diff entre ramas, cambiar de rama |
| 5. PRs | ✅ | Lista con `gh` y diff de cada PR |
| 8. Editor | 🟡 | Editor de GPUI Kit con resaltado (syntect) y guardado ⌘S. Falta: LSP, búsqueda |
| 7. Chat ACP | ✅ | `kuaky-acp`: cliente ACP propio (JSON-RPC por stdio, sin runtime async), probado contra un agente falso. Chat con Markdown, pensamiento, herramientas, permisos con botones, plan y diffs de cada edición. Claude Code vía `claude-agent-acp` (el que instaló Zed); también Antigravity (`agy acp`), Gemini (`--acp`), Codex (`codex-acp`) y OpenCode (`acp`) si están instalados |
| 9. Worktrees y distribución | 🟡 | Worktree por sesión con cambios / terminal / crear PR / fusionar / eliminar; `kuaky.app` con ícono (`script/bundle-macos.sh`). Falta: instalador de Windows, auto-actualización |
| Uso diario | ✅ | Menús nativos, paneles redimensionables, ⌘P, estado persistente y sesiones para reanudar |

Capturas de desarrollo: `cargo run -p kuaky --features screenshot` con `KUAKY_SCREENSHOT=archivo.png` (más `KUAKY_DEMO_TERMINAL`, `KUAKY_DEMO_PANEL`, `KUAKY_DEMO_FILE`, `KUAKY_DEMO_DIFF`).

---

## 1. Hallazgo clave: ya existen apps de agentes hechas con GPUI

En la lista oficial [awesome-gpui](https://github.com/zed-industries/awesome-gpui) hay más de 20 apps de agentes nativas. Varias tienen **licencia MIT o Apache**, así que podemos copiar y adaptar su código citando el origen.

| Proyecto | Licencia | Tamaño | Qué tiene | Stack |
|---|---|---|---|---|
| [**Arbor**](https://github.com/penso/arbor) | MIT | 81k líneas · 849⭐ | Worktrees, terminales, diffs con árbol, tarjetas de PR, chat ACP, uso de tokens, daemon y MCP | gpui 0.2.2 (sin GPUI Kit), alacritty_terminal, portable-pty, git2 + gix |
| [**Krad ADE**](https://github.com/joordih/krad-ade) | MIT o Apache-2.0 | 278k líneas · alpha | Chat por protocolo de cada agente (Claude stream-json, Codex app-server, opencode), terminal, worktrees, PR/MR, barra de uso por plan, LSP | **gpui-kit 0.7**, libghostty-vt, portable-pty, git CLI |
| [**OxiMux**](https://github.com/nhtera/OxiMux) | Apache-2.0 | 542k líneas · 50⭐ | Pestañas de Claude/Codex/Pi, adaptadores ACP, hooks de estado, worktrees, grafo de commits, `gh pr create` | gpui-kit (fork), alacritty_terminal, portable-pty, git CLI |
| [**TokenMonitor**](https://github.com/iFence/TokenMonitor) | MIT | pequeño | **Solo uso de tokens**: Claude, Codex, Gemini, Antigravity, OpenCode…; heatmap, gráficas, costos | GPUI + gpui-component |
| [gpui_xterm](https://github.com/Modolet/gpui_xterm) | MIT | pequeño | Componente de terminal para **gpui-kit**: selección, scroll, portapapeles, temas | gpui-kit 0.9, alacritty_terminal 0.25 |
| [gpui-terminal](https://github.com/zortax/gpui-terminal) | MIT/Apache | pequeño | Terminal para GPUI (sin selección ni scrollback todavía) | gpui 0.2.2, alacritty_terminal |
| GPUI Kit `examples/tig` | Apache-2.0 | 1.6k líneas | Visor de historial git con el componente `Diff` (unified/split, navegación por cambios) | gpui-kit 0.11 |
| GPUI Kit `examples/dock`, `examples/editor` | Apache-2.0 | 600 y 1.2k líneas | Layout con paneles; editor con resaltado | gpui-kit 0.11 |

**No tocar** (licencias GPL/AGPL que nos obligarían a liberar todo kuaky con la misma licencia):
- Crates de Zed `terminal`, `terminal_view`, `git`, `agent_servers`, `acp_thread`: GPL-3.0. Solo `gpui` es Apache.
- GitComet, Vibex, Termua, Codux, Termic, Claude Squad: AGPL o GPL.

Se pueden **leer** para entender ideas, pero no copiar código.

### ¿Fork o desde cero?

**Recomiendo desde cero con GPUI Kit 0.11, trayendo piezas concretas.** Razones:
- Arbor usa `gpui` 0.2.2 sin GPUI Kit, así que perderíamos Dock, Tree, Diff, Editor y temas.
- Krad y OxiMux son enormes (278k y 542k líneas) y arrastran funciones que no queremos (móvil, SSH, simulador, extensiones).
- Copiar crates pequeños y autocontenidos nos deja un código más limpio y más ligero.

Antes de empezar conviene **instalar y usar Arbor y Krad unos minutos** para ver qué te gusta y qué no.

---

## 2. Qué tomamos de dónde

| Módulo de kuaky | Base que adaptamos | Licencia | Notas |
|---|---|---|---|
| Ventana, layout, temas | GPUI Kit `examples/dock` | Apache | Dock con paneles izquierda/centro/derecha/abajo |
| Árbol de archivos | Componente `Tree` de GPUI Kit + crates `ignore` y `notify` | Apache/MIT | Respeta `.gitignore` y se actualiza solo |
| Emulador de terminal | Arbor `arbor-terminal-emulator` (2.4k líneas, solo alacritty + thiserror) | MIT | Rust puro; compila en Windows sin Zig |
| Vista de terminal | `gpui_xterm` (selección, scroll, portapapeles) | MIT | Copiamos el código, porque depende de gpui-kit 0.9 y nosotros vamos con 0.11 |
| PTY | Krad `krad-pty` (665 líneas, portable-pty) | MIT/Apache | ConPTY en Windows |
| Entorno del shell | OxiMux `shell-env` | Apache | **Importante en macOS**: una app lanzada desde el Dock no hereda tu `PATH`, así que hay que leer el entorno del shell de login para encontrar `claude`, `codex` y `agy` |
| Estado de las sesiones | OxiMux `agent-hooks` (dialectos de hooks de Claude/Codex/Pi) | Apache | Saber si un agente está trabajando, esperando permiso o en reposo |
| Chat estructurado | Crate oficial `agent-client-protocol` 5.5 + Krad `krad-chat` (acp, codex, opencode, usage) como referencia | Apache · MIT/Apache | El SDK no depende de tokio (usa futures y async-process), así que encaja con GPUI |
| Git (lectura) | `gix`: status, diff, blame, listar y crear worktrees | MIT/Apache | |
| Git (escritura) | CLI `git`, con Krad `krad-repo` (status, worktree, patch, history) como referencia | MIT/Apache | gix todavía no hace checkout, merge ni rebase |
| Vista de diffs e historial | GPUI Kit `Diff` + `examples/tig` | Apache | Diff por archivo, entre ramas y por commit |
| PRs | CLI `gh` (`--json`), con Krad `krad-github` como referencia | MIT/Apache | Usa tu sesión de GitHub |
| Consumo de tokens | Analizadores de splitrail (`claude_code.rs`, `codex_cli.rs`, `gemini_cli.rs`, `antigravity.rs`, `opencode.rs`) + TokenMonitor para las vistas | MIT | Precios desde el JSON de LiteLLM (MIT) |
| Editor | GPUI Kit `Editor` + `examples/editor` | Apache | Tree-sitter ya incluido; LSP bajo demanda |
| Búsqueda difusa | `nucleo` (de Helix) | MPL-2.0 | Solo la usamos como dependencia; no copiamos su código |

**Regla:** todo archivo copiado o adaptado lleva un encabezado con el origen, la licencia y el commit. Además mantenemos un archivo `THIRD_PARTY.md`.

---

## 3. Estructura del repositorio

```
kuaky/
├── Cargo.toml              workspace; versiones fijadas (gpui-kit =0.11.0 → gpui-pre =0.3.14)
├── THIRD_PARTY.md          código adaptado y sus licencias
├── crates/
│   ├── kuaky/              binario: arranque, ventana, menú, atajos
│   ├── kuaky-core/         tipos de dominio, config (TOML), proyectos recientes
│   ├── kuaky-ui/           vistas: sesiones, árbol, git, PRs, dashboard, settings
│   ├── kuaky-term/         emulador (alacritty) + TerminalView para GPUI
│   ├── kuaky-pty/          portable-pty + entorno del shell de login
│   ├── kuaky-agents/       registro de harnesses, detección, lanzamiento, hooks, cliente ACP
│   ├── kuaky-git/          gix (lectura) + git CLI (escritura), worktrees
│   ├── kuaky-github/       wrapper de `gh`
│   └── kuaky-usage/        analizadores por agente + caché SQLite + precios
└── docs/
```

Regla de dependencias: solo `kuaky-ui` y `kuaky` dependen de GPUI. El resto es Rust puro y se puede probar sin abrir ventanas.

---

## 4. Fases

Cada fase termina con algo usable y probado en **macOS y Windows**.

| Fase | Entregable | Listo cuando… |
|---|---|---|
| **0. Fundación** | Workspace, CI (GitHub Actions macOS + Windows), ventana GPUI Kit con tema oscuro | La ventana abre en ambos SO y medimos la RAM base en release |
| **1. Proyecto y layout** | Abrir carpeta; Dock con sesiones (izq.), pestañas (centro), árbol de archivos (der.), panel inferior; recordar layout y proyectos recientes | Navegas el árbol de un repo grande sin lag y se actualiza al crear o borrar archivos |
| **2. Terminal** | `kuaky-term` + `kuaky-pty`; pestañas de terminal con tu shell | Corres `claude`, `codex` y `agy` dentro de kuaky con tu login, con colores, selección y scroll |
| **3. Sesiones** | Detección de CLIs instaladas, botón "nueva sesión", lista con estado (trabajando / esperando / inactivo) vía hooks | Ves de un vistazo qué agente necesita tu atención |
| **4. Git** | Rama actual y cambio de rama, cambios sin commit con diff, diff entre ramas, historial | Revisas lo que hizo un agente sin salir de kuaky |
| **5. PRs** | Lista de PRs abiertos, detalle, diff y estado de checks vía `gh` | Ves y revisas PRs del repo abierto |
| **6. Consumo** | Analizadores Claude/Codex/Gemini/Antigravity, caché SQLite, dashboard (KPIs, barras por día, heatmap, modelos, agentes) | El dashboard se parece al screenshot de referencia con tus datos reales |
| **7. Chat ACP** | Modo chat con `agy acp`, `claude-agent-acp`, `codex-acp`: mensajes, tool calls, permisos, diffs propuestos | Puedes trabajar con un agente sin su TUI |
| **8. Editor** | Abrir y editar archivos con resaltado, buscar en proyecto, paleta de comandos; LSP opcional | Haces ediciones rápidas sin abrir otro editor |
| **9. Worktrees y distribución** | Una sesión = un worktree; `.app`/`.dmg` y `.msi`, auto-actualización | Instalas kuaky como cualquier app |

**Orden acordado (2026-11-23):** 0 → **6 (dashboard de consumo como ventana de inicio)** → 1 → 2 → 3 → 4 → 5 → 7 → 8 → 9.

### Ventana de inicio: "Agent usage"

Es lo primero que ves al abrir kuaky. Está inspirada en el screenshot de referencia: **mismo espíritu, distinto diseño**.

| Zona | Contenido | Diferencias con la referencia |
|---|---|---|
| Encabezado | Título, rango de fechas, selector 7d / 30d / 90d / Todo | Agregamos "Abrir carpeta" y proyectos recientes en la misma barra |
| 4 tarjetas KPI | Tokens de entrada (% desde caché), tokens de salida (por turno), sesiones y turnos, tiempo de agente (máximo en paralelo) + delta vs periodo anterior + mini barras | Acento de color propio y delta en pastilla |
| Tokens por día | Barras espejo: entrada arriba, salida abajo | Colores de kuaky; tooltip con el día |
| Cuándo trabajan | Heatmap día × hora | Escala en el color de acento, no en grises |
| Modelos | Barra apilada por proveedor + tabla modelo → entrada, salida, turnos | |
| Agentes | Tabla agente → entrada, salida, tiempo; clic para filtrar todo el dashboard | |
| Proyectos recientes | Lista de carpetas con su actividad | **Nuevo:** abre la vista de proyecto |

Datos: `kuaky-usage` lee los logs locales de Claude Code, Codex, Gemini CLI y OpenCode (Antigravity, Grok y Junie más adelante) y agrega en memoria. La caché SQLite queda para cuando la lectura tarde.

---

## 5. Riesgos y cómo los cubrimos

| Riesgo | Mitigación |
|---|---|
| GPUI y GPUI Kit cambian su API (pre-1.0) | Fijar versiones exactas (`=0.11.0`) y actualizar a propósito, una vez por mes como máximo |
| Código adaptado de varias fuentes con estilos distintos | Adaptarlo a nuestras interfaces y no pegarlo tal cual; tests en los crates sin GPUI |
| `PATH` incompleto en apps de escritorio en macOS | Leer el entorno del shell de login desde la fase 2 |
| ConPTY en Windows (redimensionado, colores) | CI y pruebas manuales en Windows en cada fase |
| `claude-agent-acp` requiere Node o un binario aparte; `codex-acp` cambió de repo | El modo terminal (PTY) funciona desde el día uno; ACP es la fase 7 |
| libghostty-vt sería mejor emulador, pero requiere Zig para compilar | Empezar con alacritty_terminal; Arbor ya tiene un adaptador experimental de ghostty para migrar después |
| Mezclar código GPL o AGPL por accidente | Lista de "no tocar" arriba + `cargo-deny` en CI para revisar licencias |

---

## 6. Pendiente

- **Medir RAM real de GPUI Kit.** Cloné GPUI Kit, Arbor, Krad y OxiMux en una carpeta temporal para revisarlos, pero compilarlos se bloqueó porque ejecuta código de terceros sin tu permiso. Puedes correr la galería tú mismo (comandos abajo) o autorizarme.
- Probar Arbor y Krad ADE como usuario para decidir detalles de diseño.
- Confirmar el nombre `kuaky` y el ícono.

```bash
git clone --depth 1 https://github.com/longbridge/gpui-kit.git ~/Desktop/proyectos/personal/gpui-kit
```

```bash
cd ~/Desktop/proyectos/personal/gpui-kit && cargo run --release
```

```bash
cd ~/Desktop/proyectos/personal/gpui-kit && cargo run --release -p example-tig -- .
```

## Fuentes

- awesome-gpui: https://github.com/zed-industries/awesome-gpui
- GPUI Kit: https://github.com/longbridge/gpui-kit · https://gpui-kit.com/
- Arbor: https://github.com/penso/arbor · Krad ADE: https://github.com/joordih/krad-ade · OxiMux: https://github.com/nhtera/OxiMux
- TokenMonitor: https://github.com/iFence/TokenMonitor · splitrail: https://github.com/Piebald-AI/splitrail
- gpui_xterm: https://github.com/Modolet/gpui_xterm · gpui-terminal: https://github.com/zortax/gpui-terminal
- ACP Rust SDK: https://github.com/agentclientprotocol/rust-sdk · claude-agent-acp: https://github.com/agentclientprotocol/claude-agent-acp · codex-acp: https://github.com/agentclientprotocol/codex-acp
- gitoxide (estado): https://github.com/GitoxideLabs/gitoxide/blob/main/crate-status.md · gitui: https://github.com/gitui-org/gitui
- Licencias de crates de Zed: `crates/*/Cargo.toml` en https://github.com/zed-industries/zed
- LiteLLM precios: https://github.com/BerriAI/litellm
