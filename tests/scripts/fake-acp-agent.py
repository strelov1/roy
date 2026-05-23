#!/usr/bin/env python3
"""Minimal ACP agent for hermetic AcpTransport tests. Speaks JSON-RPC over
stdio. With --permission, asks for permission before finishing a turn and only
finishes after the client auto-allows."""
import sys, json

permission = "--permission" in sys.argv
exit_on_initialize = "--exit-on-initialize" in sys.argv
no_initialize_reply = "--no-initialize-reply" in sys.argv
jsonrpc_error = "--jsonrpc-error" in sys.argv
exit_mid_turn = "--exit-mid-turn" in sys.argv

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
        if exit_on_initialize:
            sys.exit(0)
        if no_initialize_reply:
            continue
        if jsonrpc_error:
            out({"jsonrpc": "2.0", "id": mid,
                 "error": {"code": -32000, "message": "auth required"}})
            continue
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
        if exit_mid_turn:
            out({"jsonrpc": "2.0", "method": "session/update",
                 "params": {"sessionId": sid,
                            "update": {"sessionUpdate": "agent_message_chunk",
                                       "content": {"type": "text", "text": "partial"}}}})
            sys.exit(0)
        if permission:
            out({"jsonrpc": "2.0", "id": 9001, "method": "session/request_permission",
                 "params": {"sessionId": sid, "toolCall": {"title": "Bash"},
                            "options": [{"optionId": "allow", "name": "Allow"}]}})
            pending = (mid, sid)
        else:
            finish_turn(mid, sid)
    elif mid == 9001 and "result" in m and pending is not None:
        outcome = m.get("result", {}).get("outcome", {})
        if outcome.get("optionId") == "allow":
            # client allowed the tool; complete the turn
            finish_turn(pending[0], pending[1])
        pending = None
