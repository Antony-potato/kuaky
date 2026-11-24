//! Información de git y GitHub de un proyecto, usando las CLIs `git` y `gh`
//! (así se reusan tu configuración y tu sesión de GitHub).

pub mod conflicts;
mod env;
pub mod graph;
mod parse;

pub use conflicts::{ConflictBlock, ConflictChoice, ConflictFile};
pub use env::{command, find_agent, find_program, login_path, shell_command};
pub use graph::{GraphCommit, GraphRow, LaneEdge, RefBadge, RefKind};
pub use parse::{Branch, Change, ChangeKind, Commit, PrDetail, PullRequest, Worktree};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MergePreview {
    Clean,
    Conflicts(Vec<String>),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MergeMethod {
    Merge,
    Squash,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MergeOutcome {
    Merged,
    Conflicts(Vec<String>),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PrMergeMethod {
    Merge,
    Squash,
    Rebase,
}

use anyhow::{Context as _, bail};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BranchProgress {
    pub ahead: u32,
    pub merged: bool,
}

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

    /// Foto del árbol de trabajo (archivos con seguimiento) sin tocarlo:
    /// `git stash create`, o `HEAD` si no hay cambios. Se guarda en
    /// `refs/kuaky/checkpoints/<name>` para que git no la borre.
    pub fn checkpoint(&self, name: &str) -> anyhow::Result<String> {
        let stash = self.git(&["stash", "create"])?.trim().to_string();
        let sha = if stash.is_empty() { self.git(&["rev-parse", "HEAD"])?.trim().to_string() } else { stash };
        let reference = format!("refs/kuaky/checkpoints/{name}");
        self.git(&["update-ref", &reference, &sha])?;
        Ok(sha)
    }

    /// Devuelve los archivos con seguimiento al estado de un checkpoint.
    /// Los archivos nuevos creados después no se borran.
    pub fn restore_checkpoint(&self, sha: &str) -> anyhow::Result<()> {
        anyhow::ensure!(sha.bytes().all(|b| b.is_ascii_hexdigit()) && sha.len() >= 7, "checkpoint inválido");
        self.git(&["restore", "--source", sha, "--worktree", "--", "."])?;
        Ok(())
    }

    /// Progreso de `branch` respecto a `base`: commits por delante y si ya
    /// está integrada en `base`.
    pub fn branch_progress(&self, base: &str, branch: &str) -> Option<BranchProgress> {
        let range = format!("{base}..{branch}");
        let ahead = self.git(&["rev-list", "--count", &range]).ok()?.trim().parse().ok()?;
        let merged = self.git(&["merge-base", "--is-ancestor", branch, base]).is_ok();
        Some(BranchProgress { ahead, merged: merged && ahead == 0 })
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
                "number,title,author,headRefName,baseRefName,isDraft,updatedAt,url,reviewDecision,additions,deletions,mergeable,mergeStateStatus",
            ],
        )?;
        parse::pull_requests(&out)
    }

    pub fn pr_diff(&self, number: u64) -> anyhow::Result<String> {
        gh_in(&self.root, &["pr", "diff", &number.to_string(), "--color", "never"])
    }

    /// Descripción, estado de merge y conteos de un PR.
    pub fn pr_detail(&self, number: u64) -> anyhow::Result<PrDetail> {
        let out = gh_in(&self.root, &["pr", "view", &number.to_string(), "--json", "body,mergeable,mergeStateStatus,changedFiles,state,commits"])?;
        Ok(serde_json::from_str(&out)?)
    }

    pub fn pr_merge(&self, number: u64, method: PrMergeMethod) -> anyhow::Result<()> {
        let flag = match method {
            PrMergeMethod::Merge => "--merge",
            PrMergeMethod::Squash => "--squash",
            PrMergeMethod::Rebase => "--rebase",
        };
        gh_in(&self.root, &["pr", "merge", &number.to_string(), flag])?;
        Ok(())
    }

    /// Commit history with lane assignment across all local/remote refs.
    pub fn graph_log(&self, limit: usize) -> anyhow::Result<Vec<GraphRow>> {
        let out = self.git(&[
            "log",
            "--all",
            "--topo-order",
            "-z",
            &format!("--max-count={limit}"),
            "--format=%H%x00%h%x00%P%x00%D%x00%an%x00%at%x00%s",
        ])?;
        Ok(graph::assign_lanes(graph::parse_graph_commits(&out)))
    }

    /// Unified patch for a single commit (handles root commits too).
    pub fn diff_commit(&self, hash: &str) -> anyhow::Result<String> {
        if hash.starts_with('-') {
            bail!("invalid commit hash");
        }
        self.git(&["--no-pager", "show", "--no-color", "--format=", "--patch", "--find-renames", hash])
    }

    /// Non-destructive merge preview using `git merge-tree --write-tree` (git >= 2.38).
    /// Does not touch the index or working tree.
    pub fn preview_merge(&self, base: &str, branch: &str) -> anyhow::Result<MergePreview> {
        if base.starts_with('-') || branch.starts_with('-') {
            bail!("invalid branch name");
        }
        let out = command("git")
            .current_dir(&self.root)
            .args(["merge-tree", "--write-tree", "--name-only", base, branch])
            .output()
            .context("failed to run git merge-tree")?;
        if out.status.success() {
            return Ok(MergePreview::Clean);
        }
        let stdout = String::from_utf8_lossy(&out.stdout);
        let stderr = String::from_utf8_lossy(&out.stderr);
        if out.status.code() != Some(1) {
            let msg = if stderr.trim().is_empty() { stdout.trim() } else { stderr.trim() };
            bail!("{msg}");
        }
        let mut files: Vec<String> = Vec::new();
        let mut lines = stdout.lines();
        // First line is the tree OID.
        let _ = lines.next();
        for line in lines {
            if line.is_empty() {
                break;
            }
            if !files.contains(&line.to_string()) {
                files.push(line.to_string());
            }
        }
        if files.is_empty() {
            for line in stdout.lines() {
                if let Some(rest) = line.split("Merge conflict in ").nth(1) {
                    let p = rest.trim().to_string();
                    if !p.is_empty() && !files.contains(&p) {
                        files.push(p);
                    }
                }
            }
        }
        Ok(MergePreview::Conflicts(files))
    }

    /// Merges or squashes `branch` into the current checked-out branch.
    pub fn merge_branch(&self, branch: &str, method: MergeMethod) -> anyhow::Result<MergeOutcome> {
        if branch.starts_with('-') {
            bail!("invalid branch name");
        }
        let args: Vec<&str> = match method {
            MergeMethod::Merge => vec!["merge", "--no-ff", branch],
            MergeMethod::Squash => vec!["merge", "--squash", branch],
        };
        let out = command("git")
            .current_dir(&self.root)
            .args(&args)
            .output()
            .context("failed to run git merge")?;
        if out.status.success() {
            if method == MergeMethod::Squash {
                self.git(&["commit", "-m", &format!("Squash merge branch '{branch}'")])?;
            }
            return Ok(MergeOutcome::Merged);
        }
        let conflicts = self.conflicted_files().unwrap_or_default();
        if !conflicts.is_empty() {
            return Ok(MergeOutcome::Conflicts(conflicts));
        }
        let stderr = String::from_utf8_lossy(&out.stderr);
        let stdout = String::from_utf8_lossy(&out.stdout);
        let msg = if stderr.trim().is_empty() { stdout.trim() } else { stderr.trim() };
        bail!("{msg}");
    }

    pub fn conflicted_files(&self) -> anyhow::Result<Vec<String>> {
        let out = self.git(&["diff", "--name-only", "--diff-filter=U"])?;
        Ok(out.lines().map(str::trim).filter(|s| !s.is_empty()).map(str::to_string).collect())
    }

    pub fn is_merging(&self) -> bool {
        self.git(&["rev-parse", "-q", "--verify", "MERGE_HEAD"]).is_ok()
    }

    pub fn stage_file(&self, rel_path: &str) -> anyhow::Result<()> {
        self.git(&["add", "--", rel_path])?;
        Ok(())
    }

    pub fn complete_merge(&self, message: Option<&str>) -> anyhow::Result<()> {
        let remaining = self.conflicted_files()?;
        for path in &remaining {
            let full = self.root.join(path);
            if let Ok(content) = std::fs::read_to_string(&full) {
                if conflicts::has_conflict_markers(&content) {
                    bail!("Unresolved conflict markers remain in {path}");
                }
            }
            self.stage_file(path)?;
        }
        match message {
            Some(msg) if !msg.trim().is_empty() => {
                self.git(&["commit", "-m", msg.trim()])?;
            }
            _ => {
                if self.is_merging() {
                    self.git(&["commit", "--no-edit"])?;
                } else {
                    self.git(&["commit", "-m", "Complete merge"])?;
                }
            }
        }
        Ok(())
    }

    pub fn abort_merge(&self) -> anyhow::Result<()> {
        if self.is_merging() {
            self.git(&["merge", "--abort"])?;
        } else {
            self.git(&["reset", "--merge"])?;
        }
        Ok(())
    }

    /// Removes the worktree (if provided) and force-deletes the branch.
    pub fn discard_branch(&self, branch: &str, worktree: Option<&Path>) -> anyhow::Result<()> {
        if branch.starts_with('-') {
            bail!("invalid branch name");
        }
        if let Some(wt) = worktree {
            let s = wt.to_string_lossy().into_owned();
            self.git(&["worktree", "remove", "--force", &s])?;
        }
        self.git(&["branch", "-D", branch])?;
        Ok(())
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

    fn rand_suffix() -> String {
        use std::sync::atomic::{AtomicU64, Ordering};
        static SEQ: AtomicU64 = AtomicU64::new(1);
        let n = SEQ.fetch_add(1, Ordering::Relaxed);
        let ns = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
        format!("{ns}-{n}")
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

    #[test]
    fn previews_merges_and_builds_graph() {
        let dir = temp_repo();
        std::fs::write(dir.join("file.txt"), "base\n").unwrap();
        git_in(&dir, &["add", "."]).unwrap();
        git_in(&dir, &["commit", "-q", "-m", "initial"]).unwrap();

        // Clean branch
        git_in(&dir, &["checkout", "-q", "-b", "clean-feat"]).unwrap();
        std::fs::write(dir.join("other.txt"), "hi\n").unwrap();
        git_in(&dir, &["add", "."]).unwrap();
        git_in(&dir, &["commit", "-q", "-m", "add other"]).unwrap();

        // Conflicting branch
        git_in(&dir, &["checkout", "-q", "main"]).unwrap();
        std::fs::write(dir.join("file.txt"), "main change\n").unwrap();
        git_in(&dir, &["commit", "-q", "-am", "main edit"]).unwrap();

        git_in(&dir, &["checkout", "-q", "-b", "conflict-feat", "HEAD~1"]).unwrap();
        std::fs::write(dir.join("file.txt"), "feature change\n").unwrap();
        git_in(&dir, &["commit", "-q", "-am", "feature edit"]).unwrap();

        git_in(&dir, &["checkout", "-q", "main"]).unwrap();
        let repo = Repo::discover(&dir).unwrap();

        assert_eq!(repo.preview_merge("main", "clean-feat").unwrap(), MergePreview::Clean);
        let preview = repo.preview_merge("main", "conflict-feat").unwrap();
        assert_eq!(preview, MergePreview::Conflicts(vec!["file.txt".into()]));

        // Graph covers all branches
        let graph = repo.graph_log(50).unwrap();
        assert_eq!(graph.len(), 4);
        assert!(graph.iter().any(|r| r.commit.is_head));

        // Real conflict merge -> resolve -> complete
        let outcome = repo.merge_branch("conflict-feat", MergeMethod::Merge).unwrap();
        assert_eq!(outcome, MergeOutcome::Conflicts(vec!["file.txt".into()]));
        assert!(repo.is_merging());
        std::fs::write(dir.join("file.txt"), "resolved\n").unwrap();
        repo.complete_merge(None).unwrap();
        assert!(!repo.is_merging());

        // Discard clean-feat
        repo.discard_branch("clean-feat", None).unwrap();
        assert!(!repo.branches().unwrap().iter().any(|b| b.name == "clean-feat"));

        std::fs::remove_dir_all(&dir).ok();
    }
}
