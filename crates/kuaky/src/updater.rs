//! Actualizaciones de kuaky desde los releases de GitHub (con `gh`, que también
//! sirve si el repositorio es privado).
//!
//! Revisa al abrir y cada 6 horas. Si hay una versión nueva sale "Update" en la
//! barra de título; al aceptar, baja el paquete, reemplaza la app y la reabre.
//! - macOS: `kuaky-<versión>-macos-arm64.zip` → reemplaza el `.app` que está corriendo.
//! - Windows: `kuaky-<versión>-windows-x64-setup.exe` → corre el instalador.

use gpui_kit::*;
use serde::Deserialize;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// Repositorio de donde salen las versiones.
const REPO: &str = "Antony-potato/kuaky";

#[derive(Clone, Debug, PartialEq)]
pub struct Update {
    pub version: String,
    tag: String,
    asset: String,
}

#[derive(Clone, Default)]
enum Status {
    #[default]
    Idle,
    Available(Update),
    Installing(String),
    Failed(String),
}

#[derive(Default)]
struct UpdateState {
    started: bool,
    status: Status,
}

impl Global for UpdateState {}

#[derive(Deserialize)]
struct Release {
    #[serde(rename = "tagName")]
    tag_name: String,
    #[serde(default)]
    assets: Vec<Asset>,
}

#[derive(Deserialize)]
struct Asset {
    name: String,
}

/// `1.2.10` > `1.2.9`; ignora una `v` al principio y lo que va tras `-`.
fn newer(candidate: &str, current: &str) -> bool {
    let parse = |v: &str| -> Vec<u64> {
        v.trim_start_matches('v').split('-').next().unwrap_or("").split('.').map(|p| p.parse().unwrap_or(0)).collect()
    };
    let (a, b) = (parse(candidate), parse(current));
    for i in 0..a.len().max(b.len()) {
        let (x, y) = (a.get(i).copied().unwrap_or(0), b.get(i).copied().unwrap_or(0));
        if x != y {
            return x > y;
        }
    }
    false
}

fn asset_for_this_os(assets: &[Asset]) -> Option<String> {
    let want = if cfg!(target_os = "macos") {
        "-macos-arm64.zip"
    } else if cfg!(windows) {
        "-windows-x64-setup.exe"
    } else {
        return None;
    };
    assets.iter().find(|a| a.name.ends_with(want)).map(|a| a.name.clone())
}

/// Último release, si es más nuevo que esta versión y trae paquete para este sistema.
fn check() -> anyhow::Result<Option<Update>> {
    let out = gh(&["release", "view", "--repo", REPO, "--json", "tagName,assets"])?;
    let release: Release = serde_json::from_str(&out)?;
    if !newer(&release.tag_name, env!("CARGO_PKG_VERSION")) {
        return Ok(None);
    }
    Ok(asset_for_this_os(&release.assets).map(|asset| Update { version: release.tag_name.trim_start_matches('v').to_string(), tag: release.tag_name, asset }))
}

fn gh(args: &[&str]) -> anyhow::Result<String> {
    let out = kuaky_git::command("gh").args(args).output()?;
    if !out.status.success() {
        anyhow::bail!("{}", String::from_utf8_lossy(&out.stderr).trim());
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// El `.app` que está corriendo (solo si kuaky está instalada como app).
fn running_app() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    exe.ancestors().find(|p| p.extension().is_some_and(|e| e == "app")).map(Path::to_path_buf)
}

/// Baja el paquete y lo deja en su lugar. Devuelve qué abrir al terminar.
fn install(update: &Update) -> anyhow::Result<PathBuf> {
    let dir = std::env::temp_dir().join(format!("kuaky-update-{}", update.version));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir)?;
    let dir_s = dir.to_string_lossy().into_owned();
    gh(&["release", "download", &update.tag, "--repo", REPO, "--pattern", &update.asset, "--dir", &dir_s, "--clobber"])?;
    let file = dir.join(&update.asset);
    if cfg!(windows) {
        return Ok(file);
    }
    let app = running_app().ok_or_else(|| anyhow::anyhow!("kuaky isn't running as an installed app (kuaky.app), so it can't replace itself."))?;
    replace_app(&file, &app, &dir)?;
    Ok(app)
}

