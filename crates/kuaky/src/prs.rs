//! Pull requests ligados a las sesiones: qué sesión hizo cada PR, un solo botón
//! *Merge*, y *Resolve & merge* (un agente resuelve los conflictos en la rama
//! y, cuando termina su turno, kuaky hace el merge).

use super::*;
use crate::pr_view::{PrEvent, PrView};

/// Números de PR que aparecen en un texto (`…/pull/2278`).
pub(crate) fn pr_numbers_in(text: &str) -> Vec<u64> {
    let mut out = Vec::new();
    for (i, _) in text.match_indices("/pull/") {
        let digits: String = text[i + 6..].chars().take_while(|c| c.is_ascii_digit()).collect();
        if let Ok(n) = digits.parse::<u64>() {
            if !out.contains(&n) {
                out.push(n);
            }
        }
    }
    out
}

impl ProjectView {
    pub(super) fn open_prs(&self) -> Vec<PullRequest> {
        self.git.prs.as_ref().and_then(|p| p.as_ref().ok()).cloned().unwrap_or_default()
    }

    /// Rama de una carpeta de trabajo (worktree o la carpeta principal).
    fn branch_of(&self, cwd: &Path) -> Option<String> {
        let canon = |p: &Path| p.canonicalize().unwrap_or_else(|_| p.to_path_buf());
        let cwd = canon(cwd);
        if let Some(wt) = self.git.worktrees.iter().filter(|w| !w.main).find(|w| canon(&w.path) == cwd) {
            return wt.branch.clone();
        }
        (cwd == canon(&self.root)).then(|| self.git.branch.clone()).flatten()
    }

    /// Sesión (viva o guardada en el mapa) → PR. Por rama de su carpeta, o por
    /// el enlace del PR que el agente escribió en el chat.
    pub(super) fn sync_session_prs(&mut self, cx: &mut Context<Self>) {
        let prs = self.open_prs();
        let default = self.git.default_branch.clone().unwrap_or_else(|| "main".into());
        let by_branch = |branch: &str| prs.iter().find(|p| p.head_ref_name == branch && branch != default).map(|p| p.number);
        let mut map: HashMap<String, u64> = HashMap::new();
        for tab in &self.tabs {
            match &tab.kind {
                TabKind::Chat { view, .. } => {
                    let v = view.read(cx);
                    let mentioned = v.mentioned_prs.iter().rev().find(|n| prs.iter().any(|p| p.number == **n)).copied();
                    if let Some(n) = mentioned.or_else(|| self.branch_of(&v.launch.cwd).and_then(|b| by_branch(&b))) {
                        map.insert(v.uid.clone(), n);
                    }
                }
                TabKind::Terminal { view, cwd, harness, .. } if *harness != Harness::Shell => {
                    if let Some(n) = self.branch_of(cwd).and_then(|b| by_branch(&b)) {
                        map.insert(view.read(cx).uid.clone(), n);
                    }
                }
                _ => {}
            }
        }
        // Sesiones cerradas del mapa (para "esta PR la hizo…").
        for line in &self.lines.lines {
            for s in &line.sessions {
                if map.contains_key(&s.uid) || s.cwd.is_empty() {
                    continue;
                }
                if let Some(n) = self.branch_of(Path::new(&s.cwd)).and_then(|b| by_branch(&b)) {
                    map.insert(s.uid.clone(), n);
                }
            }
        }
        // El chat muestra su PR arriba.
        for tab in &self.tabs {
            if let TabKind::Chat { view, .. } = &tab.kind {
                let uid = view.read(cx).uid.clone();
                let pr = map.get(&uid).and_then(|n| prs.iter().find(|p| p.number == *n)).map(|p| (p.number, p.has_conflicts()));
                if view.read(cx).pr != pr {
                    view.update(cx, |v, cx| {
                        v.pr = pr;
                        cx.notify();
                    });
                }
            }
        }
        self.session_prs = map;
        // Las pestañas de PR abiertas se actualizan.
        for tab in &self.tabs {
            if let TabKind::Pr(view) = &tab.kind {
                let n = view.read(cx).number;
                if let Some(pr) = prs.iter().find(|p| p.number == n).cloned() {
                    let session = self.session_of_pr(n, cx);
                    view.update(cx, |v, cx| v.set_pr(pr, session, cx));
                }
            }
        }
    }

