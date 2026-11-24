//! Temas de kuaky.
//!
//! Tokens de diseño en tres niveles (ver `docs/diseno/temas.md`):
//!
//! 1. **Primitivos** (`primitives`): valores crudos con nombre de color (`ink.950`, `shu.600`).
//! 2. **Semánticos** (`tokens`): para qué sirve el color (`background`, `accent`,
//!    `status.warning`). Es lo único que usa la interfaz, a través de `style::*`.
//! 3. **Componentes** (`components`): colores de GPUI Kit (`list.active.background`…).
//!    Con `"kit": "derive"` se calculan desde los semánticos; con `"kit": "default"`
//!    se deja el tema oscuro o claro de GPUI Kit sin tocar.
//!
//! Un valor puede ser `#rrggbb`, `#rrggbbaa` o un alias `{nombre}` a otro token
//! (la sintaxis de alias del formato DTCG 2025.10).
//!
//! Los temas vienen de `assets/themes/*.json` (incluidos en el binario) y de
//! `<config>/kuaky/themes/*.json`, que gana si repite un nombre.

use crate::state;
use gpui_kit::component::{Theme, ThemeConfig, ThemeMode, ThemeRegistry};
use gpui_kit::{App, Global, Rgba, SharedString, Window};
use serde::Deserialize;
use std::collections::HashMap;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::{LazyLock, RwLock};

/// Temas que vienen con kuaky.
const BUILTIN: &[&str] = &[
    include_str!("../assets/themes/kuaky-original.json"),
    include_str!("../assets/themes/sumi.json"),
    include_str!("../assets/themes/papel.json"),
];

pub const DEFAULT_DARK: &str = "Sumi";
pub const DEFAULT_LIGHT: &str = "Papel";

// ---------------------------------------------------------------------------
// Archivo de tema
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
struct ThemeFile {
    name: String,
    #[serde(default)]
    appearance: Appearance,
    #[serde(default)]
    description: String,
    #[serde(default)]
    kit: KitMode,
    #[serde(default)]
    primitives: HashMap<String, String>,
    #[serde(default)]
    tokens: HashMap<String, String>,
    #[serde(default)]
    categorical: Vec<String>,
    #[serde(default)]
    agents: HashMap<String, String>,
    #[serde(default)]
    components: serde_json::Map<String, serde_json::Value>,
    #[serde(default)]
    highlight: Option<serde_json::Value>,
    #[serde(default)]
    shape: Shape,
    /// Fuentes para títulos y números grandes, en orden de preferencia.
    /// Se usa la primera instalada; si ninguna lo está, la de la interfaz.
    #[serde(default, rename = "font.display")]
    font_display: Vec<String>,
    /// Sol de color de acento junto al nombre de kuaky en la barra de título.
    #[serde(default, rename = "brand.mark")]
    brand_mark: bool,
}

#[derive(Deserialize, Clone, Copy, PartialEq, Eq, Default, Debug)]
#[serde(rename_all = "lowercase")]
pub enum Appearance {
    #[default]
    Dark,
    Light,
}

#[derive(Deserialize, Clone, Copy, PartialEq, Eq, Default)]
#[serde(rename_all = "lowercase")]
enum KitMode {
    #[default]
    Derive,
    Default,
}

#[derive(Deserialize, Clone, Copy, Debug)]
pub struct Shape {
    #[serde(default = "default_radius")]
    pub radius: f32,
    #[serde(rename = "radius.lg", default = "default_radius_lg")]
    pub radius_lg: f32,
}

fn default_radius() -> f32 {
    6.
}
fn default_radius_lg() -> f32 {
    8.
}

impl Default for Shape {
    fn default() -> Self {
        Self { radius: default_radius(), radius_lg: default_radius_lg() }
    }
}

// ---------------------------------------------------------------------------
// Paleta resuelta (lo que lee `style::*`)
// ---------------------------------------------------------------------------

