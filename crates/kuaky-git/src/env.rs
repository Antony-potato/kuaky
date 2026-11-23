//! PATH del shell de login.
//!
//! Una app abierta desde el Dock de macOS recibe un PATH mínimo
//! (`/usr/bin:/bin:…`), sin Homebrew ni las rutas de tus herramientas. Para
//! encontrar `gh`, `claude`, `codex` o `agy` se pregunta una vez al shell de
//! login cuál es su PATH.

use std::{
    ffi::OsString,
    path::PathBuf,
    process::{Command, Stdio},
    sync::OnceLock,
};

static PATH: OnceLock<OsString> = OnceLock::new();

/// PATH completo del usuario (el del proceso más el del shell de login).
pub fn login_path() -> &'static OsString {
    PATH.get_or_init(|| {
        let current = std::env::var_os("PATH").unwrap_or_default();
        let mut dirs: Vec<PathBuf> = shell_path().map(|p| std::env::split_paths(&p).collect()).unwrap_or_default();
        for d in std::env::split_paths(&current) {
            if !dirs.contains(&d) {
                dirs.push(d);
            }
        }
        std::env::join_paths(dirs).unwrap_or(current)
    })
}

#[cfg(unix)]
fn shell_path() -> Option<OsString> {
    let shell = std::env::var("SHELL").ok().filter(|s| !s.is_empty()).unwrap_or_else(|| "/bin/zsh".into());
    // Marcadores para ignorar lo que impriman los archivos de inicio del shell.
    let out = Command::new(shell)
        .args(["-l", "-i", "-c", "printf '__KUAKY__%s__KUAKY__' \"$PATH\""])
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    let text = String::from_utf8_lossy(&out.stdout);
    let start = text.find("__KUAKY__")? + "__KUAKY__".len();
    let end = start + text[start..].find("__KUAKY__")?;
    Some(OsString::from(&text[start..end]))
}

#[cfg(windows)]
fn shell_path() -> Option<OsString> {
    None
}

/// Ruta completa de un programa usando el PATH de login.
pub fn find_program(name: &str) -> Option<PathBuf> {
    let exts: &[&str] = if cfg!(windows) { &[".exe", ".cmd", ".bat", ""] } else { &[""] };
    std::env::split_paths(login_path()).find_map(|dir| {
        exts.iter()
            .map(|ext| dir.join(format!("{name}{ext}")))
            .find(|p| p.is_file())
    })
}

/// Busca la CLI de un agente: primero en el PATH de login y luego en las
/// carpetas donde otros editores instalan agentes (p. ej. Zed guarda Claude
/// Code en `external_agents/registry/claude-acp/<versión>/claude`).
pub fn find_agent(name: &str) -> Option<PathBuf> {
    if let Some(p) = find_program(name) {
        return Some(p);
    }
    let exe = if cfg!(windows) { format!("{name}.exe") } else { name.to_string() };
    let mut candidates: Vec<PathBuf> = Vec::new();
    for registry in zed_agent_registries() {
        let Ok(agents) = std::fs::read_dir(&registry) else { continue };
        for agent in agents.filter_map(Result::ok) {
            let Ok(versions) = std::fs::read_dir(agent.path()) else { continue };
            candidates.extend(versions.filter_map(Result::ok).map(|v| v.path().join(&exe)).filter(|p| p.is_file()));
        }
    }
    let home = home_dir();
    candidates.extend(
        [".claude/local", ".local/bin", ".bun/bin", ".npm-global/bin"]
            .iter()
            .map(|d| home.join(d).join(&exe))
            .filter(|p| p.is_file()),
    );
    // La versión instalada más reciente.
    candidates.into_iter().max_by_key(|p| std::fs::metadata(p).and_then(|m| m.modified()).ok())
}

fn zed_agent_registries() -> Vec<PathBuf> {
    let home = home_dir();
    let mut dirs = vec![
        home.join("Library/Application Support/Zed/external_agents/registry"),
        home.join(".local/share/zed/external_agents/registry"),
    ];
    if let Some(local) = std::env::var_os("LOCALAPPDATA") {
        dirs.push(PathBuf::from(local).join("Zed/external_agents/registry"));
    }
    dirs
}

fn home_dir() -> PathBuf {
    std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" })
        .map(PathBuf::from)
        .unwrap_or_default()
}

/// Texto para ejecutar `path` desde el shell, con comillas si hace falta.
pub fn shell_command(path: &std::path::Path) -> String {
    let s = path.to_string_lossy();
    if cfg!(windows) {
        format!("& '{}'", s.replace('\'', "''"))
    } else if s.chars().all(|c| c.is_ascii_alphanumeric() || "/._-+".contains(c)) {
        s.into_owned()
    } else {
        format!("'{}'", s.replace('\'', "'\\''"))
    }
}

/// `Command` con el PATH de login y sin abrir consola en Windows.
pub fn command(program: &str) -> Command {
    let resolved = find_program(program).map(|p| p.into_os_string()).unwrap_or_else(|| program.into());
    let mut cmd = Command::new(resolved);
    cmd.env("PATH", login_path());
    cmd.stdin(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    cmd
}
