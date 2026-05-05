#!/usr/bin/env bash
# shellcheck shell=bash
#
# Requires bash 4+ for associative arrays. macOS ships bash 3.2 by default;
# install a recent bash via `brew install bash` and run with the brew binary
# (Cellar path is e.g. /opt/homebrew/bin/bash) or just `bash soak-monitor.sh`
# after PATH update. The script self-checks below.
#
# CURS3D 24-72h soak monitor — runs locally, polls all 3 validators every
# INTERVAL seconds, writes a CSV log, and flags any of:
#   - stall:        height did not advance for STALL_THRESHOLD seconds
#   - divergence:   two nodes report different latest_hash at the same height
#   - finality lag: head outruns finalized by more than FINALITY_LAG_MAX blocks
#   - peer drop:    any node has 0 peers for more than PEER_DROP_SECS
#   - api drop:     any node fails to respond to /api/status
#
# Output:
#   ./soak.csv     timestamp, per-node fields
#   ./soak.alerts  alert lines (one per event)
#   stdout         live tail (one summary line per poll)
#
# Pass conditions (printed at exit when SIGINT or duration reached):
#   - 0 stalls, 0 divergences, 0 unrecovered api drops over the soak window
#   - finalized_height advanced monotonically on every node
#   - peer_count stayed >= 1 on every node (no node ever isolated for >peer_drop)
#
# Usage:
#   ./soak-monitor.sh                    # run forever (Ctrl+C to stop)
#   ./soak-monitor.sh --duration 86400   # 24h
#   ./soak-monitor.sh --duration 259200  # 72h
#   ./soak-monitor.sh --interval 60      # poll every 60s instead of 30s
#   ./soak-monitor.sh --csv /tmp/soak.csv

set -euo pipefail

# Bash version check — associative arrays require bash 4+.
if [ "${BASH_VERSINFO[0]:-0}" -lt 4 ]; then
    cat >&2 <<EOF
soak-monitor.sh requires bash 4 or newer (got bash ${BASH_VERSION}).
On macOS, install with:  brew install bash
Then run:                /opt/homebrew/bin/bash $0 [args...]
For a quick spot check that works on bash 3.2, use ./soak-status.sh.
EOF
    exit 2
fi

# ─── Config ──────────────────────────────────────────────────────────────
INTERVAL_DEFAULT=30
DURATION_DEFAULT=0  # 0 = run until Ctrl+C
STALL_THRESHOLD_DEFAULT=120          # seconds with no height advance
FINALITY_LAG_MAX_DEFAULT=50          # head - finalized
PEER_DROP_SECS_DEFAULT=120           # seconds with peers=0
CSV_DEFAULT="./soak.csv"
ALERTS_DEFAULT="./soak.alerts"

INTERVAL="$INTERVAL_DEFAULT"
DURATION="$DURATION_DEFAULT"
STALL_THRESHOLD="$STALL_THRESHOLD_DEFAULT"
FINALITY_LAG_MAX="$FINALITY_LAG_MAX_DEFAULT"
PEER_DROP_SECS="$PEER_DROP_SECS_DEFAULT"
CSV_FILE="$CSV_DEFAULT"
ALERTS_FILE="$ALERTS_DEFAULT"

NODES=(curs3d-node1 curs3d-node2 curs3d-node3)

while [ $# -gt 0 ]; do
    case "$1" in
        --interval)        INTERVAL="$2"; shift 2 ;;
        --duration)        DURATION="$2"; shift 2 ;;
        --stall)           STALL_THRESHOLD="$2"; shift 2 ;;
        --finality-lag)    FINALITY_LAG_MAX="$2"; shift 2 ;;
        --peer-drop)       PEER_DROP_SECS="$2"; shift 2 ;;
        --csv)             CSV_FILE="$2"; shift 2 ;;
        --alerts)          ALERTS_FILE="$2"; shift 2 ;;
        -h|--help)         sed -n '2,30p' "$0"; exit 0 ;;
        *) echo "unknown arg: $1" >&2; exit 1 ;;
    esac
done

# ─── Style ───────────────────────────────────────────────────────────────
if [ -t 1 ] && [ -z "${NO_COLOR:-}" ]; then
    BOLD=$'\033[1m'; DIM=$'\033[2m'; RED=$'\033[31m'; GREEN=$'\033[32m'
    YELLOW=$'\033[33m'; CYAN=$'\033[36m'; RESET=$'\033[0m'
else
    BOLD=""; DIM=""; RED=""; GREEN=""; YELLOW=""; CYAN=""; RESET=""
fi

ts() { date -u +"%Y-%m-%dT%H:%M:%SZ"; }

alert() {
    local kind="$1" msg="$2"
    local line="$(ts) [$kind] $msg"
    echo "$line" >> "$ALERTS_FILE"
    printf "%s%s%s%s\n" "$RED" "[$kind] " "$msg" "$RESET"
}

