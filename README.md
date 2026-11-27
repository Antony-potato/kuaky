# kuaky

Editor de escritorio ligero, escrito en Rust con [GPUI](https://github.com/zed-industries/zed) y [GPUI Kit](https://gpui-kit.com/), para trabajar con agentes de código (Claude Code, Codex, Antigravity) usando las sesiones que ya tienes iniciadas.

- **Inicio:** dashboard de consumo de tokens de todos tus agentes.
- **Proyecto:** árbol de archivos, terminales con tus agentes, ramas, diffs y PRs.

Plataformas: macOS (Apple Silicon) y Windows (x64).

## Instalar

Descarga la última versión en [Releases](https://github.com/Antony-potato/kuaky/releases/latest). kuaky se actualiza sola después: avisa en la barra de título cuando hay una versión nueva.

**Windows:** corre `kuaky-X.Y.Z-windows-x64-setup.exe`. Se instala para tu usuario, sin permisos de administrador. El instalador todavía no está firmado: si Windows muestra "Windows protegió tu PC", da clic en **Más información → Ejecutar de todas formas**.

**macOS:** descomprime `kuaky-X.Y.Z-macos-arm64.zip` y mueve `kuaky.app` a Aplicaciones. La app todavía no está firmada por Apple; la primera vez quítale la cuarentena:

```bash
xattr -dr com.apple.quarantine /Applications/kuaky.app
```

### Compilar desde el código

```bash
script/bundle-macos.sh             # macOS: crea target/release/bundle/kuaky.app
script/bundle-macos.sh --install   # y la copia a ~/Applications
cargo build --release -p kuaky     # Windows: target/release/kuaky.exe
```

## Desarrollo

```bash
cargo run            # abre kuaky
cargo test --workspace   # pruebas
cargo run --release -p kuaky-usage --example resumen   # consumo local en la terminal
```

Documentación de diseño en [`docs/`](docs/): investigación, stack, competencia y plan.

¿Quieres contribuir? Ve [`CONTRIBUTING.md`](CONTRIBUTING.md). ¿Encontraste un problema de seguridad? Ve [`SECURITY.md`](SECURITY.md). Código de terceros: [`THIRD_PARTY.md`](THIRD_PARTY.md).

## Licencia

kuaky es software libre: puedes usarlo, modificarlo y redistribuirlo bajo la [GNU General Public License v3.0 o posterior](LICENSE). Las contribuciones se aceptan con el [CLA](CLA.md).

© 2026 Antonio Cortazar Jimenez
