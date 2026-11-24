# Plan: chat propio de agentes por ACP

Interfaz de agentes propia de kuaky (estilo panel de agentes de Zed) para
Claude Code, Antigravity y Codex, hablando ACP (*Agent Client Protocol*) en
lugar de leer una terminal.

La terminal se queda: sirve para el uso normal y para lo que ACP no cubre
(algunos comandos `/`, pantallas de configuración de cada CLI). El chat por ACP
es la forma "estructurada": mapa exacto, permisos con botones y conexiones
automáticas entre sesiones ([plan-mapa-agentes.md](plan-mapa-agentes.md)).

## Qué es ACP (resumen)

- JSON-RPC 2.0 por líneas sobre stdin/stdout del agente. kuaky es el
  **cliente**; el agente es el **servidor**.
- El agente corre localmente con el login y la suscripción del usuario.
- Flujo:
  1. `initialize` → capacidades de ambos lados.
  2. `session/new` (o `session/load` para retomar).
  3. `session/prompt` con bloques de contenido (texto, archivos, imágenes).
  4. El agente emite `session/update` en vivo: texto, pensamiento, plan, tool
     calls, cambios de modo, comandos disponibles.
  5. El agente pide cosas al cliente: `session/request_permission`,
     `fs/read_text_file`, `fs/write_text_file`, `terminal/*`.
  6. La respuesta a `session/prompt` trae `stopReason` = fin del turno.
- Verificar cada método contra la versión del protocolo que hablen los
  adaptadores instalados antes de implementarlo (algunas partes, como modelos
  y opciones de sesión, han cambiado entre versiones).

## Agentes

| Agente | Cómo habla ACP | Estado en kuaky |
|---|---|---|
| Claude Code | Adaptador `claude-agent-acp` (Zed). Ya instalado por Zed; `find_agent` lo encuentra | Handshake real probado |
| Antigravity | Modo propio: `agy acp` | Detectado; falta probar sesión real |
| Codex | Adaptador `codex-acp` (Zed). Requiere instalar Codex y el adaptador | No instalado |

Por agente hay que comprobar: `loadSession`, modos, selección de modelo,
comandos `/` disponibles, si usa `fs/*` y `terminal/*` del cliente o los suyos,
y qué datos de uso de tokens manda.

## Lo que ya existe

- `crates/kuaky-acp`: cliente con hilos y canales. `initialize`,
  `session/new`, `session/prompt`, `session/cancel`, respuesta a permisos.
  Eventos: texto, pensamiento, tool call y su actualización (con diffs), plan,
  permiso, fin de turno, error. Probado con `tests/mock_agent.py`.
- `crates/kuaky/src/chat_view.rs`: vista básica (Markdown, tarjetas de
  herramientas, permisos con botones, diff en pestaña aparte). El botón "chat"
  de la barra se quita (ver [plan-interfaz.md](plan-interfaz.md)); esta vista se
  rehace según este plan.

## Arquitectura

### Protocolo (`kuaky-acp`)

- **Tipos**: evaluar usar el crate de esquema oficial
  (`agent-client-protocol-schema`, Apache-2.0) solo por los tipos serde, y
  mantener nuestro transporte con hilos (sin runtime async). Si no conviene,
  ampliar nuestros tipos a mano.
- **Completar el lado cliente**:
  - `session/load` (retomar) y listado de sesiones si el agente lo ofrece.
  - `session/set_mode` y selección de modelo / opciones de sesión.
  - `authenticate` cuando el agente lo pida.
  - Bloques de prompt: `text`, `resource_link` (archivo mencionado),
    `resource` (contenido embebido), `image`.
  - Actualizaciones: `user_message_chunk` (al retomar), `available_commands_update`,
    `current_mode_update`, y las de opciones/título de sesión si existen.
  - Capacidades que ofrece kuaky:
    - `fs/read_text_file`: lee del buffer abierto si el archivo está en el
      editor (con cambios sin guardar), si no del disco.
    - `fs/write_text_file`: escribe pasando por el editor (deshacer, LSP,
      diff de revisión).
    - `terminal/create|output|wait_for_exit|kill|release`: los comandos del
      agente corren en un `kuaky-term` visible dentro de la tarjeta.
