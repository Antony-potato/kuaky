# Código de terceros

Archivos copiados o adaptados de otros proyectos. Cada archivo adaptado indica su origen en el encabezado.

| Archivo de kuaky | Origen | Licencia |
|---|---|---|
| `crates/kuaky/assets/icons/*.svg` | [LobeHub Icons](https://github.com/lobehub/lobe-icons), paquete `@lobehub/icons-static-svg` 1.106.0. Los monocromáticos (`anthropic`, `openai`, `grok`, `opencode`) se recolorearon a `#E8EAEE` para el tema oscuro | MIT |

| Íconos de interfaz (Lucide) | [Lucide Icons](https://lucide.dev/) vía `gpui-kit-assets` 0.11.0 (`house`, `folder`, `git-compare`, `git-branch`, `git-pull-request`, `git-graph`, `git-merge`, `git-merge-conflict`, `terminal`, `search`, `settings`, `plus`, `x`, `chevron-*`, `check`, `pencil`, `trash`, `refresh-cw`) | ISC (derivados de Feather: MIT) |

| `crates/kuaky/assets/canvas/rough.js` | [Rough.js](https://github.com/rough-stuff/rough) 4.6.6 (`bundled/rough.js`, sin cambios), trazo a mano del lienzo de diagramas | MIT |

| `crates/kuaky-usage/data/prices.json` | Subconjunto de la tabla de precios de [LiteLLM](https://github.com/BerriAI/litellm) (`model_prices_and_context_window.json`); kuaky también descarga la tabla completa una vez al día | MIT |

Formatos de logs de agentes (Claude Code, Codex, Gemini CLI, OpenCode, Grok CLI, Antigravity CLI) documentados a partir de splitrail (MIT), ccusage (MIT) y tokscale (MIT); el código de `kuaky-usage` es propio.

Los logotipos son marcas de sus respectivos dueños (Anthropic, OpenAI, Google, xAI, SST/opencode) y se usan solo para identificar cada herramienta.
