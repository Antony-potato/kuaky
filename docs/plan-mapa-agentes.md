# Plan: mapa de metro de lo que hace cada agente

Problema: se abren muchas sesiones de agentes (planes grandes, features, bugs) y
después no se sabe qué sesión es de qué cosa ni en qué se quedó cada una.

Idea: ver el avance como un mapa de metro, donde `main` es la vía principal.

## Modelo

| Concepto | Qué es | Ejemplo |
|---|---|---|
| **Línea** | Un objetivo: plan, feature o bug. Lo que el usuario tiene en la cabeza | "Bug · terminal brinca" |
| **Sub-línea** | Una parte de un plan grande. Sale de la línea del plan y vuelve a ella | "Plan interfaz → barra lateral" |
| **Estación** | Un paso del objetivo: hecho, en curso o pendiente | "Debounce del resize" |
| **Tren** | Una sesión de agente que avanza sobre una línea | "Claude Code · te espera" |
| **Vía principal** | `main`. Una línea sale con su rama o worktree y vuelve al hacer merge | |

- Una línea puede tener varias sesiones, en paralelo o una tras otra.
- Las dependencias de un plan se ven como "esta sub-línea espera a aquella"
  (la estación de cruce no se completa hasta que llega la otra).

### Estados de una estación

- Llena: paso hecho.
- Anillo del color de la línea: un agente está trabajando ahí.
- Anillo ámbar: el agente te necesita (permiso, pregunta o terminó y espera
  revisión).
- Hueca: pendiente.
- Tramo punteado: lo que falta hasta el merge.

## Creación automática mientras se habla con el agente

El mapa se arma solo, sin escribir nada a mano. Para no gastar tokens extra, el
modo por defecto usa solo datos que ya existen.

### Fuentes (sin costo)

1. **Prompts del usuario** (hook `UserPromptSubmit` de Claude Code; historial
   JSONL en los demás). Cada prompt es un evento en la línea. Varios prompts
   seguidos sobre lo mismo se agrupan en una estación.
2. **Título de la terminal** (`TermEvent::Title`). Claude Code y otros agentes
   lo actualizan con un resumen de la tarea actual. Sirve como nombre de la
   estación en curso.
3. **Tareas del agente**: la lista de todos (`TodoWrite` / tareas en el JSONL
   de Claude Code) y el plan en modo plan. Cada tarea pasa a ser una estación
   pendiente, y se marca hecha cuando el agente la completa.
4. **Hitos de git**: primer commit en la rama, PR abierto, revisión, merge.
   El merge cierra la línea y la une a `main`.
5. **Estado del agente** (hooks y salida de la terminal): trabajando, te
   espera, terminado.

### Resumen opcional con un modelo barato (apagado por defecto)

Si se activa en Configuración, un modelo pequeño (p. ej. Haiku) resume cada
cierto tiempo:
- nombres más claros para las estaciones;
- la ficha "¿En qué me quedé?".

Gasta tokens, por eso es opcional y muestra cuánto cuesta.

### A qué línea va cada sesión

1. Sesión abierta desde el "+" de una línea → hereda esa línea.
2. Sesión en una rama o worktree que ya pertenece a una línea → esa línea.
3. Sesión nueva sin línea → se crea una **línea provisional** con el nombre
   del título de la terminal o del primer prompt.
   - Se puede renombrar, cambiar de tipo (plan, feature, bug) o arrastrar a
     otra línea para fusionarla.
   - Si no tiene actividad en unos días y no tiene cambios, se archiva sola.

## "¿En qué me quedé?"

Al hacer clic en una línea se abre una ficha con:
- lo último que se hizo;
- el siguiente paso pendiente;
- qué sesiones tocaron ese objetivo, con botón para reanudar;
- si algún agente está esperando;
- rama, cambios y PR.

## Varios agentes en la misma línea

1. **En paralelo, cada uno en su propia vía.** El segundo agente que se abre en
   una línea con otro trabajando recibe su propio worktree creado desde la rama
   de la línea. En el mapa la vía se desdobla en carriles paralelos.
2. **Una estación por agente.** Al abrir la sesión se elige a qué estación va
   (p. ej. "barra lateral" o "tests"). El mapa muestra qué tren está en qué
   parada.