    /// Sesión que hizo un PR: (uid, nombre). Prefiere una abierta.
    pub(super) fn session_of_pr(&self, number: u64, cx: &App) -> Option<(String, String)> {
        let uids: Vec<&String> = self.session_prs.iter().filter(|(_, n)| **n == number).map(|(u, _)| u).collect();
        let live = self.trains(cx);
        let uid = uids.iter().find(|u| live.contains_key(**u)).or(uids.first())?.to_string();
        let name = self
            .session_title(&uid, cx)
            .or_else(|| self.lines.line_of(&uid).map(|l| l.name.clone()))
            .unwrap_or_else(|| "session".into());
        Some((uid, name))
    }

    fn session_title(&self, uid: &str, cx: &App) -> Option<String> {
        self.tabs.iter().find_map(|t| match &t.kind {
            TabKind::Chat { view, .. } if view.read(cx).uid == uid => {
                let v = view.read(cx);
                v.title.clone().or_else(|| v.first_prompt().map(|p| crate::lines::short(p, 40)))
            }
            TabKind::Terminal { view, custom_name, .. } if view.read(cx).uid == uid => {
                Some(custom_name.clone().filter(|s| !s.is_empty()).unwrap_or_else(|| view.read(cx).auto_title().to_string()))
            }
            _ => None,
        })
    }

    pub(super) fn open_pr(&mut self, number: u64, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(i) = self.tabs.iter().position(|t| matches!(&t.kind, TabKind::Pr(v) if v.read(cx).number == number)) {
            if let (TabKind::Pr(v), Some(repo)) = (&self.tabs[i].kind, self.repo.clone()) {
                v.update(cx, |v, cx| v.reload(repo, cx));
            }
            self.activate(i, window, cx);
            return;
        }
        let (Some(repo), Some(pr)) = (self.repo.clone(), self.open_prs().into_iter().find(|p| p.number == number)) else { return };
        let session = self.session_of_pr(number, cx);
        let view = cx.new(|cx| PrView::new(repo, pr, session, cx));
        let sub = cx.subscribe_in(&view, window, |this, _, ev: &PrEvent, window, cx| match ev {
            PrEvent::Merge(n) => this.merge_pr(*n, window, cx),
            PrEvent::ResolveAndMerge(n) => this.resolve_and_merge(*n, window, cx),
            PrEvent::OpenSession(uid) => this.focus_session(uid, window, cx),
        });
        self._subs.push(sub);
        self.push_tab(TabKind::Pr(view), window, cx);
    }

    fn pr_view_of(&self, number: u64, cx: &App) -> Option<Entity<PrView>> {
        self.tabs.iter().find_map(|t| match &t.kind {
            TabKind::Pr(v) if v.read(cx).number == number => Some(v.clone()),
            _ => None,
        })
    }

    fn set_pr_busy(&self, number: u64, busy: Option<String>, error: Option<String>, cx: &mut Context<Self>) {
        if let Some(v) = self.pr_view_of(number, cx) {
            v.update(cx, |v, cx| {
                v.busy = busy;
                v.error = error;
                cx.notify();
            });
        }
    }

