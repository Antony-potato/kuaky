//! `cargo run -p kuaky-acp --example sondear -- <agente> [args…]`
//! Hace `initialize` y `session/new` (no envía mensajes, no gasta tokens).
fn main() -> anyhow::Result<()> {
    let mut args = std::env::args().skip(1);
    let program = std::path::PathBuf::from(args.next().expect("falta la ruta del agente"));
    let rest: Vec<String> = args.collect();
    let cwd = std::env::current_dir()?;
    let t = std::time::Instant::now();
    let client = kuaky_acp::Client::spawn(&program, &rest, &cwd, &[])?;
    println!("initialize ok en {:?}: {:?}", t.elapsed(), client.agent_name);
    let session = client.new_session(&cwd)?;
    println!("session/new ok en {:?}: {session}", t.elapsed());
    Ok(())
}
