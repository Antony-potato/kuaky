# Plan: interfaz, sesiones, uso restante y Git

Siguiente bloque después del 5 (búsqueda + LSP). Se empieza cuando el bloque 5
esté en commit, porque casi todo toca `crates/kuaky/src/project.rs`.

Cada parte lleva su propio commit y una captura de comprobación.

## A. Errores que estorban

1. **Atajos que se roba kuaky (Ctrl+O, Ctrl+P, Ctrl+Tab…).**
   - Causa: `ctrl-o` está asignado a "Abrir carpeta" para toda la app
     (`app.rs`), y `ctrl-p` / `ctrl-tab` en el contexto `Project`. La terminal
     nunca los recibe (`ctrl-p` es el historial de la shell; el agente pide
     `ctrl+o` para expandir).
   - Arreglo: en Mac solo atajos con `⌘`. En Windows, los atajos con `ctrl` no
     aplican cuando la terminal tiene el foco. Con la terminal enfocada, todas
     las teclas van al agente.
2. **La terminal brinca al mover paneles.**
   - Causa: `TerminalView::sync_size` redimensiona el PTY en cada cuadro del
     arrastre; el agente (TUI) se redibuja decenas de veces.
   - Arreglo: aplicar el tamaño ~120 ms después de soltar, mantener el scroll
     pegado abajo y no redimensionar pestañas ocultas (tamaño 0).
3. **Textos encimados en la barra lateral.** "olvidar" encima del título,
   "Claude Code" partido en dos líneas, "+" cortado. Recortar con "…" y dar un
   ancho mínimo a la barra.

## B. Nueva distribución de la ventana

4. **Izquierda: solo sesiones abiertas.**
   - Arriba, botón **New session +** que abre un menú con los harness
     instalados (Claude Code, Antigravity, Terminal…).
   - En ese menú se elige dónde trabaja el agente: **This folder** o **New
     branch (separate copy)**, con tooltip que lo explique. Reemplaza el
     interruptor "Aislar en worktree".
   - Se quitan los botones "chat" y "+" sueltos. El chat por ACP se queda en el
     código pero oculto, hasta tener nuestra propia interfaz de agentes (estilo
     Zed).
   - No se muestran agentes que no estén instalados.
   - Sesiones anteriores (reanudar): sección plegable o dentro del mismo menú.
5. **Actividad compacta en una barra inferior** (estilo VS Code/Zed).
   - Ejemplo: `Opus 5.5 · 381 resp · 176M tok · 9 min ago`.
   - Clic → panel con el detalle. Sale de la barra izquierda.
6. **Panel derecho en dos zonas.**
   - Arriba: botones con íconos para Files, Changes, Branches, PRs y Graph.
     Cada sección se puede ocultar; pulsar de nuevo su botón la cierra.
   - Abajo: botones de terminal para abrir una o varias terminales ahí.
   - Divisor ajustable entre ambas zonas.
7. **Configuración (`⌘,`).** Agentes visibles, ruta del programa de cada uno,
   argumentos extra, carpeta de worktrees. Se guarda en
   `<config>/kuaky/state.json` (o un `settings.json` aparte).
8. **Pulido visual.**
   - Inicio con ícono de casita en la barra de título (tooltip con `⌘⇧H`).
   - Toda la interfaz en inglés.
   - Íconos Lucide de `gpui-kit-assets` (licencia ISC, agregar a
     `THIRD_PARTY.md`): `house`, `folder`, `git-compare`, `git-branch`,
     `git-pull-request`, `git-graph`, `git-merge`, `git-merge-conflict`,
     `terminal`, `search`, `settings`, `plus`, `x`.

## C. Uso restante de cada agente

