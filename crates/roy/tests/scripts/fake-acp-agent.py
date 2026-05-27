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
  --flood N              on the default prompt branch, emit N AssistantText
                         chunks ("flood-0\\n", "flood-1\\n", ...) before the
                         final "ack" chunk and terminal Result. Used by tests
                         that stress the broadcast/journal pipeline.
"""
import sys, json, os

flags = set()
flood_n = 0
meta_out = None
env_out = None
_argv = sys.argv[1:]
_i = 0
while _i < len(_argv):
    a = _argv[_i]
    if a == "--flood":
        flood_n = int(_argv[_i + 1])
        _i += 2
    elif a == "--meta-out":
        meta_out = _argv[_i + 1]
        _i += 2
    elif a == "--env-out":
        env_out = _argv[_i + 1]
        _i += 2
    else:
        flags.add(a)
        _i += 1


def record_meta(m):
    if meta_out is not None:
        with open(meta_out, "w") as f:
            json.dump(m.get("params", {}).get("_meta"), f)


def record_env():
    if env_out is not None:
        with open(env_out, "w") as f:
            json.dump({"ROY_SESSION_ID": os.environ.get("ROY_SESSION_ID")}, f)


record_env()

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
        record_meta(m)
        out({"jsonrpc": "2.0", "id": mid, "result": {"sessionId": "fake-acp-sid"}})
    elif method == "session/load":
        record_meta(m)
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
