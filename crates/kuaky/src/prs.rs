//! Pull requests ligados a las sesiones: qué sesión hizo cada PR, un solo botón
//! *Merge*, y *Resolve & merge* (un agente resuelve los conflictos en la rama
//! y, cuando termina su turno, kuaky hace el merge).

use super::*;
use crate::pr_view::{PrEvent, PrView};

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum PrFilterMenu {
    Date,
    Author,
}

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
                TabKind::Terminal { view, cwd, worktree, harness, .. } if *harness != Harness::Shell => {
                    let branch = worktree.clone().or_else(|| self.branch_of(cwd));
                    if let Some(n) = branch.as_deref().and_then(by_branch) {
                        map.insert(view.read(cx).uid.clone(), n);
                    }
                }
                _ => {}
            }
        }
        // Sesiones cerradas del mapa (para "esta PR la hizo…"), por carpeta o rama de su línea.
        for line in &self.lines.lines {
            let line_pr = line.branch.as_deref().and_then(by_branch);
            for s in &line.sessions {
                if map.contains_key(&s.uid) {
                    continue;
                }
                let by_cwd = (!s.cwd.is_empty()).then(|| self.branch_of(Path::new(&s.cwd)).and_then(|b| by_branch(&b))).flatten();
                if let Some(n) = by_cwd.or(line_pr) {
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
        let agents: Vec<Harness> = self.available_harnesses(cx).into_iter().filter(|h| *h != Harness::Shell).collect();
        // Las pestañas de PR abiertas se actualizan.
        for tab in &self.tabs {
            if let TabKind::Pr(view) = &tab.kind {
                let n = view.read(cx).number;
                if let Some(pr) = prs.iter().find(|p| p.number == n).cloned() {
                    let session = self.session_of_pr(n, cx);
                    let agents = agents.clone();
                    view.update(cx, |v, cx| v.set_pr(pr, session, agents, cx));
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
        let agents: Vec<Harness> = self.available_harnesses(cx).into_iter().filter(|h| *h != Harness::Shell).collect();
        let view = cx.new(|cx| PrView::new(repo, pr, session, agents, cx));
        let sub = cx.subscribe_in(&view, window, |this, _, ev: &PrEvent, window, cx| match ev {
            PrEvent::Merge(n) => this.merge_pr(*n, window, cx),
            PrEvent::ResolveAndMerge(n) => this.resolve_and_merge(*n, window, cx),
            PrEvent::OpenSession(uid) => this.focus_session(uid, window, cx),
            PrEvent::StartSession { number, harness, body } => this.start_pr_session(*number, *harness, body.clone(), window, cx),
        });
        self._subs.push(sub);
        self.push_tab(TabKind::Pr(view), window, cx);
    }

    /// Inicia una sesión nueva con el contexto completo del PR (en su worktree si existe,
    /// o en la raíz) y envía el mensaje inicial de inmediato sin pedir confirmación.
    pub(super) fn start_pr_session(
        &mut self,
        number: u64,
        harness: Harness,
        extra_body: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(pr) = self.open_prs().into_iter().find(|p| p.number == number) else { return };
        let worktree = self
            .git
            .worktrees
            .iter()
            .find(|w| w.branch.as_deref() == Some(pr.head_ref_name.as_str()))
            .map(|w| w.path.clone());
        let cwd = worktree.clone().unwrap_or_else(|| self.root.clone());
        let line = self
            .lines
            .lines
            .iter()
            .find(|l| l.branch.as_deref() == Some(pr.head_ref_name.as_str()))
            .map(|l| l.id.clone());
        let body = extra_body
            .as_deref()
            .map(str::trim)
            .filter(|b| !b.is_empty())
            .or_else(|| {
                let b = pr.body.trim();
                (!b.is_empty()).then_some(b)
            });
        let prompt = pr_session_prompt(&pr, body, worktree.as_deref());
        let has_chat = harness != Harness::Shell && (self.chat_agents.contains_key(&harness) || harness.chat_command().is_some());
        if has_chat {
            let opts = ChatOpts { line, cwd: Some(cwd), ..Default::default() };
            self.open_chat_with(harness, opts, window, cx);
            if let Some(TabKind::Chat { view, .. }) = self.tabs.last().map(|t| &t.kind) {
                view.update(cx, |v, cx| {
                    if !v.mentioned_prs.contains(&number) {
                        v.mentioned_prs.push(number);
                    }
                    v.submit_text(prompt, cx);
                });
                self.sync_session_prs(cx);
            }
        } else {
            let opts = SpawnOpts { line, ..Default::default() };
            self.spawn_terminal(harness, cwd, Some(pr.head_ref_name.clone()), opts, window, cx);
            if let Some(TabKind::Terminal { view, .. }) = self.tabs.last().map(|t| &t.kind) {
                let uid = view.read(cx).uid.clone();
                view.update(cx, |v, _| v.submit_text(&prompt));
                self.session_prs.insert(uid, number);
                self.sync_session_prs(cx);
            }
        }
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
        if self.opens_as_chat(harness, cx) {
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

    fn pr_filter_just_closed(&self) -> bool {
        self.pr_filter_closed.is_some_and(|t| t.elapsed() < Duration::from_millis(300))
    }

    fn pr_session_just_closed(&self) -> bool {
        self.pr_session_closed.is_some_and(|t| t.elapsed() < Duration::from_millis(300))
    }

    /// Panel derecho de PRs: barra de filtros (día y autor) + actualización + filas.
    pub(super) fn prs_panel_view(&self, cx: &mut Context<Self>) -> AnyElement {
        if self.repo.is_none() {
            return panel_message("This folder is not a git repository.").into_any_element();
        }
        let tz = jiff::tz::TimeZone::system();
        let today = jiff::Zoned::now().date();
        let all_prs = self.open_prs();
        let authors = pr_authors(&all_prs);
        let filtered: Vec<&PullRequest> = all_prs
            .iter()
            .filter(|pr| pr_matches_filters(pr, &self.pr_date_filter, self.pr_author_filter.as_deref(), today, &tz))
            .collect();
        let has_filters = self.pr_date_filter != DateFilter::All || self.pr_author_filter.is_some();

        let date_open = self.pr_filter_menu == Some(PrFilterMenu::Date);
        let date_active = self.pr_date_filter != DateFilter::All;
        let author_open = self.pr_filter_menu == Some(PrFilterMenu::Author);
        let author_active = self.pr_author_filter.is_some();

        let date_item = |id: SharedString, text: String, target: DateFilter, cx: &mut Context<Self>| {
            let selected = self.pr_date_filter == target;
            h_flex()
                .id(id)
                .gap_2()
                .px_2()
                .py_1()
                .rounded_md()
                .text_xs()
                .cursor_pointer()
                .text_color(if selected { style::text() } else { style::muted() })
                .when(selected, |d| d.bg(style::panel_hover()))
                .hover(|d| d.bg(style::panel_hover()).text_color(style::text()))
                .child(div().w(px(12.)).flex_none().when(selected, |d| d.child(icons::ui("check", px(11.), style::accent()))))
                .child(div().flex_1().min_w_0().truncate().child(text))
                .on_click(cx.listener(move |this, _, _, cx| {
                    cx.stop_propagation();
                    this.pr_date_filter = target.clone();
                    this.pr_filter_menu = None;
                    this.pr_filter_pick_day = false;
                    cx.notify();
                }))
                .into_any_element()
        };

        let day_selected = matches!(self.pr_date_filter, DateFilter::Day(_));
        let pick_open = self.pr_filter_pick_day || day_selected;

        let toolbar = h_flex()
            .gap_1()
            .items_center()
            .child(
                h_flex()
                    .id("pr-filter-date")
                    .relative()
                    .gap_1()
                    .px_2()
                    .py_1()
                    .rounded_md()
                    .border_1()
                    .border_color(style::border())
                    .text_xs()
                    .cursor_pointer()
                    .text_color(if date_active || date_open { style::text() } else { style::muted() })
                    .when(date_active || date_open, |d| d.bg(style::panel_hover()))
                    .hover(|d| d.bg(style::panel_hover()).text_color(style::text()))
                    .tooltip(|window, cx| Tooltip::new("Filter PRs by last updated day").build(window, cx))
                    .child(div().max_w(px(84.)).truncate().child(self.pr_date_filter.label()))
                    .child(icons::ui("chevron-down", px(10.), style::faint()))
                    .on_click(cx.listener(|this, _, _, cx| {
                        cx.stop_propagation();
                        this.pr_session_menu = None;
                        this.pr_filter_menu = if this.pr_filter_menu == Some(PrFilterMenu::Date) || this.pr_filter_just_closed() {
                            None
                        } else {
                            Some(PrFilterMenu::Date)
                        };
                        if this.pr_filter_menu.is_none() {
                            this.pr_filter_pick_day = false;
                        }
                        cx.notify();
                    }))
                    .when(date_open, |d| {
                        let days = DateFilter::last_14_days(today);
                        d.child(
                            deferred(
                                div().absolute().top_full().left_0().mt_1().child(
                                    v_flex()
                                        .id("pr-date-menu")
                                        .occlude()
                                        .w(px(192.))
                                        .p_1()
                                        .rounded_lg()
                                        .border_1()
                                        .border_color(style::border())
                                        .bg(style::panel())
                                        .shadow_lg()
                                        .text_color(style::text())
                                        .child(date_item("pf-all".into(), "All".into(), DateFilter::All, cx))
                                        .child(date_item("pf-today".into(), "Today".into(), DateFilter::Today, cx))
                                        .child(date_item("pf-yesterday".into(), "Yesterday".into(), DateFilter::Yesterday, cx))
                                        .child(date_item("pf-week".into(), "This week".into(), DateFilter::ThisWeek, cx))
                                        .child(
                                            h_flex()
                                                .id("pf-pick-day")
                                                .gap_2()
                                                .px_2()
                                                .py_1()
                                                .rounded_md()
                                                .text_xs()
                                                .cursor_pointer()
                                                .text_color(if day_selected { style::text() } else { style::muted() })
                                                .hover(|d| d.bg(style::panel_hover()).text_color(style::text()))
                                                .child(
                                                    div()
                                                        .w(px(12.))
                                                        .flex_none()
                                                        .child(icons::ui(if pick_open { "chevron-down" } else { "chevron-right" }, px(11.), style::faint())),
                                                )
                                                .child("Pick a day…")
                                                .on_click(cx.listener(|this, _, _, cx| {
                                                    cx.stop_propagation();
                                                    this.pr_filter_pick_day = !this.pr_filter_pick_day;
                                                    cx.notify();
                                                })),
                                        )
                                        .when(pick_open, |d| {
                                            d.child(
                                                div()
                                                    .id("pf-days-list")
                                                    .max_h(px(210.))
                                                    .overflow_y_scroll()
                                                    .pl_3()
                                                    .child(v_flex().children(days.into_iter().map(|(ymd, text)| {
                                                        date_item(SharedString::from(format!("pf-day-{ymd}")), text, DateFilter::Day(ymd), cx)
                                                    }))),
                                            )
                                        })
                                        .on_mouse_down_out(cx.listener(|this, _, _, cx| {
                                            if this.pr_filter_menu == Some(PrFilterMenu::Date) {
                                                this.pr_filter_menu = None;
                                                this.pr_filter_pick_day = false;
                                                this.pr_filter_closed = Some(Instant::now());
                                                cx.notify();
                                            }
                                        })),
                                ),
                            )
                            .with_priority(1),
                        )
                    }),
            )
            .child(
                h_flex()
                    .id("pr-filter-author")
                    .relative()
                    .flex_1()
                    .min_w_0()
                    .gap_1()
                    .px_2()
                    .py_1()
                    .rounded_md()
                    .border_1()
                    .border_color(style::border())
                    .text_xs()
                    .cursor_pointer()
                    .text_color(if author_active || author_open { style::text() } else { style::muted() })
                    .when(author_active || author_open, |d| d.bg(style::panel_hover()))
                    .hover(|d| d.bg(style::panel_hover()).text_color(style::text()))
                    .tooltip(|window, cx| Tooltip::new("Filter PRs by author").build(window, cx))
                    .child(div().flex_1().min_w_0().truncate().child(
                        self.pr_author_filter.clone().unwrap_or_else(|| "Author".into()),
                    ))
                    .child(icons::ui("chevron-down", px(10.), style::faint()))
                    .on_click(cx.listener(|this, _, _, cx| {
                        cx.stop_propagation();
                        this.pr_session_menu = None;
                        this.pr_filter_menu = if this.pr_filter_menu == Some(PrFilterMenu::Author) || this.pr_filter_just_closed() {
                            None
                        } else {
                            Some(PrFilterMenu::Author)
                        };
                        cx.notify();
                    }))
                    .when(author_open, |d| {
                        let current = self.pr_author_filter.clone();
                        d.child(
                            deferred(
                                div().absolute().top_full().right_0().mt_1().child(
                                    v_flex()
                                        .id("pr-author-menu")
                                        .occlude()
                                        .w(px(196.))
                                        .max_h(px(260.))
                                        .overflow_y_scroll()
                                        .p_1()
                                        .rounded_lg()
                                        .border_1()
                                        .border_color(style::border())
                                        .bg(style::panel())
                                        .shadow_lg()
                                        .text_color(style::text())
                                        .child(
                                            h_flex()
                                                .id("pa-all")
                                                .gap_2()
                                                .px_2()
                                                .py_1()
                                                .rounded_md()
                                                .text_xs()
                                                .cursor_pointer()
                                                .text_color(if current.is_none() { style::text() } else { style::muted() })
                                                .when(current.is_none(), |d| d.bg(style::panel_hover()))
                                                .hover(|d| d.bg(style::panel_hover()).text_color(style::text()))
                                                .child(
                                                    div()
                                                        .w(px(12.))
                                                        .flex_none()
                                                        .when(current.is_none(), |d| d.child(icons::ui("check", px(11.), style::accent()))),
                                                )
                                                .child("All authors")
                                                .on_click(cx.listener(|this, _, _, cx| {
                                                    cx.stop_propagation();
                                                    this.pr_author_filter = None;
                                                    this.pr_filter_menu = None;
                                                    cx.notify();
                                                })),
                                        )
                                        .when(authors.is_empty(), |d| {
                                            d.child(div().px_2().py_1().text_xs().text_color(style::faint()).child("No authors"))
                                        })
                                        .children(authors.into_iter().map(|(login, count)| {
                                            let selected = current.as_deref() == Some(login.as_str());
                                            let target = login.clone();
                                            h_flex()
                                                .id(SharedString::from(format!("pa-{login}")))
                                                .gap_2()
                                                .px_2()
                                                .py_1()
                                                .rounded_md()
                                                .text_xs()
                                                .cursor_pointer()
                                                .text_color(if selected { style::text() } else { style::muted() })
                                                .when(selected, |d| d.bg(style::panel_hover()))
                                                .hover(|d| d.bg(style::panel_hover()).text_color(style::text()))
                                                .child(
                                                    div()
                                                        .w(px(12.))
                                                        .flex_none()
                                                        .when(selected, |d| d.child(icons::ui("check", px(11.), style::accent()))),
                                                )
                                                .child(div().flex_1().min_w_0().truncate().child(login))
                                                .child(div().flex_none().text_color(style::faint()).child(count.to_string()))
                                                .on_click(cx.listener(move |this, _, _, cx| {
                                                    cx.stop_propagation();
                                                    this.pr_author_filter = Some(target.clone());
                                                    this.pr_filter_menu = None;
                                                    cx.notify();
                                                }))
                                        }))
                                        .on_mouse_down_out(cx.listener(|this, _, _, cx| {
                                            if this.pr_filter_menu == Some(PrFilterMenu::Author) {
                                                this.pr_filter_menu = None;
                                                this.pr_filter_closed = Some(Instant::now());
                                                cx.notify();
                                            }
                                        })),
                                ),
                            )
                            .with_priority(1),
                        )
                    }),
            )
            .child(
                div()
                    .id("prs-refresh")
                    .flex_none()
                    .p_1()
                    .rounded_md()
                    .border_1()
                    .border_color(style::border())
                    .cursor_pointer()
                    .hover(|d| d.bg(style::panel_hover()))
                    .tooltip(|window, cx| Tooltip::new("Refresh PRs").build(window, cx))
                    .child(icons::ui("refresh-cw", px(12.), if self.git.prs_loading { style::faint() } else { style::muted() }))
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.load_prs(cx);
                        cx.notify();
                    })),
            );

        let body = match &self.git.prs {
            None => panel_message("Loading PRs…").into_any_element(),
            Some(Err(e)) => panel_message(e.clone()).into_any_element(),
            Some(Ok(prs)) if prs.is_empty() => panel_message("No open PRs.").into_any_element(),
            Some(Ok(_)) if filtered.is_empty() && has_filters => h_flex()
                .p_3()
                .gap_2()
                .items_center()
                .text_xs()
                .text_color(style::faint())
                .child("No PRs match these filters.")
                .child(
                    div()
                        .id("prs-clear-filters")
                        .cursor_pointer()
                        .text_color(style::accent())
                        .hover(|d| d.text_color(style::text()))
                        .child("Clear")
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.pr_date_filter = DateFilter::All;
                            this.pr_author_filter = None;
                            cx.notify();
                        })),
                )
                .into_any_element(),
            Some(Ok(_)) => v_flex()
                .gap_0p5()
                .children(filtered.into_iter().map(|pr| self.pr_row(pr, cx)))
                .into_any_element(),
        };
        div()
            .id("prs")
            .size_full()
            .overflow_y_scroll()
            .child(
                v_flex()
                    .p_2()
                    .gap_1()
                    .child(toolbar)
                    .when_some(self.git.pr_action_error.clone(), |d, e| {
                        d.child(div().px_2().py_1().text_xs().text_color(style::bad()).child(e))
                    })
                    .child(body),
            )
            .into_any_element()
    }

    /// Fila del panel de PRs: título, botón de sesión (Open / Start) y botón de merge.
    pub(super) fn pr_row(&self, pr: &PullRequest, cx: &mut Context<Self>) -> AnyElement {
        let number = pr.number;
        let conflicts = pr.has_conflicts();
        let merging = self.git.pr_merging == Some(number) || self.pending_merges.values().any(|n| *n == number);
        let session = self.session_of_pr(number, cx);
        let menu_open = self.pr_session_menu == Some(number);
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
                    .when(!pr.author.login.is_empty(), |d| d.child(pr.author.login.clone()))
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
                        div().flex_1().min_w_0().child(match session {
                            Some((uid, name)) => {
                                let tip: SharedString = format!("Open session: {}", crate::lines::short(&name, 60)).into();
                                h_flex()
                                    .id(("pr-session", number as usize))
                                    .gap_1()
                                    .px_1p5()
                                    .py_0p5()
                                    .rounded_sm()
                                    .border_1()
                                    .border_color(style::border())
                                    .text_xs()
                                    .text_color(style::text())
                                    .cursor_pointer()
                                    .hover(|d| d.bg(style::bg()))
                                    .tooltip(move |window, cx| Tooltip::new(tip.clone()).build(window, cx))
                                    .child(icons::ui("message-square", px(11.), style::accent()))
                                    .child("Open session")
                                    .on_click(cx.listener(move |this, _, window, cx| {
                                        cx.stop_propagation();
                                        this.focus_session(&uid, window, cx);
                                    }))
                                    .into_any_element()
                            }
                            None => h_flex()
                                .id(("pr-start-session", number as usize))
                                .relative()
                                .gap_1()
                                .px_1p5()
                                .py_0p5()
                                .rounded_sm()
                                .border_1()
                                .border_color(style::border())
                                .text_xs()
                                .text_color(if menu_open { style::text() } else { style::muted() })
                                .when(menu_open, |d| d.bg(style::bg()))
                                .cursor_pointer()
                                .hover(|d| d.bg(style::bg()).text_color(style::text()))
                                .tooltip(|window, cx| Tooltip::new("Start an agent session with this PR's context").build(window, cx))
                                .child(icons::ui("plus", px(11.), style::muted()))
                                .child("Start session…")
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    cx.stop_propagation();
                                    this.pr_filter_menu = None;
                                    this.pr_session_menu = if this.pr_session_menu == Some(number) || this.pr_session_just_closed() {
                                        None
                                    } else {
                                        Some(number)
                                    };
                                    cx.notify();
                                }))
                                .when(menu_open, |d| {
                                    d.child(
                                        deferred(div().absolute().top_full().left_0().mt_1().child(self.agent_menu_for_pr(number, cx)))
                                            .with_priority(1),
                                    )
                                })
                                .into_any_element(),
                        }),
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

