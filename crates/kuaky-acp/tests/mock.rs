//! Conversación completa contra un agente ACP falso (sin red ni tokens).
#![cfg(unix)]

use kuaky_acp::{Client, Event, McpServer};
use std::{path::PathBuf, time::Duration};

#[test]
fn full_turn_with_permission_and_diff() {
    let script = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/mock_agent.py");
    let client = Client::spawn(&PathBuf::from("python3"), &[script.to_string_lossy().into_owned()], &std::env::temp_dir(), &[]).unwrap();
    assert_eq!(client.agent_name.as_deref(), Some("Mock Agent"));
    let session = client.new_session(&std::env::temp_dir()).unwrap();
    assert_eq!(session, "s1");

    let events = client.events();
    client.prompt(&session, "hola").unwrap();

    let mut message = String::new();
    let mut seen = Vec::new();
    loop {
        let ev = events.recv_timeout(Duration::from_secs(10)).expect("el agente dejó de responder");
        match &ev {
            Event::MessageChunk(t) => message.push_str(t),
            Event::Permission(req) => {
                assert_eq!(req.title, "Editar a.txt");
                assert_eq!(req.options.len(), 2);
                client.answer_permission(&req.request_id, Some("allow")).unwrap();
            }
            Event::ToolCallUpdate(call) if call.id == "t1" => {
                assert_eq!(call.status.as_deref(), Some("completed"));
                assert_eq!(call.diffs[0].new_text, "b\n");
            }
            Event::TurnEnded(reason) => {
                assert_eq!(reason, "end_turn");
                seen.push(ev.clone());
                break;
            }
            Event::Error(e) => panic!("error: {e}"),
            Event::Exited => panic!("el agente terminó"),
            _ => {}
        }
        seen.push(ev);
    }
    assert!(message.starts_with("Hola mundo"), "{message}");
    assert!(seen.iter().any(|e| matches!(e, Event::Usage { used: 52000, size: 200000, .. })));
    assert!(seen.iter().any(|e| matches!(e, Event::Usage { rate_limit: Some(r), .. } if r.pointer("/unifiedWindows/five_hour").is_some())));
    assert!(seen.iter().any(|e| matches!(e, Event::ThoughtChunk(_))));
    assert!(seen.iter().any(|e| matches!(e, Event::ToolCall(c) if c.kind.as_deref() == Some("edit"))));
    assert!(seen.iter().any(|e| matches!(e, Event::Plan(p) if p.len() == 1)));
}

#[test]
fn load_session_replays_and_config_changes() {
    let script = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/mock_agent.py");
    let client = Client::spawn(&PathBuf::from("python3"), &[script.to_string_lossy().into_owned()], &std::env::temp_dir(), &[]).unwrap();
    assert!(client.capabilities.load_session);
    assert!(client.capabilities.image);
    let info = client.new_session_info(&std::env::temp_dir()).unwrap();
    assert_eq!(info.config[0].category, "mode");
    assert_eq!(info.config[0].current, "default");

    let events = client.events();
    let loaded = client.load_session(&std::env::temp_dir(), "old").unwrap();
    assert_eq!(loaded.config[0].current, "a");
    let mut replay = Vec::new();
    while let Ok(ev) = events.recv_timeout(Duration::from_millis(300)) {
        replay.push(ev);
    }
    assert!(replay.contains(&Event::UserMessageChunk("pregunta vieja".into())));
    assert!(replay.contains(&Event::MessageChunk("respuesta vieja".into())));

    let updated = client.set_config_option("old", "model", "b").unwrap();
    assert_eq!(updated[0].current, "b");
}

#[test]
fn file_writes_and_commands_are_reported() {
    let script = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/mock_agent.py");
    let client = Client::spawn(&PathBuf::from("python3"), &[script.to_string_lossy().into_owned()], &std::env::temp_dir(), &[]).unwrap();
    let session = client.new_session(&std::env::temp_dir()).unwrap();
    let events = client.events();
    client.prompt(&session, "hola").unwrap();
    let (mut wrote, mut commands, mut located) = (false, false, false);
    let mut written = None;
    loop {
        match events.recv_timeout(Duration::from_secs(10)).expect("sin respuesta") {
            Event::FileWritten { path, new_text, .. } => {
                assert!(path.to_string_lossy().contains("kuaky-mock-acp-"));
                written = Some(path);
                assert_eq!(new_text, "uno\ndos\n");
                wrote = true;
            }
            Event::Commands(c) => commands = c.iter().any(|c| c.name == "review"),
            Event::ToolCall(c) => located |= c.locations.first().is_some_and(|l| l.line == Some(3)),
            Event::Permission(req) => client.answer_permission(&req.request_id, Some("allow")).unwrap(),
            Event::TurnEnded(_) => break,
            Event::Error(e) => panic!("{e}"),
            _ => {}
        }
    }
    if let Some(path) = written {
        let _ = std::fs::remove_file(path);
    }
    assert!(wrote && commands && located);
}

#[test]
fn agent_death_is_reported() {
    let script = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/mock_agent.py");
    let client = Client::spawn(&PathBuf::from("python3"), &[script.to_string_lossy().into_owned()], &std::env::temp_dir(), &[]).unwrap();
    let session = client.new_session(&std::env::temp_dir()).unwrap();
    assert!(!client.is_closed());
    client.prompt(&session, "die").unwrap();
    let events = client.events();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    loop {
        match events.recv_timeout(std::time::Duration::from_secs(10)) {
            Ok(Event::Exited) => break,
            Ok(_) => {}
            Err(e) => panic!("no Exited: {e}"),
        }
        assert!(std::time::Instant::now() < deadline);
    }
    assert!(client.is_closed());
}

#[test]
fn passes_mcp_servers_to_the_session() {
    let script = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/mock_agent.py");
    let client = Client::spawn(&PathBuf::from("python3"), &[script.to_string_lossy().into_owned()], &std::env::temp_dir(), &[]).unwrap();
    client.set_mcp_servers(vec![McpServer {
        name: "echo".into(),
        command: PathBuf::from("/bin/kuaky"),
        args: vec!["mcp".into()],
        env: vec![("KUAKY_SESSION".into(), "s7".into())],
    }]);
    let session = client.new_session(&std::env::temp_dir()).unwrap();
    assert_eq!(session, "mcp:echo:/bin/kuaky:KUAKY_SESSION:mcp__echo");
}
