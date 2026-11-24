//! Árbol de archivos del proyecto. Carga cada carpeta al expandirla y respeta
//! `.gitignore`.

use crate::style;
use gpui_kit::component::{h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use kuaky_git::ChangeKind;
use std::{
    collections::{HashMap, HashSet},
    path::{Path, PathBuf},
};

#[derive(Clone)]
struct Node {
    path: PathBuf,
    name: String,
    is_dir: bool,
}

pub enum FileTreeEvent {
    OpenFile(PathBuf),
}

pub struct FileTree {
    root: PathBuf,
    expanded: HashSet<PathBuf>,
    children: HashMap<PathBuf, Vec<Node>>,
    selected: Option<PathBuf>,
    /// Estado git por ruta relativa al repo.
    changes: HashMap<PathBuf, ChangeKind>,
}

impl EventEmitter<FileTreeEvent> for FileTree {}

impl FileTree {
    pub fn new(root: PathBuf, cx: &mut Context<Self>) -> Self {
        let mut this = Self {
            root: root.clone(),
            expanded: HashSet::from([root.clone()]),
            children: HashMap::new(),
            selected: None,
            changes: HashMap::new(),
        };
        this.children.insert(root.clone(), list_dir(&root));
        // Capturas: `KUAKY_DEMO_EXPAND=crates,crates/kuaky` abre esas carpetas.
        #[cfg(feature = "screenshot")]
        if let Ok(dirs) = std::env::var("KUAKY_DEMO_EXPAND") {
            for d in dirs.split(',') {
                let p = root.join(d);
                this.expanded.insert(p.clone());
                this.children.insert(p.clone(), list_dir(&p));
            }
        }
        this.start_refresh(cx);
        this
    }

    pub fn set_changes(&mut self, repo_root: &Path, changes: &[kuaky_git::Change], cx: &mut Context<Self>) {
        self.changes = changes.iter().map(|c| (repo_root.join(&c.path), c.kind)).collect();
        cx.notify();
    }

    pub fn select(&mut self, path: &Path, cx: &mut Context<Self>) {
        self.selected = Some(path.to_path_buf());
        cx.notify();
    }

    /// Relee las carpetas abiertas cada pocos segundos: los agentes crean y
    /// borran archivos mientras trabajan.
    fn start_refresh(&self, cx: &mut Context<Self>) {
        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(std::time::Duration::from_secs(3)).await;
                let Ok(dirs) = this.read_with(cx, |this, _| this.expanded.iter().cloned().collect::<Vec<_>>()) else {
                    break;
                };
                let listed = cx
                    .background_executor()
                    .spawn(async move { dirs.into_iter().map(|d| { let l = list_dir(&d); (d, l) }).collect::<Vec<_>>() })
                    .await;
                let alive = this
                    .update(cx, |this, cx| {
                        let mut changed = false;
                        for (dir, nodes) in listed {
                            let same = this.children.get(&dir).is_some_and(|old| {
                                old.len() == nodes.len() && old.iter().zip(&nodes).all(|(a, b)| a.path == b.path)
                            });
                            if !same {
                                this.children.insert(dir, nodes);
                                changed = true;
                            }
                        }
                        if changed {
                            cx.notify();
                        }
                    })
                    .is_ok();
                if !alive {
                    break;
                }
            }
        })
        .detach();
    }

    fn toggle(&mut self, node: &Node, cx: &mut Context<Self>) {
        self.selected = Some(node.path.clone());
        if node.is_dir {
            if !self.expanded.remove(&node.path) {
                self.expanded.insert(node.path.clone());
                self.children.insert(node.path.clone(), list_dir(&node.path));
            }
        } else {
            cx.emit(FileTreeEvent::OpenFile(node.path.clone()));
        }
        cx.notify();
    }

    /// Filas visibles. `last[k]` dice si el nodo (o su ancestro en el nivel `k`)
    /// es el último de su carpeta: así se dibujan las ramas del árbol.
    fn visible(&self) -> Vec<(Vec<bool>, Node)> {
        let mut out = Vec::new();
        self.push_children(&self.root, &mut Vec::new(), &mut out);
        out
    }

    fn push_children(&self, dir: &Path, path: &mut Vec<bool>, out: &mut Vec<(Vec<bool>, Node)>) {
        let Some(nodes) = self.children.get(dir) else { return };
        for (i, node) in nodes.iter().enumerate() {
            path.push(i + 1 == nodes.len());
            out.push((path.clone(), node.clone()));
            if node.is_dir && self.expanded.contains(&node.path) {
                self.push_children(&node.path, path, out);
            }
            path.pop();
        }
    }

    fn dir_has_changes(&self, dir: &Path) -> bool {
        self.changes.keys().any(|p| p.starts_with(dir))
    }
}