/// Tokens semánticos ya resueltos a colores.
#[derive(Clone, Debug)]
pub struct Palette {
    pub appearance: Appearance,
    pub background: Rgba,
    pub surface: Rgba,
    pub surface_hover: Rgba,
    pub border: Rgba,
    pub text: Rgba,
    pub text_muted: Rgba,
    pub text_faint: Rgba,
    pub accent: Rgba,
    pub accent_foreground: Rgba,
    pub data_output: Rgba,
    pub data_other: Rgba,
    pub success: Rgba,
    pub danger: Rgba,
    pub warning: Rgba,
    pub info: Rgba,
    pub merged: Rgba,
    /// 8 colores para líneas, ramas y sesiones. Vacío = se usan los colores guardados tal cual.
    pub categorical: Vec<Rgba>,
    /// Color por agente (`claude`, `codex`, `gemini`, `antigravity`, `opencode`, `grok`).
    pub agents: HashMap<String, Rgba>,
    pub shape: Shape,
    /// Fuentes candidatas para títulos; `display_font` es la que quedó elegida.
    pub display_candidates: Vec<String>,
    pub display_font: Option<SharedString>,
    pub brand_mark: bool,
}

/// Un tema cargado.
pub struct KuakyTheme {
    pub name: String,
    pub description: String,
    pub palette: Palette,
    /// Tema de GPUI Kit derivado (`None` = el de GPUI Kit por defecto).
    kit: Option<Rc<ThemeConfig>>,
    /// De dónde salió (`None` = incluido en kuaky).
    pub source: Option<PathBuf>,
}

impl KuakyTheme {
    pub fn is_dark(&self) -> bool {
        self.palette.appearance == Appearance::Dark
    }
}

/// Convierte `#rrggbb` / `#rrggbbaa` en color.
pub fn parse_hex(s: &str) -> Option<Rgba> {
    let hex = s.trim().strip_prefix('#')?;
    let v = u32::from_str_radix(hex, 16).ok()?;
    let (rgb, a) = match hex.len() {
        6 => (v, 0xff),
        8 => (v >> 8, v & 0xff),
        _ => return None,
    };
    Some(Rgba {
        r: ((rgb >> 16) & 0xff) as f32 / 255.,
        g: ((rgb >> 8) & 0xff) as f32 / 255.,
        b: (rgb & 0xff) as f32 / 255.,
        a: a as f32 / 255.,
    })
}

fn to_hex(c: Rgba, alpha: f32) -> String {
    let ch = |x: f32| (x.clamp(0., 1.) * 255.).round() as u8;
    format!("#{:02x}{:02x}{:02x}{:02x}", ch(c.r), ch(c.g), ch(c.b), ch(alpha))
}

struct Resolver<'a> {
    primitives: &'a HashMap<String, String>,
    tokens: &'a HashMap<String, String>,
}

impl Resolver<'_> {
    /// Sigue alias `{nombre}` (primero en tokens, luego en primitivos) hasta un color.
    fn color(&self, value: &str) -> anyhow::Result<Rgba> {
        let mut v = value.trim().to_string();
        for _ in 0..8 {
            if let Some(name) = v.strip_prefix('{').and_then(|r| r.strip_suffix('}')) {
                v = self
                    .tokens
                    .get(name)
                    .or_else(|| self.primitives.get(name))
                    .ok_or_else(|| anyhow::anyhow!("alias desconocido: {{{name}}}"))?
                    .trim()
                    .to_string();
            } else {
                return parse_hex(&v).ok_or_else(|| anyhow::anyhow!("color inválido: {v}"));
            }
        }
        anyhow::bail!("alias circular: {value}")
    }

    fn token(&self, key: &str) -> anyhow::Result<Rgba> {
        let raw = self.tokens.get(key).ok_or_else(|| anyhow::anyhow!("falta el token {key}"))?;
        self.color(raw)
    }
}

/// Lee un archivo de tema y lo resuelve.
pub fn parse(json: &str) -> anyhow::Result<KuakyTheme> {
    let file: ThemeFile = serde_json::from_str(json)?;
    let r = Resolver { primitives: &file.primitives, tokens: &file.tokens };
    let palette = palette(&file, &r).map_err(|e| anyhow::anyhow!("{}: {e}", file.name))?;
    let kit = match file.kit {
        KitMode::Default => None,
        KitMode::Derive => Some(Rc::new(kit_config(&file, &palette, &r)?)),
    };
    Ok(KuakyTheme { name: file.name, description: file.description, palette, kit, source: None })
}

