//! Conversación completa contra un agente ACP falso (sin red ni tokens).
#![cfg(unix)]

use kuaky_acp::{Client, Event};
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
            Event::ToolCallUpdate(call) => {
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
    assert_eq!(message, "Hola mundo");
    assert!(seen.iter().any(|e| matches!(e, Event::ThoughtChunk(_))));
    assert!(seen.iter().any(|e| matches!(e, Event::ToolCall(c) if c.kind.as_deref() == Some("edit"))));
    assert!(seen.iter().any(|e| matches!(e, Event::Plan(p) if p.len() == 1)));
}