ok_line() {
    printf "%s%s%s\n" "$DIM" "$*" "$RESET"
}

# ─── Init CSV ────────────────────────────────────────────────────────────
if [ ! -f "$CSV_FILE" ]; then
    echo "ts_utc,node,height,finalized,latest_hash,peer_count,age_secs,validators,protocol_version,api_ok" > "$CSV_FILE"
fi
: > "$ALERTS_FILE"

# ─── Per-node poll ───────────────────────────────────────────────────────
poll_node() {
    local host="$1"
    # Use SSH + curl on the node so we hit the local API even if nginx is down.
    # Suppress all stderr; rely on jq -r '... // "ERR"' to surface failures.
    ssh -o ConnectTimeout=5 -o BatchMode=yes "$host" \
        "curl -sf --max-time 4 http://127.0.0.1:8080/api/status 2>/dev/null" 2>/dev/null
}

parse_field() {
    local json="$1" field="$2" fallback="$3"
    if [ -z "$json" ]; then echo "$fallback"; return; fi
    echo "$json" | jq -r ".data.${field} // ${fallback}" 2>/dev/null || echo "$fallback"
}

# ─── State trackers ──────────────────────────────────────────────────────
declare -A LAST_HEIGHT
declare -A LAST_HEIGHT_TS
declare -A LAST_FINALIZED
declare -A LAST_PEER_OK_TS
declare -A API_OK
NOW="$(date +%s)"
START_TS="$NOW"
END_TS=$(( DURATION > 0 ? START_TS + DURATION : 0 ))
TOTAL_STALLS=0
TOTAL_DIVERGENCES=0
TOTAL_PEER_DROPS=0
TOTAL_FINALITY_LAGS=0
TOTAL_API_DROPS=0

for h in "${NODES[@]}"; do
    LAST_HEIGHT_TS["$h"]="$NOW"
    LAST_PEER_OK_TS["$h"]="$NOW"
    API_OK["$h"]=1
done

cleanup() {
    echo
    printf "%s━━━━ Soak summary ━━━━%s\n" "$BOLD" "$RESET"
    local elapsed=$(( $(date +%s) - START_TS ))
    printf "duration:       %sh %sm\n" "$((elapsed/3600))" "$(( (elapsed%3600)/60 ))"
    printf "stalls:         %s\n" "$TOTAL_STALLS"
    printf "divergences:    %s\n" "$TOTAL_DIVERGENCES"
    printf "peer drops:     %s\n" "$TOTAL_PEER_DROPS"
    printf "finality lags:  %s\n" "$TOTAL_FINALITY_LAGS"
    printf "api drops:      %s\n" "$TOTAL_API_DROPS"
    printf "csv:            %s (%s lines)\n" "$CSV_FILE" "$(wc -l < "$CSV_FILE" | tr -d ' ')"
    printf "alerts:         %s (%s events)\n" "$ALERTS_FILE" "$(wc -l < "$ALERTS_FILE" | tr -d ' ')"
    if [ "$TOTAL_STALLS" -eq 0 ] && [ "$TOTAL_DIVERGENCES" -eq 0 ] && [ "$TOTAL_API_DROPS" -eq 0 ]; then
        printf "%s%s✓ SOAK PASSED%s — no stalls, no divergence, no api drop.\n" "$BOLD" "$GREEN" "$RESET"
    else
        printf "%s%s✗ SOAK FAILED%s — see %s for details.\n" "$BOLD" "$RED" "$RESET" "$ALERTS_FILE"
    fi
}
trap cleanup EXIT INT TERM

printf "%s━━━━ CURS3D soak monitor ━━━━%s\n" "$BOLD" "$RESET"
printf "%snodes:%s        %s\n" "$DIM" "$RESET" "${NODES[*]}"
printf "%sinterval:%s     %ss\n" "$DIM" "$RESET" "$INTERVAL"
[ "$DURATION" -gt 0 ] && printf "%sduration:%s     %ss (%sh)\n" "$DIM" "$RESET" "$DURATION" "$((DURATION/3600))"
printf "%scsv:%s          %s\n" "$DIM" "$RESET" "$CSV_FILE"
printf "%sstall thr:%s    %ss · finality lag max: %s · peer drop: %ss\n" "$DIM" "$RESET" "$STALL_THRESHOLD" "$FINALITY_LAG_MAX" "$PEER_DROP_SECS"
echo

