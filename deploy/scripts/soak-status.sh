#!/usr/bin/env bash
#
# CURS3D one-shot soak status — quick spot check of all 3 validators.
# Use during a soak run to eyeball health without waiting for the next
# soak-monitor.sh poll cycle.
#
# Usage:
#   ./soak-status.sh                # human-readable table
#   ./soak-status.sh --json         # machine-readable
#
# Compatible with macOS bash 3.2 (no associative arrays).

set -euo pipefail

NODES="curs3d-node1 curs3d-node2 curs3d-node3"
JSON=0
[ "${1:-}" = "--json" ] && JSON=1

if [ -t 1 ] && [ "$JSON" -eq 0 ]; then
    BOLD=$'\033[1m'; DIM=$'\033[2m'; GREEN=$'\033[32m'; RED=$'\033[31m'
    YELLOW=$'\033[33m'; RESET=$'\033[0m'
else
    BOLD=""; DIM=""; GREEN=""; RED=""; YELLOW=""; RESET=""
fi

# Collect data into a temp file: lines of "host height finalized hash peers age ok"
TMP="$(mktemp)"
trap 'rm -f "$TMP"' EXIT

for h in $NODES; do
    json="$(ssh -o ConnectTimeout=5 -o BatchMode=yes "$h" \
        "curl -sf --max-time 4 http://127.0.0.1:8080/api/status 2>/dev/null" 2>/dev/null || true)"
    if [ -z "$json" ]; then
        echo "$h 0 0 ? 0 0 0" >> "$TMP"
        continue
    fi
    height="$(echo "$json" | jq -r '.data.height // 0')"
    final="$(echo "$json" | jq -r '.data.finalized_height // 0')"
    hash="$(echo "$json" | jq -r '.data.latest_hash // "?"')"
    peers="$(echo "$json" | jq -r '.data.peer_count // 0')"
    age="$(echo "$json" | jq -r '.data.latest_block_age_secs // 0')"
    echo "$h $height $final $hash $peers $age 1" >> "$TMP"
done

if [ "$JSON" -eq 1 ]; then
    printf '{\n'
    first=1
    while read -r host height final hash peers age ok; do
        [ "$first" -eq 0 ] && printf ',\n'
        first=0
        if [ "$ok" = "0" ]; then
            printf '  "%s": null' "$host"
        else
            printf '  "%s": {"height": %s, "finalized": %s, "latest_hash": "%s", "peers": %s, "age_secs": %s}' \
                "$host" "$height" "$final" "$hash" "$peers" "$age"
        fi
    done < "$TMP"
    printf '\n}\n'
    exit 0
fi

# Human-readable table
printf "%s━━━━ CURS3D soak status ━━━━%s\n" "$BOLD" "$RESET"
printf "%-15s %8s %8s %8s %6s %7s  %s\n" "NODE" "HEIGHT" "FINAL" "LAG" "PEERS" "AGE" "HASH"
printf "%s%s%s\n" "$DIM" "$(printf -- '-%.0s' {1..78})" "$RESET"

# Aggregate stats
MAX_AGE=0
MIN_PEERS=99
MIN_FINAL=999999999
DIVERGENCE=0
PREV_HEIGHT_HASH=""  # "height:hash" for divergence check

while read -r host height final hash peers age ok; do
    short="${host##*-}"
    if [ "$ok" = "0" ]; then
        printf "%s%-15s  api unreachable%s\n" "$RED" "$short" "$RESET"
        continue
    fi
    lag=$((height - final))
    short_hash="${hash:0:16}"

    color=""
    [ "$age" -gt 30 ] && color="$YELLOW"
    [ "$age" -gt 60 ] && color="$RED"
    [ "$peers" -eq 0 ] && color="$RED"

    printf "%s%-15s %8s %8s %8s %6s %6ss%s  %s\n" \
        "$color" "$short" "$height" "$final" "$lag" "$peers" "$age" "$RESET" "$short_hash"

    [ "$age" -gt "$MAX_AGE" ] && MAX_AGE="$age"
    [ "$peers" -lt "$MIN_PEERS" ] && MIN_PEERS="$peers"
    [ "$final" -lt "$MIN_FINAL" ] && MIN_FINAL="$final"

    # Divergence: check if any other node has same height but different hash
    while read -r oh oheight ofinal ohash _ _ ook; do
        [ "$oh" = "$host" ] && continue
        [ "$ook" = "0" ] && continue
        [ "$oheight" = "$height" ] && [ "$ohash" != "$hash" ] && DIVERGENCE=1
    done < "$TMP"
done < "$TMP"

echo
if [ "$DIVERGENCE" -eq 1 ]; then
    printf "%s%s✗ DIVERGENCE — nodes at the same height report different hashes.%s\n" "$BOLD" "$RED" "$RESET"
elif [ "$MIN_PEERS" -eq 0 ]; then
    printf "%s%s! ISOLATION — at least one node has 0 peers.%s\n" "$BOLD" "$RED" "$RESET"
elif [ "$MAX_AGE" -gt 60 ]; then
    printf "%s%s! STALL — block age exceeds 60s on at least one node.%s\n" "$BOLD" "$YELLOW" "$RESET"
elif [ "$MIN_FINAL" -eq 0 ]; then
    printf "%s%s! NO FINALITY — at least one node has finalized=0.%s\n" "$BOLD" "$YELLOW" "$RESET"
else
    printf "%s%s✓ HEALTHY — all 3 nodes converged, finalizing, latest block <30s old.%s\n" "$BOLD" "$GREEN" "$RESET"
fi
