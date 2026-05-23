#!/usr/bin/env bash
# Fake stream-json agent for hermetic transport tests. Ignores its CLI args.
# For each line read on stdin, emits an assistant text echo then a result.
echo '{"type":"system","subtype":"init","session_id":"fake","cwd":"/tmp"}'
while IFS= read -r line; do
  echo '{"type":"assistant","message":{"content":[{"type":"text","text":"ack"}]}}'
  echo '{"type":"result","subtype":"success","is_error":false,"result":"ack","total_cost_usd":0.0}'
done