3. **Estación de unión.** Cuando los carriles terminan, se juntan en la rama de
   la línea. Antes, `git merge-tree --write-tree` revisa conflictos sin tocar
   archivos:
   - limpio → se unen y la línea sigue;
   - conflictos → estación ámbar con "Resolver" (manual o con agente, ver
     parte E de [plan-interfaz.md](plan-interfaz.md)).

   Al final, la línea completa vuelve a `main` con su merge.
4. **Misma carpeta (sin worktree).** kuaky vigila qué archivos edita cada
   agente (hooks `PostToolUse` y `git status`). Si dos agentes tocan el mismo
   archivo, aviso: "⚠ Claude y Antigravity están editando project.rs".
5. **Relevo entre sesiones.** Cada línea guarda una nota de contexto (qué se
   hizo, qué falta, rama). Al abrir una sesión nueva en la línea, la nota queda
   escrita en el prompt sin enviar (`Launch::prefill`). El usuario decide si la
   manda.

## Conexiones entre sesiones

Una sesión puede trabajar sobre el resultado de otra. Ejemplo: Claude revisa lo
que hizo Antigravity.

### Tipos de conexión

| Tipo | Qué hace | Permisos del agente que llega |
|---|---|---|
| **Review** | Revisa el diff de la otra sesión y escribe hallazgos | Solo lectura |
| **Tester** | Escribe y corre tests sobre lo que hizo la otra | Puede editar tests, en su propio worktree |
| **Segunda opinión** | Recibe el mismo plan o prompt; se comparan las propuestas lado a lado | Solo lectura (modo plan) |
| **Relevo** | Continúa la línea donde la otra sesión la dejó | Normal |

### Cómo se crea

- En el board: clic en una estación → **Review with… / Test with… /
  Second opinion… / Hand off to…** → elegir harness. Arrastrar un tren sobre
  otro abre el mismo menú.
