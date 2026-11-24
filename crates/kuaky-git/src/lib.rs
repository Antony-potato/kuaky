//! Información de git y GitHub de un proyecto, usando las CLIs `git` y `gh`
//! (así se reusan tu configuración y tu sesión de GitHub).

mod env;
mod parse;

pub use env::{command, find_agent, find_program, login_path, shell_command};
pub use parse::{Branch, Change, ChangeKind, Commit, PullRequest, Worktree};

use anyhow::{Context as _, bail};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone)]
pub struct Repo {
    root: PathBuf,
}

impl Repo {
    /// Abre el repositorio que contiene `path`, si existe.
    pub fn discover(path: &Path) -> Option<Repo> {
        let out = git_in(path, &["rev-parse", "--show-toplevel"]).ok()?;
        let root = PathBuf::from(out.trim());
        root.is_dir().then_some(Repo { root })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    fn git(&self, args: &[&str]) -> anyhow::Result<String> {
        git_in(&self.root, args)
    }

    pub fn current_branch(&self) -> anyhow::Result<String> {
        let name = self.git(&["branch", "--show-current"])?.trim().to_string();
        if !name.is_empty() {
            return Ok(name);
        }
        // HEAD separado: mostrar el commit.
        Ok(format!("({})", self.git(&["rev-parse", "--short", "HEAD"])?.trim()))
    }

    /// Rama principal: la que apunta `origin/HEAD`, o `main`/`master`.
    pub fn default_branch(&self) -> Option<String> {
        if let Ok(out) = self.git(&["symbolic-ref", "--quiet", "--short", "refs/remotes/origin/HEAD"]) {
            let name = out.trim().trim_start_matches("origin/").to_string();
            if !name.is_empty() {
                return Some(name);
            }
        }
        ["main", "master", "develop"]
            .into_iter()
            .find(|b| self.git(&["rev-parse", "--verify", "--quiet", &format!("refs/heads/{b}")]).is_ok())
            .map(str::to_string)
    }

    pub fn branches(&self) -> anyhow::Result<Vec<Branch>> {
        let out = self.git(&[
            "for-each-ref",
            "--sort=-committerdate",
            "--format=%(HEAD)%00%(refname:short)%00%(upstream:short)%00%(upstream:track)%00%(committerdate:unix)%00%(subject)",
            "refs/heads",
        ])?;
        Ok(parse::branches(&out))
    }

    /// Archivos con cambios (staged, sin staged y nuevos).
    pub fn status(&self) -> anyhow::Result<Vec<Change>> {
        let out = self.git(&["status", "--porcelain=v1", "-z", "--untracked-files=all"])?;
        Ok(parse::status(&out))
    }

    /// Diff de un archivo contra HEAD (incluye cambios staged). Para archivos
    /// nuevos devuelve el archivo completo como añadido.
    pub fn diff_change(&self, change: &Change) -> anyhow::Result<String> {
        if change.kind == ChangeKind::Untracked {
            // `git diff --no-index` sale con 1 cuando hay diferencias.
            let out = command("git")
                .current_dir(&self.root)
                .args(["--no-pager", "diff", "--no-color", "--no-index", "--", null_device(), &change.path])
                .output()
                .context("no se pudo ejecutar git")?;
            return Ok(String::from_utf8_lossy(&out.stdout).into_owned());
        }
        let has_head = self.git(&["rev-parse", "--verify", "--quiet", "HEAD"]).is_ok();
        let base = if has_head { "HEAD" } else { "--cached" };
        self.git(&["--no-pager", "diff", "--no-color", "--find-renames", base, "--", &change.path])
    }

    /// Todos los cambios del árbol de trabajo contra HEAD.
    pub fn diff_worktree(&self) -> anyhow::Result<String> {
        let mut patch = self.git(&["--no-pager", "diff", "--no-color", "--find-renames", "HEAD"]).unwrap_or_default();
        for change in self.status()?.iter().filter(|c| c.kind == ChangeKind::Untracked) {
            patch.push_str(&self.diff_change(change)?);
        }
        Ok(patch)
    }

    /// Lo que `head` agrega sobre `base` (como en un PR): `git diff base...head`.
    pub fn diff_refs(&self, base: &str, head: &str) -> anyhow::Result<String> {
        self.git(&["--no-pager", "diff", "--no-color", "--find-renames", &format!("{base}...{head}")])
    }

    pub fn log(&self, revision: &str, limit: usize) -> anyhow::Result<Vec<Commit>> {
        let out = self.git(&[
            "log",
            "-z",
            &format!("--max-count={limit}"),
            "--format=%h%x00%an%x00%at%x00%s",
            revision,
            "--",
        ])?;
        Ok(parse::commits(&out))
    }

    pub fn checkout(&self, branch: &str) -> anyhow::Result<()> {
        if branch.starts_with('-') {
            bail!("nombre de rama inválido");
        }
        self.git(&["checkout", branch]).map(|_| ())
    }

    /// Crea un worktree aislado con su propia rama `kuaky/<name>` a partir de
    /// HEAD, en `~/.kuaky/worktrees/<repo>/<name>`, y devuelve su ruta.
    pub fn create_worktree(&self, name: &str) -> anyhow::Result<PathBuf> {
        self.create_worktree_in(&dirs_home().join(".kuaky").join("worktrees"), name)
    }

    pub fn create_worktree_in(&self, worktrees_dir: &Path, name: &str) -> anyhow::Result<PathBuf> {
        let safe: String = name
            .chars()
            .map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '-' })
            .collect();
        if safe.is_empty() || safe.starts_with('-') {
            bail!("nombre de worktree inválido");
        }
        let repo_name = self.root.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "repo".into());
        let base = worktrees_dir.join(repo_name);
        std::fs::create_dir_all(&base)?;
        let path = base.join(&safe);
        let branch = format!("kuaky/{safe}");
        let path_str = path.to_string_lossy().into_owned();
        self.git(&["worktree", "add", "-b", &branch, &path_str, "HEAD"])?;
        Ok(path)
    }

    /// Worktrees del repositorio (el principal incluido).
    pub fn worktrees(&self) -> anyhow::Result<Vec<Worktree>> {
        let out = self.git(&["worktree", "list", "--porcelain"])?;
        Ok(parse::worktrees(&out))
    }

    /// Lo que un worktree tiene respecto a `base`: commits de su rama más
    /// cambios sin commit.
    pub fn diff_worktree_branch(&self, base: &str, wt: &Worktree) -> anyhow::Result<String> {
        let mut patch = match &wt.branch {
            Some(branch) => self.diff_refs(base, branch)?,
            None => String::new(),
        };
        if let Some(repo) = Repo::discover(&wt.path) {
            patch.push_str(&repo.diff_worktree().unwrap_or_default());
        }
        Ok(patch)
    }

    /// PRs abiertos del repositorio (requiere `gh` con sesión iniciada).
    pub fn pull_requests(&self) -> anyhow::Result<Vec<PullRequest>> {
        let out = gh_in(
            &self.root,
            &[
                "pr",
                "list",
                "--state",
                "open",
                "--limit",
                "50",
                "--json",
                "number,title,author,headRefName,baseRefName,isDraft,updatedAt,url,reviewDecision,additions,deletions",
            ],
        )?;
        parse::pull_requests(&out)
    }

    pub fn pr_diff(&self, number: u64) -> anyhow::Result<String> {
        gh_in(&self.root, &["pr", "diff", &number.to_string(), "--color", "never"])
    }
}

