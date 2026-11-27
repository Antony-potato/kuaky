<p align="center">
  <img src="./assets/icon/kuaky.svg" width="80" height="80" alt="Logo de kuaky" />
</p>

<h1 align="center">kuaky</h1>

<p align="center">Un editor de escritorio ligero y nativo para trabajar con agentes de código.</p>

<p align="center">
  <a href="https://github.com/Antony-potato/kuaky/releases/latest"><strong>Descargar</strong></a> ·
  <a href="#instalar">Instalar</a> ·
  <a href="./CONTRIBUTING.md">Contribuir</a> ·
  <a href="./docs/">Notas de diseño</a>
</p>

kuaky junta en una sola ventana tus agentes de código, tu código y tu trabajo con git. Está escrito en Rust con [GPUI](https://github.com/zed-industries/zed) y [GPUI Kit](https://gpui-kit.com/), así que abre rápido y consume poco.

Funciona con Claude Code, Codex, Gemini CLI, OpenCode y Antigravity. Si un agente está instalado y con sesión iniciada en tu computadora, kuaky lo puede usar.

## Qué te da

- **Tus suscripciones.** kuaky no vende tokens ni te pide API keys. Usa los agentes en los que ya iniciaste sesión.
- **Tu consumo de un vistazo.** El inicio muestra los tokens y el costo equivalente en API de todos tus agentes y proyectos, junto a los límites de tu plan.
- **Un chat de verdad con cada agente.** Planes, herramientas, botones para dar permisos y un diff de cada edición que puedes aceptar o rechazar.
- **Agentes que trabajan juntos.** Un agente puede abrir una sesión en otro agente y pasarle parte del trabajo.
- **Un mapa de tu trabajo.** Las sesiones se agrupan en líneas por meta, con su estado en vivo: trabajando, te necesita, terminado.
- **Git integrado.** Un worktree por sesión, diffs entre ramas y pull requests que puedes revisar, fusionar o usar para empezar una sesión.
- **Un editor cuando lo necesitas.** Árbol de archivos, resaltado de sintaxis, búsqueda en el proyecto, terminales y servidores de lenguaje para diagnósticos, autocompletado e ir a la definición.
- **Diagramas y vista previa.** Los agentes pueden dibujar un diagrama para explicar algo, o mostrar tu app corriendo en una pestaña.
- **Abierto.** El código es [GPL-3.0](./LICENSE). Ve [Licencia](#licencia).

## Instalar

Descarga la última versión en [Releases](https://github.com/Antony-potato/kuaky/releases/latest):

| Plataforma            | Archivo                              |
| --------------------- | ------------------------------------ |
| macOS (Apple Silicon) | `kuaky-X.Y.Z-macos-arm64.zip`        |
| Windows (x64)         | `kuaky-X.Y.Z-windows-x64-setup.exe`  |

Después kuaky se actualiza sola: cuando sale una versión nueva, aparece el botón **Update** en la barra de título.

> [!NOTE]
> Los instaladores todavía no están firmados.
> - **Windows:** si ves "Windows protegió tu PC", da clic en **Más información → Ejecutar de todas formas**. El instalador no necesita permisos de administrador.
> - **macOS:** mueve `kuaky.app` a Aplicaciones y corre una vez `xattr -dr com.apple.quarantine /Applications/kuaky.app`.

### Inicia sesión en un agente

kuaky necesita al menos un agente instalado y con sesión iniciada, como [Claude Code](https://claude.com/product/claude-code) o [Codex CLI](https://developers.openai.com/codex/cli). En **Settings → Agents** puedes instalar los que te falten.

### Compilar desde el código

```bash
cargo run                  # abre kuaky
cargo test --workspace     # corre las pruebas
script/bundle-macos.sh     # macOS: crea target/release/bundle/kuaky.app
```

## Contribuir

Los arreglos chicos y enfocados son los que más fácil entran. Para un cambio grande, abre primero un issue para ponernos de acuerdo.

- Lee [CONTRIBUTING.md](./CONTRIBUTING.md) antes de abrir un pull request. El [CLA](./CLA.md) se firma una sola vez, en tu primer PR.
- ¿Encontraste un problema de seguridad? No abras un issue: ve [SECURITY.md](./SECURITY.md).
- Las [notas de diseño](./docs/) explican el stack y el plan.

## Licencia

kuaky es software libre: puedes usarlo, modificarlo y compartirlo bajo la [GNU General Public License v3.0 o posterior](./LICENSE). El código tomado de otros proyectos está en [THIRD_PARTY.md](./THIRD_PARTY.md).

## Marca y forks

La GPL aplica **solo al código**. No da ningún derecho sobre el nombre **kuaky**, su logo ni su ícono.

Si haces un fork o redistribuyes kuaky:

- **Usa tu propio nombre e ícono** en cualquier fork público, build, sitio web o tienda de apps.
- **Deja claro que tu proyecto no es oficial** y que no está afiliado ni respaldado por kuaky.

© 2026 Antonio Cortazar Jimenez
