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
        send({"jsonrpc": "2.0", "id": mid, "result": {"protocolVersion": 1, "agentInfo": {"name": "mock", "title": "Mock Agent"},
              "agentCapabilities": {"loadSession": True, "promptCapabilities": {"image": True}}}})
    elif method == "session/new":
        send({"jsonrpc": "2.0", "id": mid, "result": {"sessionId": "s1",
              "modes": {"currentModeId": "default", "availableModes": [{"id": "default", "name": "Default"}, {"id": "plan", "name": "Plan"}]}}})
    elif method == "session/load":
        s = msg["params"]["sessionId"]
        update(s, {"sessionUpdate": "user_message_chunk", "content": {"type": "text", "text": "pregunta vieja"}})
        update(s, {"sessionUpdate": "agent_message_chunk", "content": {"type": "text", "text": "respuesta vieja"}})
        send({"jsonrpc": "2.0", "id": mid, "result": {"configOptions": [{"id": "model", "name": "Model", "category": "model",
              "currentValue": "a", "options": [{"value": "a", "name": "A"}, {"value": "b", "name": "B"}]}]}})
    elif method == "session/set_config_option":
        p = msg["params"]
        send({"jsonrpc": "2.0", "id": mid, "result": {"configOptions": [{"id": p["configId"], "name": "Model", "category": "model",
              "currentValue": p["value"], "options": [{"value": "a", "name": "A"}, {"value": "b", "name": "B"}]}]}})
    elif method == "session/prompt":
        s = msg["params"]["sessionId"]
        if any(b.get("text") == "die" for b in msg["params"].get("prompt", [])):
            sys.exit(1)  # simula que el proceso del agente se cae
        update(s, {"sessionUpdate": "agent_thought_chunk", "content": {"type": "text", "text": "pensando"}})
        update(s, {"sessionUpdate": "agent_message_chunk", "content": {"type": "text", "text": "Hola "}})
        update(s, {"sessionUpdate": "agent_message_chunk", "content": {"type": "text", "text": "mundo"}})
        update(s, {"sessionUpdate": "available_commands_update", "availableCommands": [{"name": "review", "description": "Review changes"}]})
        update(s, {"sessionUpdate": "tool_call", "toolCallId": "t1", "title": "Editar a.txt", "kind": "edit", "status": "pending",
                   "locations": [{"path": "/tmp/a.txt", "line": 3}]})
        # Archivos a través del cliente: leer, escribir dentro y fuera de la carpeta permitida.
        import os, tempfile
        root = os.path.realpath(tempfile.gettempdir())
        target = os.path.join(root, "kuaky-mock-acp.txt")
        send({"jsonrpc": "2.0", "id": 97, "method": "fs/write_text_file", "params": {"sessionId": s, "path": target, "content": "uno\ndos\n"}})
        w = read()
        assert w.get("id") == 97 and "error" not in w, w
        send({"jsonrpc": "2.0", "id": 98, "method": "fs/read_text_file", "params": {"sessionId": s, "path": target, "line": 2, "limit": 1}})
        r = read()
        assert r["result"]["content"] == "dos\n", r
        send({"jsonrpc": "2.0", "id": 99, "method": "fs/write_text_file", "params": {"sessionId": s, "path": "/etc/kuaky-nope", "content": "x"}})
        fs_reply = read()
        assert fs_reply.get("id") == 99 and "error" in fs_reply, fs_reply
        # Lo que no ofrece kuaky (terminal) recibe error.
        send({"jsonrpc": "2.0", "id": 96, "method": "terminal/create", "params": {"sessionId": s, "command": "ls"}})
        t = read()
        assert t.get("id") == 96 and "error" in t, t
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
        for i, cmd in enumerate(["git status --short", "cargo test -p kuaky"]):
            update(s, {"sessionUpdate": "tool_call", "toolCallId": f"c{i}", "title": cmd, "kind": "execute", "status": "completed"})
        update(s, {"sessionUpdate": "agent_message_chunk", "content": {"type": "text", "text": "\n\nListo: el archivo quedó editado y los tests pasan."}})
        update(s, {"sessionUpdate": "tool_call", "toolCallId": "sa1", "title": "Find auth code", "kind": "think", "status": "in_progress",
                   "rawInput": {"subagent_type": "Explore", "description": "Find auth code"}, "_meta": {"claudeCode": {"toolName": "Agent"}}})
        update(s, {"sessionUpdate": "tool_call_update", "toolCallId": "sa1", "status": "completed"})
        update(s, {"sessionUpdate": "tool_call", "toolCallId": "sa2", "title": "Review PR 2278", "kind": "think", "status": "in_progress",
                   "rawInput": {"subagent_type": "general-purpose", "description": "Review PR 2278"}, "_meta": {"claudeCode": {"toolName": "Agent"}}})
        for i, (title, kind, status) in enumerate([("Read src/payments.rs", "read", "completed"), ("grep -rn \"refund\" src", "execute", "completed"),
                                                    ("Read src/refunds/mod.rs", "read", "in_progress")]):
            update(s, {"sessionUpdate": "tool_call", "toolCallId": f"sa2-{i}", "title": title, "kind": kind, "status": status,
                       "_meta": {"claudeCode": {"toolName": "Read" if kind == "read" else "Bash", "parentToolUseId": "sa2"}}})
        update(s, {"sessionUpdate": "usage_update", "used": 52000, "size": 200000, "cost": {"amount": 0.42, "currency": "USD"}})
        send({"jsonrpc": "2.0", "id": mid, "result": {"stopReason": "end_turn"}})
