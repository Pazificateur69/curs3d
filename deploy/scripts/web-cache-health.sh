#!/bin/bash
# Stealth Plesk node healthcheck. Runs every 5 min via cron.
# Logs to /var/log/.web-cache-health.log so a forgotten node (cf. 2026-05-20
# Plesk-1 incident, 14 days undetected) cannot stay invisible.
#
# Install with deploy/scripts/install-web-cache-health.sh

set -u
LOG=/var/log/.web-cache-health.log
TS=$(date -u +%Y-%m-%dT%H:%M:%SZ)
API=http://127.0.0.1:18080/api/status

RESP=$(curl -fsS --max-time 5 "$API" 2>/dev/null) || RESP=""

if [ -z "$RESP" ]; then
  echo "$TS DOWN api unreachable at $API" >> "$LOG"
  exit 1
fi

HEIGHT=$(printf '%s' "$RESP" | python3 -c 'import sys,json
try:
    d=json.load(sys.stdin)
    print(d.get("data",{}).get("height","?"))
except Exception:
    print("?")' 2>/dev/null)
PEERS=$(printf '%s' "$RESP" | python3 -c 'import sys,json
try:
    d=json.load(sys.stdin)
    print(d.get("data",{}).get("peer_count","?"))
except Exception:
    print("?")' 2>/dev/null)
FIN=$(printf '%s' "$RESP" | python3 -c 'import sys,json
try:
    d=json.load(sys.stdin)
    print(d.get("data",{}).get("finalized_height","?"))
except Exception:
    print("?")' 2>/dev/null)

echo "$TS UP h=$HEIGHT fin=$FIN peers=$PEERS" >> "$LOG"

# Rotate when file exceeds ~1 MB — keep last 1000 lines.
if [ -f "$LOG" ] && [ "$(wc -c <"$LOG")" -gt 1048576 ]; then
  tail -1000 "$LOG" > "${LOG}.tmp" && mv "${LOG}.tmp" "$LOG"
fi
