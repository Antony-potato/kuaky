#!/usr/bin/env python3
"""Agente ACP falso para pruebas: responde por stdio como lo haría un agente real."""
import json
import sys


def send(msg):
    sys.stdout.write(json.dumps(msg) + "\n")
    sys.stdout.flush()


def update(session, upd):
    send({"jsonrpc": "2.0", "method": "session/update", "params": {"sessionId": session, "update": upd}})


def read():
    line = sys.stdin.readline()
    if not line:
        sys.exit(0)
    return json.loads(line)


while True:
    msg = read()
    method, mid = msg.get("method"), msg.get("id")
    if method == "initialize":
        send({"jsonrpc": "2.0", "id": mid, "result": {"protocolVersion": 1, "agentInfo": {"name": "mock", "title": "Mock Agent"}, "agentCapabilities": {}}})
    elif method == "session/new":
        send({"jsonrpc": "2.0", "id": mid, "result": {"sessionId": "s1"}})
    elif method == "session/prompt":
        s = msg["params"]["sessionId"]
        update(s, {"sessionUpdate": "agent_thought_chunk", "content": {"type": "text", "text": "pensando"}})
        update(s, {"sessionUpdate": "agent_message_chunk", "content": {"type": "text", "text": "Hola "}})
        update(s, {"sessionUpdate": "agent_message_chunk", "content": {"type": "text", "text": "mundo"}})
        update(s, {"sessionUpdate": "tool_call", "toolCallId": "t1", "title": "Editar a.txt", "kind": "edit", "status": "pending"})
        # El agente pide a kuaky algo que no ofrece: debe recibir error.
        send({"jsonrpc": "2.0", "id": 99, "method": "fs/read_text_file", "params": {"sessionId": s, "path": "/x"}})
        fs_reply = read()
        assert fs_reply.get("id") == 99 and "error" in fs_reply, fs_reply
        send({"jsonrpc": "2.0", "id": 100, "method": "session/request_permission", "params": {
            "sessionId": s,
            "toolCall": {"toolCallId": "t1", "title": "Editar a.txt"},
            "options": [
                {"optionId": "allow", "name": "Permitir", "kind": "allow_once"},
                {"optionId": "reject", "name": "Rechazar", "kind": "reject_once"},
            ],
        }})
        answer = read()
        chosen = answer["result"]["outcome"].get("optionId")
        status = "completed" if chosen == "allow" else "failed"
        update(s, {"sessionUpdate": "tool_call_update", "toolCallId": "t1", "status": status, "content": [
            {"type": "diff", "path": "/tmp/a.txt", "oldText": "a\n", "newText": "b\n"},
        ]})
        update(s, {"sessionUpdate": "plan", "entries": [{"content": "Paso 1", "priority": "high", "status": "completed"}]})
        send({"jsonrpc": "2.0", "id": mid, "result": {"stopReason": "end_turn"}})
