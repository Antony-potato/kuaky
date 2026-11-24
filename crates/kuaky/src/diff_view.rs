//! Pestaña de diff (cambios locales, ramas o PRs) con el componente `Diff` de
//! GPUI Kit.

use crate::style;
use gpui_kit::component::diff::{Diff, DiffFile, DiffMode, DiffState};
use gpui_kit::component::{h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

pub struct DiffView {
    pub title: SharedString,
    subtitle: SharedString,
    state: Entity<DiffState>,
    loading: bool,
    error: Option<String>,
    files: usize,
    mode: DiffMode,
}

impl DiffView {
    /// `load` corre en segundo plano y devuelve el patch en formato unificado.
    pub fn new(
        title: impl Into<SharedString>,
        subtitle: impl Into<SharedString>,
        load: impl FnOnce() -> anyhow::Result<String> + Send + 'static,
        cx: &mut Context<Self>,
    ) -> Self {
        let state = cx.new(|cx| DiffState::new([], cx).with_mode(DiffMode::Unified));
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move { load().and_then(|patch| DiffFile::parse(&patch).map_err(|e| anyhow::anyhow!("{e}"))) })
                .await;
            this.update(cx, |this, cx| {
                this.loading = false;
                match result {
                    Ok(files) => {
                        this.files = files.len();
                        this.state.update(cx, |s, cx| s.set_files(files, cx));
                    }
                    Err(err) => this.error = Some(format!("{err:#}")),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
        Self {
            title: title.into(),
            subtitle: subtitle.into(),
            state,
            loading: true,
            error: None,
            files: 0,
            mode: DiffMode::Unified,
        }
    }

    fn toggle_mode(&mut self, cx: &mut Context<Self>) {
        self.mode = if self.mode == DiffMode::Unified { DiffMode::Split } else { DiffMode::Unified };
        let mode = self.mode;
        self.state.update(cx, |s, cx| s.set_mode(mode, cx));
        cx.notify();
    }
}

impl Render for DiffView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let status = if self.loading {
            "loading…".to_string()
        } else if self.error.is_none() {
            if self.files == 1 { "1 file".to_string() } else { format!("{} files", self.files) }
        } else {
            String::new()
        };
        v_flex()
            .size_full()
            .child(
                h_flex()
                    .h(px(30.))
                    .px_3()
                    .gap_3()
                    .text_xs()
                    .border_b_1()
                    .border_color(style::border())
                    .child(div().text_color(style::text()).child(self.subtitle.clone()))
                    .child(div().text_color(style::muted()).child(status))
                    .child(div().flex_1())
                    .child(
                        div()
                            .id("diff-mode")
                            .cursor_pointer()
                            .text_color(style::muted())
                            .hover(|d| d.text_color(style::text()))
                            .child(if self.mode == DiffMode::Unified { "Split view" } else { "Unified view" })
                            .on_click(cx.listener(|this, _, _, cx| this.toggle_mode(cx))),
                    ),
            )
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .when_some(self.error.clone(), |d, e| d.p_4().text_sm().text_color(style::bad()).child(e))
                    .when(!self.loading && self.error.is_none() && self.files == 0, |d| {
                        d.p_4().text_sm().text_color(style::muted()).child("No differences.")
                    })
                    .when(self.error.is_none() && self.files > 0, |d| d.child(Diff::new(&self.state).size_full())),
            )
    }
}
