#!/bin/bash
#
# CURS3D node healthcheck with alerting.
#
# What it checks:
#   - HTTP API on 127.0.0.1:8080 responds within 5s
#   - Chain height is advancing (compared to last run, persisted in /var/lib/curs3d/healthcheck.state)
#   - systemd service curs3d is `active`
#
# What it does on failure:
#   - Logs to /var/log/curs3d-healthcheck.log
#   - Restarts curs3d.service only for process/API failures by default
#   - Posts an alert to a Discord and/or Telegram webhook (if configured)
#   - Tracks restart counter to detect loops; alerts STUCK_LOOP if > 3 restarts in 10 min
#
# Notification config (read from /etc/curs3d/alerts.env, mode 0600 — optional):
#   DISCORD_WEBHOOK_URL=https://discord.com/api/webhooks/.../...
#   TELEGRAM_BOT_TOKEN=123456:ABC...
#   TELEGRAM_CHAT_ID=-1001234567890
#   ALERT_HOSTNAME=curs3d-node1            # override $(hostname -s) in messages
#   CURS3D_HEALTHCHECK_RESTART_ON_STUCK=0 # default: alert-only for consensus stalls
#
# Schedule (cron entry in /etc/cron.d/curs3d-healthcheck):
#   */2 * * * * root /usr/local/bin/curs3d-healthcheck.sh
#

set -uo pipefail

LOG="/var/log/curs3d-healthcheck.log"
STATE="/var/lib/curs3d/healthcheck.state"
ENV_FILE="/etc/curs3d/alerts.env"
RESTART_WINDOW_FILE="/var/lib/curs3d/healthcheck.restart_window"
STUCK_ALERT_FILE="/var/lib/curs3d/healthcheck.stuck_alert"
RESTART_LIMIT=3
RESTART_WINDOW_SECS=600
RESTART_ON_STUCK="${CURS3D_HEALTHCHECK_RESTART_ON_STUCK:-0}"
STUCK_ALERT_INTERVAL_SECS="${CURS3D_HEALTHCHECK_STUCK_ALERT_INTERVAL_SECS:-1800}"
HOSTNAME_DEFAULT="$(hostname -s)"

mkdir -p "$(dirname "$LOG")" "$(dirname "$STATE")"

if [ -f "$ENV_FILE" ]; then
    # shellcheck disable=SC1090
    . "$ENV_FILE"
fi
ALERT_HOSTNAME="${ALERT_HOSTNAME:-$HOSTNAME_DEFAULT}"

log() { echo "[$(date -Is)] $*" >> "$LOG"; }

notify() {
    local severity="$1" message="$2"
    local emoji
    case "$severity" in
        OK) emoji=":white_check_mark:" ;;
        WARN) emoji=":warning:" ;;
        CRITICAL) emoji=":rotating_light:" ;;
        *) emoji=":information_source:" ;;
    esac
    local full="$emoji  [$ALERT_HOSTNAME] $message"

    if [ -n "${DISCORD_WEBHOOK_URL:-}" ]; then
        local trimmed="${full:0:1900}"
        curl -sS -m 10 -X POST -H "Content-Type: application/json" \
            -d "$(jq -nc --arg c "$trimmed" '{content:$c}')" \
            "$DISCORD_WEBHOOK_URL" >/dev/null \
            || log "discord notify failed"
    fi
    if [ -n "${TELEGRAM_BOT_TOKEN:-}" ] && [ -n "${TELEGRAM_CHAT_ID:-}" ]; then
        curl -sS -m 10 -X POST \
            "https://api.telegram.org/bot${TELEGRAM_BOT_TOKEN}/sendMessage" \
            --data-urlencode "chat_id=${TELEGRAM_CHAT_ID}" \
            --data-urlencode "text=${full}" >/dev/null \
            || log "telegram notify failed"
    fi
}