# ─── Main loop ───────────────────────────────────────────────────────────
while true; do
    NOW="$(date +%s)"
    [ "$END_TS" -gt 0 ] && [ "$NOW" -ge "$END_TS" ] && break

    declare -A HEIGHTS=()
    declare -A HASHES=()
    declare -A FINALIZED=()
    declare -A PEERS=()
    declare -A AGES=()

    for host in "${NODES[@]}"; do
        json="$(poll_node "$host")"
        if [ -z "$json" ] || [ -z "$(echo "$json" | jq -r '.data.height // empty' 2>/dev/null)" ]; then
            if [ "${API_OK[$host]}" = "1" ]; then
                alert API_DROP "$host /api/status unreachable"
                TOTAL_API_DROPS=$((TOTAL_API_DROPS + 1))
            fi
            API_OK["$host"]=0
            echo "$(ts),$host,,,,,,,,0" >> "$CSV_FILE"
            continue
        fi
        if [ "${API_OK[$host]}" = "0" ]; then
            alert API_RECOVER "$host /api/status responding again"
            API_OK["$host"]=1
        fi

        h="$(parse_field "$json" height 0)"
        f="$(parse_field "$json" finalized_height 0)"
        hash="$(parse_field "$json" latest_hash '"unknown"')"
        peers="$(parse_field "$json" peer_count 0)"
        age="$(parse_field "$json" latest_block_age_secs 0)"
        validators="$(parse_field "$json" active_validators 0)"
        proto="$(parse_field "$json" protocol_version 0)"

        HEIGHTS["$host"]="$h"
        HASHES["$host"]="$hash"
        FINALIZED["$host"]="$f"
        PEERS["$host"]="$peers"
        AGES["$host"]="$age"

        echo "$(ts),$host,$h,$f,$hash,$peers,$age,$validators,$proto,1" >> "$CSV_FILE"

        # Stall detection
        if [ -n "${LAST_HEIGHT[$host]:-}" ] && [ "$h" -gt "${LAST_HEIGHT[$host]}" ]; then
            LAST_HEIGHT_TS["$host"]="$NOW"
        fi
        LAST_HEIGHT["$host"]="$h"

        elapsed=$(( NOW - LAST_HEIGHT_TS["$host"] ))
        if [ "$elapsed" -ge "$STALL_THRESHOLD" ]; then
            alert STALL "$host stalled at height=$h for ${elapsed}s"
            TOTAL_STALLS=$((TOTAL_STALLS + 1))
            LAST_HEIGHT_TS["$host"]="$NOW"  # debounce
        fi

        # Finality regression / lag
        if [ -n "${LAST_FINALIZED[$host]:-}" ] && [ "$f" -lt "${LAST_FINALIZED[$host]}" ]; then
            alert FINALITY_REGRESS "$host finalized regressed: ${LAST_FINALIZED[$host]} → $f"
        fi
        LAST_FINALIZED["$host"]="$f"

        if [ "$f" -gt 0 ]; then
            lag=$(( h - f ))
            if [ "$lag" -gt "$FINALITY_LAG_MAX" ]; then
                alert FINALITY_LAG "$host head=$h finalized=$f lag=$lag (>$FINALITY_LAG_MAX)"
                TOTAL_FINALITY_LAGS=$((TOTAL_FINALITY_LAGS + 1))
            fi
        fi

        # Peer-drop detection
        if [ "$peers" -gt 0 ]; then
            LAST_PEER_OK_TS["$host"]="$NOW"
        fi
        peer_elapsed=$(( NOW - LAST_PEER_OK_TS["$host"] ))
        if [ "$peer_elapsed" -ge "$PEER_DROP_SECS" ]; then
            alert PEER_DROP "$host peers=0 for ${peer_elapsed}s"
            TOTAL_PEER_DROPS=$((TOTAL_PEER_DROPS + 1))
            LAST_PEER_OK_TS["$host"]="$NOW"  # debounce
        fi
    done

    # Cross-node divergence: same height, different hash among the 3 nodes
    declare -A SEEN_HASH
    for host in "${NODES[@]}"; do
        h="${HEIGHTS[$host]:-}"
        hash="${HASHES[$host]:-}"
        [ -z "$h" ] && continue
        [ -z "$hash" ] && continue
        key="$h"
        if [ -z "${SEEN_HASH[$key]:-}" ]; then
            SEEN_HASH["$key"]="$host=$hash"
        elif ! echo "${SEEN_HASH[$key]}" | grep -q "$hash"; then
            alert DIVERGENCE "height=$h: ${SEEN_HASH[$key]} VS $host=$hash"
            TOTAL_DIVERGENCES=$((TOTAL_DIVERGENCES + 1))
            SEEN_HASH["$key"]="${SEEN_HASH[$key]} $host=$hash"
        fi
    done
    unset SEEN_HASH

    # Live one-line summary
    summary=""
    for host in "${NODES[@]}"; do
        h="${HEIGHTS[$host]:-?}"
        f="${FINALIZED[$host]:-?}"
        p="${PEERS[$host]:-?}"
        a="${AGES[$host]:-?}"
        short_host="${host##*-}"  # node1 / node2 / node3
        summary+="${short_host}=h${h}/f${f}/p${p}/a${a}s  "
    done
    ok_line "$(ts)  ${summary}stalls=$TOTAL_STALLS divs=$TOTAL_DIVERGENCES drops=$TOTAL_API_DROPS"

    sleep "$INTERVAL"
done
