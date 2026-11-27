# Contribuir a kuaky

¡Gracias por ayudar! Issues y pull requests son bienvenidos.

## Antes de tu primer PR: el CLA

kuaky es software libre bajo [GPL-3.0-or-later](LICENSE). Para aceptar contribuciones pedimos firmar un [acuerdo de contribución (CLA)](CLA.md):

- Sigues siendo dueño de tu código.
- Le das permiso al mantenedor de usarlo en kuaky con cualquier licencia, incluidas versiones comerciales.

Se firma una sola vez: al abrir tu primer PR, el bot de CLA Assistant te deja un enlace, entras con tu cuenta de GitHub y aceptas.

## El PR

- Un cambio por PR, con una descripción de qué arregla o agrega.
- El CI compila y corre las pruebas en macOS y Windows; tiene que pasar:

```bash
cargo build --workspace --locked
cargo test --workspace --locked
```

- Escribe el código como el que lo rodea: mismos nombres, mismo estilo, misma densidad de comentarios.

## Problemas de seguridad

No abras un issue público: ve [SECURITY.md](SECURITY.md).

---

# Contributing (English)

Issues and pull requests are welcome. kuaky is licensed under [GPL-3.0-or-later](LICENSE). Before your first PR is merged you sign the [CLA](CLA.md) once: you keep your copyright and allow the maintainer to use your contribution under any license, including commercial ones. The CLA Assistant bot posts the link on your PR. CI must pass (`cargo build --workspace --locked` and `cargo test --workspace --locked`). Security issues: see [SECURITY.md](SECURITY.md).