/// Nivel 2: los tokens semánticos del archivo, resueltos a colores.
fn palette(file: &ThemeFile, r: &Resolver) -> anyhow::Result<Palette> {
    let t = |k: &str| r.token(k);
    Ok(Palette {
        appearance: file.appearance,
        background: t("background")?,
        surface: t("surface")?,
        surface_hover: t("surface.hover")?,
        border: t("border")?,
        text: t("text")?,
        text_muted: t("text.muted")?,
        text_faint: t("text.faint")?,
        accent: t("accent")?,
        accent_foreground: t("accent.foreground")?,
        data_output: t("data.output")?,
        data_other: t("data.other")?,
        success: t("status.success")?,
        danger: t("status.danger")?,
        warning: t("status.warning")?,
        info: t("status.info")?,
        merged: t("status.merged")?,
        categorical: file.categorical.iter().map(|v| r.color(v)).collect::<anyhow::Result<_>>()?,
        agents: file.agents.iter().map(|(k, v)| Ok((k.clone(), r.color(v)?))).collect::<anyhow::Result<_>>()?,
        shape: file.shape,
        display_candidates: file.font_display.clone(),
        display_font: None,
        brand_mark: file.brand_mark,
    })
}

/// Nivel 3: cada color de GPUI Kit apunta a un token semántico, con opacidad.
/// `clear` = transparente; `categorical.N` = el color categórico N.
const KIT_MAP: &[(&str, &str, f32)] = &[
    ("background", "background", 1.),
    ("foreground", "text", 1.),
    ("border", "border", 1.),
    ("muted.background", "surface", 1.),
    ("muted.foreground", "text.muted", 1.),
    ("accent.background", "surface.hover", 1.),
    ("accent.foreground", "text", 1.),
    ("primary.background", "accent", 1.),
    ("primary.hover.background", "accent", 0.9),
    ("primary.active.background", "accent", 1.),
    ("primary.foreground", "accent.foreground", 1.),
    ("secondary.background", "surface", 1.),
    ("secondary.hover.background", "surface.hover", 1.),
    ("secondary.active.background", "surface.hover", 1.),
    ("secondary.foreground", "text", 1.),
    ("popover.background", "surface", 1.),
    ("popover.foreground", "text", 1.),
    ("input.border", "border", 1.),
    ("ring", "accent", 1.),
    ("caret", "accent", 1.),
    ("selection.background", "accent", 0.3),
    ("link.foreground", "accent", 1.),
    ("link.hover.foreground", "accent", 1.),
    ("link.active.foreground", "accent", 1.),
    ("list.background", "background", 1.),
    ("list.even.background", "background", 1.),
    ("list.head.background", "surface", 1.),
    ("list.hover.background", "surface.hover", 1.),
    ("list.active.background", "accent", 0.15),
    ("list.active.border", "accent", 1.),
    ("list.highlight.foreground", "accent", 1.),
    ("table.background", "background", 1.),
    ("table.even.background", "background", 1.),
    ("table.head.background", "surface", 1.),
    ("table.head.foreground", "text.muted", 1.),
    ("table.hover.background", "surface.hover", 1.),
    ("table.active.background", "accent", 0.15),
    ("table.active.border", "accent", 1.),
    ("table.row.border", "border", 1.),
    ("title_bar.background", "surface", 1.),
    ("title_bar.border", "border", 1.),
    ("status_bar.background", "surface", 1.),
    ("status_bar.border", "border", 1.),
    ("tab_bar.background", "surface", 1.),
    ("tab_bar.segmented.background", "surface", 1.),
    ("tab.background", "clear", 0.),
    ("tab.foreground", "text.muted", 1.),
    ("tab.active.background", "background", 1.),
    ("tab.active.foreground", "text", 1.),
    ("sidebar.background", "surface", 1.),
    ("sidebar.foreground", "text", 1.),
    ("sidebar.border", "border", 1.),
    ("sidebar.accent.background", "surface.hover", 1.),
    ("sidebar.accent.foreground", "text", 1.),
    ("sidebar.primary.background", "accent", 1.),
    ("sidebar.primary.foreground", "accent.foreground", 1.),
    ("scrollbar.background", "clear", 0.),
    ("scrollbar.thumb.background", "text.muted", 0.35),
    ("scrollbar.thumb.hover.background", "text.muted", 0.6),
    ("success.background", "status.success", 1.),
    ("success.foreground", "background", 1.),
    ("danger.background", "status.danger", 1.),
    ("danger.foreground", "background", 1.),
    ("warning.background", "status.warning", 1.),
    ("warning.foreground", "background", 1.),
    ("info.background", "status.info", 1.),
    ("info.foreground", "background", 1.),
    ("drag_border", "accent", 1.),
    ("drop_target.background", "accent", 0.25),
    ("window.border", "border", 1.),
    ("skeleton.background", "surface", 1.),
    ("progress_bar.background", "accent", 1.),
    ("slider.bar.background", "accent", 1.),
    ("slider.thumb.background", "text", 1.),
    ("switch.background", "border", 1.),
    ("group_box.background", "surface", 1.),
    ("group_box.foreground", "text", 1.),
    ("accordion.background", "surface", 1.),
    ("description_list_label.foreground", "text.muted", 1.),
    ("chart.1", "categorical.0", 1.),
    ("chart.2", "categorical.1", 1.),
    ("chart.3", "categorical.2", 1.),
    ("chart.4", "categorical.3", 1.),
    ("chart.5", "categorical.4", 1.),
    ("chart.bullish", "status.success", 1.),
    ("chart.bearish", "status.danger", 1.),
    ("chart.grid", "border", 1.),
];

