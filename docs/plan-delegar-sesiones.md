# Plan: que un agente abra otra sesión ("tú haz esto, que Antigravity haga lo otro")

Objetivo: decirle a Claude (o a cualquier agente en un chat de kuaky) *"que una
sesión de Antigravity siga con los tests y tú haz la barra lateral"*, y que él
mismo abra esa sesión como sub-línea de la suya, sin pasos a mano.

## Cómo funciona

1. Cada chat (ACP) recibe en `session/new` y `session/load` el servidor MCP
   `kuaky` (`mcpServers`). El agente lo lanza como `kuaky mcp`: es el mismo
   binario de kuaky, que con ese argumento solo atiende MCP por stdio.
2. El agente ve cuatro herramientas:

   | Herramienta | Qué hace |
   |---|---|
   | `open_session(agent, task, title?, mode?, worktree?)` | Abre otro agente como chat y le manda la tarea al momento |
   | `list_sessions()` | La sesión principal y las que salieron de ella, con su estado |
   | `read_session(session)` | Estado, plan y última respuesta de una sesión |
   | `send_to_session(session, message)` | Le manda un mensaje (en cola si está ocupada) |

3. `kuaky mcp` no tiene la interfaz: deja cada llamada como archivo en el buzón
   de la app (`<config>/kuaky/ipc/<pid>/<id>.call`) y espera `<id>.reply`. La
   app revisa el buzón cada 250 ms. Archivos en vez de un socket: igual en
   macOS y Windows.
4. Cada sesión lleva en su entorno su uid (`KUAKY_SESSION`) y la clave del buzón
   (`KUAKY_IPC_TOKEN`): kuaky sabe quién llama y otro proceso no puede abrir
   sesiones.

Código: `crates/kuaky-mcp` (servidor y buzón), `crates/kuaky/src/delegate.rs`
(lo que hace cada herramienta), `kuaky-acp` (`Client::set_mcp_servers`).

## Decisiones

- **Sin preguntar:** la sesión se abre y empieza a trabajar en cuanto el agente
  llama a `open_session`. Se abre en segundo plano (no cambia de pestaña ni
  quita el foco); la barra izquierda y el mapa la muestran, y avisa cuando
  termina o pide permiso.
- **Tope configurable** en Settings → Agents ("Sessions opened by agents"):
  el número que quieras (3 por defecto; vacío o 0 = sin límite), contando las
  sesiones abiertas a la vez aparte de la principal. Una sesión abierta así también puede abrir
  otras, y cuentan para el mismo tope (se sigue `opened_by` hasta la
  principal). Cerrar una pestaña libera su lugar.
- **Misma carpeta por defecto.** Casi siempre son archivos distintos. El agente
  pide `worktree: true` solo si los dos tienen que tocar los mismos archivos;
  entonces la sub-línea recibe su rama `kuaky/<agente>-<fecha>`.
- **El resultado se pide desde la principal** (`read_session`); no vuelve solo.
- **Modos:** `branch` (por defecto) crea una sub-línea que cuelga de la línea
  de quien llama; `handoff` deja la sesión nueva en la misma línea (relevo) y
  marca la estación "Hand-off · <agente>".
- **Qué recibe la sesión nueva:** el título, la tarea que escribió el agente,
  quién la abrió y dónde trabaja, y la nota de la línea (`relay_note`). Nunca
  la conversación completa.
- **Permisos:** en Claude las herramientas de kuaky no piden permiso
  (`_meta.claudeCode.options.allowedTools = ["mcp__kuaky"]`). Otros agentes
  pueden pedirlo la primera vez, con su tarjeta normal en el chat.
- Quién abrió cada sesión se guarda con la línea (`LineSession::opened_by`), así
  que la familia sigue igual al reabrir kuaky.

## Pendiente

- **Terminales.** Hoy solo los chats (ACP) reciben las herramientas. Para las
  sesiones en terminal:
  - Claude Code: `claude --mcp-config <archivo>` junto al `--settings` que ya
    se pasa, con `KUAKY_SESSION` y `KUAKY_IPC_TOKEN` en el entorno de la
    terminal y `mcp__kuaky` en `permissions.allow` del archivo de ajustes.
  - Codex, Antigravity, Gemini, OpenCode: cada CLI tiene su propio archivo de
    MCP; habría que generar uno por sesión o pasarlo por argumentos.
  - `open_session` desde una terminal podría abrir la nueva sesión como
    terminal o como chat, según Settings → Agents.
  - `read_session` de una terminal: la última respuesta sale del registro
    JSONL (Claude) o del título; es menos preciso que por ACP.
- Ver en la barra izquierda quién abrió cada sesión (p. ej. "abierta por Claude").