impl Render for FileTree {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let rows = self.visible();
        div().id("file-tree").size_full().overflow_y_scroll().child(
            v_flex()
                .py_1()
                .children(rows.into_iter().enumerate().map(|(i, (last, node))| {
                    let selected = self.selected.as_deref() == Some(node.path.as_path());
                    let expanded = self.expanded.contains(&node.path);
                    let change = self.changes.get(&node.path).copied();
                    let dir_changed = node.is_dir && self.dir_has_changes(&node.path);
                    let name_color = match change {
                        Some(ChangeKind::Untracked | ChangeKind::Added) => style::good().into(),
                        Some(ChangeKind::Deleted | ChangeKind::Conflicted) => style::bad().into(),
                        Some(_) => style::soft(style::accent(), 1.0),
                        None if dir_changed => style::soft(style::accent(), 0.8),
                        None => style::text().into(),
                    };
                    let (icon, icon_color): (&'static str, Hsla) = if node.is_dir {
                        (if expanded { "folder-open" } else { "folder" }, style::soft(style::accent(), 0.75))
                    } else {
                        (file_icon(&node.name), style::muted().into())
                    };
                    let n = node.clone();
                    h_flex()
                        .id(("node", i))
                        .h(px(24.))
                        .pl(px(6.))
                        .pr_2()
                        .gap_1()
                        .text_sm()
                        .cursor_pointer()
                        .when(selected, |d| d.bg(style::panel_hover()))
                        .hover(|d| d.bg(style::panel_hover()))
                        .children(tree_guides(&last))
                        .child(
                            div().w(px(12.)).flex_none().when(node.is_dir, |d| {
                                d.child(crate::icons::ui(if expanded { "chevron-down" } else { "chevron-right" }, px(12.), style::faint()))
                            }),
                        )
                        .child(crate::icons::ui(icon, px(14.), icon_color))
                        .child(
                            div()
                                .flex_1()
                                .truncate()
                                .text_color(name_color)
                                .when(node.is_dir, |d| d.font_weight(FontWeight::MEDIUM))
                                .child(node.name.clone()),
                        )
                        .when_some(change, |d, c| {
                            d.child(div().text_xs().text_color(style::muted()).child(c.letter()))
                        })
                        .on_click(cx.listener(move |this, _, _, cx| this.toggle(&n, cx)))
                })),
        )
    }
}

/// Líneas del árbol: una columna por nivel; la última trae el codo (├ o └).
fn tree_guides(last: &[bool]) -> Vec<AnyElement> {
    const W: f32 = 14.;
    const X: f32 = 6.;
    let line = || style::border();
    let n = last.len();
    (0..n)
        .map(|k| {
            let col = div().w(px(W)).h_full().flex_none().relative();
            if k + 1 < n {
                // Ancestro: sigue la línea si todavía le quedan hermanos abajo.
                col.when(!last[k], |d| d.child(div().absolute().left(px(X)).top_0().bottom_0().w(px(1.)).bg(line())))
            } else {
                // Este nodo: ├ (con más hermanos) o └ (último).
                col.child(div().absolute().left(px(X)).top_0().when(last[k], |d| d.h(px(12.))).when(!last[k], |d| d.bottom_0()).w(px(1.)).bg(line()))
                    .child(div().absolute().left(px(X)).top(px(12.)).w(px(W - X)).h(px(1.)).bg(line()))
            }
            .into_any_element()
        })
        .collect()
}

/// Ícono según el tipo de archivo.
fn file_icon(name: &str) -> &'static str {
    let ext = name.rsplit_once('.').map(|(_, e)| e.to_lowercase()).unwrap_or_default();
    match ext.as_str() {
        "rs" | "ts" | "tsx" | "js" | "jsx" | "mjs" | "cjs" | "py" | "go" | "java" | "kt" | "swift" | "c" | "h" | "cpp" | "cs" | "rb" | "php"
        | "sh" | "zsh" | "html" | "css" | "scss" | "vue" | "svelte" | "sql" => "file-code",
        "md" | "mdx" | "txt" | "rst" => "file-text",
        "json" | "toml" | "yaml" | "yml" | "lock" | "ini" | "env" | "xml" => "file-cog",
        "png" | "jpg" | "jpeg" | "gif" | "webp" | "svg" | "ico" => "image",
        _ => "file",
    }
}

fn list_dir(dir: &Path) -> Vec<Node> {
    let mut nodes: Vec<Node> = ignore::WalkBuilder::new(dir)
        .max_depth(Some(1))
        .hidden(false)
        .git_ignore(true)
        .git_global(true)
        .git_exclude(true)
        .require_git(false)
        .filter_entry(|e| e.file_name() != ".git" && e.file_name() != ".DS_Store")
        .build()
        .filter_map(Result::ok)
        .filter(|e| e.depth() == 1)
        .map(|e| Node {
            is_dir: e.file_type().is_some_and(|t| t.is_dir()),
            name: e.file_name().to_string_lossy().into_owned(),
            path: e.into_path(),
        })
        .collect();
    nodes.sort_by(|a, b| b.is_dir.cmp(&a.is_dir).then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase())));
    nodes
}
