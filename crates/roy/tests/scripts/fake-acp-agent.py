#!/usr/bin/env python3
"""Minimal ACP agent for hermetic AcpTransport tests. Speaks JSON-RPC over
stdio against the official agent-client-protocol client.

Flags:
  --permission           ask permission before finishing; finish only if the
                         client selects the allow_once option by its id.
  --cancellable          stream one chunk then wait; finish (cancelled) only
                         after receiving session/cancel.
  --exit-mid-turn        stream one chunk then crash (non-zero exit) so the
                         SDK's child-monitor surfaces the failure.
  --exit-on-initialize   crash (non-zero exit) on the initialize request.
  --no-initialize-reply  never answer initialize.
  --jsonrpc-error        answer initialize with a JSON-RPC error.
"""
import sys, json

flags = set()
flood_n = 0
_argv = sys.argv[1:]
_i = 0
while _i < len(_argv):
    a = _argv[_i]
    if a == "--flood":
        flood_n = int(_argv[_i + 1])
        _i += 2
    else:
        flags.add(a)
        _i += 1

ALLOW_ID = "opt-allow-1"


def out(o):
    sys.stdout.write(json.dumps(o) + "\n")
    sys.stdout.flush()


def chunk(sid, text):
    out({"jsonrpc": "2.0", "method": "session/update",
         "params": {"sessionId": sid,
                    "update": {"sessionUpdate": "agent_message_chunk",
                               "content": {"type": "text", "text": text}}}})


def result(prompt_id, stop_reason):
    out({"jsonrpc": "2.0", "id": prompt_id, "result": {"stopReason": stop_reason}})


# Prompt awaiting an external event (permission response or session/cancel).
pending = None  # (prompt_id, session_id)

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
        if "--exit-on-initialize" in flags:
            sys.exit(1)
        if "--no-initialize-reply" in flags:
            continue
        if "--jsonrpc-error" in flags:
            out({"jsonrpc": "2.0", "id": mid,
                 "error": {"code": -32000, "message": "auth required"}})
            continue
        out({"jsonrpc": "2.0", "id": mid,
             "result": {"protocolVersion": 1,
                        "agentCapabilities": {"loadSession": True}}})
    elif method == "session/new":
        out({"jsonrpc": "2.0", "id": mid, "result": {"sessionId": "fake-acp-sid"}})
    elif method == "session/load":
        out({"jsonrpc": "2.0", "id": mid, "result": {}})
    elif method == "session/set_mode":
        out({"jsonrpc": "2.0", "id": mid, "result": {}})
    elif method == "session/prompt":
        sid = m["params"]["sessionId"]
        if "--exit-mid-turn" in flags:
            chunk(sid, "partial")
            sys.exit(1)
        if "--permission" in flags:
            out({"jsonrpc": "2.0", "id": 9001, "method": "session/request_permission",
                 "params": {"sessionId": sid, "toolCall": {"toolCallId": "t1", "title": "Bash"},
                            "options": [
                                {"optionId": ALLOW_ID, "name": "Allow", "kind": "allow_once"},
                                {"optionId": "opt-reject-1", "name": "Reject", "kind": "reject_once"}]}})
            pending = (mid, sid)
        elif "--cancellable" in flags:
            chunk(sid, "working")
            pending = (mid, sid)
        else:
            for _k in range(flood_n):
                chunk(sid, f"flood-{_k}\n")
            chunk(sid, "ack")
            result(mid, "end_turn")
    elif method == "session/cancel":
        # Notification (no id). Confirm cancellation of the active prompt.
        if pending is not None:
            result(pending[0], "cancelled")
            pending = None
    elif mid == 9001 and "result" in m and pending is not None:
        outcome = m.get("result", {}).get("outcome", {})
        if outcome.get("outcome") == "selected" and outcome.get("optionId") == ALLOW_ID:
            chunk(pending[1], "ack")
            result(pending[0], "end_turn")
        else:
            result(pending[0], "cancelled")
        pending = None
