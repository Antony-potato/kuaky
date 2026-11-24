# Plan: subagentes debajo de cada conversación

Objetivo: en la barra izquierda, debajo de cada sesión, ver si tiene **subagentes trabajando** (Explore, Review, etc.) sin que la lista crezca de alto.

## Cómo se detectan

| Sesión | De dónde sale | Empieza | Termina |
|---|---|---|---|
| Claude Code en terminal | su registro `~/.claude/projects/<carpeta>/<id>.jsonl` (ya lo lee `TaskWatcher`) | `tool_use` con nombre `Task` o `Agent` → `description`, `subagent_type` | `tool_result` con el mismo `tool_use_id` (error = falló) |
| Chat propio (ACP, Claude) | `tool_call` cuyo `_meta.claudeCode.toolName` es `Task`/`Agent`; título = descripción | `status: in_progress` | `completed` / `failed` |
| Pasos de cada subagente (chat) | `tool_call` con `_meta.claudeCode.parentToolUseId` | — | — |
| Antigravity / Codex | todavía no exponen subagentes | — | — |

Cambios:
1. `agent_tasks.rs`: además de tareas, guardar `Subagent { id, kind, description, started, finished, failed }`.
2. `kuaky-acp`: leer `_meta` de `tool_call` → `tool_name` y `parent_id`.
3. `ChatView`: lista de subagentes (activos y terminados) a partir de sus herramientas.

## Cómo se ven (no ocupar alto)

- **Plegado por defecto:** una sola línea debajo de la sesión: `2 subagents working ›` (con spinner) o `1 subagent done` (gris).
- **Al abrir:** una fila por subagente: tipo · descripción corta · tiempo. Máximo 4 filas + "+N more".
- **Los terminados se apagan** a los 2 minutos y dejan de mostrarse (siguen en el chat).
- **Clic en un subagente:** abre la sesión y salta a esa herramienta en el chat.
- **Se recuerda** qué sesiones dejaste abiertas o plegadas.
- En el chat, el grupo de herramientas dice "Ran 2 subagents ›" y al abrirlo muestra los pasos de cada uno.

## Diseño de la lista

Tres propuestas (mockup en la conversación):
- **A · Lista compacta:** el objetivo como etiqueta gris pequeña; cada sesión es una fila con un punto de estado; subagentes en una fila plegable.
- **B · Tarjeta por objetivo:** cada objetivo es una tarjeta con barra de avance; subagentes como chips.
- **C · Árbol:** objetivo → sesión → subagentes con líneas guía (como el árbol de archivos); cada nivel se pliega.

Colores: neutros; el color solo marca estado (trabajando, te necesita). El tema se define con el mood que vas a pasar.

## Orden

1. Detección en terminal (Claude) y en chat (ACP).
2. Fila plegable en la barra izquierda con la propuesta elegida.
3. Clic → saltar a la herramienta en el chat.
4. Temas (después del mood).

## Estado (24-nov)

Hecho con la propuesta **A**:
- Detección en terminal (registro de Claude Code: `Agent`/`Task` → resultado) y en chat (`_meta.claudeCode.toolName` + `rawInput`).
- Debajo de cada sesión, una línea plegada ("2 subagents working ›", con spinner); al abrirla, hasta 4 filas con qué hace, estado y tiempo; "+N more".
- Los terminados desaparecen a los 2 minutos; los que corren se ocultan tras 2 h (sesión muerta).
- Clic en un subagente: abre la sesión y, en el chat, salta a esa herramienta con su grupo abierto.
- En el chat, el grupo dice "Ran 2 subagents ›" y cada tarjeta "Subagent · Explore".

También hecho:
- **Se recuerda** qué listas de subagentes dejaste abiertas (por sesión, se guarda con el proyecto).
- **Pasos de cada subagente:**
  - en el chat, dentro de su tarjeta, plegados en una línea ("3 steps · now: Read src/refunds/mod.rs"); al abrir, uno por fila (últimos 30);
  - en la barra izquierda, debajo del subagente que corre, su paso actual;
  - en el chat salen de `_meta.claudeCode.parentToolUseId` y ya no se mezclan con la lista principal;
  - en terminal, de su propio registro `<sesión>/subagents/agent-*.jsonl` (enlazado por `toolUseId` en `agent-*.meta.json`).
