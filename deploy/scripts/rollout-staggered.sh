#!/usr/bin/env bash
#
# CURS3D — staggered rolling restart (zero-downtime upgrade for binary-only
# changes that share the on-disk data format).
#
# Invariant: at all times at least 2 of 3 validators are running, so the BFT
# 2/3-stake quorum holds and finality keeps making progress while one node
# is being restarted. The mutual-bootnode mesh in deploy/systemd/curs3d-node*.service
# means the down node simply rejoins via gossipsub when it comes back.
#
# When NOT to use this script:
#   - Storage format changes (sled <-> redb). Use full-rollout.sh --wipe — every
#     node must restart with a fresh chain DB, so a coordinated cold reboot
#     is required.
#   - Hardforks that change the consensus protocol or genesis hash (v3 -> v4,
#     v4 -> v5). Mixed-version peers diverge silently — coordinate a wipe.
#   - Genesis regeneration. Same as above.
#
# When TO use this script:
#   - Routine code change (bug fix, perf improvement, new endpoint).
#   - Cargo dep bump that does not change the on-disk schema.
#   - Adding instrumentation/logging.
#   - Anything that keeps storage::CURRENT_SCHEMA_VERSION the same and is
#     gossipsub-topic-compatible (protocol_version_at_height unchanged).
#
# Pre-requisites:
#   - target/aarch64-unknown-linux-gnu/release/curs3d  (node1, node2 — Oracle ARM)
#   - target/x86_64-unknown-linux-gnu/release/curs3d   (node3 — IONOS x86)
#   - SSH config aliases curs3d-node1 / curs3d-node2 / curs3d-node3 (see DEPLOY_RUNBOOK.md)
#   - sudo NOPASSWD for `systemctl restart curs3d` on each node (already configured)
#
# Usage:
#   ./deploy/scripts/rollout-staggered.sh                  # default: node3 → node2 → node1, 600s observe
#   OBSERVE_SECS=300 ./deploy/scripts/rollout-staggered.sh # shorter observation
#   ORDER='curs3d-node3 curs3d-node2 curs3d-node1' ./deploy/scripts/rollout-staggered.sh

set -euo pipefail

BOLD=$'\033[1m'; DIM=$'\033[2m'; RED=$'\033[31m'; GREEN=$'\033[32m'
YELLOW=$'\033[33m'; CYAN=$'\033[36m'; RESET=$'\033[0m'

step() { printf "%s▸%s %s\n" "$CYAN" "$RESET" "$*"; }
ok()   { printf "%s✓%s %s\n" "$GREEN" "$RESET" "$*"; }
warn() { printf "%s!%s %s\n" "$YELLOW" "$RESET" "$*"; }
die()  { printf "%s✗%s %s\n" "$RED" "$RESET" "$*" >&2; exit "${2:-1}"; }
info() { printf "%s%s%s\n" "$DIM" "$*" "$RESET"; }

case "${1:-}" in
    -h|--help) sed -n '1,38p' "$0"; exit 0 ;;
    "") ;;
    *) die "unknown argument: $1 (try --help)" ;;
esac

ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
ARM_BIN="$ROOT/target/aarch64-unknown-linux-gnu/release/curs3d"
X86_BIN="$ROOT/target/x86_64-unknown-linux-gnu/release/curs3d"

# Ordered: x86 first (smallest blast radius if it fails — IONOS is the newest
# validator, easier to recover), then ARM peers, finally bootstrap node1 last
# (its API/site/captcha are user-visible so we want it dark for the shortest
# possible time).
ORDER="${ORDER:-curs3d-node3 curs3d-node2 curs3d-node1}"
OBSERVE_SECS="${OBSERVE_SECS:-600}"
RPC="${CURS3D_RPC_URL:-https://rpc.curs3d.fr/eth}"
STATUS_URL="${CURS3D_STATUS_URL:-https://api.curs3d.fr/api/status}"

declare -A BIN_OF=(
    [curs3d-node1]="$ARM_BIN"
    [curs3d-node2]="$ARM_BIN"
    [curs3d-node3]="$X86_BIN"
)

