//! Vista de proyecto (se completa en las siguientes fases).

use crate::style;
use gpui_kit::component::v_flex;
use gpui_kit::*;
use std::path::{Path, PathBuf};

pub enum ProjectEvent {
    GoHome,
}

pub struct ProjectView {
    root: PathBuf,
}

impl EventEmitter<ProjectEvent> for ProjectView {}

impl ProjectView {
    pub fn new(root: PathBuf, _window: &mut Window, _cx: &mut Context<Self>) -> Self {
        Self { root }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }
}

impl Render for ProjectView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        v_flex()
            .size_full()
            .items_center()
            .justify_center()
            .gap_2()
            .child(self.root.display().to_string())
            .child(
                div()
                    .id("back")
                    .cursor_pointer()
                    .text_color(style::accent())
                    .child("← Inicio")
                    .on_click(cx.listener(|_, _, _, cx| cx.emit(ProjectEvent::GoHome))),
            )
    }
}
