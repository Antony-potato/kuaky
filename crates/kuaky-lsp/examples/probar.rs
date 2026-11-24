//! `cargo run -p kuaky-lsp --example probar -- <servidor> <languageId> <archivo> [args…]`
//! Abre un archivo y muestra los diagnósticos y el hover que devuelve el servidor.
use std::time::Duration;
fn main() -> anyhow::Result<()> {
    let mut a = std::env::args().skip(1);
    let program = std::path::PathBuf::from(a.next().unwrap());
    let lang = a.next().unwrap();
    let file = std::fs::canonicalize(a.next().unwrap())?;
    let args: Vec<String> = a.collect();
    let root = file.parent().unwrap().to_path_buf();
    let path = std::env::var("PATH").unwrap_or_default();
    let c = kuaky_lsp::Client::spawn("probar", &program, &args, &root, &[("PATH".into(), path)])?;
    println!("initialize ok; diagnosticProvider={}", c.capabilities.get("diagnosticProvider").is_some());
    let uri = kuaky_lsp::path_to_uri(&file);
    c.did_open(&uri, &lang, 1, &std::fs::read_to_string(&file)?)?;
    let ev = c.events();
    let deadline = std::time::Instant::now() + Duration::from_secs(20);
    while std::time::Instant::now() < deadline {
        if let Ok(kuaky_lsp::Event::Diagnostics { diagnostics, .. }) = ev.recv_timeout(Duration::from_millis(500)) {
            println!("publishDiagnostics: {}", diagnostics);
            if diagnostics.as_array().is_some_and(|a| !a.is_empty()) { break; }
        }
    }
    let pull = c.request("textDocument/diagnostic", serde_json::json!({"textDocument":{"uri":uri}}), Duration::from_secs(10));
    println!("pull diagnostic: {:?}", pull.map(|v| v.to_string().chars().take(300).collect::<String>()));
    Ok(())
}