- **Robustez**: reconexión si el agente muere; timeouts por método; registro
  del tráfico JSON en modo debug.

### Estado de la sesión (`crates/kuaky/src/agent/`)

Separar datos de vista:
- `AgentThread` (entidad): mensajes, tool calls, plan, permisos pendientes,
  modo, modelo, estado (conectando / listo / trabajando / te espera / error).
- `AgentStore` (entidad por proyecto): hilos abiertos, procesos de agentes
  (un proceso por harness, varias sesiones dentro si el agente lo admite),
  persistencia.
- Vistas: `ThreadView` (conversación), `Composer` (entrada), `ReviewPanel`
  (cambios del turno).

### Persistencia

- Transcripción propia en `<data>/kuaky/threads/<id>.jsonl`: historial,
  búsqueda y mapa aunque el agente no ofrezca `session/load`.
- Metadatos (título, línea del mapa, harness, rama, worktree) en `state.json`.

## Referencias: qué tomamos de Zed y de las apps de Claude

| Función | Zed | Claude (Code desktop / CLI / claude.ai) | kuaky |
|---|---|---|---|
| Mensajes del usuario | "Tarjeta" clicable para editar; poca diferencia visual con el agente | — | Muy distintos (ver abajo) |
| Saltar entre mis prompts | `ctrl-alt-pageup/pagedown` (poco descubrible) | CLI: `Esc Esc` lista tus mensajes para volver | Riel de prompts + lista buscable + `⌥↑/⌥↓` |
| Editar un mensaje anterior | Clic en la tarjeta y reenviar | claude.ai: editar crea una rama con flechas ‹ 1/2 › | Editar = nueva rama, la anterior se conserva |
| Volver atrás (rewind) | Checkpoints según el agente | `/rewind`: restaurar código, conversación o ambos | Checkpoint propio por turno + elegir qué restaurar |
| Mensajes mientras trabaja | Cola; "Send now"; editar/borrar en cola | Corrección sin detener; el agente la lee tras la acción actual | Cola + "Send now" + editar en cola |
| Revisar cambios | Multibuffer, aceptar/rechazar por bloque | Diff con comentarios por línea que el agente aplica; `/code-review` con "Fix this one" | Ambos: aceptar/rechazar y comentar líneas |
| Nivel de detalle | — | Normal / Thinking / Verbose (`Ctrl+O`) | Igual, con otro atajo (no `ctrl+o`) |
| Pregunta al margen | — | `/btw`: chat lateral que no ensucia el hilo | Fase posterior (sesión ACP aparte o `session/fork`) |
| Seguir al agente | Abre en el editor lo que toca | — | Igual, opcional |
| Contexto | `@` archivos, símbolos, diagnósticos, diff de rama, URLs; pegar código → mención | `@archivo`, `+` para adjuntar imágenes/PDF | `@` archivos, carpetas, selección, diagnósticos, diff de rama |
| Uso de contexto | Tokens junto al selector | % de contexto en tarjeta | % de contexto + tokens del turno |
| Títulos | Automáticos, editables, regenerar | Renombrar con clic | Automáticos y editables |
| Avisos | Notificación y sonido al terminar | Notificación al terminar | Igual + "te espera" en el mapa |
| Exportar | Abrir hilo como Markdown, copiar respuesta | — | Copiar como Markdown, exportar hilo |

## Navegación y mensajes (lo que no funcionaba en Zed)

Problema: en una conversación larga no se encuentra lo que uno escribió y no se
distingue bien de lo que escribió el agente.

### Distinguir mis mensajes

- **Mis mensajes**: tarjeta con fondo propio, borde de color de acento a la
  izquierda, etiqueta "You" con hora y número de prompt (`#3`). Texto en la
  fuente de la interfaz, no en Markdown de respuesta.
- **Agente**: sin tarjeta, texto directo con el ícono del agente al inicio de
  cada turno. Herramientas como filas compactas grises.
