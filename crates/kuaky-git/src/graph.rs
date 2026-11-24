//! Git commit graph with color-coded branch lane assignment.

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RefKind {
    Head,
    LocalBranch,
    RemoteBranch,
    Tag,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RefBadge {
    pub kind: RefKind,
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GraphCommit {
    pub hash: String,
    pub short_hash: String,
    pub parents: Vec<String>,
    pub refs: Vec<RefBadge>,
    pub is_head: bool,
    pub author: String,
    pub time: i64,
    pub subject: String,
}

/// Edge connecting the top or bottom half of a row between two lane columns.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LaneEdge {
    pub from: usize,
    pub to: usize,
    pub color: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GraphRow {
    pub commit: GraphCommit,
    /// Column where this commit's dot is drawn.
    pub lane: usize,
    /// Color index of this commit's lane.
    pub color: usize,
    /// Lanes passing straight through from top to bottom (not touched by this commit): `(col, color)`.
    pub pass_through: Vec<(usize, usize)>,
    /// Lines entering from the top edge into this commit's dot: `(from_col, to_col, color)`.
    pub incoming: Vec<LaneEdge>,
    /// Lines leaving from this commit's dot to the bottom edge: `(from_col, to_col, color)`.
    pub outgoing: Vec<LaneEdge>,
    /// Number of lanes needed across this row.
    pub width: usize,
}

/// Parses `git log --all --topo-order -z --format=%H%x00%h%x00%P%x00%D%x00%an%x00%at%x00%s`.
pub fn parse_graph_commits(out: &str) -> Vec<GraphCommit> {
    let mut commits = Vec::new();
    let fields: Vec<&str> = out.split('\0').map(|s| s.trim_start_matches('\n')).collect();
    for f in fields.chunks_exact(7) {
        let hash = f[0].trim();
        if hash.is_empty() {
            continue;
        }
        let parents: Vec<String> = f[2].split_whitespace().map(str::to_string).collect();
        let (is_head, refs) = parse_refs(f[3]);
        commits.push(GraphCommit {
            hash: hash.to_string(),
            short_hash: f[1].to_string(),
            parents,
            refs,
            is_head,
            author: f[4].to_string(),
            time: f[5].parse().unwrap_or(0),
            subject: f[6].to_string(),
        });
    }
    commits
}

fn parse_refs(raw: &str) -> (bool, Vec<RefBadge>) {
    let mut is_head = false;
    let mut out = Vec::new();
    for part in raw.split(',').map(str::trim).filter(|s| !s.is_empty()) {
        if let Some(branch) = part.strip_prefix("HEAD -> ") {
            is_head = true;
            out.push(RefBadge { kind: RefKind::Head, name: branch.trim().to_string() });
        } else if part == "HEAD" {
            is_head = true;
            out.push(RefBadge { kind: RefKind::Head, name: "HEAD".to_string() });
        } else if let Some(tag) = part.strip_prefix("tag: ") {
            out.push(RefBadge { kind: RefKind::Tag, name: tag.trim().to_string() });
        } else if part.ends_with("/HEAD") {
            continue;
        } else if part.contains('/') {
            out.push(RefBadge { kind: RefKind::RemoteBranch, name: part.to_string() });
        } else {
            out.push(RefBadge { kind: RefKind::LocalBranch, name: part.to_string() });
        }
    }
    (is_head, out)
}

/// Assigns deterministic colored lanes and top/bottom edge geometry for a topo-ordered commit list.
pub fn assign_lanes(commits: Vec<GraphCommit>) -> Vec<GraphRow> {
    let mut active: Vec<Option<(String, usize)>> = Vec::new();
    let mut next_color: usize = 0;
    let mut rows = Vec::with_capacity(commits.len());

    for commit in commits {
        // Find all active lanes expecting this commit hash.
        let matching: Vec<usize> = active
            .iter()
            .enumerate()
            .filter_map(|(i, slot)| slot.as_ref().is_some_and(|(h, _)| h == &commit.hash).then_some(i))
            .collect();

        let (lane, color, mut incoming) = if let Some(&first) = matching.first() {
            let color = active[first].as_ref().map(|(_, c)| *c).unwrap_or(0);
            let mut inc = vec![LaneEdge { from: first, to: first, color }];
            for &extra in &matching[1..] {
                let ec = active[extra].as_ref().map(|(_, c)| *c).unwrap_or(color);
                inc.push(LaneEdge { from: extra, to: first, color: ec });
                active[extra] = None;
            }
            (first, color, inc)
        } else {
            // Allocate first free lane or append a new one.
            let c = next_color;
            next_color += 1;
            let idx = if let Some(free) = active.iter().position(Option::is_none) {
                active[free] = Some((commit.hash.clone(), c));
                free
            } else {
                active.push(Some((commit.hash.clone(), c)));
                active.len() - 1
            };
            (idx, c, Vec::new())
        };

        // Pass-through lanes are all other occupied columns before we route parents.
        let mut pass_through = Vec::new();
        for (i, slot) in active.iter().enumerate() {
            if i != lane {
                if let Some((_, c)) = slot {
                    pass_through.push((i, *c));
                }
            }
        }

        // Route outgoing edges to parents.
        let mut outgoing = Vec::new();
        if commit.parents.is_empty() {
            active[lane] = None;
        } else {
            // First parent tries to stay in `lane` with the same color.
            // If an earlier (leftmost) lane already expects it, merge into that left lane;
            // if a rightmost lane expects it, stay on `lane` so the branch merges back cleanly.
            let first_parent = &commit.parents[0];
            if let Some(existing) = active
                .iter()
                .enumerate()
                .find_map(|(i, s)| (i < lane && s.as_ref().is_some_and(|(h, _)| h == first_parent)).then_some(i))
            {
                let ec = active[existing].as_ref().map(|(_, c)| *c).unwrap_or(color);
                outgoing.push(LaneEdge { from: lane, to: existing, color: ec });
                active[lane] = None;
            } else {
                active[lane] = Some((first_parent.clone(), color));
                outgoing.push(LaneEdge { from: lane, to: lane, color });
            }

            // Secondary (merge) parents either connect to an existing lane or allocate a new one.
            for parent in &commit.parents[1..] {
                if let Some(existing) = active
                    .iter()
                    .enumerate()
                    .find_map(|(i, s)| s.as_ref().is_some_and(|(h, _)| h == parent).then_some(i))
                {
                    let ec = active[existing].as_ref().map(|(_, c)| *c).unwrap_or(color);
                    outgoing.push(LaneEdge { from: lane, to: existing, color: ec });
                } else {
                    let pc = next_color;
                    next_color += 1;
                    let idx = if let Some(free) = active.iter().position(Option::is_none) {
                        active[free] = Some((parent.clone(), pc));
                        free
                    } else {
                        active.push(Some((parent.clone(), pc)));
                        active.len() - 1
                    };
                    outgoing.push(LaneEdge { from: lane, to: idx, color: pc });
                }
            }
        }

        // Trim trailing empty slots.
        while active.last().is_some_and(Option::is_none) {
            active.pop();
        }

        let max_col = [lane]
            .into_iter()
            .chain(pass_through.iter().map(|(c, _)| *c))
            .chain(incoming.iter().flat_map(|e| [e.from, e.to]))
            .chain(outgoing.iter().flat_map(|e| [e.from, e.to]))
            .max()
            .unwrap_or(0);

        // Sort incoming so straight line draws cleanly.
        incoming.sort_by_key(|e| e.from);

        rows.push(GraphRow {
            commit,
            lane,
            color,
            pass_through,
            incoming,
            outgoing,
            width: max_col + 1,
        });
    }

    rows
}

#[cfg(test)]
mod tests {
    use super::*;

    fn c(hash: &str, parents: &[&str], refs: &str) -> GraphCommit {
        let (is_head, parsed) = parse_refs(refs);
        GraphCommit {
            hash: hash.into(),
            short_hash: hash[..hash.len().min(7)].into(),
            parents: parents.iter().map(|s| s.to_string()).collect(),
            refs: parsed,
            is_head,
            author: "dev".into(),
            time: 1_700_000_000,
            subject: format!("commit {hash}"),
        }
    }

    #[test]
    fn assigns_lanes_for_branch_and_merge() {
        // m (merge) -> [a2, b1]; a2 -> [a1]; b1 -> [a1]; a1 -> []
        let rows = assign_lanes(vec![
            c("m", &["a2", "b1"], "HEAD -> main"),
            c("b1", &["a1"], "feature"),
            c("a2", &["a1"], ""),
            c("a1", &[], "tag: v0.1"),
        ]);
        assert_eq!(rows.len(), 4);
        assert_eq!(rows[0].lane, 0);
        assert!(rows[0].commit.is_head);
        assert_eq!(rows[0].outgoing.len(), 2);
        // Merge parent b1 got lane 1
        assert_eq!(rows[1].lane, 1);
        // a2 stayed on lane 0 while b1 passed or converged
        assert_eq!(rows[2].lane, 0);
        // a1 collapses back to lane 0
        assert_eq!(rows[3].lane, 0);
    }

    #[test]
    fn parses_git_log_output() {
        let raw = "aaaa111\0aaaa\0bbbb222 cccc333\0HEAD -> main, origin/main, tag: v1\0Ana\01700000000\0Merge feature\0";
        let list = parse_graph_commits(raw);
        assert_eq!(list.len(), 1);
        assert!(list[0].is_head);
        assert_eq!(list[0].parents, vec!["bbbb222", "cccc333"]);
        assert_eq!(list[0].refs.len(), 3);
        assert_eq!(list[0].refs[0].kind, RefKind::Head);
        assert_eq!(list[0].refs[1].kind, RefKind::RemoteBranch);
        assert_eq!(list[0].refs[2].kind, RefKind::Tag);
    }
}
