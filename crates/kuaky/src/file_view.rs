//! Archivo abierto en el editor de GPUI Kit (resaltado con syntect).

use crate::style;
use gpui_kit::component::ActiveTheme as _;
use gpui_kit::component::input::{Editor, EditorState, GoToDefinition, InputEvent, ShowCompletions};
use gpui_kit::component::{h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use std::path::PathBuf;

const MAX_BYTES: u64 = 4 * 1024 * 1024;

actions!(file_view, [Save]);

pub fn init(cx: &mut App) {
    #[cfg(target_os = "macos")]
    cx.bind_keys([KeyBinding::new("cmd-s", Save, Some("FileView"))]);
    #[cfg(not(target_os = "macos"))]
    cx.bind_keys([KeyBinding::new("ctrl-s", Save, Some("FileView"))]);
    cx.bind_keys([
        KeyBinding::new("ctrl-space", ShowCompletions, Some("Input && mode == editor")),
        KeyBinding::new("f12", GoToDefinition, Some("Input && mode == editor")),
    ]);
}

pub struct FileView {
    pub path: PathBuf,
    editor: Option<Entity<EditorState>>,
    message: Option<String>,
    pub dirty: bool,
    /// Servidor de lenguaje conectado (nombre) y su proveedor.
    lsp: Option<(&'static str, std::rc::Rc<crate::lsp::Provider>)>,
    sync_task: Option<Task<()>>,
    _subs: Vec<Subscription>,
}

impl FileView {
    pub fn new(path: PathBuf, lsp_store: Option<Entity<crate::lsp::LspStore>>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let mut this = Self {
            path: path.clone(),
            editor: None,
            message: None,
            dirty: false,
            lsp: None,
            sync_task: None,
            _subs: Vec::new(),
        };
        match read_text(&path) {
            Ok(text) => {
                let language = path
                    .extension()
                    .map(|e| e.to_string_lossy().into_owned())
                    .or_else(|| path.file_name().map(|n| n.to_string_lossy().into_owned()))
                    .unwrap_or_else(|| "text".into());
                let editor = cx.new(|cx| {
                    EditorState::new(window, cx)
                        .language(language)
                        .line_number(true)
                        .soft_wrap(false)
                        .default_value(text)
                });
                this._subs.push(cx.subscribe(&editor, |this: &mut Self, _, ev: &InputEvent, cx| {
                    if matches!(ev, InputEvent::Change) {
                        if !this.dirty {
                            this.dirty = true;
                            cx.notify();
                        }
                        this.schedule_sync(cx);
                    }
                }));
                if let Some(store) = &lsp_store {
                    this.lsp = store.update(cx, |store, cx| store.attach(&path, &editor, cx));
                    this._subs.push(cx.observe(store, |_, _, cx| cx.notify()));
                }
                this.editor = Some(editor);
            }
            Err(msg) => this.message = Some(msg),
        }
        this
    }

    /// Envía los cambios al servidor de lenguaje tras una pausa al escribir,
    /// para que los diagnósticos se actualicen.
    fn schedule_sync(&mut self, cx: &mut Context<Self>) {
        if self.lsp.is_none() {
            return;
        }
        self.sync_task = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(std::time::Duration::from_millis(400)).await;
            this.update(cx, |this, cx| {
                if let (Some((_, provider)), Some(editor)) = (&this.lsp, &this.editor) {
                    provider.sync(&editor.read(cx).value());
                }
            })
            .ok();
        }));
    }

    /// URI del documento en el servidor de lenguaje, si está conectado.
    pub fn lsp_uri(&self) -> Option<String> {
        self.lsp.as_ref().map(|(_, p)| p.uri().to_string())
    }

    pub fn title(&self) -> String {
        self.path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()
    }

    /// Mueve el cursor a `line`/`column` (base 0).
    pub fn goto(&mut self, line: u32, column: u32, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(editor) = &self.editor {
            editor.update(cx, |s, cx| {
                s.set_cursor_position(gpui_kit::component::input::Position { line, character: column }, window, cx)
            });
        }
    }

    /// Dispara una función LSP para capturas de desarrollo (`KUAKY_DEMO_LSP=hover|completion|definition`).
    #[cfg(feature = "screenshot")]
    pub fn demo_lsp(&mut self, mode: String, window: &mut Window, cx: &mut Context<Self>) {
        use gpui_kit::component::input::{HoverProvider as _, RopeExt as _};
        cx.spawn_in(window, async move |this, cx| {
            cx.background_executor().timer(std::time::Duration::from_secs(4)).await;
            let info = this
                .read_with(cx, |this, cx| {
                    let editor = this.editor.clone()?;
                    let provider = this.lsp.as_ref().map(|(_, p)| p.clone())?;
                    let text = editor.read(cx).text().clone();
                    Some((editor, provider, text))
                })
                .ok()
                .flatten();
            let Some((editor, provider, text)) = info else { return };
            let raw = text.to_string();
            match mode.as_str() {
                "hover" => {
                    let offset = raw.find("mal:").unwrap_or(0);
                    let task = cx.update(|window, cx| provider.hover(&text, offset, window, cx)).ok();
                    if let Some(task) = task {
                        if let Ok(Some(hover)) = task.await {
                            let range = hover
                                .range
                                .map(|r| text.position_to_offset(&r.start)..text.position_to_offset(&r.end))
                                .unwrap_or(offset..offset + 3);
                            let _ = editor.update(cx, |s, cx| s.present_hover(range, hover, cx));
                        }
                    }
                }
                "completion" => {
                    if let Some(dot) = raw.find("persona.") {
                        let offset = dot + "persona.".len();
                        let pos = text.offset_to_position(offset);
                        let _ = editor.update_in(cx, |s, window, cx| {
                            s.set_cursor_position(pos, window, cx);
                            s.show_completions(window, cx);
                        });
                    }
                }
                "definition" => {
                    if let Some(idx) = raw.find("sumar(") {
                        let pos = text.offset_to_position(idx + 1);
                        let _ = editor.update_in(cx, |s, window, cx| {
                            s.set_cursor_position(pos, window, cx);
                            window.dispatch_action(Box::new(GoToDefinition), cx);
                        });
                    }
                }
                _ => {}
            }
        })
        .detach();
    }

    fn save(&mut self, _: &Save, _: &mut Window, cx: &mut Context<Self>) {
        let Some(editor) = &self.editor else { return };
        let text = editor.read(cx).value();
        match std::fs::write(&self.path, text.as_bytes()) {
            Ok(()) => {
                self.dirty = false;
                self.message = None;
                if let Some((_, provider)) = &self.lsp {
                    provider.saved(&text);
                }
            }
            Err(err) => self.message = Some(format!("Failed to save: {err}")),
        }
        cx.notify();
    }
}