- **Separador de turno**: línea fina con duración del turno y archivos
  cambiados (`2m 14s · 3 files +40 −12`).

### Saber siempre a qué pregunta pertenece lo que leo

- **Prompt fijo arriba (sticky)**: al hacer scroll dentro de una respuesta
  larga, una versión de una línea del prompt que la originó queda pegada arriba
  de la conversación. Clic → salta a ese prompt.

### Ir a mis mensajes anteriores

- **Riel de prompts**: a la derecha de la conversación, una marca por cada
  mensaje mío (como marcas en la barra de scroll). Pasar el mouse muestra el
  inicio del prompt; clic salta ahí. Las marcas de permiso pendiente o error se
  ven en ámbar/rojo.
- **Lista de mis prompts** (`⌘⇧J`): ventana con todos mis mensajes del hilo,
  buscable. Enter salta; también desde ahí: editar, rewind o fork.
- **Atajos**: `⌥↑` / `⌥↓` mensaje mío anterior / siguiente (con la
  conversación enfocada o el composer vacío). `↑` en composer vacío recupera
  el último mensaje para editarlo.
- **Botones flotantes** al hacer scroll: "↑ Previous prompt" y
  "↓ Latest".

### Editar, volver atrás y ramas

- **Editar un mensaje anterior** abre la opción de qué hacer con el código:
  - "Keep current code" (solo cambia la conversación);
  - "Restore code to this point" (vuelve al checkpoint de ese turno).
- **La conversación anterior no se pierde**: se convierte en una rama, con
  flechas ‹ 1/2 › en el mensaje editado para cambiar entre versiones.
- Cómo se hace con ACP:
  - si el agente soporta `session/fork` (RFD oficial, con cursor opcional para
    bifurcar en un punto), se usa;
  - `session/rewind` todavía es una propuesta;
  - si no hay soporte, kuaky abre una sesión nueva sembrada con el resumen de la
    conversación hasta ese punto (como "New From Summary" de Zed).

## Interfaz

### Dónde vive

- Pestaña "Chat" en el área central (ya existe `TabKind::Chat`).
- En el menú **New session +**, por harness: **Terminal** o **Chat**.
- En la barra izquierda las sesiones de chat aparecen igual que las de
  terminal, con estado y línea del mapa.

### Encabezado

- Ícono y nombre del agente, título de la sesión (editable).
- Selector de modo (p. ej. en Claude Code: normal, aceptar ediciones, plan).
- Selector de modelo si el agente lo expone.
- Línea del mapa a la que pertenece y botón para verla.
- Uso del turno / sesión si el agente lo manda; si no, el de los lectores de
  `kuaky-usage`.

### Conversación

- **Mensajes del usuario**: texto, archivos mencionados como chips, imágenes.
- **Mensajes del agente**: Markdown con streaming (`TextView::markdown`),
  bloques de código con resaltado y botón copiar.
- **Pensamiento**: plegado por defecto, una línea con "Thinking…".
- **Plan**: lista fija arriba de la conversación (pendiente / en curso /
  hecho). Alimenta las estaciones del mapa.
- **Tool calls** como tarjetas compactas:
  - ícono por tipo (leer, editar, ejecutar, buscar, web, pensar);
  - estado (pendiente, en curso, hecho, falló);
  - ubicaciones clicables → abren el archivo en esa línea;
  - ediciones: diff en línea plegable, con "Open diff";
  - comandos: salida de la terminal en vivo dentro de la tarjeta.
- **Permisos**: tarjeta destacada con las opciones del agente (permitir una
  vez, siempre, rechazar). Atajos de teclado. La sesión se marca "te espera"
  en la barra, el mapa y una notificación.
- Lista virtualizada para conversaciones largas (RAM baja).

### Entrada (composer)

- Multilínea; Enter envía, Shift+Enter salto de línea, Esc detiene el turno.
- `@` para mencionar archivos y carpetas (buscador difuso de `quick_open`) →
  `resource_link`; selección del editor como `resource`.
- Pegar o arrastrar imágenes si el agente acepta `image`.
- `/` muestra los comandos que anunció el agente
  (`available_commands_update`).