impl Palette {
    /// Color de un token semántico por su nombre del archivo de tema.
    fn token(&self, name: &str) -> Option<Rgba> {
        if let Some(i) = name.strip_prefix("categorical.") {
            let i: usize = i.parse().ok()?;
            return Some(self.categorical.get(i).copied().unwrap_or(self.accent));
        }
        Some(match name {
            "background" => self.background,
            "surface" => self.surface,
            "surface.hover" => self.surface_hover,
            "border" => self.border,
            "text" => self.text,
            "text.muted" => self.text_muted,
            "text.faint" => self.text_faint,
            "accent" => self.accent,
            "accent.foreground" => self.accent_foreground,
            "data.output" => self.data_output,
            "data.other" => self.data_other,
            "status.success" => self.success,
            "status.danger" => self.danger,
            "status.warning" => self.warning,
            "status.info" => self.info,
            "status.merged" => self.merged,
            _ => return None,
        })
    }
}

/// Tema de GPUI Kit derivado de la paleta; `components` del archivo va encima.
fn kit_config(file: &ThemeFile, p: &Palette, r: &Resolver) -> anyhow::Result<ThemeConfig> {
    let mut colors = serde_json::Map::new();
    for &(key, token, alpha) in KIT_MAP {
        let hex = match p.token(token) {
            Some(c) => to_hex(c, c.a * alpha),
            None => "#00000000".to_string(),
        };
        colors.insert(key.to_string(), hex.into());
    }
    for (k, v) in &file.components {
        let v = match v.as_str() {
            Some(s) if s.starts_with('{') => {
                let c = r.color(s)?;
                to_hex(c, c.a).into()
            }
            _ => v.clone(),
        };
        colors.insert(k.clone(), v);
    }
    let mut cfg = serde_json::json!({
        "name": format!("kuaky/{}", file.name),
        "mode": if p.appearance == Appearance::Dark { "dark" } else { "light" },
        "radius": p.shape.radius.round() as usize,
        "radius.lg": p.shape.radius_lg.round() as usize,
        "colors": colors,
    });
    if let Some(hl) = &file.highlight {
        cfg["highlight"] = hl.clone();
    }
    Ok(serde_json::from_value(cfg)?)
}

// ---------------------------------------------------------------------------
// Tema activo
// ---------------------------------------------------------------------------

static ACTIVE: LazyLock<RwLock<Palette>> =
    LazyLock::new(|| RwLock::new(parse(BUILTIN[0]).expect("el tema Kuaky Original es válido").palette));

/// Lee la paleta activa. `style::*` pasa por aquí.
pub fn with<R>(f: impl FnOnce(&Palette) -> R) -> R {
    let guard = ACTIVE.read().unwrap_or_else(|e| e.into_inner());
    f(&guard)
}

/// Temas disponibles.
#[derive(Default)]
pub struct Registry {
    pub themes: Vec<KuakyTheme>,
    pub active: String,
}

impl Global for Registry {}

/// Carpeta de temas del usuario: `<config>/kuaky/themes`.
pub fn user_dir() -> Option<PathBuf> {
    Some(dirs::config_dir()?.join("kuaky").join("themes"))
}