/// Saca el `.app` del zip y lo pone en lugar de `app` (si algo falla, queda la de antes).
fn replace_app(file: &Path, app: &Path, dir: &Path) -> anyhow::Result<()> {
    let unpacked = dir.join("unpacked");
    let status = std::process::Command::new("ditto").args(["-x", "-k"]).arg(&file).arg(&unpacked).status()?;
    anyhow::ensure!(status.success(), "could not unpack {}", file.display());
    let new_app = std::fs::read_dir(&unpacked)?
        .filter_map(Result::ok)
        .map(|e| e.path())
        .find(|p| p.extension().is_some_and(|e| e == "app"))
        .ok_or_else(|| anyhow::anyhow!("the download has no kuaky.app inside"))?;
    // Reemplazo en dos pasos: si algo falla, la app de antes sigue ahí.
    let parent = app.parent().ok_or_else(|| anyhow::anyhow!("odd install path"))?;
    let staged = parent.join(".kuaky-update.app");
    let old = parent.join(".kuaky-old.app");
    let _ = std::fs::remove_dir_all(&staged);
    let _ = std::fs::remove_dir_all(&old);
    let status = std::process::Command::new("ditto").arg(&new_app).arg(&staged).status()?;
    anyhow::ensure!(status.success(), "could not copy the new version next to {}", app.display());
    std::fs::rename(app, &old)?;
    if let Err(e) = std::fs::rename(&staged, app) {
        let _ = std::fs::rename(&old, app);
        return Err(e.into());
    }
    let _ = std::fs::remove_dir_all(&old);
    Ok(())
}

/// Empieza a revisar (una vez por proceso). Llamado desde la barra de título.
fn ensure_started(cx: &mut App) {
    if cx.try_global::<UpdateState>().is_some_and(|s| s.started) {
        return;
    }
    cx.set_global(UpdateState { started: true, status: Status::Idle });
    if std::env::var_os("KUAKY_SCREENSHOT").is_some() || kuaky_git::find_program("gh").is_none() {
        return;
    }
    cx.spawn(async move |cx| loop {
        cx.background_executor().timer(Duration::from_secs(15)).await;
        if let Ok(Some(update)) = cx.background_executor().spawn(async { check() }).await {
            let changed = cx
                .update(|cx| {
                    let state = cx.global_mut::<UpdateState>();
                    if matches!(state.status, Status::Idle | Status::Available(_)) && !matches!(&state.status, Status::Available(u) if *u == update) {
                        state.status = Status::Available(update);
                        return true;
                    }
                    false
                });
            if changed {
                cx.update(|cx| cx.refresh_windows());
            }
        }
        cx.background_executor().timer(Duration::from_secs(6 * 3600)).await;
    })
    .detach();
}

fn set_status(status: Status, cx: &mut App) {
    cx.global_mut::<UpdateState>().status = status;
    cx.refresh_windows();
}

fn start_install(update: Update, window: &mut Window, cx: &mut App) {
    let answer = window.prompt(
        PromptLevel::Info,
        &format!("Update kuaky to {}?", update.version),
        Some("kuaky downloads the new version and restarts. Open sessions come back on restart; agents in the middle of a turn are stopped."),
        &["Update and restart", "Later"],
        cx,
    );
    cx.spawn(async move |cx| {
        if answer.await != Ok(0) {
            return;
        }
        let _ = cx.update(|cx| set_status(Status::Installing(update.version.clone()), cx));
        let result = cx.background_executor().spawn({
            let update = update.clone();
            async move { install(&update) }
        })
        .await;
        match result {
            Ok(target) => {
                // Abre la versión nueva cuando esta ya cerró.
                #[cfg(target_os = "macos")]
                let _ = std::process::Command::new("sh")
                    .arg("-c")
                    .arg(format!("sleep 1; open -n {}", shell_quote(&target)))
                    .spawn();
                #[cfg(windows)]
                let _ = std::process::Command::new(&target).args(["/SILENT", "/CLOSEAPPLICATIONS", "/RESTARTAPPLICATIONS"]).spawn();
                #[cfg(not(any(target_os = "macos", windows)))]
                let _ = target;
                let _ = cx.update(|cx| cx.quit());
            }
            Err(e) => {
                let _ = cx.update(|cx| set_status(Status::Failed(format!("{e:#}")), cx));
            }
        }
    })
    .detach();
}