/// Prompt inicial para una sesión ligada a un PR (enviado al abrir el chat).
fn pr_session_prompt(pr: &PullRequest, body: Option<&str>, worktree: Option<&Path>) -> String {
    let mut lines = vec![
        format!("PR #{}: {}", pr.number, pr.title),
        format!("Branches: {} → {}", pr.head_ref_name, pr.base_ref_name),
    ];
    if !pr.author.login.is_empty() {
        lines.push(format!("Author: {}", pr.author.login));
    }
    if !pr.url.is_empty() {
        lines.push(format!("URL: {}", pr.url));
    }
    if let Some(wt) = worktree {
        lines.push(format!("Worktree: {}", wt.to_string_lossy()));
    }
    if let Some(desc) = body.map(str::trim).filter(|b| !b.is_empty()) {
        let truncated: String = desc.chars().take(2000).collect();
        let suffix = if desc.chars().count() > 2000 { "…" } else { "" };
        lines.push(format!("\nDescription:\n{truncated}{suffix}"));
    }
    lines.push(format!(
        "\nTo inspect the diff, run `gh pr diff {}` or `git diff {}...{}`.",
        pr.number, pr.base_ref_name, pr.head_ref_name
    ));
    lines.push("Review the PR context, summarize the changes briefly, and let me know when you're ready.".into());
    lines.join("\n")
}

