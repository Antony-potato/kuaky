# Sistema de temas de kuaky

Fecha: 2026-11-24 · Código: [`crates/kuaky/src/theme.rs`](../../crates/kuaky/src/theme.rs), temas en [`crates/kuaky/assets/themes/`](../../crates/kuaky/assets/themes/) · Propuestas visuales: [propuestas.html](propuestas.html)

## 0. Estado

- **Dirección elegida: C · Sumi / Papel** (carteles japoneses). Sumi es el tema oscuro, Papel el claro, y por defecto kuaky sigue el claro/oscuro del sistema (`mode: system`).
- Temas incluidos: **Sumi**, **Papel** y **Kuaky Original** (el diseño anterior, por si se quiere volver). Etiqueta y Rejilla se quitaron de la app; están descritos en [propuestas.html](propuestas.html).
- Lo propio de C además de los colores: títulos y números grandes en serif mincho (`font.display`, la primera instalada de Hiragino Mincho ProN, Yu Mincho, Noto Serif JP, Georgia) y el sol de acento junto al nombre (`brand.mark`).
- **Cambiar de tema en Settings → Appearance:** los botones System / Light / Dark cambian el modo al instante. Un clic en un tema (Sumi, Papel u Original) lo aplica en ese momento. Si el modo es System y el tema es de la otra apariencia (por ejemplo Papel con el Mac en oscuro), el modo pasa a Light para que se vea (`theme::mode_after_pick`, con prueba).
- Animaciones hechas: ver §6.

## 1. Qué se guardó del diseño actual

| Qué | Dónde |
|---|---|
| Etiqueta de git con el código tal como estaba | `diseno-original-v0` (`git checkout diseno-original-v0` para volver a verlo) |
| El diseño actual como tema | `assets/themes/kuaky-original.json` (mismos colores exactos, componentes de GPUI Kit sin tocar). Se elige en Settings → Appearance |
| Capturas de antes | `docs/diseno/capturas/actual-home.png`, `actual-proyecto.png` |

Una prueba (`theme::tests::original_keeps_current_colors`) falla si alguien cambia por error los colores del tema original.

## 2. Qué hace la industria (investigación)

1. **Tokens de diseño en tres niveles.** Es el patrón común en sistemas de diseño grandes (eBay, Wix, ServiceTitan, Zebra):
   - **Primitivos:** valores crudos con nombre de color (`ink.950 = #0b0c0f`). No se usan directamente en la interfaz.
   - **Semánticos:** nombran *para qué* es el color, no *qué color es* (`background`, `accent`, `status.warning`). Apuntan a primitivos.
   - **Componentes:** colores de un componente concreto (`list.active.background`), que apuntan a semánticos.

   El modo oscuro o claro, o un tema nuevo, se hace **cambiando a qué primitivo apunta cada token semántico**. Las vistas no cambian.
2. **Formato DTCG 2025.10.** El Design Tokens Community Group del W3C publicó en octubre de 2025 la primera versión estable de su formato de archivo. Tomamos de ahí la sintaxis de alias `{nombre}`. El formato completo (`$value`, `$type`) es más verboso de lo que necesitamos. Si algún día exportamos a Figma o Style Dictionary, se puede convertir con un script.
3. **Temas como archivos JSON + modo del sistema (como Zed).** Zed guarda cada tema en un JSON con `appearance: light | dark`, y en ajustes guarda `mode: system | light | dark` más un tema claro y uno oscuro. Con `system` sigue al sistema operativo. Copiamos ese modelo.
4. **GPUI Kit ya tiene su capa de temas.** `ThemeRegistry` + `ThemeConfig` (JSON con unas 90 claves de color de componentes, radios, fuentes) + `MotionTokens` (duraciones y curvas). No la reemplazamos: **generamos su `ThemeConfig` desde nuestros tokens semánticos**, así inputs, pestañas, listas, scrollbars y menús combinan con el tema.
5. **Movimiento reducido.** macOS ("Reducir movimiento") y Windows ("Efectos de animación") tienen un ajuste de accesibilidad. GPUI Kit lo lee al iniciar (`gpui_base::reduce_motion`) y `with_animation` ya pinta un solo cuadro estático cuando está activo. **No hay que hacer nada extra**, solo usar `with_animation` y no animar a mano con timers.
6. **Contraste WCAG 2.x.** Texto normal ≥ 4.5:1, texto principal idealmente ≥ 7:1. Hay una prueba que lo revisa para cada tema incluido.

