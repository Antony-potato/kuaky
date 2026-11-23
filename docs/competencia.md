# Qué stack usan los demás

Fecha: 2026-11-23 · Datos tomados de los README y manifiestos (`package.json`, `Cargo.toml`, `go.mod`) de cada repo. "?" = no verificado.

## 1. Orquestadores de agentes (lo más parecido a kuaky)

| Proyecto | Shell de la app | Interfaz | Terminal | Editor | Cómo corre agentes | Git / PRs | Licencia · ⭐ |
|---|---|---|---|---|---|---|---|
| **Paradev** (antes Superset) | Electron 44 | React 19, Vite, Tailwind 4, TanStack, tRPC | xterm.js 6 (WebGL) + node-pty con daemon propio | CodeMirror 6, Tiptap, `@pierre/diffs` | PTY **y ACP** (`claude-agent-acp`, `codex-acp`) para el modo chat | `simple-git`, worktree por workspace | MIT · 16.5k · solo macOS |
| **Emdash** | Electron 45 | React 19, Vite, Tailwind 4, Base UI, cmdk | xterm.js 6 + node-pty | Monaco | PTY (detecta las CLIs instaladas) + SDK de ACP | worktrees; Octokit (GitHub), GitLab, Linear | Apache-2.0 · 6.3k |
| **Orca** | Electron 43 | React 19, Tailwind 4, Radix, shadcn? | xterm.js (WebGL) + node-pty | Monaco, Tiptap | PTY + chat con Claude Agent SDK | worktrees | MIT · ? (verificar estrellas) |
| **Crystal** → Nimbalyst | Electron 37 | React 19, Tailwind 3, Radix | xterm.js 5 + node-pty | Monaco | PTY con `claude` | worktrees | MIT · 3.1k · *deprecado* |
| **Vibe Kanban** | **Tauri** + web | React 18, Tailwind 3, Radix | xterm.js 5 | CodeMirror 6 | ~30 crates Rust (axum, tokio); "executors" para 10+ agentes | **`git2`** (libgit2) | Apache-2.0 · 28.8k · *cerrando* |
| **Maestro** | **Tauri 2** | React 19, Tailwind 4, zustand, cmdk | xterm.js 6 | CodeMirror 6 | Claude Code, Codex (spawn de shell?) | git CLI?; PR/MR GitHub + GitLab | MIT · 0 |
| **Termic** | **Tauri 2** | React 19, Tailwind 4, Radix | xterm.js 5 (WebGL) + **`portable-pty`** (Rust) | CodeMirror 6 (`@codemirror/merge` para diffs) | PTY con las CLIs reales, sin SDKs; sandbox en macOS | git CLI? | AGPL-3.0 · 337 |
| **Claude Squad** | **TUI** (Go) | Bubble Tea + Lipgloss | tmux + `creack/pty` | — | PTY dentro de tmux | worktrees; `gh` para PRs | AGPL-3.0 · 8.9k |
| **Conductor** | ? (cerrado, solo Mac) | ? | ? | ? | Claude Code, Codex, Cursor, OpenCode con tu login | worktree + rama por tarea | Propietario |

## 2. Editores y terminales "grandes"

| Producto | Stack | Notas |
|---|---|---|
| **Zed** | Rust + **GPUI** (Metal / DirectX 11) | Terminal con `alacritty_terminal`, Tree-sitter, LSP, creó **ACP** |
| **Warp** | Rust + **WarpUI** propia (GPU) | Cliente abierto en 2026 (UI MIT, resto AGPL); corre Claude Code/Codex dentro de su terminal |
| **Lapce** | Rust + **Floem** (wgpu) | `alacritty_terminal`, `git2`, LSP en un proceso aparte, plugins WASI. Sin agentes |
| **Cursor, Windsurf, Antigravity** | Forks de **VS Code** (Electron + Monaco + xterm.js) | Antigravity viene de tecnología licenciada de Windsurf |
| **Codex app** (OpenAI) | **Electron** | OpenAI lo eligió para poder llegar a otros SO; hubo quejas por no ser nativo |
| **CodexMonitor** | Tauri | MIT |