note_restart() {
    local now
    now="$(date +%s)"
    : > "${RESTART_WINDOW_FILE}.tmp"
    if [ -f "$RESTART_WINDOW_FILE" ]; then
        awk -v cutoff="$((now - RESTART_WINDOW_SECS))" '$1 >= cutoff' "$RESTART_WINDOW_FILE" > "${RESTART_WINDOW_FILE}.tmp"
    fi
    echo "$now" >> "${RESTART_WINDOW_FILE}.tmp"
    mv "${RESTART_WINDOW_FILE}.tmp" "$RESTART_WINDOW_FILE"
    wc -l < "$RESTART_WINDOW_FILE" | tr -d ' '
}

restart_curs3d() {
    local reason="$1"
    log "RESTART: $reason"
    systemctl restart curs3d
    local count
    count="$(note_restart)"
    if [ "$count" -gt "$RESTART_LIMIT" ]; then
        notify CRITICAL "curs3d restart loop: $count restarts in last ${RESTART_WINDOW_SECS}s. Reason of last restart: $reason"
    else
        notify WARN "curs3d restarted ($count/$RESTART_LIMIT in ${RESTART_WINDOW_SECS}s). Reason: $reason"
    fi
}

notify_stuck_once() {
    local message="$1"
    local now
    now="$(date +%s)"
    local last=0
    if [ -f "$STUCK_ALERT_FILE" ]; then
        last="$(cat "$STUCK_ALERT_FILE" 2>/dev/null || echo 0)"
    fi
    if [ $((now - last)) -ge "$STUCK_ALERT_INTERVAL_SECS" ]; then
        echo "$now" > "$STUCK_ALERT_FILE"
        notify WARN "$message"
    fi
}

# 1. systemd active check
if ! systemctl is-active --quiet curs3d; then
    restart_curs3d "service inactive"
    exit 1
fi

# 2. API reachable
STATUS_JSON=$(curl -sf --max-time 5 http://127.0.0.1:8080/api/status || true)
if [ -z "$STATUS_JSON" ]; then
    restart_curs3d "API unreachable on 127.0.0.1:8080"
    exit 1
fi

HEIGHT=$(echo "$STATUS_JSON" | jq -r '.data.height // empty')
FINALIZED=$(echo "$STATUS_JSON" | jq -r '.data.finalized_height // empty')

if [ -z "$HEIGHT" ]; then
    restart_curs3d "API returned malformed status (no height)"
    exit 1
fi

# 3. Chain progress — height should advance; allow 2 min of stagnation.
NOW="$(date +%s)"
if [ -f "$STATE" ]; then
    PREV_HEIGHT=$(awk 'NR==1{print $1}' "$STATE")
    PREV_TS=$(awk 'NR==1{print $2}' "$STATE")
    PREV_HEIGHT="${PREV_HEIGHT:-0}"
    PREV_TS="${PREV_TS:-$NOW}"
else
    PREV_HEIGHT=0
    PREV_TS=$NOW
fi

STUCK_THRESHOLD="${CURS3D_HEALTHCHECK_STUCK_THRESHOLD_SECS:-600}"
ELAPSED=$((NOW - PREV_TS))

if [ "$HEIGHT" -gt "$PREV_HEIGHT" ]; then
    echo "$HEIGHT $NOW" > "$STATE"
    log "OK height=$HEIGHT finalized=${FINALIZED:-?}"
else
    if [ "$ELAPSED" -ge "$STUCK_THRESHOLD" ]; then
        log "WARN chain stagnant at height=$HEIGHT for ${ELAPSED}s (restart_on_stuck=$RESTART_ON_STUCK)"
        notify_stuck_once "chain stagnant at height=$HEIGHT for ${ELAPSED}s; not restarting automatically"
        if [ "$RESTART_ON_STUCK" = "1" ]; then
            restart_curs3d "chain stuck at height=$HEIGHT for ${ELAPSED}s"
            exit 1
        fi
    else
        log "stagnant height=$HEIGHT (${ELAPSED}s, threshold ${STUCK_THRESHOLD}s)"
    fi
fi

if [ -n "$FINALIZED" ] && [ "$FINALIZED" -ne 0 ]; then
    LAG=$((HEIGHT - FINALIZED))
    if [ "$LAG" -gt 50 ]; then
        log "WARN finality lag: height=$HEIGHT finalized=$FINALIZED lag=$LAG"
    fi
fi