## 2b. Guías propias que se aplicaron

| Guía | Qué se tomó |
|---|---|
| Tu `DESIGN.md` del portafolio ("workstation editorial") | Es casi la propuesta A. El tema **Etiqueta** usa sus mismos neutros `ink` y su naranja `#ff6a1a`, radio de 4 px, bordes de 1 px sin sombras, la regla 60-30-10 (el naranja solo en lo que tiene intención) y etiquetas en mono mayúsculas. **Etiqueta Claro** es su modo claro (papel `#f4f2ed`) |
| Skill `frontend-design` | Elegir una dirección clara y ejecutarla con precisión. Tipografía con carácter (no la del sistema). Un momento de carga bien orquestado vale más que animaciones sueltas |
| Skill `hyperframes` (guía de movimiento) | La curva depende del papel (entrar: ease-out larga; moverse: ease-in-out; salir: ease-in). Las salidas duran 40-50 % menos que las entradas. Listas escalonadas de 40-80 ms. Nada de rebote en la interfaz, ni pulsos o "respiraciones" en bucle, ni que todo entre con fundido y subiendo |
| Skill `design-whimsy-injector` | Solo lo compatible con tu línea: cada detalle debe tener un propósito y respetar "Reducir movimiento". Se descartaron hover con elevación y sombra (chocan con el estilo sin sombras) |
| Regla `clean-code-and-principles` | Funciones cortas, sin código especulativo (YAGNI). Por eso se quitaron del código los tokens de movimiento hasta que haya animaciones que los usen, y la sección de Appearance se partió en funciones pequeñas |
| Remotion / HyperFrames | Son para **videos**, no para la interfaz de la app. Sirven para un video de demo de kuaky; por licencia conviene HyperFrames (ya lo tienes como skill) |

## 3. Cómo quedó en kuaky

```
assets/themes/*.json  ─┐                    ┌─► style::bg(), style::accent()…  (vistas de kuaky)
~/Library/Application  ├─► theme::parse ──► Palette (tokens semánticos)
  Support/kuaky/themes ┘        │                    
                                └─► ThemeConfig derivado ─► Theme de GPUI Kit (componentes)
```

- **`theme.rs`** carga los temas, resuelve alias, valida que estén todos los tokens y aplica el elegido.
- **`style.rs`** mantiene las mismas funciones de antes (`style::bg()`, `style::accent()`…), pero ahora leen el tema activo. Las vistas casi no cambiaron. Se añadieron `warn()`, `info()`, `merged()`, `on_accent()` y `lane()`, se quitaron los 22 colores escritos a mano que había (`0xfbbf24` y otros) y los botones con fondo de acento usan `on_accent()` para el texto.
- **GPUI Kit (nivel 3)** se llena desde una tabla (`KIT_MAP` en `theme.rs`): cada color de componente apunta a un token semántico con su opacidad.
- **Colores categóricos guardados** (líneas, sesiones, ramas del grafo se guardan como `0xrrggbb` en `state.json`): `style::lane(c)` reconoce la familia del color (turquesa, violeta, naranja…) y lo pinta con el color equivalente del tema. Así no hace falta migrar `state.json`.
- **Ajustes → Appearance:** System / Light / Dark, lista de temas con muestra de colores, carpeta de temas propios y botón "Reload themes".
- **`KUAKY_THEME=<nombre>`** fuerza un tema (útil para capturas): `KUAKY_THEME=Sumi cargo run`.

### Formato de un tema