## 3. Qué se repite

1. **Casi todos son Electron + React + Tailwind + xterm.js + node-pty.** Lo eligen por velocidad de desarrollo, no por ligereza. Cada uno trae un Chromium completo.
2. **Los que usan Rust se van a Tauri** (Vibe Kanban, Maestro, Termic): la lógica en Rust, pero la interfaz sigue siendo React en un WebView.
3. **GUI 100% nativa en Rust solo la tienen los editores grandes** (Zed, Warp, Lapce). **Ningún orquestador de agentes es nativo.** Ese hueco es nuestra diferencia.
4. **Correr agentes:** todos empiezan con **PTY** (la CLI real, con tu login). Los más nuevos (Paradev, Emdash, Orca, Zed) agregan **ACP o SDK** para tener un modo chat estructurado. Es la misma estrategia de dos modos que propusimos.
5. **Git:** el patrón universal es **un worktree por sesión de agente**. Casi todos llaman a la CLI `git` (o `simple-git`). Los de Rust usan `git2`. Para PRs: Octokit o `gh`.
6. **Editor:** Monaco (pesado) o CodeMirror 6 (más ligero). Los nativos tienen editor propio.
7. **Diffs:** componentes dedicados (`@pierre/diffs`, `react-diff-viewer`, `@codemirror/merge`, `@git-diff-view/react`).
8. **Plataformas:** muchos son solo macOS (Conductor, Codex app, Paradev). Hacerlo bien en **Windows** también nos diferencia.

## 4. Qué significa para kuaky

| Decisión | Lo que hacen los demás | Nuestra propuesta |
|---|---|---|
| Interfaz | Electron/Tauri + React | GPUI + GPUI Kit (nativo, como Zed) |
| Terminal | xterm.js + node-pty | `alacritty_terminal` + `portable-pty` (como Zed/Lapce/Termic) |
| Agentes | PTY + ACP/SDK | Igual: PTY primero, ACP después |
| Git | git CLI / `git2` + worktrees | `gix` para leer, `git` CLI para escribir, worktrees |
| PRs | Octokit / `gh` | `gh` (usa tu sesión) |
| Plataformas | Muchos solo Mac | Mac + Windows desde el inicio |

**Si en algún momento priorizamos velocidad de desarrollo sobre ligereza**, el camino probado es **Tauri 2 + React + Tailwind + xterm.js + CodeMirror 6** (Termic y Maestro son buenos ejemplos). Ahí sí entran shadcn y React Bits.

## Fuentes

- Emdash: https://github.com/generalaction/emdash
- Paradev/Superset: https://github.com/superset-sh/superset
- Orca: https://github.com/stablyai/orca
- Crystal: https://github.com/stravu/crystal
- Vibe Kanban: https://github.com/BloopAI/vibe-kanban
- Maestro: https://github.com/KarimUOB/Maestro
- Termic: https://github.com/simion/termic
- Claude Squad: https://github.com/smtg-ai/claude-squad
- Lapce: https://github.com/lapce/lapce
- Conductor: https://www.conductor.build/ · https://thenewstack.io/a-hands-on-review-of-conductor-an-ai-parallel-runner-app/
- Codex app (Electron): https://techcrunch.com/2026/02/02/openai-launches-new-macos-app-for-agentic-coding/ · https://www.devclass.com/development/2026/02/05/traditional-ides-not-the-right-tool-for-development-with-agentic-ai-openai-claims/4090132
- Antigravity / Cursor / Windsurf (forks de VS Code): https://visualstudiomagazine.com/Articles/2026/01/26/What-a-Difference-a-VS-Code-Fork-Makes-Antigravity-Cursor-and-Windsurf-Compared.aspx
- CodexMonitor: https://nimbalyst.com/blog/best-codex-gui-tools-and-desktop-apps-2026/
- libghostty (motor de terminal embebible, alternativa futura): https://mitchellh.com/writing/libghostty-is-coming
