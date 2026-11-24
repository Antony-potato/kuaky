#!/usr/bin/env python3
"""Agente ACP falso que usa las herramientas de kuaky (para capturas de desarrollo).

Con el mensaje "delegate" abre otra sesión con `open_session` por el servidor
MCP que kuaky le pasó en `session/new`, y después la lista y la lee. A la
tarea que recibe la sesión abierta contesta con un plan y una respuesta.
"""
import json
import subprocess
import sys
import time

servers = []


def send(msg):
    sys.stdout.write(json.dumps(msg) + "\n")
    sys.stdout.flush()


def update(session, upd):
    send({"jsonrpc": "2.0", "method": "session/update", "params": {"sessionId": session, "update": upd}})


def say(session, text):
    update(session, {"sessionUpdate": "agent_message_chunk", "content": {"type": "text", "text": text}})


class Mcp:
    """Cliente MCP mínimo sobre stdio."""

    def __init__(self, server):
        env = {e["name"]: e["value"] for e in server.get("env", [])}
        import os
        self.proc = subprocess.Popen([server["command"], *server.get("args", [])], stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                                     env={**os.environ, **env}, text=True)
        self.next = 0
        self.rpc("initialize", {"protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": {"name": "mock"}})
        self.proc.stdin.write(json.dumps({"jsonrpc": "2.0", "method": "notifications/initialized"}) + "\n")
        self.proc.stdin.flush()

    def rpc(self, method, params):
        self.next += 1
        self.proc.stdin.write(json.dumps({"jsonrpc": "2.0", "id": self.next, "method": method, "params": params}) + "\n")
        self.proc.stdin.flush()
        return json.loads(self.proc.stdout.readline())

    def call(self, name, args):
        r = self.rpc("tools/call", {"name": name, "arguments": args})
        if "error" in r:
            return "error: " + r["error"]["message"]
        text = r["result"]["content"][0]["text"]
        return ("error: " if r["result"].get("isError") else "") + text


while True:
    line = sys.stdin.readline()
    if not line:
        sys.exit(0)
    msg = json.loads(line)
    method, mid = msg.get("method"), msg.get("id")
    if method == "initialize":
        send({"jsonrpc": "2.0", "id": mid, "result": {"protocolVersion": 1, "agentInfo": {"name": "mock", "title": "Mock Delegator"},
              "agentCapabilities": {"loadSession": False}}})
    elif method == "session/new":
        servers = msg["params"].get("mcpServers", [])
        send({"jsonrpc": "2.0", "id": mid, "result": {"sessionId": f"d{time.time_ns()}"}})
    elif method == "session/prompt":
        s = msg["params"]["sessionId"]
        text = "".join(b.get("text", "") for b in msg["params"].get("prompt", []))
        if text.strip() == "delegate":
            kuaky = [x for x in servers if x["name"] == "kuaky"]
            if not kuaky:
                say(s, "kuaky did not pass its tools.")
            else:
                mcp = Mcp(kuaky[0])
                opened = mcp.call("open_session", {"agent": "antigravity", "title": "Sidebar tests",
                                                   "task": "Write tests for the sidebar tree in crates/kuaky/src/metro.rs. I keep project.rs."})
                say(s, f"**open_session** → {opened}\n\n")
                sid = opened.split("`")[1] if "`" in opened else ""
                time.sleep(3)
                say(s, f"**list_sessions** →\n{mcp.call('list_sessions', {})}\n\n")
                say(s, f"**read_session** → {mcp.call('read_session', {'session': sid})}")
        elif "opened this session from kuaky" in text:
            update(s, {"sessionUpdate": "plan", "entries": [
                {"content": "Read the sidebar tree", "status": "completed", "priority": "high"},
                {"content": "Write the tests", "status": "in_progress", "priority": "high"}]})
            say(s, "Added 3 tests for the sidebar tree; all pass.")
        else:
            say(s, "ok")
        send({"jsonrpc": "2.0", "id": mid, "result": {"stopReason": "end_turn"}})
