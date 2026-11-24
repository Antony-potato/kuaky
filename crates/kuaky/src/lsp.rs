//! Servidores de lenguaje para el editor: diagnósticos, autocompletado, hover
//! e ir a definición. Un servidor por lenguaje y proyecto, compartido entre
//! los archivos abiertos. Se usan los que tengas instalados o los que Zed ya
//! descargó en `Application Support/Zed/languages`.

use crate::style;
use gpui_kit::component::input::{
    CompletionProvider, DefinitionProvider, EditorState, HoverProvider, Rope, RopeExt as _,
    TextDecoration, TextDecorationCollection,
};
use gpui_kit::*;
use kuaky_lsp::{Client, Event, path_to_uri, uri_to_path};
use serde_json::{Value, json};
use std::{
    cell::{Cell, RefCell},
    collections::HashMap,
    path::{Path, PathBuf},
    rc::Rc,
    sync::Arc,
    time::Duration,
};

const TIMEOUT: Duration = Duration::from_secs(8);

/// Un servidor de lenguaje conocido.
struct Spec {
    name: &'static str,
    /// Programas a buscar en el PATH, con sus argumentos.
    commands: &'static [(&'static str, &'static [&'static str])],
    /// Rutas relativas dentro de `Zed/languages` (servidores que Zed descargó).
    zed: &'static [(&'static str, &'static [&'static str])],
}

const RUST: Spec = Spec { name: "rust-analyzer", commands: &[("rust-analyzer", &[])], zed: &[] };
const TYPESCRIPT: Spec = Spec {
    name: "vtsls",
    commands: &[("vtsls", &["--stdio"]), ("typescript-language-server", &["--stdio"])],
    zed: &[("vtsls/node_modules/.bin/vtsls", &["--stdio"])],
};
const PYTHON: Spec = Spec {
    name: "basedpyright",
    commands: &[("basedpyright-langserver", &["--stdio"]), ("pyright-langserver", &["--stdio"])],
    zed: &[("basedpyright/node_modules/.bin/basedpyright-langserver", &["--stdio"])],
};
const JSON: Spec = Spec {
    name: "json",
    commands: &[("vscode-json-language-server", &["--stdio"])],
    zed: &[("json-language-server/node_modules/.bin/vscode-json-language-server", &["--stdio"])],
};
const BASH: Spec = Spec {
    name: "bash",
    commands: &[("bash-language-server", &["start"])],
    zed: &[("bash-language-server/node_modules/.bin/bash-language-server", &["start"])],
};

/// Servidor y `languageId` para un archivo.
fn spec_for(path: &Path) -> Option<(&'static Spec, &'static str)> {
    let ext = path.extension()?.to_str()?.to_ascii_lowercase();
    Some(match ext.as_str() {
        "rs" => (&RUST, "rust"),
        "ts" | "mts" | "cts" => (&TYPESCRIPT, "typescript"),
        "tsx" => (&TYPESCRIPT, "typescriptreact"),
        "js" | "mjs" | "cjs" => (&TYPESCRIPT, "javascript"),
        "jsx" => (&TYPESCRIPT, "javascriptreact"),
        "py" | "pyi" => (&PYTHON, "python"),
        "json" | "jsonc" => (&JSON, "json"),
        "sh" | "bash" => (&BASH, "shellscript"),
        _ => return None,
    })
}

fn find_command(spec: &Spec) -> Option<(PathBuf, Vec<String>)> {
    let to_args = |a: &[&str]| a.iter().map(|s| s.to_string()).collect::<Vec<_>>();
    for (program, args) in spec.commands {
        if let Some(path) = kuaky_git::find_program(program) {
            return Some((path, to_args(args)));
        }
    }
    if spec.name == "rust-analyzer" {
        let cargo_bin = dirs::home_dir()?.join(".cargo/bin/rust-analyzer");
        if cargo_bin.is_file() {
            return Some((cargo_bin, Vec::new()));
        }
    }
    let zed = zed_languages_dir()?;
    spec.zed
        .iter()
        .map(|(rel, args)| (zed.join(rel), to_args(args)))
        .find(|(p, _)| p.is_file())
}

fn zed_languages_dir() -> Option<PathBuf> {
    let home = dirs::home_dir()?;
    [
        home.join("Library/Application Support/Zed/languages"),
        home.join(".local/share/zed/languages"),
        dirs::data_local_dir().map(|d| d.join("Zed/languages")).unwrap_or_default(),
    ]
    .into_iter()
    .find(|p| p.is_dir())
}

