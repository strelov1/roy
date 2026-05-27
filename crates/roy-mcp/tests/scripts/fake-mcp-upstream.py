#!/usr/bin/env python3
"""Minimal MCP-over-stdio fake. Configurable via env:
  FAKE_TOOLS - JSON array of tool descriptors (default: one "echo" tool)
  FAKE_NAME  - server name (default: fake-upstream)

Used by crates/roy-mcp/tests/serve_connections.rs to drive the proxy
end-to-end without a real upstream binary. Behavior is deterministic and
intentionally narrow — extend only if a test needs a new code path.
"""
import json
import os
import sys

DEFAULT_TOOLS = [
    {
        "name": "echo",
        "description": "Echo input back as text.",
        "inputSchema": {
            "type": "object",
            "properties": {"msg": {"type": "string"}},
            "required": ["msg"],
        },
    }
]


def main():
    tools = json.loads(os.environ.get("FAKE_TOOLS", json.dumps(DEFAULT_TOOLS)))
    name = os.environ.get("FAKE_NAME", "fake-upstream")
    for line in sys.stdin:
        line = line.strip()
        if not line:
            continue
        try:
            req = json.loads(line)
        except json.JSONDecodeError:
            continue
        method = req.get("method", "")
        rid = req.get("id")
        if method == "initialize":
            resp = {
                "jsonrpc": "2.0",
                "id": rid,
                "result": {
                    "protocolVersion": "2024-11-05",
                    "capabilities": {"tools": {}},
                    "serverInfo": {"name": name, "version": "0"},
                },
            }
        elif method == "notifications/initialized":
            continue
        elif method == "tools/list":
            resp = {"jsonrpc": "2.0", "id": rid, "result": {"tools": tools}}
        elif method == "tools/call":
            params = req.get("params", {})
            tool = params.get("name", "")
            args = params.get("arguments", {})
            if tool == "echo":
                resp = {
                    "jsonrpc": "2.0",
                    "id": rid,
                    "result": {
                        "content": [{"type": "text", "text": str(args.get("msg", ""))}],
                        "isError": False,
                    },
                }
            else:
                resp = {
                    "jsonrpc": "2.0",
                    "id": rid,
                    "error": {"code": -32602, "message": f"unknown tool {tool}"},
                }
        elif rid is None:
            continue
        else:
            resp = {
                "jsonrpc": "2.0",
                "id": rid,
                "error": {"code": -32601, "message": f"method not found: {method}"},
            }
        sys.stdout.write(json.dumps(resp) + "\n")
        sys.stdout.flush()


if __name__ == "__main__":
    main()