```jsonc
{
  "name": "Sumi",
  "appearance": "dark",            // dark | light
  "description": "…",
  "kit": "derive",                 // derive: GPUI Kit toma los colores del tema · default: GPUI Kit sin tocar
  "primitives": { "sumi.950": "#14120f", "shu.600": "#d1402a" },
  "tokens": {                      // los 16 tokens semánticos son obligatorios
    "background": "{sumi.950}",
    "accent": "{shu.600}",
    "accent.foreground": "#fffaf2"
    // surface, surface.hover, border, text, text.muted, text.faint,
    // data.output, data.other,
    // status.success, status.danger, status.warning, status.info, status.merged
  },
  "categorical": ["…8 colores…"],  // vacío = los colores guardados se usan tal cual
  "agents": { "claude": "#d97a55", "codex": "…" },
  "components": { "list.active.background": "{accent}" },  // opcional: corrige un color de GPUI Kit
  "highlight": { },                // opcional: resaltado de sintaxis (formato de Zed)
  "font.display": ["Hiragino Mincho ProN", "Yu Mincho", "Georgia"],  // opcional: títulos y números grandes
  "brand.mark": true,              // opcional: sol de acento junto a "kuaky"
  "shape":  { "radius": 4, "radius.lg": 6 }
}
```

Para hacer un tema propio: copia uno de `assets/themes/` a `~/Library/Application Support/kuaky/themes/mi-tema.json`, cambia `name` y los colores, y pulsa **Reload themes**.

## 4. Lo que falta (siguiente fase, cuando elijas propuesta)

| Pendiente | Por qué |
|---|---|
| Usar `shape.radius` en las vistas propias | Hoy las vistas usan `.rounded_md()` fijo. Solo los componentes de GPUI Kit respetan el radio del tema. Etiqueta y Rejilla necesitan esquinas rectas |
| `on_accent()` en el botón "Allow" del chat | Es el único que falta. No se tocó `chat_view.rs` porque otra sesión lo está editando |
| Tokens de tipografía (`font.ui`, `font.mono`) | Tu DESIGN.md usa Geist y Geist Mono (licencia OFL, se pueden incluir en el binario con `add_fonts`). Las propuestas A y B dependen de etiquetas en mono mayúsculas |
| Recarga en vivo de la carpeta de temas | Hoy hay botón "Reload themes". Se puede vigilar la carpeta con `notify`, como hace `ThemeRegistry::watch_dir` |

## 5. Reglas de movimiento (para la fase de animaciones)

Salen de tu DESIGN.md (§6) y de la guía de movimiento de HyperFrames, ajustadas a una app de escritorio que se usa todo el día:

| Regla | Valor |
|---|---|
| Curva de entrada | `cubic-bezier(0.16, 1, 0.3, 1)` (out-expo). Es la misma que `easing_enter` de GPUI Kit |
| Curva de movimiento | `cubic-bezier(0.76, 0, 0.24, 1)` (in-out-quart) |
| Curva de salida | `cubic-bezier(0.4, 0, 1, 1)` (`easing_exit` de GPUI Kit) |
| Duraciones | micro 120-160 ms · normal 200-240 ms · lenta 320 ms. Las salidas son ~45 % más cortas que las entradas |
| Escalonado | 50 ms entre filas, máximo 8 filas |
| Resortes | Solo en posición (el indicador de pestaña), con amortiguación 0.85. Opacidad y color nunca rebotan |
| Prohibido | Rebotes en la interfaz, pulsos o "respiraciones" en bucle, sombras en hover, animar todo lo que aparece, animar dentro del editor y la terminal |
| Accesibilidad | "Reducir movimiento" ya lo respeta `with_animation`. No animar con timers propios |

GPUI no tiene filtro de desenfoque en elementos, así que las transiciones con blur de HyperFrames se sustituyen por fundidos cortos.

## 6. Animaciones implementadas

Todo vive en [`motion.rs`](../../crates/kuaky/src/motion.rs): curva de entrada out-expo, tiempos `FAST` 160 ms, `NORMAL` 240 ms, `SLOW` 320 ms, desfase de filas 50 ms (máx. 8 filas).