fn shell_quote(p: &Path) -> String {
    format!("'{}'", p.to_string_lossy().replace('\'', "'\\''"))
}

/// Botón de la barra de título: "Update 0.2.0" (o el progreso / el error).
pub fn title_button(cx: &mut App) -> Option<AnyElement> {
    use crate::style;
    use gpui_kit::component::h_flex;
    use gpui_kit::component::tooltip::Tooltip;
    ensure_started(cx);
    let status = cx.global::<UpdateState>().status.clone();
    let pill = |id: &'static str| {
        h_flex()
            .id(id)
            .gap_1()
            .px_2()
            .py_0p5()
            .mr_1()
            .items_center()
            .rounded_md()
            .text_xs()
            .border_1()
            .border_color(style::border())
            .occlude().on_mouse_down(MouseButton::Left, crate::app::no_drag)
    };
    match status {
        Status::Idle => None,
        Status::Available(update) => {
            let tip: SharedString = format!("kuaky {} is out (you have {})", update.version, env!("CARGO_PKG_VERSION")).into();
            Some(
                pill("update-kuaky")
                    .cursor_pointer()
                    .text_color(style::text())
                    .hover(|d| d.bg(style::panel_hover()))
                    .tooltip(move |window, cx| Tooltip::new(tip.clone()).build(window, cx))
                    .child(crate::icons::ui("download", px(12.), style::text()))
                    .child(format!("Update {}", update.version))
                    .on_click(move |_, window, cx| start_install(update.clone(), window, cx))
                    .into_any_element(),
            )
        }
        Status::Installing(v) => Some(pill("update-kuaky").text_color(style::muted()).child(format!("Updating to {v}…")).into_any_element()),
        Status::Failed(e) => {
            let tip: SharedString = e.into();
            Some(
                pill("update-kuaky")
                    .cursor_pointer()
                    .text_color(style::bad())
                    .tooltip(move |window, cx| Tooltip::new(tip.clone()).build(window, cx))
                    .child("Update failed")
                    .on_click(|_, _, cx| set_status(Status::Idle, cx))
                    .into_any_element(),
            )
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{Asset, asset_for_this_os, newer};

    #[cfg(target_os = "macos")]
    #[test]
    fn replaces_the_app_with_the_downloaded_one() {
        let root = std::env::temp_dir().join(format!("kuaky-upd-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let make = |dir: &std::path::Path, text: &str| {
            let bin = dir.join("kuaky.app/Contents/MacOS");
            std::fs::create_dir_all(&bin).unwrap();
            std::fs::write(bin.join("kuaky"), text).unwrap();
        };
        // La instalada (vieja) y el zip de la nueva, como lo arma el release.
        make(&root.join("Applications"), "old");
        make(&root.join("build"), "new");
        let zip = root.join("kuaky-9.9.9-macos-arm64.zip");
        let ok = std::process::Command::new("ditto").args(["-c", "-k", "--keepParent"]).arg(root.join("build/kuaky.app")).arg(&zip).status().unwrap();
        assert!(ok.success());
        let app = root.join("Applications/kuaky.app");
        super::replace_app(&zip, &app, &root.join("work")).unwrap();
        assert_eq!(std::fs::read_to_string(app.join("Contents/MacOS/kuaky")).unwrap(), "new");
        assert!(!root.join("Applications/.kuaky-old.app").exists());
        assert!(!root.join("Applications/.kuaky-update.app").exists());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn compares_versions() {
        assert!(newer("v0.2.0", "0.1.0"));
        assert!(newer("0.1.10", "0.1.9"));
        assert!(!newer("v0.1.0", "0.1.0"));
        assert!(!newer("0.0.9", "0.1.0"));
        assert!(newer("1.0", "0.9.9"));
    }

    #[test]
    fn picks_the_package_for_this_system() {
        let assets = vec![
            Asset { name: "kuaky-0.2.0-macos-arm64.zip".into() },
            Asset { name: "kuaky-0.2.0-windows-x64-setup.exe".into() },
        ];
        let got = asset_for_this_os(&assets);
        if cfg!(target_os = "macos") {
            assert_eq!(got.as_deref(), Some("kuaky-0.2.0-macos-arm64.zip"));
        } else if cfg!(windows) {
            assert_eq!(got.as_deref(), Some("kuaky-0.2.0-windows-x64-setup.exe"));
        }
    }
}