pub enum LspStoreEvent {
    /// Ir a una ubicación (ir a definición en otro archivo).
    OpenLocation { path: PathBuf, line: u32, column: u32 },
}

enum Slot {
    Starting(Vec<(String, &'static str, WeakEntity<EditorState>, Rc<Provider>)>),
    Ready(Arc<Client>),
    Failed,
}

struct Doc {
    editor: WeakEntity<EditorState>,
    decorations: TextDecorationCollection,
    provider: Rc<Provider>,
}

pub struct LspStore {
    root: PathBuf,
    servers: HashMap<&'static str, Slot>,
    docs: HashMap<String, Doc>,
}

impl EventEmitter<LspStoreEvent> for LspStore {}

impl LspStore {
    pub fn new(root: PathBuf) -> Self {
        Self { root, servers: HashMap::new(), docs: HashMap::new() }
    }

    /// Conecta el editor de `path` a su servidor. Devuelve el nombre del
    /// servidor y el proveedor (para sincronizar cambios y guardados), o
    /// `None` si no hay servidor para ese lenguaje en este equipo.
    pub fn attach(&mut self, path: &Path, editor: &Entity<EditorState>, cx: &mut Context<Self>) -> Option<(&'static str, Rc<Provider>)> {
        let (spec, language) = spec_for(path)?;
        let uri = path_to_uri(path);
        let provider = Rc::new(Provider::new(uri.clone(), cx.weak_entity()));
        let decorations = editor.update(cx, |state, cx| state.create_decorations_collection(Vec::new(), cx));
        self.docs.insert(uri.clone(), Doc { editor: editor.downgrade(), decorations, provider: provider.clone() });
        match self.servers.get_mut(spec.name) {
            Some(Slot::Ready(client)) => {
                let client = client.clone();
                configure(&client, &uri, language, editor, &provider, cx);
            }
            Some(Slot::Starting(pending)) => pending.push((uri, language, editor.downgrade(), provider.clone())),
            Some(Slot::Failed) => {
                self.docs.remove(&uri);
                return None;
            }
            None => {
                let Some((program, args)) = find_command(spec) else {
                    self.docs.remove(&uri);
                    return None;
                };
                self.servers.insert(spec.name, Slot::Starting(vec![(uri, language, editor.downgrade(), provider.clone())]));
                self.start(spec.name, program, args, cx);
            }
        }
        Some((spec.name, provider))
    }

    fn start(&mut self, name: &'static str, program: PathBuf, args: Vec<String>, cx: &mut Context<Self>) {
        let root = self.root.clone();
        cx.spawn(async move |this, cx| {
            let env = vec![("PATH".to_string(), kuaky_git::login_path().to_string_lossy().into_owned())];
            let result = cx
                .background_executor()
                .spawn(async move { Client::spawn(name, &program, &args, &root, &env) })
                .await;
            let client = match result {
                Ok(c) => Arc::new(c),
                Err(err) => {
                    tracing::warn!(server = name, %err, "no se pudo iniciar el servidor de lenguaje");
                    this.update(cx, |this, _| {
                        this.servers.insert(name, Slot::Failed);
                    })
                    .ok();
                    return;
                }
            };
            tracing::info!(server = name, "servidor de lenguaje listo");
            let events = client.events();
            let ok = this
                .update(cx, |this, cx| {
                    if let Some(Slot::Starting(pending)) = this.servers.insert(name, Slot::Ready(client.clone())) {
                        for (uri, language, editor, provider) in pending {
                            if let Some(editor) = editor.upgrade() {
                                configure(&client, &uri, language, &editor, &provider, cx);
                            }
                        }
                    }
                })
                .is_ok();
            if !ok {
                return;
            }
            while let Ok(event) = events.recv_async().await {
                let alive = this.update(cx, |this, cx| this.on_event(event, cx)).is_ok();
                if !alive {
                    break;
                }
            }
        })
        .detach();
    }

    fn on_event(&mut self, event: Event, cx: &mut Context<Self>) {
        let Event::Diagnostics { uri, diagnostics } = event else { return };
        let Some(doc) = self.docs.get(&uri) else {
            tracing::debug!(%uri, known = ?self.docs.keys().collect::<Vec<_>>(), "diagnósticos de un documento no abierto");
            return;
        };
        let Some(editor) = doc.editor.upgrade() else {
            self.docs.remove(&uri);
            return;
        };
        let list: Vec<lsp_types::Diagnostic> = match serde_json::from_value(diagnostics) {
            Ok(list) => list,
            Err(err) => {
                tracing::warn!(%err, "diagnósticos con formato inesperado");
                return;
            }
        };
        tracing::debug!(%uri, count = list.len(), "diagnósticos");
        let text = editor.read(cx).text().clone();
        // GPUI Kit con `syntect` (sin `tree-sitter`) usa el stub de `StatusColors`,
        // donde `error`/`warning`/`info`/`hint` devuelven `Hsla::default()` (transparente),
        // lo que deja invisibles tanto los subrayados nativos como `DiagnosticPopover`.
        // Pintamos los subrayados con `TextDecorationCollection` y mostramos el mensaje
        // en el `HoverProvider` (que usa el popover normal del tema).
        let decorations: Vec<TextDecoration> = list
            .iter()
            .filter_map(|d| diagnostic_decoration(&text, d))
            .collect();
        doc.decorations.set(decorations, cx);
        doc.provider.set_diagnostics(list);
        cx.notify();
    }

    fn client_for(&self, uri: &str) -> Option<Arc<Client>> {
        let path = uri_to_path(uri)?;
        let (spec, _) = spec_for(&path)?;
        match self.servers.get(spec.name)? {
            Slot::Ready(c) => Some(c.clone()),
            _ => None,
        }
    }

    pub fn detach(&mut self, uri: &str) {
        self.docs.remove(uri);
        if let Some(client) = self.client_for(uri) {
            let _ = client.did_close(uri);
        }
    }
}

fn diagnostic_color(severity: Option<lsp_types::DiagnosticSeverity>) -> Hsla {
    match severity {
        Some(lsp_types::DiagnosticSeverity::WARNING) => rgb(0xfbbf24).into(),
        Some(lsp_types::DiagnosticSeverity::INFORMATION) => rgb(0x60a5fa).into(),
        Some(lsp_types::DiagnosticSeverity::HINT) => style::muted().into(),
        _ => style::bad().into(),
    }
}

fn diagnostic_decoration(text: &Rope, d: &lsp_types::Diagnostic) -> Option<TextDecoration> {
    let len = text.len();
    if len == 0 {
        return None;
    }
    let mut start = text.position_to_offset(&d.range.start).min(len);
    let mut end = text.position_to_offset(&d.range.end).min(len);
    if end <= start {
        if start < len {
            let ch = text.chars_at(start).next();
            end = ch.filter(|&c| c != '\n').map_or(start, |c| start + c.len_utf8());
        }
        if end <= start && start > 0 {
            let prev = text.chars_at(start).reversed().next();
            if let Some(c) = prev.filter(|&c| c != '\n') {
                start -= c.len_utf8();
                end = start + c.len_utf8();
            }
        }
    }
    if end <= start {
        return None;
    }
    Some(TextDecoration::new(
        start..end,
        HighlightStyle {
            underline: Some(UnderlineStyle {
                color: Some(diagnostic_color(d.severity)),
                thickness: px(1.5),
                wavy: true,
            }),
            ..Default::default()
        },
    ))
}

/// Abre el documento en el servidor y conecta los proveedores del editor.
fn configure(client: &Arc<Client>, uri: &str, language: &str, editor: &Entity<EditorState>, provider: &Rc<Provider>, cx: &mut Context<LspStore>) {
    let text = editor.read(cx).value().to_string();
    if client.did_open(uri, language, 1, &text).is_err() {
        return;
    }
    provider.connect(client.clone(), text);
    let store = cx.weak_entity();
    editor.update(cx, |state, cx| {
        let lsp = state.lsp_mut();
        lsp.completion_provider = Some(provider.clone());
        lsp.hover_provider = Some(provider.clone());
        lsp.definition_provider = Some(provider.clone());
        lsp.show_document = Some(Rc::new(move |params: &lsp_types::ShowDocumentParams, _: &mut Window, cx: &mut App| {
            let Some(path) = uri_to_path(params.uri.as_str()) else { return false };
            let path = path.canonicalize().unwrap_or(path);
            let start = params.selection.map(|s| s.start).unwrap_or_default();
            store
                .update(cx, |_, cx| cx.emit(LspStoreEvent::OpenLocation { path, line: start.line, column: start.character }))
                .is_ok()
        }));
        state.refresh(cx);
    });
}

/// Proveedor LSP de un documento: sincroniza el texto antes de cada consulta.
pub struct Provider {
    uri: String,
    client: RefCell<Option<Arc<Client>>>,
    version: Cell<i32>,
    last_text: RefCell<String>,
    triggers: RefCell<Vec<String>>,
    diagnostics: RefCell<Vec<lsp_types::Diagnostic>>,
    _store: WeakEntity<LspStore>,
}

impl Provider {
    fn new(uri: String, store: WeakEntity<LspStore>) -> Self {
        Self {
            uri,
            client: RefCell::new(None),
            version: Cell::new(1),
            last_text: RefCell::new(String::new()),
            triggers: RefCell::new(Vec::new()),
            diagnostics: RefCell::new(Vec::new()),
            _store: store,
        }
    }

    fn connect(&self, client: Arc<Client>, text: String) {
        *self.triggers.borrow_mut() = client.completion_triggers();
        *self.client.borrow_mut() = Some(client);
        *self.last_text.borrow_mut() = text;
    }

    fn set_diagnostics(&self, list: Vec<lsp_types::Diagnostic>) {
        *self.diagnostics.borrow_mut() = list;
    }

    /// `(errores, avisos)` actuales en el documento.
    pub fn counts(&self) -> (usize, usize) {
        let diags = self.diagnostics.borrow();
        let errors = diags.iter().filter(|d| matches!(d.severity, None | Some(lsp_types::DiagnosticSeverity::ERROR))).count();
        let warnings = diags.iter().filter(|d| d.severity == Some(lsp_types::DiagnosticSeverity::WARNING)).count();
        (errors, warnings)
    }

    pub fn uri(&self) -> &str {
        &self.uri
    }

    /// Envía el texto actual si cambió desde el último envío.
    pub fn sync(&self, text: &str) -> Option<Arc<Client>> {
        let client = self.client.borrow().clone()?;
        if *self.last_text.borrow() != text {
            self.version.set(self.version.get() + 1);
            let _ = client.did_change(&self.uri, self.version.get(), text);
            *self.last_text.borrow_mut() = text.to_string();
        }
        Some(client)
    }

    pub fn saved(&self, text: &str) {
        if let Some(client) = self.sync(text) {
            let _ = client.did_save(&self.uri, text);
        }
    }

    fn position_params(&self, rope: &Rope, offset: usize) -> Option<(Arc<Client>, Value)> {
        let client = self.sync(&rope.to_string())?;
        let pos = rope.offset_to_position(offset);
        Some((client, json!({ "textDocument": { "uri": self.uri }, "position": { "line": pos.line, "character": pos.character } })))
    }
}

fn word_prefix_range(rope: &Rope, offset: usize) -> std::ops::Range<usize> {
    let word_len: usize = rope
        .chars_at(offset)
        .reversed()
        .take_while(|ch| ch.is_alphanumeric() || *ch == '_')
        .map(char::len_utf8)
        .sum();
    offset.saturating_sub(word_len)..offset
}

fn filter_and_prepare_completions(
    mut items: Vec<lsp_types::CompletionItem>,
    prefix: &str,
    replace_range: lsp_types::Range,
) -> Vec<lsp_types::CompletionItem> {
    if !prefix.is_empty() {
        let lower = prefix.to_ascii_lowercase();
        let mut ranked: Vec<(u8, lsp_types::CompletionItem)> = items
            .into_iter()
            .filter_map(|item| {
                let hay = item.filter_text.as_deref().unwrap_or(&item.label).to_ascii_lowercase();
                if hay.starts_with(&lower) {
                    Some((0, item))
                } else if hay.contains(&lower) {
                    Some((1, item))
                } else {
                    None
                }
            })
            .collect();
        ranked.sort_by_key(|(rank, _)| *rank);
        items = ranked.into_iter().map(|(_, item)| item).collect();
    }
    items.truncate(80);
    for item in &mut items {
        if item.text_edit.is_none() {
            let new_text = item.insert_text.take().unwrap_or_else(|| item.label.clone());
            item.text_edit = Some(lsp_types::CompletionTextEdit::Edit(lsp_types::TextEdit {
                range: replace_range,
                new_text,
            }));
        }
    }
    items
}

fn pos_in_range(pos: lsp_types::Position, r: lsp_types::Range) -> bool {
    let p = (pos.line, pos.character);
    let s = (r.start.line, r.start.character);
    let e = (r.end.line, r.end.character);
    p >= s && (p < e || (s == e && p == e))
}

fn format_diagnostics_markdown(diags: &[lsp_types::Diagnostic]) -> String {
    diags
        .iter()
        .map(|d| {
            let label = match d.severity {
                Some(lsp_types::DiagnosticSeverity::WARNING) => "Aviso",
                Some(lsp_types::DiagnosticSeverity::INFORMATION) => "Info",
                Some(lsp_types::DiagnosticSeverity::HINT) => "Sugerencia",
                _ => "Error",
            };
            let meta = match (&d.source, &d.code) {
                (Some(s), Some(lsp_types::NumberOrString::Number(n))) => format!(" `({s} {n})`"),
                (Some(s), Some(lsp_types::NumberOrString::String(c))) => format!(" `({s} {c})`"),
                (Some(s), None) => format!(" `({s})`"),
                _ => String::new(),
            };
            format!("**{label}**{meta}: {}", d.message)
        })
        .collect::<Vec<_>>()
        .join("\n\n")
}

fn hover_to_markdown(contents: lsp_types::HoverContents) -> String {
    match contents {
        lsp_types::HoverContents::Scalar(s) => marked_to_markdown(s),
        lsp_types::HoverContents::Array(arr) => arr.into_iter().map(marked_to_markdown).collect::<Vec<_>>().join("\n\n"),
        lsp_types::HoverContents::Markup(m) => m.value,
    }
}

fn marked_to_markdown(m: lsp_types::MarkedString) -> String {
    match m {
        lsp_types::MarkedString::String(s) => s,
        lsp_types::MarkedString::LanguageString(ls) => format!("```{}\n{}\n```", ls.language, ls.value),
    }
}

impl CompletionProvider for Provider {
    fn completions(
        &self,
        text: &Rope,
        offset: usize,
        mut trigger: lsp_types::CompletionContext,
        _: &mut Window,
        cx: &mut App,
    ) -> Task<anyhow::Result<lsp_types::CompletionResponse>> {
        let Some((client, mut params)) = self.position_params(text, offset) else {
            return Task::ready(Ok(lsp_types::CompletionResponse::Array(Vec::new())));
        };
        // gpui-base manda `TRIGGER_CHARACTER` con toda la palabra escrita; si no es
        // realmente un carácter disparador del servidor (como `.`), enviamos `INVOKED`.
        if trigger.trigger_kind == lsp_types::CompletionTriggerKind::TRIGGER_CHARACTER {
            let is_real_trigger = trigger
                .trigger_character
                .as_deref()
                .is_some_and(|ch| self.triggers.borrow().iter().any(|t| t == ch));
            if !is_real_trigger {
                trigger.trigger_kind = lsp_types::CompletionTriggerKind::INVOKED;
                trigger.trigger_character = None;
            }
        }
        let word_range = word_prefix_range(text, offset);
        let prefix = text.slice(word_range.clone()).to_string();
        let replace_range = lsp_types::Range {
            start: text.offset_to_position(word_range.start),
            end: text.offset_to_position(word_range.end),
        };
        params["context"] = serde_json::to_value(trigger).unwrap_or(Value::Null);
        cx.background_executor().spawn(async move {
            let v = client.request("textDocument/completion", params, TIMEOUT)?;
            if v.is_null() {
                return Ok(lsp_types::CompletionResponse::Array(Vec::new()));
            }
            let response: lsp_types::CompletionResponse = serde_json::from_value(v)?;
            let items = match response {
                lsp_types::CompletionResponse::Array(items) => items,
                lsp_types::CompletionResponse::List(list) => list.items,
            };
            Ok(lsp_types::CompletionResponse::Array(filter_and_prepare_completions(
                items,
                &prefix,
                replace_range,
            )))
        })
    }

    fn is_completion_trigger(&self, _offset: usize, new_text: &str, _: &mut App) -> bool {
        let Some(last) = new_text.chars().last() else { return false };
        last.is_alphanumeric() || last == '_' || self.triggers.borrow().iter().any(|t| t.ends_with(last))
    }
}

impl HoverProvider for Provider {
    fn hover(&self, text: &Rope, offset: usize, _: &mut Window, cx: &mut App) -> Task<anyhow::Result<Option<lsp_types::Hover>>> {
        let pos = text.offset_to_position(offset);
        let matching_diags: Vec<lsp_types::Diagnostic> = self
            .diagnostics
            .borrow()
            .iter()
            .filter(|d| pos_in_range(pos, d.range))
            .cloned()
            .collect();
        let diag_md = (!matching_diags.is_empty()).then(|| format_diagnostics_markdown(&matching_diags));
        let diag_range = matching_diags.first().map(|d| d.range);

        let Some((client, params)) = self.position_params(text, offset) else {
            return Task::ready(Ok(diag_md.map(|value| lsp_types::Hover {
                contents: lsp_types::HoverContents::Markup(lsp_types::MarkupContent {
                    kind: lsp_types::MarkupKind::Markdown,
                    value,
                }),
                range: diag_range,
            })));
        };
        cx.background_executor().spawn(async move {
            let server_hover: Option<lsp_types::Hover> = match client.request("textDocument/hover", params, TIMEOUT) {
                Ok(v) if !v.is_null() => serde_json::from_value(v).ok(),
                _ => None,
            };
            Ok(match (diag_md, server_hover) {
                (Some(diag), Some(hover)) => {
                    let body = hover_to_markdown(hover.contents);
                    let value = if body.trim().is_empty() { diag } else { format!("{diag}\n\n---\n\n{body}") };
                    Some(lsp_types::Hover {
                        contents: lsp_types::HoverContents::Markup(lsp_types::MarkupContent {
                            kind: lsp_types::MarkupKind::Markdown,
                            value,
                        }),
                        range: hover.range.or(diag_range),
                    })
                }
                (Some(diag), None) => Some(lsp_types::Hover {
                    contents: lsp_types::HoverContents::Markup(lsp_types::MarkupContent {
                        kind: lsp_types::MarkupKind::Markdown,
                        value: diag,
                    }),
                    range: diag_range,
                }),
                (None, hover) => hover,
            })
        })
    }
}

impl DefinitionProvider for Provider {
    fn definitions(
        &self,
        text: &Rope,
        offset: usize,
        _: &mut Window,
        cx: &mut App,
    ) -> Task<anyhow::Result<Vec<lsp_types::LocationLink>>> {
        let Some((client, params)) = self.position_params(text, offset) else {
            return Task::ready(Ok(Vec::new()));
        };
        cx.background_executor().spawn(async move {
            let v = client.request("textDocument/definition", params, TIMEOUT)?;
            if v.is_null() {
                return Ok(Vec::new());
            }
            let response: lsp_types::GotoDefinitionResponse = serde_json::from_value(v)?;
            let to_link = |l: lsp_types::Location| lsp_types::LocationLink {
                origin_selection_range: None,
                target_uri: l.uri,
                target_range: l.range,
                target_selection_range: l.range,
            };
            Ok(match response {
                lsp_types::GotoDefinitionResponse::Scalar(l) => vec![to_link(l)],
                lsp_types::GotoDefinitionResponse::Array(ls) => ls.into_iter().map(to_link).collect(),
                lsp_types::GotoDefinitionResponse::Link(links) => links,
            })
        })
    }
}

#[cfg(test)]
mod tests {
    use super::{filter_and_prepare_completions, pos_in_range};

    #[test]
    fn filters_and_sets_replace_range_on_completions() {
        let items = vec![
            lsp_types::CompletionItem { label: "edad".into(), ..Default::default() },
            lsp_types::CompletionItem { label: "nombre".into(), ..Default::default() },
            lsp_types::CompletionItem { label: "renombrar".into(), ..Default::default() },
        ];
        let range = lsp_types::Range::new(lsp_types::Position::new(6, 28), lsp_types::Position::new(6, 30));
        let out = filter_and_prepare_completions(items, "no", range);
        let labels: Vec<&str> = out.iter().map(|i| i.label.as_str()).collect();
        assert_eq!(labels, vec!["nombre", "renombrar"]);
        assert!(matches!(
            &out[0].text_edit,
            Some(lsp_types::CompletionTextEdit::Edit(e)) if e.range == range && e.new_text == "nombre"
        ));
    }

    #[test]
    fn matches_position_in_diagnostic_range() {
        let r = lsp_types::Range::new(lsp_types::Position::new(3, 6), lsp_types::Position::new(3, 9));
        assert!(pos_in_range(lsp_types::Position::new(3, 6), r));
        assert!(pos_in_range(lsp_types::Position::new(3, 8), r));
        assert!(!pos_in_range(lsp_types::Position::new(3, 9), r));
        let empty = lsp_types::Range::new(lsp_types::Position::new(1, 4), lsp_types::Position::new(1, 4));
        assert!(pos_in_range(lsp_types::Position::new(1, 4), empty));
    }
}