- Cola: mensajes escritos mientras el agente trabaja se envían al terminar el
  turno (o "Send now" que cancela y envía).

### Revisión de cambios

- **Seguir al agente** (opcional): abrir en el editor los archivos que edita,
  resaltando las líneas.
- **Panel de cambios del turno**: todas las ediciones del turno juntas
  (multi-archivo), aceptar o rechazar por bloque. Rechazar restaura ese bloque.
- **Comentarios por línea**: en el diff, "+" junto a una línea para dejar un
  comentario; `⌘Enter` envía todos juntos al agente como un prompt para que los
  aplique.
- **Revisión por otro agente** (conexión Review): los hallazgos salen como
  tarjeta agrupada por archivo con "Walk through" (recorre en el diff) y
  "Fix this one" / "Fix all" (envía al agente autor).
- **Nivel de detalle**: Normal (herramientas plegadas), Thinking (más
  razonamiento), Verbose (todo). Selector en el encabezado.
- **Checkpoints**: antes de cada turno, foto del árbol de trabajo en una ref
  oculta (`git stash create` → `refs/kuaky/checkpoints/<thread>/<n>`).
  Botón "Restore to here" en cada mensaje del usuario.

### Varios hilos

- Varias pestañas de chat a la vez, de distintos agentes.
- Indicador por pestaña: trabajando, te espera, terminó.
- Notificación del sistema cuando un hilo te espera o termina y kuaky no está
  al frente.

## Integración con el resto

- **Mapa de metro**: `plan` → estaciones; tool call en curso → estación
  activa; `locations` → archivos tocados; permiso → "te espera";
  `stopReason` → turno terminado.
- **Conexiones**: review, tester, segunda opinión y relevo envían el prompt
  con `session/prompt` tras aprobación. Cadenas con tope de rondas.
- **Worktrees**: un hilo puede nacer en un worktree nuevo (mismo selector
  "This folder / New branch").
- **Uso**: el consumo de los hilos ACP aparece en el dashboard como el de la
  terminal (los agentes escriben sus mismos registros locales).

## Seguridad y costos

- Nada se envía a un agente sin acción del usuario, salvo cadenas que el
  usuario aprobó explícitamente con su tope.
- `fs/write_text_file` solo dentro de la carpeta del proyecto o worktree;
  fuera de ahí, pedir confirmación.
- `terminal/create`: los comandos corren con el entorno de login del usuario,
  visibles en la tarjeta, cancelables.
- Las pruebas automáticas usan el agente falso; con agentes reales solo el
  handshake (sin prompts, para no gastar tokens).

## Licencias

- No copiar código de `agent_ui` / `agent` de Zed (GPL). Solo se lee el
  protocolo y la documentación.
- Crate de esquema ACP: Apache-2.0 (anotar en `THIRD_PARTY.md` si se usa).

## Fases

1. **Protocolo completo** en `kuaky-acp`: tipos, `session/load`, modos,
   modelo, `authenticate`, bloques de prompt, actualizaciones faltantes,
   `fs/*`, `terminal/*`. Ampliar el agente falso para cada uno. Probar
   handshake real con `claude-agent-acp` y `agy acp`.
2. **Estado y persistencia**: `AgentThread`, `AgentStore`, transcripción
   propia, retomar.
3. **Conversación**: mensajes con streaming, pensamiento, plan, tarjetas de
   herramientas, permisos, lista virtualizada. Desde el principio: mensajes del
   usuario muy distintos, prompt fijo arriba, riel de prompts, lista `⌘⇧J` y
   atajos `⌥↑/⌥↓` (es lo que fallaba en Zed).
4. **Composer**: menciones `@`, imágenes, comandos `/`, cola, cancelar.
5. **Revisión**: diff en línea, panel de cambios del turno, aceptar/rechazar,
   comentarios por línea, checkpoints, editar con ramas ‹ 1/2 › y rewind,
   nivel de detalle, seguir al agente.
6. **Encabezado y varios hilos**: modo, modelo, uso, título, indicadores,
   notificaciones.