# ─── Pre-flight ──────────────────────────────────────────────────────────
step "${BOLD}Pre-flight${RESET}"
[ -x "$ARM_BIN" ] || die "missing ARM binary: $ARM_BIN — run cross build first (see DEPLOY_RUNBOOK.md \"Cross-compile depuis Mac\")"
[ -x "$X86_BIN" ] || die "missing x86 binary: $X86_BIN — run cross build first (see DEPLOY_RUNBOOK.md \"Cross-compile depuis Mac\")"
ok "ARM binary: $(stat -f%z "$ARM_BIN" 2>/dev/null || stat -c%s "$ARM_BIN") bytes"
ok "x86 binary: $(stat -f%z "$X86_BIN" 2>/dev/null || stat -c%s "$X86_BIN") bytes"

# Snapshot pre-rollout chain state so we can prove progress per-node.
START_HEIGHT_HEX="$(curl -sf --max-time 5 -X POST "$RPC" -H 'Content-Type: application/json' \
    -d '{"jsonrpc":"2.0","method":"eth_blockNumber","params":[],"id":1}' \
    | jq -r '.result // "0x0"' 2>/dev/null || echo "0x0")"
START_HEIGHT="$(printf '%d\n' "$START_HEIGHT_HEX" 2>/dev/null || echo 0)"
ok "Starting height: $START_HEIGHT"
[ "$START_HEIGHT" -gt 0 ] || die "chain head is 0 — refusing to staggered-rollout into a non-producing chain. Use full-rollout.sh first."

# ─── Push binaries (parallel — does not affect chain state) ──────────────
step "${BOLD}Pushing binaries to all 3 nodes (parallel)${RESET}"
PIDS=()
for HOST in $ORDER; do
    BIN="${BIN_OF[$HOST]}"
    (
        scp -q "$BIN" "$HOST:/tmp/curs3d.new" \
            && ssh "$HOST" "chmod +x /tmp/curs3d.new" \
            && echo "  pushed → $HOST"
    ) &
    PIDS+=($!)
done
for pid in "${PIDS[@]}"; do wait "$pid"; done
ok "All binaries staged at /tmp/curs3d.new on each node."

