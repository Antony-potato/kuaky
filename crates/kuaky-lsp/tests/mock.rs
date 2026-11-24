//! Conversación contra un servidor LSP falso.
#![cfg(unix)]

use kuaky_lsp::{Client, Event, path_to_uri};
use serde_json::json;
use std::{path::PathBuf, time::Duration};

#[test]
fn initialize_open_diagnostics_hover_definition() {
    let script = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/mock_server.py");
    let root = std::env::temp_dir();
    let client = Client::spawn("mock", &PathBuf::from("python3"), &[script.to_string_lossy().into_owned()], &root, &[]).unwrap();
    assert_eq!(client.completion_triggers(), vec![".".to_string()]);

    let uri = path_to_uri(&root.join("a.rs"));
    client.did_open(&uri, "rust", 1, "fn main() {}\n").unwrap();
    match client.events().recv_timeout(Duration::from_secs(5)).unwrap() {
        Event::Diagnostics { uri: u, diagnostics } => {
            assert_eq!(u, uri);
            assert_eq!(diagnostics[0]["message"], "error de prueba");
        }
        other => panic!("evento inesperado: {other:?}"),
    }

    let pos = json!({ "textDocument": { "uri": uri }, "position": { "line": 0, "character": 3 } });
    let hover = client.request("textDocument/hover", pos.clone(), Duration::from_secs(5)).unwrap();
    assert_eq!(hover["contents"]["value"], "**fn main**");
    let def = client.request("textDocument/definition", pos, Duration::from_secs(5)).unwrap();
    assert_eq!(def[0]["range"]["start"]["line"], 2);
}