fn load_all() -> Vec<KuakyTheme> {
    let mut themes: Vec<KuakyTheme> = Vec::new();
    let mut add = |t: KuakyTheme| {
        themes.retain(|x| x.name != t.name);
        themes.push(t);
    };
    for json in BUILTIN {
        match parse(json) {
            Ok(t) => add(t),
            Err(err) => tracing::error!(%err, "tema incluido inválido"),
        }
    }
    if let Some(dir) = user_dir()
        && let Ok(entries) = std::fs::read_dir(&dir)
    {
        let mut paths: Vec<PathBuf> =
            entries.flatten().map(|e| e.path()).filter(|p| p.extension().is_some_and(|x| x == "json")).collect();
        paths.sort();
        for path in paths {
            match std::fs::read_to_string(&path).map_err(anyhow::Error::from).and_then(|s| parse(&s)) {
                Ok(mut t) => {
                    t.source = Some(path);
                    add(t);
                }
                Err(err) => tracing::warn!(path = %path.display(), %err, "tema del usuario inválido"),
            }
        }
    }
    themes
}

pub fn init(cx: &mut App) {
    cx.set_global(Registry { themes: load_all(), active: String::new() });
    apply(None, cx);
}

/// Vuelve a leer los temas (incluidos y de la carpeta del usuario) y aplica el elegido.
pub fn reload(cx: &mut App) {
    let reg = cx.global_mut::<Registry>();
    reg.themes = load_all();
    reg.active.clear();
    apply(None, cx);
}

/// Nombre del tema que toca según los ajustes y la apariencia del sistema.
/// `KUAKY_THEME=<nombre>` lo fuerza (capturas de desarrollo).
fn wanted(window: Option<&Window>, cx: &App) -> String {
    if let Ok(name) = std::env::var("KUAKY_THEME") {
        return name;
    }
    let s = &state::get(cx).settings.theme;
    let dark = match s.mode.as_str() {
        "light" => false,
        "system" | "" => system_is_dark(window, cx),
        _ => true,
    };
    if dark { s.dark_or_default().to_string() } else { s.light_or_default().to_string() }
}

/// El sistema operativo está en modo oscuro.
pub fn system_is_dark(window: Option<&Window>, cx: &App) -> bool {
    let appearance = window.map(|w| w.appearance()).unwrap_or_else(|| cx.window_appearance());
    matches!(appearance, gpui_kit::WindowAppearance::Dark | gpui_kit::WindowAppearance::VibrantDark)
}

/// Modo después de elegir un tema en Settings. Elegir un tema siempre lo
/// muestra: en "system" se queda en "system" solo si el sistema ya está en la
/// apariencia de ese tema; si no, pasa al modo del tema.
pub fn mode_after_pick(mode: &str, system_dark: bool, theme_dark: bool) -> &'static str {
    let follows_system = mode == "system" || mode.is_empty();
    match (follows_system && system_dark == theme_dark, theme_dark) {
        (true, _) => "system",
        (false, true) => "dark",
        (false, false) => "light",
    }
}

/// Aplica el tema elegido en los ajustes. Llamar tras cambiar `settings.theme`
/// o cuando cambia la apariencia del sistema.
pub fn apply(window: Option<&Window>, cx: &mut App) {
    let name = wanted(window, cx);
    let reg = cx.global::<Registry>();
    let Some(theme) = reg.themes.iter().find(|t| t.name == name).or_else(|| reg.themes.first()) else {
        return;
    };
    if reg.active == theme.name && !reg.active.is_empty() && cx.has_global::<Theme>() {
        return;
    }
    let (name, mut palette, kit, dark) = (theme.name.clone(), theme.palette.clone(), theme.kit.clone(), theme.is_dark());
    palette.display_font = installed_font(&palette.display_candidates, cx);
    *ACTIVE.write().unwrap_or_else(|e| e.into_inner()) = palette;
    cx.global_mut::<Registry>().active = name;

    let mode = if dark { ThemeMode::Dark } else { ThemeMode::Light };
    if !cx.has_global::<Theme>() {
        Theme::change(mode, None, cx);
    }
    let kit = kit.unwrap_or_else(|| {
        let r = ThemeRegistry::global(cx);
        if dark { r.default_dark_theme().clone() } else { r.default_light_theme().clone() }
    });
    {
        let t = Theme::global_mut(cx);
        if dark {
            t.dark_theme = kit;
        } else {
            t.light_theme = kit;
        }
    }
    Theme::change(mode, None, cx);
}