fn git_in(dir: &Path, args: &[&str]) -> anyhow::Result<String> {
    run(dir, "git", args)
}

fn gh_in(dir: &Path, args: &[&str]) -> anyhow::Result<String> {
    if find_program("gh").is_none() {
        bail!("GitHub CLI (`gh`) not found. Install it and run `gh auth login`.");
    }
    run(dir, "gh", args)
}

fn run(dir: &Path, program: &str, args: &[&str]) -> anyhow::Result<String> {
    let out = command(program)
        .current_dir(dir)
        .args(args)
        .output()
        .with_context(|| format!("no se pudo ejecutar {program}"))?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        bail!("{}", err.trim());
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

fn dirs_home() -> PathBuf {
    std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" })
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir)
}

fn null_device() -> &'static str {
    if cfg!(windows) { "NUL" } else { "/dev/null" }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_repo() -> PathBuf {
        let dir = std::env::temp_dir().join(format!("kuaky-git-{}-{}", std::process::id(), rand_suffix()));
        std::fs::create_dir_all(&dir).unwrap();
        for args in [
            vec!["init", "-q", "-b", "main"],
            vec!["config", "user.email", "test@example.com"],
            vec!["config", "user.name", "Test"],
            vec!["config", "commit.gpgsign", "false"],
        ] {
            git_in(&dir, &args).unwrap();
        }
        dir
    }

    fn rand_suffix() -> u128 {
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()
    }

    #[test]
    fn status_diff_branches_and_log() {
        let dir = temp_repo();
        std::fs::write(dir.join("a.txt"), "uno\n").unwrap();
        git_in(&dir, &["add", "."]).unwrap();
        git_in(&dir, &["commit", "-q", "-m", "primero"]).unwrap();
        git_in(&dir, &["checkout", "-q", "-b", "feature"]).unwrap();
        std::fs::write(dir.join("a.txt"), "uno\ndos\n").unwrap();
        git_in(&dir, &["commit", "-q", "-am", "segundo"]).unwrap();
        std::fs::write(dir.join("a.txt"), "uno\ndos\ntres\n").unwrap();
        std::fs::write(dir.join("nuevo.txt"), "hola\n").unwrap();

        let repo = Repo::discover(&dir).unwrap();
        assert_eq!(repo.current_branch().unwrap(), "feature");
        assert_eq!(repo.default_branch().as_deref(), Some("main"));

        let status = repo.status().unwrap();
        assert_eq!(status.len(), 2);
        let untracked = status.iter().find(|c| c.path == "nuevo.txt").unwrap();
        assert_eq!(untracked.kind, ChangeKind::Untracked);
        assert!(repo.diff_change(untracked).unwrap().contains("+hola"));
        let modified = status.iter().find(|c| c.path == "a.txt").unwrap();
        assert_eq!(modified.kind, ChangeKind::Modified);
        assert!(repo.diff_change(modified).unwrap().contains("+tres"));

        let branch_diff = repo.diff_refs("main", "feature").unwrap();
        assert!(branch_diff.contains("+dos") && !branch_diff.contains("+tres"));

        let branches = repo.branches().unwrap();
        assert_eq!(branches.len(), 2);
        assert!(branches.iter().any(|b| b.name == "feature" && b.current));

        let log = repo.log("HEAD", 10).unwrap();
        assert_eq!(log.len(), 2);
        assert_eq!(log[0].subject, "segundo");

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn creates_isolated_worktree() {
        let dir = temp_repo();
        std::fs::write(dir.join("a.txt"), "uno\n").unwrap();
        git_in(&dir, &["add", "."]).unwrap();
        git_in(&dir, &["commit", "-q", "-m", "primero"]).unwrap();
        let repo = Repo::discover(&dir).unwrap();
        let name = format!("prueba-{}", rand_suffix());
        let wt = repo.create_worktree_in(&dir.join("wt"), &name).unwrap();
        assert!(wt.join("a.txt").is_file());
        let wt_repo = Repo::discover(&wt).unwrap();
        assert_eq!(wt_repo.current_branch().unwrap(), format!("kuaky/{name}"));
        let list = repo.worktrees().unwrap();
        assert_eq!(list.len(), 2);
        let entry = list.iter().find(|w| w.branch.as_deref() == Some(&format!("kuaky/{name}"))).unwrap();
        std::fs::write(wt.join("b.txt"), "nuevo\n").unwrap();
        assert!(repo.diff_worktree_branch("main", entry).unwrap().contains("+nuevo"));
        git_in(&dir, &["worktree", "remove", "--force", &wt.to_string_lossy()]).unwrap();
        std::fs::remove_dir_all(&dir).ok();
    }
}
