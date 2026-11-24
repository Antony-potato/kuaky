# kuaky

Editor de escritorio ligero, escrito en Rust con [GPUI](https://github.com/zed-industries/zed) y [GPUI Kit](https://gpui-kit.com/), para trabajar con agentes de código (Claude Code, Codex, Antigravity) usando las sesiones que ya tienes iniciadas.

- **Inicio:** dashboard de consumo de tokens de todos tus agentes.
- **Proyecto:** árbol de archivos, terminales con tus agentes, ramas, diffs y PRs.

Plataformas: macOS y Windows.

## Instalar en macOS

```bash
script/bundle-macos.sh             # crea target/release/bundle/kuaky.app
script/bundle-macos.sh --install   # y la copia a ~/Applications
```

## Desarrollo

```bash
cargo run            # abre kuaky
cargo test --workspace   # pruebas
cargo run --release -p kuaky-usage --example resumen   # consumo local en la terminal
```

Documentación de diseño en [`docs/`](docs/): investigación, stack, competencia y plan.