/// La primera fuente de la lista que está instalada en este equipo.
fn installed_font(candidates: &[String], cx: &App) -> Option<SharedString> {
    if candidates.is_empty() {
        return None;
    }
    let installed = cx.text_system().all_font_names();
    candidates.iter().find(|c| installed.contains(c)).map(|c| SharedString::from(c.clone()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtin_themes_parse() {
        for json in BUILTIN {
            let t = parse(json).unwrap_or_else(|e| panic!("{e}"));
            assert!(t.palette.categorical.is_empty() || t.palette.categorical.len() == 8, "{}", t.name);
            if t.name != "Kuaky Original" {
                assert!(t.kit.is_some(), "{} deriva los colores de GPUI Kit", t.name);
            }
        }
    }

    #[test]
    fn original_keeps_current_colors() {
        let p = parse(BUILTIN[0]).unwrap().palette;
        assert_eq!(p.background, gpui_kit::rgb(0x0b0c0f));
        assert_eq!(p.accent, gpui_kit::rgb(0x5eead4));
        assert_eq!(p.data_output, gpui_kit::rgb(0xa78bfa));
        assert_eq!(p.warning, gpui_kit::rgb(0xfbbf24));
    }

    #[test]
    fn picking_a_theme_always_shows_it() {
        // En "system" con el Mac oscuro: Sumi se queda en system, Papel pasa a claro.
        assert_eq!(mode_after_pick("system", true, true), "system");
        assert_eq!(mode_after_pick("system", true, false), "light");
        assert_eq!(mode_after_pick("", false, true), "dark");
        assert_eq!(mode_after_pick("", false, false), "system");
        // En un modo fijo, se cambia al del tema elegido.
        assert_eq!(mode_after_pick("dark", true, false), "light");
        assert_eq!(mode_after_pick("light", false, true), "dark");
    }

    #[test]
    fn c_is_the_default_pair() {
        let names: Vec<String> = BUILTIN.iter().map(|j| parse(j).unwrap().name).collect();
        for (name, dark) in [(DEFAULT_DARK, true), (DEFAULT_LIGHT, false)] {
            assert!(names.iter().any(|n| n == name), "{name} incluido");
            let t = BUILTIN.iter().map(|j| parse(j).unwrap()).find(|t| t.name == name).unwrap();
            assert_eq!(t.is_dark(), dark, "{name}");
        }
    }

    #[test]
    fn resolves_aliases_and_alpha() {
        assert_eq!(parse_hex("#ff000080").unwrap().a, 128. / 255.);
        let prims = HashMap::from([("red".to_string(), "#ff0000".to_string())]);
        let toks = HashMap::from([("accent".to_string(), "{red}".to_string()), ("x".to_string(), "{accent}".to_string())]);
        let r = Resolver { primitives: &prims, tokens: &toks };
        assert_eq!(r.token("x").unwrap(), gpui_kit::rgb(0xff0000));
        assert!(r.color("{nope}").is_err());
    }

    /// Contraste WCAG 2.x del texto principal y del texto sobre el acento.
    #[test]
    fn themes_have_readable_text() {
        fn lum(c: Rgba) -> f32 {
            let f = |x: f32| if x <= 0.03928 { x / 12.92 } else { ((x + 0.055) / 1.055).powf(2.4) };
            0.2126 * f(c.r) + 0.7152 * f(c.g) + 0.0722 * f(c.b)
        }
        fn contrast(a: Rgba, b: Rgba) -> f32 {
            let (x, y) = (lum(a), lum(b));
            (x.max(y) + 0.05) / (x.min(y) + 0.05)
        }
        for json in BUILTIN {
            let t = parse(json).unwrap();
            let p = &t.palette;
            assert!(contrast(p.text, p.background) >= 7.0, "{}: texto", t.name);
            assert!(contrast(p.text_muted, p.background) >= 4.5, "{}: texto atenuado", t.name);
            assert!(contrast(p.accent_foreground, p.accent) >= 4.5, "{}: texto sobre acento", t.name);
        }
    }
}