/// Autores presentes en los PRs abiertos con su cantidad, ordenados por nombre.
fn pr_authors(prs: &[PullRequest]) -> Vec<(String, usize)> {
    let mut counts: HashMap<String, usize> = HashMap::new();
    for pr in prs {
        if !pr.author.login.is_empty() {
            *counts.entry(pr.author.login.clone()).or_default() += 1;
        }
    }
    let mut out: Vec<(String, usize)> = counts.into_iter().collect();
    out.sort_by(|(a, _), (b, _)| a.to_lowercase().cmp(&b.to_lowercase()).then_with(|| a.cmp(b)));
    out
}

fn pr_matches_filters(
    pr: &PullRequest,
    date_filter: &DateFilter,
    author_filter: Option<&str>,
    today: jiff::civil::Date,
    tz: &jiff::tz::TimeZone,
) -> bool {
    if let Some(author) = author_filter {
        if pr.author.login != author {
            return false;
        }
    }
    if *date_filter != DateFilter::All {
        let Ok(ts) = pr.updated_at.parse::<jiff::Timestamp>() else { return false };
        if !date_filter.matches(ts, today, tz) {
            return false;
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::{pr_authors, pr_matches_filters, pr_numbers_in, pr_session_prompt};
    use crate::project::DateFilter;
    use kuaky_git::PullRequest;
    use std::path::Path;

    fn sample_pr(number: u64, author: &str, updated_at: &str) -> PullRequest {
        let json = format!(
            r#"{{"number":{number},"title":"Fix bug","author":{{"login":"{author}"}},"headRefName":"fix/bug","baseRefName":"main","isDraft":false,"updatedAt":"{updated_at}","url":"https://github.com/o/r/pull/{number}","body":"Detailed description"}}"#
        );
        serde_json::from_str(&json).unwrap()
    }

    #[test]
    fn finds_pr_links() {
        let t = "Ya quedó el PR: #2278 https://github.com/o/r/pull/2278 y antes /pull/2223.";
        assert_eq!(pr_numbers_in(t), [2278, 2223]);
        assert!(pr_numbers_in("sin enlaces").is_empty());
    }

    #[test]
    fn builds_pr_session_prompt_with_context_and_diff_commands() {
        let pr = sample_pr(42, "ana", "2026-11-26T15:00:00Z");
        let p = pr_session_prompt(&pr, Some(&pr.body), Some(Path::new("/tmp/wt")));
        assert!(p.contains("PR #42: Fix bug"));
        assert!(p.contains("fix/bug → main"));
        assert!(p.contains("Author: ana"));
        assert!(p.contains("https://github.com/o/r/pull/42"));
        assert!(p.contains("Detailed description"));
        assert!(p.contains("gh pr diff 42"));
        assert!(p.contains("git diff main...fix/bug"));
        assert!(p.contains("/tmp/wt"));
    }

    #[test]
    fn filters_prs_by_date_and_author() {
        let tz = jiff::tz::TimeZone::UTC;
        let today = jiff::civil::date(2026, 11, 26);
        let a = sample_pr(1, "ana", "2026-11-26T10:00:00Z");
        let b = sample_pr(2, "bob", "2026-11-25T10:00:00Z");
        let c = sample_pr(3, "ana", "2026-11-10T10:00:00Z");

        assert!(pr_matches_filters(&a, &DateFilter::Today, None, today, &tz));
        assert!(!pr_matches_filters(&b, &DateFilter::Today, None, today, &tz));
        assert!(pr_matches_filters(&b, &DateFilter::Yesterday, Some("bob"), today, &tz));
        assert!(!pr_matches_filters(&b, &DateFilter::Yesterday, Some("ana"), today, &tz));
        assert!(pr_matches_filters(&c, &DateFilter::Day("2026-11-10".into()), Some("ana"), today, &tz));
        assert_eq!(pr_authors(&[a, b, c]), vec![("ana".into(), 2), ("bob".into(), 1)]);
    }
}