- En el mapa aparece una estación del tipo de la conexión (p. ej. "Review ·
  Claude") con una flecha fina desde la estación de origen.

### Qué recibe el agente que llega

Nunca la conversación del otro agente (es larga, ruidosa y cara). Recibe el
resultado:
- objetivo de la línea y tareas;
- rama y carpeta (worktree) de la otra sesión;
- diff contra la base (`git diff <base>...<rama>`);
- instrucción según el tipo (p. ej. "revisa, no edites, lista hallazgos con
  archivo:línea").

Si el harness trae su propio comando de revisión (`/review` en Claude Code y
Codex), se usa ese.

### Ida y vuelta

1. El agente que llega termina → kuaky guarda su resultado en
   `<config>/kuaky/lines/<repo>/<línea>/<conexión>.md`.
2. Botón **"Send to <agente original>"**: deja los hallazgos en su entrada.
3. El agente original corrige → se puede pedir otra revisión o pasar a la
   estación de unión.

### Dos formas de llevarla a cabo

| | Por terminal (primero) | Por ACP (después, ver [plan-chat-acp.md](plan-chat-acp.md)) |
|---|---|---|
| Cómo se pasa el prompt | Escrito en la terminal sin enviar (`Launch::prefill`); el usuario pulsa Enter | Se envía con `session/prompt` tras aprobación en kuaky |
| Cómo sabe kuaky que terminó | Hooks (Claude Code), título y silencio de la terminal | Fin de turno exacto (`stopReason`) |
| Hallazgos | Se copian de un archivo que el agente escribe | Texto del mensaje final del agente |
| Rondas automáticas | No | Sí: implementar → revisar → corregir, con tope (por defecto 2) y aprobación en cada envío o una vez por cadena |

### Cuidados

- **Tokens**: nada se envía solo. Cada conexión dice qué agente trabajará; el
  consumo aparece en su uso.
- **Sin bucles**: tope fijo de rondas en las cadenas automáticas.
- **El revisor no edita**: solo lectura (`--permission-mode plan` en Claude
  Code; modo equivalente por ACP).

## Fuentes según cómo corre la sesión

| Dato | Sesión en terminal | Sesión por ACP |
|---|---|---|
| Estaciones pendientes | Todos del JSONL de Claude Code; plan si hay | Actualización `plan` exacta, para los tres agentes |
| Estación en curso | Título de la terminal | Tool call en curso |
| Archivos tocados | Hooks `PostToolUse` / `git status` | `locations` de cada tool call |
| Te espera | Hooks (solo Claude Code) | `session/request_permission` |
| Terminó | Hooks / silencio | `stopReason` del turno |

Con ACP el mapa deja de adivinar. Las sesiones por terminal siguen
funcionando con menos precisión.

## Dónde se ve

1. **Mini mapa en el panel derecho**, debajo de Archivos / Cambios / Ramas /
   PRs.
   - Solo líneas activas: color, estación en curso, ícono del agente y punto
     ámbar si alguno espera.
   - Tooltip al pasar el mouse: "Claude Code · barra lateral · hace 3 min".
   - Clic → board a pantalla completa.
2. **Board a pantalla completa** (como una pestaña más; `Esc` para salir).
   - Filtros: activas, te esperan, terminadas, por tipo.
   - Clic en un tren (ícono del agente) → abre su terminal o chat; si la
     sesión está cerrada, la reanuda.
   - Clic en una línea → ficha "¿En qué me quedé?" al lado, con
     "Nueva sesión en esta línea".
3. **Dentro de cada proyecto**: el mini mapa y el board son de ese repo.
2. **En Inicio**: mapa resumido de todos los proyectos, para ver de un vistazo
   qué está avanzando y qué espera.
3. **Barra izquierda**: lista compacta agrupada por línea.

   ```
   ● Plan · búsqueda + LSP      4/6   Antigravity trabajando
   ● Bug · terminal brinca      1/3   Claude te espera ⚠
   ✓ Bug · ctrl+o              merged
   ```

## Datos

Se guardan por proyecto en `<config>/kuaky/lines/<repo>.json`:

```json
{
  "lines": [{
    "id": "l1", "name": "Bug · terminal brinca", "kind": "bug",
    "color": "purple", "parent": null, "branch": "kuaky/terminal-jump",
    "sessions": ["<harness>:<session-id>"],
    "stations": [
      { "id": "s1", "title": "Reproducir", "state": "done", "source": "todo" },
      { "id": "s2", "title": "Debounce del resize", "state": "active", "source": "title" }
    ],
    "depends_on": [], "archived": false
  }]
}
```

`source` dice de dónde salió la estación: `prompt`, `title`, `todo`, `plan`,
`git`, `manual` o `summary`.

## Por fases

1. **Modelo y lista compacta.** Líneas automáticas desde sesiones y prompts,
   estados, renombrar y arrastrar. Ficha "¿En qué me quedé?" sin resumen.
2. **Estaciones automáticas.** Tareas del agente, título de la terminal e
   hitos de git.
3. **Mapa del proyecto.** Dibujo de metro con `main`, ramas que salen y vuelven,
   y sub-líneas.
4. **Mapa en Inicio**, con todos los proyectos.
5. **Conexiones por terminal**: review, tester, segunda opinión y relevo, con
   ida y vuelta manual.
6. **Resumen opcional** con modelo barato.
7. **Conexiones por ACP**: rondas automáticas con tope, cuando exista el chat
   propio ([plan-chat-acp.md](plan-chat-acp.md)).

Depende de la parte D (sesiones con nombre) y la parte E (Graph) de
[plan-interfaz.md](plan-interfaz.md): el dibujo de carriles se comparte.

## Estado (2026-11-24)

| Fase | Estado |
|---|---|
| 1. Modelo y lista compacta | Hecho: `lines.rs`; la barra izquierda agrupa las sesiones abiertas por línea |
| 2. Estaciones automáticas | Hecho: prompts (hooks), título de la terminal, tareas de Claude Code (`TaskCreate`/`TaskUpdate`/`TodoWrite`), plan por ACP, commits/PR/merge |
| 3. Mapa del proyecto | Hecho: mini mapa en el panel derecho y board a pantalla completa (`Esc` cierra) con sub-líneas, trenes y ficha "¿En qué me quedé?" |
| 4. Mapa en Inicio | Pendiente |
| 5. Conexiones por terminal | Hecho: Review / Test / Second opinion / Hand off; hallazgos en archivo y "Send to author" (escribe sin enviar) |
| 6. Resumen con modelo barato | Pendiente (opcional) |
| 7. Conexiones por ACP con rondas automáticas | Pendiente |

No hay arrastrar y soltar: mover una sesión a otra línea se hace desde la ficha (ícono ⇄).
