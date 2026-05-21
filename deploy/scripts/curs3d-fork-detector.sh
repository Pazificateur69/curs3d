#!/bin/bash
#
# CURS3D fork-detector. Polls every validator's public /api/status, compares
# heights, and alerts to Discord if the spread exceeds the threshold for
# several consecutive ticks. Designed to prevent the 2026-05-20 Plesk-1
# stealth-fork situation (14 days undetected).
#
# Runs on ONE node only (typically curs3d-node1) — picks an arbitrary node
# to be the watcher; running it on multiple nodes only multiplies alerts.
#
# Install via:
#   sudo install -m 755 deploy/scripts/curs3d-fork-detector.sh /usr/local/bin/
#   sudo install -m 644 deploy/systemd/curs3d-fork-detector.{service,timer} /etc/systemd/system/
#   sudo systemctl daemon-reload
#   sudo systemctl enable --now curs3d-fork-detector.timer
#
# Config (in /etc/curs3d/alerts.env, mode 0600):
#   DISCORD_WEBHOOK_URL=https://discord.com/api/webhooks/.../...
#   CURS3D_VALIDATOR_URLS=https://api.curs3d.fr/api/status,http://84.235.238.213:8080/api/status,...
#   CURS3D_FORK_DETECT_THRESHOLD=3            # height spread that triggers concern
#   CURS3D_FORK_DETECT_CONSECUTIVE=3          # consecutive ticks above threshold
#   CURS3D_FORK_DETECT_ALERT_INTERVAL_MIN=60  # don't re-alert within X minutes

set -uo pipefail

LOG="/var/log/curs3d-fork-detector.log"
STATE_DIR="/var/lib/curs3d"
STATE_FILE="$STATE_DIR/fork-detector.state"
ALERT_FILE="$STATE_DIR/fork-detector.last-alert"

CONFIG="/etc/curs3d/alerts.env"
if [ -f "$CONFIG" ]; then
  set -a; . "$CONFIG"; set +a
fi

THRESHOLD="${CURS3D_FORK_DETECT_THRESHOLD:-3}"
CONSECUTIVE="${CURS3D_FORK_DETECT_CONSECUTIVE:-3}"
ALERT_INTERVAL_MIN="${CURS3D_FORK_DETECT_ALERT_INTERVAL_MIN:-60}"

# Default URL list — 5-validator testnet plus the public API as a tiebreaker.
# Override via CURS3D_VALIDATOR_URLS to test other topologies.
DEFAULT_URLS="https://api.curs3d.fr/api/status,http://144.24.192.222:8080/api/status,http://84.235.238.213:8080/api/status,http://31.70.70.62:8080/api/status,http://217.154.7.175:18080/api/status,http://195.35.28.51:18080/api/status"
URLS="${CURS3D_VALIDATOR_URLS:-$DEFAULT_URLS}"

mkdir -p "$STATE_DIR"
touch "$LOG"
TS=$(date -u +%Y-%m-%dT%H:%M:%SZ)

log() { echo "$TS $*" | tee -a "$LOG" >/dev/null; }

# Read consecutive-bad counter (default 0).
CONSEC=0
if [ -f "$STATE_FILE" ]; then
  CONSEC=$(cat "$STATE_FILE" 2>/dev/null || echo 0)
fi

# Sample each URL: name=URL_HOSTNAME, h=HEIGHT
heights=()
hosts=()
IFS=',' read -ra URL_ARRAY <<< "$URLS"
for url in "${URL_ARRAY[@]}"; do
  host=$(echo "$url" | sed -E 's#^https?://([^:/]+).*#\1#')
  resp=$(curl -fsS --max-time 5 "$url" 2>/dev/null || true)
  if [ -z "$resp" ]; then
    log "WARN $host unreachable"
    continue
  fi
  h=$(printf '%s' "$resp" | python3 -c '
import sys, json
try:
    d = json.load(sys.stdin)
    print(d.get("data", {}).get("height", -1))
except Exception:
    print(-1)
' 2>/dev/null)
  if [ "$h" = "-1" ] || [ -z "$h" ]; then
    log "WARN $host malformed status response"
    continue
  fi
  hosts+=("$host=$h")
  heights+=("$h")
done

if [ "${#heights[@]}" -lt 2 ]; then
  log "INFO insufficient samples (${#heights[@]}), skipping comparison"
  exit 0
fi

# Compute min/max/spread.
MIN=$(printf '%s\n' "${heights[@]}" | sort -n | head -1)
MAX=$(printf '%s\n' "${heights[@]}" | sort -n | tail -1)
SPREAD=$((MAX - MIN))
SUMMARY=$(IFS=,; echo "${hosts[*]}")

log "tick spread=$SPREAD min=$MIN max=$MAX [$SUMMARY]"

if [ "$SPREAD" -gt "$THRESHOLD" ]; then
  CONSEC=$((CONSEC + 1))
  echo "$CONSEC" > "$STATE_FILE"
  log "DEGRADED spread=$SPREAD > threshold=$THRESHOLD consec=$CONSEC/$CONSECUTIVE"

  if [ "$CONSEC" -ge "$CONSECUTIVE" ]; then
    # Throttle: don't alert again within ALERT_INTERVAL_MIN.
    SHOULD_ALERT=1
    if [ -f "$ALERT_FILE" ]; then
      last=$(stat -c %Y "$ALERT_FILE" 2>/dev/null || stat -f %m "$ALERT_FILE" 2>/dev/null || echo 0)
      now=$(date +%s)
      if [ $((now - last)) -lt $((ALERT_INTERVAL_MIN * 60)) ]; then
        SHOULD_ALERT=0
        log "ALERT suppressed (last alert was $((now - last))s ago, throttle=${ALERT_INTERVAL_MIN}min)"
      fi
    fi

    if [ "$SHOULD_ALERT" = "1" ] && [ -n "${DISCORD_WEBHOOK_URL:-}" ]; then
      MSG=":rotating_light: **CURS3D fork suspect** :rotating_light:
Height spread between validators: **$SPREAD** (min=$MIN, max=$MAX)
Consecutive degraded ticks: $CONSEC
Per-node: $SUMMARY
Time: $TS"
      JSON=$(python3 -c "import json,sys; print(json.dumps({'content': sys.argv[1]}))" "$MSG")
      curl -fsS -X POST -H 'Content-Type: application/json' \
        --max-time 10 \
        -d "$JSON" "$DISCORD_WEBHOOK_URL" >/dev/null \
        && { touch "$ALERT_FILE"; log "ALERT sent to Discord"; } \
        || log "ERROR Discord webhook delivery failed"
    elif [ "$SHOULD_ALERT" = "1" ]; then
      log "ALERT would fire but DISCORD_WEBHOOK_URL is unset — set it in $CONFIG"
    fi
  fi
else
  if [ "$CONSEC" -gt 0 ]; then
    log "RECOVERED spread back within threshold (consec reset)"
  fi
  echo 0 > "$STATE_FILE"
fi
