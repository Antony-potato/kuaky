//! Virtualized Git commit graph panel with colored branch lanes, commit nodes,
//! and ref badges (HEAD, branches, remotes, tags). Clicking a commit opens its diff.

use crate::style;
use gpui_kit::component::{h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use kuaky_git::{GraphRow, RefKind, Repo};
use std::ops::Range;

const LANE_WIDTH: f32 = 14.;
const ROW_HEIGHT: f32 = 28.;
const LANE_COLORS: [u32; 8] = [
    0x5eead4, // teal
    0xa78bfa, // violet
    0xe07a5f, // coral
    0x60a5fa, // blue
    0x4ade80, // green
    0xfbbf24, // amber
    0xf472b6, // pink
    0x38bdf8, // sky
];

pub enum GraphEvent {
    OpenCommit { hash: String, short_hash: String, subject: String },
}

pub struct GraphView {
    repo: Option<Repo>,
    rows: Vec<GraphRow>,
    loading: bool,
    error: Option<String>,
    selected: Option<String>,
    scroll: UniformListScrollHandle,
}

impl EventEmitter<GraphEvent> for GraphView {}

impl GraphView {
    pub fn new(repo: Option<Repo>, cx: &mut Context<Self>) -> Self {
        let mut this = Self {
            repo,
            rows: Vec::new(),
            loading: false,
            error: None,
            selected: None,
            scroll: UniformListScrollHandle::new(),
        };
        this.refresh(cx);
        this
    }

    pub fn refresh(&mut self, cx: &mut Context<Self>) {
        let Some(repo) = self.repo.clone() else { return };
        if self.loading {
            return;
        }
        self.loading = true;
        cx.spawn(async move |this, cx| {
            let result = cx.background_executor().spawn(async move { repo.graph_log(300) }).await;
            this.update(cx, |this, cx| {
                this.loading = false;
                match result {
                    Ok(rows) => {
                        this.rows = rows;
                        this.error = None;
                    }
                    Err(err) => this.error = Some(format!("{err:#}")),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn render_row(&self, index: usize, row: &GraphRow, cx: &mut Context<Self>) -> AnyElement {
        let lane_cols = row.width.clamp(1, 8);
        let graph_w = px(lane_cols as f32 * LANE_WIDTH + 10.);
        let row_data = row.clone();
        let selected = self.selected.as_deref() == Some(row.commit.hash.as_str());
        let hash = row.commit.hash.clone();
        let short = row.commit.short_hash.clone();
        let subject = row.commit.subject.clone();

        h_flex()
            .id(("graph-row", index))
            .h(px(ROW_HEIGHT))
            .w_full()
            .px_2()
            .gap_1p5()
            .items_center()
            .cursor_pointer()
            .when(selected, |d| d.bg(style::panel_hover()))
            .hover(|d| d.bg(style::panel_hover()))
            .child(
                div().w(graph_w).h(px(ROW_HEIGHT)).flex_none().child(
                    canvas(
                        |_, _, _| {},
                        move |bounds, _, window, _| paint_lanes(bounds, &row_data, window),
                    )
                    .size_full(),
                ),
            )
            .child(
                h_flex()
                    .flex_1()
                    .min_w_0()
                    .gap_1()
                    .items_center()
                    .children(row.commit.refs.iter().take(2).map(|r| {
                        let (bg, fg, border): (Hsla, Rgba, Hsla) = match r.kind {
                            RefKind::Head => (style::soft(style::accent(), 0.2), style::accent(), style::soft(style::accent(), 0.55)),
                            RefKind::LocalBranch => {
                                let c = lane_color(row.color);
                                (style::soft(c, 0.16), c, style::soft(c, 0.45))
                            }
                            RefKind::Tag => (style::soft(style::warn(), 0.15), style::warn(), style::soft(style::warn(), 0.4)),
                            RefKind::RemoteBranch => (style::panel_hover().into(), style::muted(), style::border().into()),
                        };
                        div()
                            .flex_none()
                            .max_w(px(96.))
                            .truncate()
                            .px_1()
                            .rounded_sm()
                            .border_1()
                            .border_color(border)
                            .bg(bg)
                            .text_size(px(10.))
                            .text_color(fg)
                            .child(r.name.clone())
                    }))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_xs()
                            .text_color(if row.commit.is_head { style::text() } else { style::muted() })
                            .child(row.commit.subject.clone()),
                    ),
            )
            .child(
                div()
                    .flex_none()
                    .font_family(style::mono_font())
                    .text_size(px(10.))
                    .text_color(style::faint())
                    .child(row.commit.short_hash.clone()),
            )
            .on_click(cx.listener(move |this, _, _, cx| {
                this.selected = Some(hash.clone());
                cx.emit(GraphEvent::OpenCommit {
                    hash: hash.clone(),
                    short_hash: short.clone(),
                    subject: subject.clone(),
                });
                cx.notify();
            }))
            .into_any_element()
    }
}

impl Render for GraphView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if self.repo.is_none() {
            return div().p_3().text_xs().text_color(style::muted()).child("This folder is not a git repository.").into_any_element();
        }
        if let Some(err) = &self.error {
            return div().p_3().text_xs().text_color(style::bad()).child(err.clone()).into_any_element();
        }
        if self.rows.is_empty() {
            let msg = if self.loading { "Loading commit graph…" } else { "No commits yet." };
            return div().p_3().text_xs().text_color(style::muted()).child(msg).into_any_element();
        }
        let count = self.rows.len();
        v_flex()
            .size_full()
            .child(
                uniform_list(
                    "git-graph-list",
                    count,
                    cx.processor(|this, range: Range<usize>, _, cx| {
                        range
                            .filter_map(|i| this.rows.get(i).map(|r| this.render_row(i, r, cx)))
                            .collect::<Vec<_>>()
                    }),
                )
                .track_scroll(&self.scroll)
                .flex_1()
                .size_full(),
            )
            .into_any_element()
    }
}

fn lane_color(idx: usize) -> Rgba {
    style::lane(LANE_COLORS[idx % LANE_COLORS.len()])
}

fn lane_x(origin_x: Pixels, col: usize) -> Pixels {
    origin_x + px(8. + col as f32 * LANE_WIDTH)
}

fn paint_lanes(bounds: Bounds<Pixels>, row: &GraphRow, window: &mut Window) {
    let top = bounds.origin.y;
    let mid = bounds.origin.y + bounds.size.height * 0.5;
    let bot = bounds.origin.y + bounds.size.height;

    // 1. Pass-through vertical lanes.
    for &(col, color) in &row.pass_through {
        let x = lane_x(bounds.origin.x, col);
        let mut p = PathBuilder::stroke(px(1.6));
        p.move_to(point(x, top));
        p.line_to(point(x, bot));
        if let Ok(path) = p.build() {
            window.paint_path(path, lane_color(color));
        }
    }

    // 2. Incoming edges (top -> commit center).
    for edge in &row.incoming {
        let x0 = lane_x(bounds.origin.x, edge.from);
        let x1 = lane_x(bounds.origin.x, edge.to);
        let mut p = PathBuilder::stroke(px(1.8));
        p.move_to(point(x0, top));
        if edge.from == edge.to {
            p.line_to(point(x1, mid));
        } else {
            p.curve_to(point(x1, mid), point(x0, mid));
        }
        if let Ok(path) = p.build() {
            window.paint_path(path, lane_color(edge.color));
        }
    }

    // 3. Outgoing edges (commit center -> bottom).
    for edge in &row.outgoing {
        let x0 = lane_x(bounds.origin.x, edge.from);
        let x1 = lane_x(bounds.origin.x, edge.to);
        let mut p = PathBuilder::stroke(px(1.8));
        p.move_to(point(x0, mid));
        if edge.from == edge.to {
            p.line_to(point(x1, bot));
        } else {
            p.curve_to(point(x1, bot), point(x1, mid));
        }
        if let Ok(path) = p.build() {
            window.paint_path(path, lane_color(edge.color));
        }
    }

    // 4. Commit dot (double ring for HEAD).
    let cx_pt = lane_x(bounds.origin.x, row.lane);
    let r = if row.commit.is_head { px(4.5) } else { px(3.5) };
    let dot_color = lane_color(row.color);
    let dot_bounds = Bounds {
        origin: point(cx_pt - r, mid - r),
        size: size(r * 2., r * 2.),
    };
    window.paint_quad(PaintQuad {
        bounds: dot_bounds,
        corner_radii: Corners::all(r),
        background: if row.commit.is_head { style::bg().into() } else { dot_color.into() },
        border_widths: Edges::all(if row.commit.is_head { px(2.) } else { px(0.) }),
        border_color: dot_color.into(),
        border_style: BorderStyle::Solid,
    });
}
