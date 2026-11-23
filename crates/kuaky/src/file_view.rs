//! Archivo abierto en el editor de GPUI Kit (resaltado con syntect).

use crate::style;
use gpui_kit::component::ActiveTheme as _;
use gpui_kit::component::input::{Editor, EditorState, InputEvent};
use gpui_kit::component::{h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use std::path::PathBuf;

const MAX_BYTES: u64 = 4 * 1024 * 1024;

actions!(file_view, [Save]);

pub fn init(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new("cmd-s", Save, Some("FileView")),
        KeyBinding::new("ctrl-s", Save, Some("FileView")),
    ]);
}

pub struct FileView {
    pub path: PathBuf,
    editor: Option<Entity<EditorState>>,
    message: Option<String>,
    pub dirty: bool,
    _sub: Option<Subscription>,
}

impl FileView {
    pub fn new(path: PathBuf, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let mut this = Self { path: path.clone(), editor: None, message: None, dirty: false, _sub: None };
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
                this._sub = Some(cx.subscribe(&editor, |this: &mut Self, _, ev: &InputEvent, cx| {
                    if matches!(ev, InputEvent::Change) && !this.dirty {
                        this.dirty = true;
                        cx.notify();
                    }
                }));
                this.editor = Some(editor);
            }
            Err(msg) => this.message = Some(msg),
        }
        this
    }

    pub fn title(&self) -> String {
        self.path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()
    }

    fn save(&mut self, _: &Save, _: &mut Window, cx: &mut Context<Self>) {
        let Some(editor) = &self.editor else { return };
        let text = editor.read(cx).value();
        match std::fs::write(&self.path, text.as_bytes()) {
            Ok(()) => {
                self.dirty = false;
                self.message = None;
            }
            Err(err) => self.message = Some(format!("No se pudo guardar: {err}")),
        }
        cx.notify();
    }
}

fn read_text(path: &PathBuf) -> Result<String, String> {
    let meta = std::fs::metadata(path).map_err(|e| e.to_string())?;
    if meta.len() > MAX_BYTES {
        return Err(format!("El archivo pesa {} MB; kuaky abre archivos de hasta 4 MB.", meta.len() / 1_048_576));
    }
    let bytes = std::fs::read(path).map_err(|e| e.to_string())?;
    if bytes.iter().take(8000).any(|&b| b == 0) {
        return Err("Es un archivo binario.".into());
    }
    String::from_utf8(bytes).map_err(|_| "El archivo no está en UTF-8.".into())
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
                    .when(self.dirty, |d| d.child(div().text_color(style::accent()).child("● sin guardar · ⌘S")))
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