fn read_text(path: &PathBuf) -> Result<String, String> {
    let meta = std::fs::metadata(path).map_err(|e| e.to_string())?;
    if meta.len() > MAX_BYTES {
        return Err(format!("File is {} MB; kuaky opens files up to 4 MB.", meta.len() / 1_048_576));
    }
    let bytes = std::fs::read(path).map_err(|e| e.to_string())?;
    if bytes.iter().take(8000).any(|&b| b == 0) {
        return Err("Binary file.".into());
    }
    String::from_utf8(bytes).map_err(|_| "File is not valid UTF-8.".into())
}

impl Render for FileView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        v_flex()
            .key_context("FileView")
            .on_action(cx.listener(Self::save))
            .size_full()
            .child(
                h_flex()
                    .h(px(30.))
                    .px_3()
                    .gap_2()
                    .text_xs()
                    .text_color(style::muted())
                    .border_b_1()
                    .border_color(style::border())
                    .child(crate::home::tilde(&self.path))
                    .when_some(self.lsp.as_ref().map(|(name, p)| (*name, p.counts())), |d, (name, (errors, warnings))| {
                        d.child(div().text_color(style::good()).child(format!("● {name}")))
                            .when(errors > 0, |d| {
                                let label = if errors == 1 { "1 error".into() } else { format!("{errors} errors") };
                                d.child(div().text_color(style::bad()).child(label))
                            })
                            .when(warnings > 0, |d| {
                                let label = if warnings == 1 { "1 warning".into() } else { format!("{warnings} warnings") };
                                d.child(div().text_color(rgb(0xfbbf24)).child(label))
                            })
                    })
                    .when(self.dirty, |d| d.child(div().text_color(style::accent()).child("● unsaved · ⌘S")))
                    .when_some(self.message.clone().filter(|_| self.editor.is_some()), |d, m| {
                        d.child(div().text_color(style::bad()).child(m))
                    }),
            )
            .child(match &self.editor {
                Some(editor) => div()
                    .flex_1()
                    .min_h_0()
                    .child(
                        Editor::new(editor)
                            .bordered(false)
                            .h_full()
                            .font_family(cx.theme().mono_font_family.clone())
                            .text_size(px(13.)),
                    )
                    .into_any_element(),
                None => div()
                    .flex_1()
                    .flex()
                    .items_center()
                    .justify_center()
                    .text_color(style::muted())
                    .child(self.message.clone().unwrap_or_default())
                    .into_any_element(),
            })
    }
}