| Agente | De dónde sale | Estado |
|---|---|---|
| Claude Code | La barra de estado recibe `rate_limits.five_hour` y `rate_limits.seven_day` (`used_percentage`, `resets_at`) | Documentado oficialmente; una versión los omitía (issue #45133), comprobar con la versión instalada |
| Codex | Eventos `token_count` de los archivos de sesión traen `rate_limits.primary` / `secondary` (`used_percent`, `window_minutes`, reinicio) | Formato no oficial; Codex no está instalado para comprobarlo |
| Antigravity / Gemini | Por investigar (posiblemente el servidor local de Antigravity) | Sin confirmar |

- **Claude Code:** kuaky ya le pasa su propia configuración con `--settings`
  para los hooks. Ahí se añade un script de barra de estado que guarda el JSON
  en un archivo de kuaky y luego llama a la barra de estado original del
  usuario, para no quitársela.
- **Dónde se ve:**
  - Barra inferior: `Claude 5h 42% · week 18% · resets 3:40 pm`.
  - Dashboard de Inicio: tarjeta por agente con barras de lo que queda.

Fuentes:
- https://code.claude.com/docs/en/statusline
- https://github.com/anthropics/claude-code/issues/45133
- https://lin-guanguo.github.io/llm-memory-research/agent-cli/codex-session-file-schema/
- https://docs.rs/crate/codexusage/0.4.0/source/src/app/codex_limits.rs

## D. Sesiones con nombre

9. **Título automático** con la tarea actual del agente:
   - Del título que el agente pone en la terminal (`TermEvent::Title`, ya llega
     pero no se usa).
   - Si no hay, del primer prompt (hook `UserPromptSubmit` de Claude Code).
   - Debajo: estado (trabajando, esperando permiso, terminado) y hace cuánto.
10. **Renombrar** con doble clic o clic derecho: nombre propio y color. Se ve en
    la pestaña, se guarda y se conserva al reanudar.

## E. Git

11. **Pestaña Graph** (como VS Code): carriles de colores por rama, puntos por
    commit, etiquetas de rama y HEAD, lista virtualizada. Clic en un commit →
    su diff. Se calcula con `git log --format=%H %P …` y asignación propia de
    carriles.
12. **Merge o descartar.**
    - Revisión previa sin tocar archivos con `git merge-tree --write-tree`
      (git ≥ 2.38): "Clean ✓" o lista de archivos en conflicto.
    - Opciones: Merge, Squash, Discard (borrar rama y worktree).
    - PRs: `gh pr merge` con el método elegido.
13. **Conflictos.**
    - Vista por archivo con "ours" / "theirs" y botones Accept ours / theirs /
      both por bloque.
    - **Resolve with agent:** lanza el agente elegido con la lista de archivos
      en conflicto, para resolverlos y correr los tests sin hacer commit.
      Al terminar (hook `Stop`), se muestra el diff para aprobarlo y el botón
      **Complete merge**.
    - Solo se lanza cuando el usuario da clic, porque gasta tokens.

## Orden

1. A (errores).
2. B (distribución, inglés e íconos).
3. C (uso restante).
4. D (sesiones con nombre).
5. E (Graph → merge → conflictos con agente).

### Actualización (24-nov)

- **PRs:** un solo botón *Merge* (método en Settings → Pull requests). Si el PR tiene conflictos, *Resolve & merge*: la sesión que hizo el PR (o una nueva en su worktree) resuelve los conflictos y, al terminar su turno, kuaky revisa que ya se pueda unir y hace el merge.
- **Vista del PR:** solo *Overview* (descripción, estado) y *Files changed*.
- **Sesión ↔ PR:** cada sesión muestra su PR (por la rama de su carpeta o por el enlace que el agente escribió en el chat); el PR muestra qué sesión lo hizo. Merge desde el chat o desde la barra izquierda.
- **Pestañas del panel derecho configurables** en Settings, con una línea que explica cada una (Branches incluye los worktrees).
- **Centro dividido:** los archivos, diffs y PRs se abren a la derecha del chat; el chat solo se quita si lo cierras. El × de la barra de pestañas esconde ese lado.
- **Barra izquierda:** *New session* más chico y la barra se pliega (⌘B) a una columna de íconos.
