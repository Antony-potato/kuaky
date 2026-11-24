#!/usr/bin/env python3
"""Servidor LSP falso: responde initialize, hover y definition, publica diagnósticos."""
import json
import sys


def read():
    length = None
    while True:
        line = sys.stdin.buffer.readline()
        if not line:
            sys.exit(0)
        line = line.decode().strip()
        if not line:
            break
        if line.lower().startswith("content-length:"):
            length = int(line.split(":")[1])
    return json.loads(sys.stdin.buffer.read(length))


def send(msg):
    body = json.dumps(msg).encode()
    sys.stdout.buffer.write(b"Content-Length: %d\r\n\r\n" % len(body) + body)
    sys.stdout.buffer.flush()


while True:
    msg = read()
    method, mid = msg.get("method"), msg.get("id")
    if method == "initialize":
        # Antes de responder, el servidor pide configuración (el cliente debe contestar).
        send({"jsonrpc": "2.0", "id": 900, "method": "workspace/configuration", "params": {"items": [{}, {}]}})
        reply = read()
        assert reply.get("id") == 900 and reply.get("result") == [None, None], reply
        send({"jsonrpc": "2.0", "id": mid, "result": {"capabilities": {"completionProvider": {"triggerCharacters": ["."]}, "hoverProvider": True}}})
    elif method == "textDocument/didOpen":
        uri = msg["params"]["textDocument"]["uri"]
        send({"jsonrpc": "2.0", "method": "textDocument/publishDiagnostics", "params": {"uri": uri, "diagnostics": [
            {"range": {"start": {"line": 0, "character": 0}, "end": {"line": 0, "character": 3}}, "severity": 1, "message": "error de prueba"}
        ]}})
    elif method == "textDocument/hover":
        send({"jsonrpc": "2.0", "id": mid, "result": {"contents": {"kind": "markdown", "value": "**fn main**"}}})
    elif method == "textDocument/definition":
        uri = msg["params"]["textDocument"]["uri"]
        send({"jsonrpc": "2.0", "id": mid, "result": [{"uri": uri, "range": {"start": {"line": 2, "character": 0}, "end": {"line": 2, "character": 4}}}]})
    elif mid is not None:
        send({"jsonrpc": "2.0", "id": mid, "result": None})