# ─── Sequential restart, one node at a time ──────────────────────────────
NODE_IDX=0
TOTAL=$(echo "$ORDER" | wc -w | tr -d ' ')
for HOST in $ORDER; do
    NODE_IDX=$((NODE_IDX + 1))
    echo
    step "${BOLD}[$NODE_IDX/$TOTAL] Restarting $HOST${RESET}"

    BEFORE_HEIGHT_HEX="$(curl -sf --max-time 5 -X POST "$RPC" -H 'Content-Type: application/json' \
        -d '{"jsonrpc":"2.0","method":"eth_blockNumber","params":[],"id":1}' \
        | jq -r '.result // "0x0"' 2>/dev/null || echo "0x0")"
    BEFORE_HEIGHT="$(printf '%d\n' "$BEFORE_HEIGHT_HEX" 2>/dev/null || echo 0)"
    info "  height before: $BEFORE_HEIGHT"

    ssh "$HOST" "sudo install -m 755 /tmp/curs3d.new /usr/local/bin/curs3d && sudo systemctl restart curs3d"
    ok "  restart issued — service back up"

    # Health gate before moving to next node:
    #   1. The chain (read via the public RPC, served from node1) must keep
    #      advancing — proves the OTHER two nodes still produce.
    #   2. The restarted node's local /api/status must answer 200, height>0,
    #      peer_count>=2 — proves it rejoined the mesh and started syncing.
    step "  Observing $OBSERVE_SECS s (chain progress + peer rejoin)"
    DEADLINE=$((SECONDS + OBSERVE_SECS))
    LAST_TICK=0
    LAST_LOCAL_HEIGHT=0
    LAST_PEER_COUNT=0
    while [ "$SECONDS" -lt "$DEADLINE" ]; do
        sleep 15
        TICK=$((SECONDS - LAST_TICK))

        H_HEX="$(curl -sf --max-time 5 -X POST "$RPC" -H 'Content-Type: application/json' \
            -d '{"jsonrpc":"2.0","method":"eth_blockNumber","params":[],"id":1}' \
            | jq -r '.result // "0x0"' 2>/dev/null || echo "0x0")"
        CHAIN_HEIGHT="$(printf '%d\n' "$H_HEX" 2>/dev/null || echo 0)"

        LOCAL_JSON="$(ssh "$HOST" "curl -sf --max-time 3 http://127.0.0.1:8080/api/status" 2>/dev/null || echo '{}')"
        LOCAL_HEIGHT="$(echo "$LOCAL_JSON" | jq -r '.data.height // 0' 2>/dev/null || echo 0)"
        PEER_COUNT="$(echo "$LOCAL_JSON" | jq -r '.data.peer_count // 0' 2>/dev/null || echo 0)"

        info "  [+$((SECONDS - DEADLINE + OBSERVE_SECS))s] chain=$CHAIN_HEIGHT  $HOST: height=$LOCAL_HEIGHT peers=$PEER_COUNT"
        LAST_LOCAL_HEIGHT="$LOCAL_HEIGHT"
        LAST_PEER_COUNT="$PEER_COUNT"

        # Quick-pass shortcut: once the restarted node has caught up to the
        # chain head AND has 2 peers, we don't need to wait the full window.
        if [ "$LOCAL_HEIGHT" -ge "$CHAIN_HEIGHT" ] && [ "$PEER_COUNT" -ge 2 ] && [ "$CHAIN_HEIGHT" -gt "$BEFORE_HEIGHT" ]; then
            ok "  $HOST caught up (height=$LOCAL_HEIGHT, peers=$PEER_COUNT) — moving on early"
            break
        fi
    done

    # Final gate at end of observation window.
    if [ "$LAST_LOCAL_HEIGHT" -lt 1 ]; then
        warn "  $HOST never returned a /api/status height > 0 within ${OBSERVE_SECS}s"
        warn "  Inspect:  ssh $HOST 'sudo journalctl -u curs3d -n 80 --no-pager'"
        die "  ABORTING staggered rollout — fix $HOST before continuing the next nodes" 3
    fi
    if [ "$LAST_PEER_COUNT" -lt 2 ]; then
        warn "  $HOST has only $LAST_PEER_COUNT peer(s) — mesh may be partitioning"
        warn "  Inspect:  ssh $HOST 'sudo journalctl -u curs3d -n 80 --no-pager'"
        die "  ABORTING staggered rollout — fix $HOST before continuing the next nodes" 3
    fi

    NEW_CHAIN_HEIGHT_HEX="$(curl -sf --max-time 5 -X POST "$RPC" -H 'Content-Type: application/json' \
        -d '{"jsonrpc":"2.0","method":"eth_blockNumber","params":[],"id":1}' \
        | jq -r '.result // "0x0"' 2>/dev/null || echo "0x0")"
    NEW_CHAIN_HEIGHT="$(printf '%d\n' "$NEW_CHAIN_HEIGHT_HEX" 2>/dev/null || echo 0)"
    if [ "$NEW_CHAIN_HEIGHT" -le "$BEFORE_HEIGHT" ]; then
        warn "  Chain did not advance during $HOST observation window ($BEFORE_HEIGHT → $NEW_CHAIN_HEIGHT)"
        warn "  This means the OTHER two nodes have stalled — DO NOT continue the rollout"
        die "  ABORTING — investigate the still-running peers" 4
    fi
    ok "  $HOST OK: local height=$LAST_LOCAL_HEIGHT  peers=$LAST_PEER_COUNT  chain advanced $BEFORE_HEIGHT → $NEW_CHAIN_HEIGHT"
done

# ─── Final summary ───────────────────────────────────────────────────────
echo
printf "%s%s━━━━ Staggered rollout complete ━━━━%s\n" "$BOLD" "$GREEN" "$RESET"
curl -sf --max-time 5 "$STATUS_URL" 2>/dev/null \
    | jq -r '.data | "  height=\(.height)  finalized=\(.finalized_height)  validators=\(.active_validators)  peers=\(.peer_count // "?")  age=\(.latest_block_age_secs // "?")s"' \
    || echo "  (could not fetch status)"
echo
ok "All 3 validators upgraded with no chain downtime. Continue watching ${STATUS_URL} for the next 1h."
