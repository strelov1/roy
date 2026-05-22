#!/usr/bin/env python3
"""Minimal ACP agent for hermetic AcpTransport tests. Speaks JSON-RPC over
stdio. With --permission, asks for permission before finishing a turn and only
finishes after the client auto-allows."""
import sys, json

permission = "--permission" in sys.argv

def out(o):
    sys.stdout.write(json.dumps(o) + "\n")
    sys.stdout.flush()

pending = None  # (prompt_id, session_id) awaiting the client's allow

def finish_turn(prompt_id, sid):
    out({"jsonrpc": "2.0", "method": "session/update",
         "params": {"sessionId": sid,
                    "update": {"sessionUpdate": "agent_message_chunk",
                               "content": {"type": "text", "text": "ack"}}}})
    out({"jsonrpc": "2.0", "id": prompt_id, "result": {"stopReason": "end_turn"}})

for line in sys.stdin:
    line = line.strip()
    if not line:
        continue
    try:
        m = json.loads(line)
    except Exception:
        continue
    mid = m.get("id")
    method = m.get("method")
    if method == "initialize":
        out({"jsonrpc": "2.0", "id": mid,
             "result": {"protocolVersion": 1, "agentCapabilities": {"loadSession": True}}})
    elif method == "session/new":
        out({"jsonrpc": "2.0", "id": mid, "result": {"sessionId": "fake-acp-sid"}})
    elif method == "session/load":
        out({"jsonrpc": "2.0", "id": mid, "result": {}})
    elif method == "session/set_mode":
        out({"jsonrpc": "2.0", "id": mid, "result": {}})
    elif method == "session/prompt":
        sid = m["params"]["sessionId"]
        if permission:
            out({"jsonrpc": "2.0", "id": 9001, "method": "session/request_permission",
                 "params": {"sessionId": sid, "toolCall": {"title": "Bash"},
                            "options": [{"optionId": "allow", "name": "Allow"}]}})
            pending = (mid, sid)
        else:
            finish_turn(mid, sid)
    elif mid == 9001 and "result" in m and pending is not None:
        # client allowed the tool; complete the turn
        finish_turn(pending[0], pending[1])
        pending = None