7. **Integración**: mapa de metro exacto, conexiones con cadenas automáticas,
   worktrees.
8. **Codex**: instalar `codex-acp`, comprobar diferencias y ajustar.

Cada fase con commit propio, tests con el agente falso y captura de la vista.

## Riesgos

- El protocolo y los adaptadores cambian: fijar versiones y probar el
  handshake en CI con el agente falso.
- `agy acp` no está documentado públicamente como los otros: comprobar qué
  partes soporta.
- Los adaptadores no exponen todo lo de la CLI (comandos `/`, `/usage`,
  configuración). La terminal sigue disponible para eso.
- Interfaz de chat buena = mucho trabajo de UI. Por eso va después del mapa
  por terminal, que funciona con los tres agentes desde ya.

## Fuentes

- Zed, Agent Panel: https://zed.dev/docs/ai/agent-panel
- Claude Code desktop: https://code.claude.com/docs/en/desktop
- Rediseño de Claude Code desktop: https://claude.com/blog/claude-code-desktop-redesign
- Checkpoints y `/rewind` en Claude Code: https://www.anthropic.com/news/enabling-claude-code-to-work-more-autonomously
- ACP, RFD `session/fork`: https://agentclientprotocol.com/rfds/session-fork
- ACP, propuesta `session/rewind`: https://github.com/agentclientprotocol/agent-client-protocol/pull/1214

## Estado (2026-11-24)

| Fase | Estado |
|---|---|
| 1. Protocolo completo | Hecho salvo `terminal/*` y `authenticate` (a propósito: el login se hace en la CLI de cada agente) |
| 2. Estado y persistencia | Hecho: los chats se guardan como sesión y se retoman con `session/load` |
| 3. Conversación | Hecho: mensajes tuyos destacados, prompt fijo, riel, lista `⌘⇧J`, `⌥↑/⌥↓`, plan, herramientas, permisos |
| 4. Composer | Hecho: `@` archivos, `/` comandos, cola con "Send now"/"Edit", `↑` recupera, `Esc` detiene. Falta pegar imágenes |
| 5. Revisión | Parcial: "Review changes" por turno, "Revert" por archivo, checkpoints con "Restore", editar con versiones ‹ 1/2 ›, nivel de detalle. Faltan aceptar/rechazar por bloque, comentarios por línea y seguir al agente |
| 6. Encabezado y varios hilos | Hecho: modo, modelo, esfuerzo y demás opciones del agente; título; notificaciones. Falta % de contexto |
| 7. Integración | Hecho: el chat alimenta el mapa (prompts, plan, fin de turno). Las conexiones siguen siendo por terminal |
| 8. Codex | Pendiente (no instalado) |

Probado con el agente falso (`crates/kuaky-acp/tests/mock_agent.py`) y con el handshake real de
`claude-agent-acp` y `agy acp` (sin enviar mensajes, para no gastar tokens).

### Actualización (24-nov)

- Nuevo diseño: tus mensajes en burbuja a la derecha (estilo app de Claude), herramientas agrupadas en una línea ("Ran 3 commands, edited a.rs +4 −1 ›") con tarjetas estilo Zed al abrir, caja de texto con los selectores adentro (on/off como interruptor).
- Línea viva mientras trabaja: qué hace, tiempo del turno y "no output for 40s"; flecha para bajar al final.
- Imágenes: pegar con ⌘V o adjuntar con +.
- % de contexto (y costo) desde `usage_update`.
- Revisión por turno: Keep / Reject por bloque (rechazar revierte ese bloque en disco) y comentarios por línea que se mandan al chat.
- Las sesiones ya no salen en las pestañas de arriba; viven en la barra izquierda.
- **Follow** (ícono de mira arriba del chat): kuaky abre los archivos que el agente lee o edita; arriba sale "Following …" con *Back to chat* y *Stop following*.
- **Codex**: usa `codex-acp` si está instalado; si no, y está la CLI `codex`, corre el adaptador oficial por `npx` (`@agentclientprotocol/codex-acp@2.10.0`, la misma versión del registro de Zed). Sin probar aquí: Codex no está instalado en esta Mac.