    /// Merge con el método de Settings.
    pub(super) fn merge_pr(&mut self, number: u64, window: &mut Window, cx: &mut Context<Self>) {
        let Some(repo) = self.repo.clone() else { return };
        if self.git.pr_merging.is_some() {
            return;
        }
        let method = self.pr_merge_method(cx);
        self.git.pr_merging = Some(number);
        self.git.pr_action_error = None;
        self.set_pr_busy(number, Some("Merging…".into()), None, cx);
        cx.notify();
        cx.spawn_in(window, async move |this, cx| {
            let res = cx.background_executor().spawn(async move { repo.pr_merge(number, method).map_err(|e| format!("{e:#}")) }).await;
            this.update_in(cx, |this, window, cx| {
                this.git.pr_merging = None;
                match res {
                    Ok(()) => {
                        this.set_pr_busy(number, Some("Merged ✓".into()), None, cx);
                        this.load_prs(cx);
                        this.graph.update(cx, |g, cx| g.refresh(cx));
                        this.notify(format!("Merged PR #{number}"), "Pull request merged.".into(), false, window, cx);
                    }
                    Err(err) => {
                        this.set_pr_busy(number, None, Some(err.clone()), cx);
                        this.git.pr_action_error = Some(format!("#{number}: {err}"));
                    }
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Un agente resuelve los conflictos del PR en su rama; al terminar su turno,
    /// kuaky revisa que ya se pueda unir y hace el merge.
    pub(super) fn resolve_and_merge(&mut self, number: u64, window: &mut Window, cx: &mut Context<Self>) {
        let Some(pr) = self.open_prs().into_iter().find(|p| p.number == number) else { return };
        let worktree = self.git.worktrees.iter().find(|w| w.branch.as_deref() == Some(pr.head_ref_name.as_str())).map(|w| w.path.clone());
        let place = match &worktree {
            Some(p) => format!(" Its worktree is {}.", p.to_string_lossy()),
            None => String::new(),
        };
        let prompt = format!(
            "PR #{number} ({head} → {base}) has merge conflicts with {base}. On the branch `{head}`{place} fetch, merge origin/{base} into it, \
             resolve every conflict keeping the intent of both sides, run the project's checks, then commit and push. \
             Don't merge the PR yourself: kuaky merges it when you finish. Reply with a short summary of what you resolved.",
            head = pr.head_ref_name,
            base = pr.base_ref_name,
        );
        self.set_pr_busy(number, Some("Resolving conflicts…".into()), None, cx);
        // La sesión que hizo el PR, si está abierta; si no, una nueva en su carpeta.
        let live = self.trains(cx);
        let owner = self.session_prs.iter().find(|(uid, n)| **n == number && live.contains_key(*uid)).map(|(u, _)| u.clone());
        if let Some(uid) = owner {
            let tab = live.get(&uid).map(|t| t.tab);
            match tab.and_then(|i| self.tabs.get(i)).map(|t| &t.kind) {
                Some(TabKind::Chat { view, .. }) => view.update(cx, |v, cx| v.submit_text(prompt, cx)),
                Some(TabKind::Terminal { view, .. }) => view.update(cx, |v, _| v.submit_text(&prompt)),
                _ => {}
            }
            self.pending_merges.insert(uid, number);
            if let Some(i) = tab {
                self.activate(i, window, cx);
            }
            return;
        }
        let cwd = worktree.unwrap_or_else(|| self.root.clone());
        let harness = self
            .available_harnesses(cx)
            .into_iter()
            .find(|h| *h == Harness::Claude)
            .or_else(|| self.available_harnesses(cx).into_iter().find(|h| *h != Harness::Shell));
        let Some(harness) = harness else {
            self.set_pr_busy(number, None, Some("No agent installed to resolve the conflicts.".into()), cx);
            return;
        };
        let line = self.lines.lines.iter().find(|l| l.branch.as_deref() == Some(pr.head_ref_name.as_str())).map(|l| l.id.clone());
        let can_chat = self.chat_agents.contains_key(&harness) || harness.chat_command().is_some();
        if self.chat_mode && can_chat {
            let opts = ChatOpts { line, cwd: Some(cwd), role: Some("resolve".into()), ..Default::default() };
            self.open_chat_with(harness, opts, window, cx);
            if let Some(TabKind::Chat { view, .. }) = self.tabs.last().map(|t| &t.kind) {
                let uid = view.read(cx).uid.clone();
                view.update(cx, |v, cx| v.submit_text(prompt, cx));
                self.pending_merges.insert(uid, number);
            }
        } else {
            let opts = SpawnOpts { line, paste: Some(prompt), role: Some("resolve".into()), ..Default::default() };
            self.spawn_terminal(harness, cwd, None, opts, window, cx);
            if let Some(TabKind::Terminal { view, .. }) = self.tabs.last().map(|t| &t.kind) {
                self.pending_merges.insert(view.read(cx).uid.clone(), number);
            }
        }
    }

    /// Terminó el turno de una sesión: si estaba resolviendo un PR, se une si ya se puede.
    pub(super) fn finish_pending_merge(&mut self, uid: &str, window: &mut Window, cx: &mut Context<Self>) {
        let Some(number) = self.pending_merges.remove(uid) else { return };
        let Some(repo) = self.repo.clone() else { return };
        self.set_pr_busy(number, Some("Checking the PR…".into()), None, cx);
        cx.spawn_in(window, async move |this, cx| {
            // GitHub tarda un poco en recalcular si se puede unir.
            let mut detail = None;
            for _ in 0..6 {
                let r = repo.clone();
                let d = cx.background_executor().spawn(async move { r.pr_detail(number) }).await;
                let ready = matches!(&d, Ok(d) if d.mergeable != "UNKNOWN");
                detail = Some(d);
                if ready {
                    break;
                }
                cx.background_executor().timer(Duration::from_secs(5)).await;
            }
            this.update_in(cx, |this, window, cx| match detail {
                Some(Ok(d)) if d.mergeable == "MERGEABLE" => this.merge_pr(number, window, cx),
                Some(Ok(_)) => {
                    let msg = format!("PR #{number} still can't be merged (conflicts or pending checks). Check the session's answer.");
                    this.set_pr_busy(number, None, Some(msg.clone()), cx);
                    this.notify(format!("PR #{number} not merged"), msg, false, window, cx);
                }
                Some(Err(e)) => this.set_pr_busy(number, None, Some(format!("{e:#}")), cx),
                None => {}
            })
            .ok();
        })
        .detach();
    }

    /// Fila del panel de PRs: título, de qué sesión salió y un solo botón.
    pub(super) fn pr_row(&self, pr: &PullRequest, cx: &mut Context<Self>) -> AnyElement {
        let number = pr.number;
        let conflicts = pr.has_conflicts();
        let merging = self.git.pr_merging == Some(number) || self.pending_merges.values().any(|n| *n == number);
        let session = self.session_of_pr(number, cx);
        let review = match pr.review_decision.as_deref() {
            Some("APPROVED") => Some(("approved", style::good())),
            Some("CHANGES_REQUESTED") => Some(("changes requested", style::bad())),
            _ => None,
        };
        v_flex()
            .id(("pr", number as usize))
            .px_2()
            .py_1p5()
            .gap_0p5()
            .rounded_md()
            .cursor_pointer()
            .hover(|d| d.bg(style::panel_hover()))
            .child(
                h_flex()
                    .gap_1p5()
                    .text_sm()
                    .child(div().text_color(style::muted()).child(format!("#{number}")))
                    .child(div().flex_1().min_w_0().truncate().child(pr.title.clone())),
            )
            .child(
                h_flex()
                    .gap_2()
                    .text_xs()
                    .text_color(style::faint())
                    .child(div().min_w_0().truncate().child(pr.head_ref_name.clone()))
                    .child(div().text_color(style::good()).child(format!("+{}", pr.additions)))
                    .child(div().text_color(style::bad()).child(format!("−{}", pr.deletions)))
                    .when(pr.is_draft, |d| d.child("draft"))
                    .when_some(review, |d, (t, c)| d.child(div().text_color(c).child(t))),
            )
            .child(
                h_flex()
                    .mt_0p5()
                    .gap_2()
                    .items_center()
                    .child(
                        div().flex_1().min_w_0().children(session.map(|(uid, name)| {
                            h_flex()
                                .id(("pr-session", number as usize))
                                .gap_1()
                                .text_xs()
                                .text_color(style::accent())
                                .cursor_pointer()
                                .hover(|d| d.text_color(style::text()))
                                .child(icons::ui("message-square", px(11.), style::accent()))
                                .child(div().truncate().child(crate::lines::short(&name, 32)))
                                .on_click(cx.listener(move |this, _, window, cx| {
                                    cx.stop_propagation();
                                    this.focus_session(&uid, window, cx);
                                }))
                        })),
                    )
                    .when(merging, |d| d.child(div().text_xs().text_color(style::accent()).child("Working…")))
                    .when(!merging && !pr.is_draft, |d| {
                        d.child(crate::pr_view::merge_button(("pr-merge", number as usize), conflicts, true).on_click(cx.listener(
                            move |this, _, window, cx| {
                                cx.stop_propagation();
                                if conflicts {
                                    this.resolve_and_merge(number, window, cx);
                                } else {
                                    this.merge_pr(number, window, cx);
                                }
                            },
                        )))
                    }),
            )
            .on_click(cx.listener(move |this, _, window, cx| this.open_pr(number, window, cx)))
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::pr_numbers_in;

    #[test]
    fn finds_pr_links() {
        let t = "Ya quedó el PR: #2278 https://github.com/o/r/pull/2278 y antes /pull/2223.";
        assert_eq!(pr_numbers_in(t), [2278, 2223]);
        assert!(pr_numbers_in("sin enlaces").is_empty());
    }
}