| # | Dónde | Qué hace | Código |
|---|---|---|---|
| 1 | Cambiar entre Home y un proyecto | La vista nueva entra con un fundido de 160 ms, sin moverse | `app.rs` (`view-…`) |
| 2 | Filas de "Work in progress" | Pasan de tenues a su color, una tras otra cada 50 ms | `home.rs` → `motion::row_in` |
| 3 | Números de Home | Cuentan desde el valor anterior al nuevo al cambiar el rango o el agente (320 ms) | `home.rs` → `Counter` |
| 4 | Línea con agente trabajando | Un segmento recorre una pista a velocidad constante (1.2 s, repite) | `home.rs` → `progress_sweep` |
| 5 | Aviso *needs you* | Aparece con un fundido una sola vez | `home.rs` |
| 6 | Settings | Entra con un fundido de 160 ms | `app.rs` (`settings-in`) |
| 7 | Rango de días seleccionado | Su fondo aparece con un fundido de 160 ms | `home.rs` (`range-bg-…`) |
| 8 | Barras de límite (5 h / semana) | Se llenan desde 0 al abrir Home (320 ms) | `home.rs` (`limit-…`) |

### Segunda tanda (como Claude Code, sin subir la carga)

| # | Dónde | Qué hace | Costo |
|---|---|---|---|
| 9 | Todos los "trabajando" (chat, subagentes, Actions) | Spinner de glifo como Claude Code (`· ✢ ✳ ✶ ✻ ✽`), 8 cuadros/s. Antes era un círculo girando a 60 | **Baja** la carga |
| 10 | Línea de actividad del chat | Un brillo recorre el verbo ("Thinking…", "Running a command…"); quieto mientras espera permiso | 15 cuadros/s, solo esa línea |
| 11 | Mensajes y tarjetas nuevas del turno en curso | Fundido de 160 ms; el historial no se anima | Solo mientras dura |
| 12 | Herramienta en curso | Su ícono parpadea lento (1 por segundo) | 8 cuadros/s |
| 13 | Al expandir un grupo de herramientas o un razonamiento | El contenido entra con fundido | Solo mientras dura |
| 14 | Punto de estado en el sidebar | Al cambiar de estado entra con fundido; si pasa a rojo (te necesita) destella dos veces | Solo al cambiar |
| 15 | Quick open (⌘P), avisos de Actions, cambio de pestaña | Fundido corto | Solo mientras dura |

**Regla de carga:** lo que se repite usa `with_max_fps` (8 o 15 cuadros/s) y `repeat_synced` para que todo vaya con el mismo reloj; lo que pasa una vez dura menos de 320 ms. Antes, mientras un agente trabajaba, kuaky repintaba la ventana ~60 veces por segundo (medido: 20-44 % de CPU en un MacBook Air M1).

**Diferencias con el plan:** el indicador del rango no se desliza (GPUI no da el ancho de cada opción sin medirlo), así que solo hace un fundido; Settings no escala porque GPUI no transforma `div`s, solo SVG. No hay salidas animadas: GPUI quita el elemento de inmediato.

## Fuentes

- Design Tokens Format Module 2025.10 (W3C DTCG): https://www.w3.org/community/reports/design-tokens/CG-FINAL-format-20251028/ · anuncio: https://w3.org/community/design-tokens/2025/10/28/design-tokens-specification-reaches-first-stable-version/
- Niveles de tokens: https://playbook.ebay.com/design-system/tokens · https://dev.wix.com/docs/wix-design-system/foundations/tokens-and-theming-basics · https://anvil.servicetitan.com/blog/posts/2026-04-13-introducing-tier-3-tokens
- Temas de Zed: https://zed.dev/docs/themes · https://zed.dev/docs/extensions/themes
- Temas de GPUI Kit: https://longbridge.github.io/gpui-component/docs/theme (y el código de `gpui-component-0.11.0/src/theme/`)
- Movimiento reducido: https://support.apple.com/guide/mac-help/mchlc03f57a1 · https://web.dev/learn/accessibility/motion
- Guías locales: `~/Desktop/proyectos/personal/portafolio/DESIGN.md`, skills `frontend-design`, `hyperframes` (`references/motion.md`), `design-whimsy-injector`, regla `clean-code-and-principles.md`
