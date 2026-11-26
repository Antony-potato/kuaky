//! Lectura de la salida de `git` y `gh`.

use serde::Deserialize;

#[derive(Debug, Clone, PartialEq)]
pub struct Branch {
    pub name: String,
    pub current: bool,
    pub upstream: Option<String>,
    pub ahead: u32,
    pub behind: u32,
    /// Fecha del último commit (segundos Unix).
    pub updated: i64,
    pub subject: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChangeKind {
    Modified,
    Added,
    Deleted,
    Renamed,
    Untracked,
    Conflicted,
}

impl ChangeKind {
    pub fn letter(self) -> &'static str {
        match self {
            ChangeKind::Modified => "M",
            ChangeKind::Added => "A",
            ChangeKind::Deleted => "D",
            ChangeKind::Renamed => "R",
            ChangeKind::Untracked => "N",
            ChangeKind::Conflicted => "!",
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Change {
    pub path: String,
    pub kind: ChangeKind,
    pub staged: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Worktree {
    pub path: std::path::PathBuf,
    /// Rama sin `refs/heads/`; `None` si el HEAD está separado.
    pub branch: Option<String>,
    pub main: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Commit {
    pub short_hash: String,
    pub author: String,
    pub time: i64,
    pub subject: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PullRequest {
    pub number: u64,
    pub title: String,
    #[serde(default)]
    pub author: Author,
    pub head_ref_name: String,
    pub base_ref_name: String,
    #[serde(default)]
    pub is_draft: bool,
    #[serde(default)]
    pub updated_at: String,
    #[serde(default)]
    pub url: String,
    #[serde(default)]
    pub body: String,
    #[serde(default)]
    pub review_decision: Option<String>,
    #[serde(default)]
    pub additions: u64,
    #[serde(default)]
    pub deletions: u64,
    /// `MERGEABLE`, `CONFLICTING` o `UNKNOWN` (GitHub lo calcula en segundo plano).
    #[serde(default)]
    pub mergeable: String,
    /// `CLEAN`, `DIRTY` (conflictos), `BEHIND`, `BLOCKED`, `UNSTABLE`…
    #[serde(default)]
    pub merge_state_status: String,
}

impl PullRequest {
    pub fn has_conflicts(&self) -> bool {
        self.mergeable == "CONFLICTING" || self.merge_state_status == "DIRTY"
    }
}

/// Lo que muestra la vista de un PR (Overview).
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PrDetail {
    #[serde(default)]
    pub body: String,
    #[serde(default)]
    pub mergeable: String,
    #[serde(default)]
    pub merge_state_status: String,
    #[serde(default)]
    pub changed_files: u64,
    #[serde(default)]
    pub state: String,
    #[serde(default)]
    pub commits: Vec<serde_json::Value>,
}

impl PrDetail {
    pub fn has_conflicts(&self) -> bool {
        self.mergeable == "CONFLICTING" || self.merge_state_status == "DIRTY"
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct Author {
    #[serde(default)]
    pub login: String,
}

pub fn branches(out: &str) -> Vec<Branch> {
    out.lines()
        .filter_map(|line| {
            let f: Vec<&str> = line.split('\0').collect();
            if f.len() < 6 {
                return None;
            }
            let (ahead, behind) = track(f[3]);
            Some(Branch {
                current: f[0] == "*",
                name: f[1].to_string(),
                upstream: (!f[2].is_empty()).then(|| f[2].to_string()),
                ahead,
                behind,
                updated: f[4].parse().unwrap_or(0),
                subject: f[5].to_string(),
            })
        })
        .collect()
}

/// "[ahead 2, behind 1]" → (2, 1)
fn track(s: &str) -> (u32, u32) {
    let num = |key: &str| {
        s.find(key)
            .map(|i| s[i + key.len()..].chars().take_while(|c| c.is_ascii_digit()).collect::<String>())
            .and_then(|n| n.parse().ok())
            .unwrap_or(0)
    };
    (num("ahead "), num("behind "))
}

pub fn status(out: &str) -> Vec<Change> {
    let mut changes = Vec::new();
    let mut fields = out.split('\0').filter(|s| !s.is_empty());
    while let Some(entry) = fields.next() {
        if entry.len() < 4 {
            continue;
        }
        let (x, y) = (entry.as_bytes()[0] as char, entry.as_bytes()[1] as char);
        let path = entry[3..].to_string();
        let kind = match (x, y) {
            ('?', '?') => ChangeKind::Untracked,
            ('U', _) | (_, 'U') | ('A', 'A') | ('D', 'D') => ChangeKind::Conflicted,
            ('R', _) | (_, 'R') => {
                // En -z el nombre original viene en el siguiente campo.
                fields.next();
                ChangeKind::Renamed
            }
            ('A', _) => ChangeKind::Added,
            ('D', _) | (_, 'D') => ChangeKind::Deleted,
            _ => ChangeKind::Modified,
        };
        changes.push(Change {
            path,
            kind,
            staged: x != ' ' && x != '?',
        });
    }
    changes
}

pub fn worktrees(out: &str) -> Vec<Worktree> {
    let mut list = Vec::new();
    for (i, block) in out.split("\n\n").filter(|b| !b.trim().is_empty()).enumerate() {
        let mut path = None;
        let mut branch = None;
        for line in block.lines() {
            if let Some(p) = line.strip_prefix("worktree ") {
                path = Some(std::path::PathBuf::from(p));
            } else if let Some(b) = line.strip_prefix("branch ") {
                branch = Some(b.trim_start_matches("refs/heads/").to_string());
            }
        }
        if let Some(path) = path {
            list.push(Worktree { path, branch, main: i == 0 });
        }
    }
    list
}

pub fn commits(out: &str) -> Vec<Commit> {
    let fields: Vec<&str> = out.split('\0').map(|s| s.trim_start_matches('\n')).collect();
    fields
        .chunks_exact(4)
        .map(|f| Commit {
            short_hash: f[0].to_string(),
            author: f[1].to_string(),
            time: f[2].parse().unwrap_or(0),
            subject: f[3].to_string(),
        })
        .collect()
}

pub fn pull_requests(json: &str) -> anyhow::Result<Vec<PullRequest>> {
    Ok(serde_json::from_str(json)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_branch_lines() {
        let out = "*\0main\0origin/main\0[ahead 2, behind 1]\01700000000\0fix\n \0dev\0\0\01600000000\0wip\n";
        let b = branches(out);
        assert_eq!(b.len(), 2);
        assert!(b[0].current);
        assert_eq!((b[0].ahead, b[0].behind), (2, 1));
        assert_eq!(b[1].upstream, None);
    }

    #[test]
    fn parses_porcelain_z() {
        let out = " M src/a.rs\0?? new.txt\0R  b.rs\0old.rs\0A  c.rs\0";
        let s = status(out);
        assert_eq!(s.len(), 4);
        assert_eq!(s[0].kind, ChangeKind::Modified);
        assert!(!s[0].staged);
        assert_eq!(s[1].kind, ChangeKind::Untracked);
        assert_eq!(s[2].kind, ChangeKind::Renamed);
        assert_eq!(s[2].path, "b.rs");
        assert_eq!(s[3].kind, ChangeKind::Added);
        assert!(s[3].staged);
    }

    #[test]
    fn parses_gh_json() {
        let json = r#"[{"number":7,"title":"Agregar login","author":{"login":"ana"},"headRefName":"login","baseRefName":"main","isDraft":false,"updatedAt":"2026-11-20T10:00:00Z","url":"https://x","reviewDecision":"APPROVED","additions":10,"deletions":2}]"#;
        let prs = pull_requests(json).unwrap();
        assert_eq!(prs[0].number, 7);
        assert_eq!(prs[0].author.login, "ana");
        assert_eq!(prs[0].review_decision.as_deref(), Some("APPROVED"));
    }
}
