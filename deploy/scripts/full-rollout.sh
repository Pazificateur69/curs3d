#!/usr/bin/env bash
#
# CURS3D — full coordinated rollout (binary + chain wipe + restart + verify).
#
# Use this script ONLY for situations that require all 3 nodes to come up
# together: storage format migrations (sled → redb, redb v1 → v2), hardforks
# (consensus/genesis change), or the very first cluster bootstrap. For
# routine binary-only updates that share the on-disk format, prefer
# `deploy/scripts/rollout-staggered.sh` — it keeps 2/3 of the validator
# cluster live during the upgrade so consensus/finality keep progressing.
#
# Pre-requisite: each node has a freshly built /home/ubuntu/curs3d/target/release/curs3d
# binary (run via SSH cargo build before invoking this script).
#
# What this does:
#   1. Coordinated stop on all 3 nodes
#   2. Install new binary + wipe chain DB on all 3
#   3. Coordinated start
#   4. Wait for chain to start producing blocks
#   5. Print final status
#
# After this finishes, fund the EVM deployer (one-time per chain wipe), then run:
#   cd contracts && ./deploy.sh --force
#
# Funding (run from your Mac, not on a node):
#   ssh curs3d-node1 "sudo -u curs3d /usr/local/bin/curs3d send \
#     --wallet /etc/curs3d/faucet.json \
#     --password-file /etc/curs3d/faucet.password \
#     --to <YOUR_EVM_DEPLOYER_HEX_NO_0x> \
#     --amount 5000000000 --fee 1000 --rpc-addr 127.0.0.1:9545"

set -euo pipefail

BOLD=$'\033[1m'; DIM=$'\033[2m'; RED=$'\033[31m'; GREEN=$'\033[32m'
YELLOW=$'\033[33m'; CYAN=$'\033[36m'; RESET=$'\033[0m'

step() { printf "%s▸%s %s\n" "$CYAN" "$RESET" "$*"; }
ok()   { printf "%s✓%s %s\n" "$GREEN" "$RESET" "$*"; }
warn() { printf "%s!%s %s\n" "$YELLOW" "$RESET" "$*"; }
die()  { printf "%s✗%s %s\n" "$RED" "$RESET" "$*" >&2; exit "${2:-1}"; }
info() { printf "%s%s%s\n" "$DIM" "$*" "$RESET"; }

NODES=(curs3d-node1 curs3d-node2 curs3d-node3)
SRC_BIN="/home/ubuntu/curs3d/target/release/curs3d"
DEST_BIN="/usr/local/bin/curs3d"
RPC_URL="${CURS3D_RPC_URL:-https://rpc.curs3d.fr/eth}"
CONVERGENCE_WAIT_SECS="${CONVERGENCE_WAIT_SECS:-360}"
FINALITY_WAIT_SECS="${FINALITY_WAIT_SECS:-420}"

WIPE=1
case "${1:-}" in
    --no-wipe) WIPE=0 ;;
    "") ;;
    -h|--help) sed -n '2,25p' "$0"; exit 0 ;;
    *) die "unknown arg: $1" ;;
esac

node_status_json() {
    ssh "$1" "curl -sf --max-time 3 http://127.0.0.1:8080/api/status" 2>/dev/null || true
}

wait_for_cluster_convergence() {
    local min_height="$1"
    local wait_secs="$2"
    local deadline=$((SECONDS + wait_secs))
    local last_report=""

    while [ "$SECONDS" -lt "$deadline" ]; do
        sleep 10

        local ok_count=0
        local expected_hash=""
        local expected_height=""
        local min_peers=999
        local min_validators=999
        local report=""

        for HOST in "${NODES[@]}"; do
            local json height hash peers validators
            json="$(node_status_json "$HOST")"
            height="$(printf '%s' "$json" | jq -r '.data.height // empty' 2>/dev/null || true)"
            hash="$(printf '%s' "$json" | jq -r '.data.latest_hash // empty' 2>/dev/null || true)"
            peers="$(printf '%s' "$json" | jq -r '.data.peer_count // 0' 2>/dev/null || echo 0)"
            validators="$(printf '%s' "$json" | jq -r '.data.active_validators // 0' 2>/dev/null || echo 0)"

            if [ -n "$height" ] && [ -n "$hash" ]; then
                ok_count=$((ok_count + 1))
                [ "$peers" -lt "$min_peers" ] && min_peers="$peers"
                [ "$validators" -lt "$min_validators" ] && min_validators="$validators"
                report="${report}  $HOST: h=$height hash=${hash:0:16} peers=$peers validators=$validators"$'\n'

                if [ -z "$expected_hash" ]; then
                    expected_hash="$hash"
                    expected_height="$height"
                elif [ "$hash" != "$expected_hash" ] || [ "$height" != "$expected_height" ]; then
                    expected_hash="__mismatch__"
                fi
            else
                report="${report}  $HOST: no local status"$'\n'
            fi
        done

        last_report="$report"
        info "  cluster check: ok=$ok_count/3 height=${expected_height:-?} peers_min=$min_peers"

        if [ "$ok_count" -eq 3 ] \
            && [ "$expected_hash" != "__mismatch__" ] \
            && [ -n "$expected_height" ] \
            && [ "$expected_height" -ge "$min_height" ] \
            && [ "$min_peers" -ge 2 ] \
            && [ "$min_validators" -ge 3 ]; then
            printf "%s" "$report"
            ok "All 3 nodes converged at height=$expected_height hash=${expected_hash:0:16}."
            return 0
        fi
    done

    warn "Cluster did not converge within ${wait_secs}s. Last observed state:"
    printf "%s" "$last_report"
    return 1
}

wait_for_cluster_finality() {
    local wait_secs="$1"
    local deadline=$((SECONDS + wait_secs))
    local last_report=""

    while [ "$SECONDS" -lt "$deadline" ]; do
        sleep 10

        local ok_count=0
        local min_finalized=999999999999
        local min_height=999999999999
        local report=""

        for HOST in "${NODES[@]}"; do
            local json height finalized hash
            json="$(node_status_json "$HOST")"
            height="$(printf '%s' "$json" | jq -r '.data.height // empty' 2>/dev/null || true)"
            finalized="$(printf '%s' "$json" | jq -r '.data.finalized_height // empty' 2>/dev/null || true)"
            hash="$(printf '%s' "$json" | jq -r '.data.latest_hash // empty' 2>/dev/null || true)"

            if [ -n "$height" ] && [ -n "$finalized" ] && [ -n "$hash" ]; then
                ok_count=$((ok_count + 1))
                [ "$height" -lt "$min_height" ] && min_height="$height"
                [ "$finalized" -lt "$min_finalized" ] && min_finalized="$finalized"
                report="${report}  $HOST: h=$height finalized=$finalized hash=${hash:0:16}"$'\n'
            else
                report="${report}  $HOST: no local status"$'\n'
            fi
        done

        last_report="$report"
        info "  finality check: ok=$ok_count/3 min_height=$min_height min_finalized=$min_finalized"

        if [ "$ok_count" -eq 3 ] && [ "$min_height" -ge 33 ] && [ "$min_finalized" -gt 0 ]; then
            printf "%s" "$report"
            ok "Finality is active on all 3 nodes."
            return 0
        fi
    done

    warn "Finality did not activate within ${wait_secs}s. Last observed state:"
    printf "%s" "$last_report"
    return 1
}

# ─── 1. Coordinated stop ─────────────────────────────────────────────────
step "${BOLD}Stopping all nodes simultaneously${RESET}"
PIDS=()
for HOST in "${NODES[@]}"; do
    (ssh "$HOST" "sudo systemctl stop curs3d" >/dev/null && echo "  stopped: $HOST") &
    PIDS+=($!)
done
for pid in "${PIDS[@]}"; do wait "$pid"; done
ok "All 3 services stopped."

# ─── 2. Install + optional wipe ──────────────────────────────────────────
if [ "$WIPE" -eq 1 ]; then
    step "${BOLD}Installing new binary + wiping chain DB${RESET}"
    # redb stores the canonical chain in curs3d.redb, but old sled directories
    # may still exist on nodes upgraded from pre-redb builds. Preserve
    # p2p_identity* so the node keeps its peer ID across restarts (node2/node3
    # already configure node1's PeerId as bootnode — regenerating it would
    # break that config until operators update unit files).
    SCRIPT="sudo install -m 755 $SRC_BIN $DEST_BIN \
        && sudo find /var/lib/curs3d -mindepth 1 -maxdepth 1 -not -name 'p2p_identity*' -exec rm -rf {} + \
        && sudo chown -R curs3d:curs3d /var/lib/curs3d 2>/dev/null || true"
else
    step "${BOLD}Installing new binary (no wipe)${RESET}"
    SCRIPT="sudo install -m 755 $SRC_BIN $DEST_BIN"
fi
PIDS=()
for HOST in "${NODES[@]}"; do
    (ssh "$HOST" "$SCRIPT" >/dev/null 2>&1 && echo "  installed: $HOST") &
    PIDS+=($!)
done
for pid in "${PIDS[@]}"; do wait "$pid"; done
if [ "$WIPE" -eq 1 ]; then
    ok "All binaries installed + chain DBs wiped."
else
    ok "All binaries installed (no wipe)."
fi

# ─── 3. Coordinated start ────────────────────────────────────────────────
step "${BOLD}Starting all nodes simultaneously${RESET}"
PIDS=()
for HOST in "${NODES[@]}"; do
    (ssh "$HOST" "sudo systemctl start curs3d" >/dev/null && echo "  started: $HOST") &
    PIDS+=($!)
done
for pid in "${PIDS[@]}"; do wait "$pid"; done
ok "All 3 services started."

# ─── 4. Wait for chain to advance ────────────────────────────────────────
step "${BOLD}Verifying 3-node convergence${RESET}"
wait_for_cluster_convergence 4 "$CONVERGENCE_WAIT_SECS" \
    || die "Cluster did not converge after coordinated restart; do not deploy contracts yet." 3

step "${BOLD}Verifying public RPC${RESET}"
H_DEC=0
for i in $(seq 1 18); do
    sleep 10
    H_HEX="$(curl -sf --max-time 5 -X POST "$RPC_URL" -H 'Content-Type: application/json' \
        -d '{"jsonrpc":"2.0","method":"eth_blockNumber","params":[],"id":1}' \
        2>/dev/null | jq -r '.result // "0x0"' 2>/dev/null || echo "0x0")"
    H_DEC="$(printf '%d\n' "$H_HEX" 2>/dev/null || echo 0)"
    info "  [+$((i*10))s] public_rpc_height=$H_DEC"
    if [ "$H_DEC" -ge 4 ]; then
        ok "Public RPC is serving the new chain (height=$H_DEC)."
        break
    fi
done

if [ "$H_DEC" -lt 4 ]; then
    warn "Public RPC still below h=4 after 3 minutes, while local nodes converged."
    warn "Inspect nginx/node1 before deploying contracts from a public RPC URL."
    exit 2
fi

step "${BOLD}Verifying finality startup${RESET}"
wait_for_cluster_finality "$FINALITY_WAIT_SECS" \
    || die "Finality did not activate cleanly; inspect validator logs before continuing." 4

# ─── 5. Final status ─────────────────────────────────────────────────────
echo
printf "%s%s━━━━ Rollout complete ━━━━%s\n" "$BOLD" "$GREEN" "$RESET"
curl -sf --max-time 5 https://api.curs3d.fr/api/status 2>/dev/null \
    | jq -r '.data | "  height=\(.height)  finalized=\(.finalized_height)  validators=\(.active_validators)  peers=\(.peer_count // "?")"' 2>/dev/null \
    || echo "  (could not fetch status)"
echo
ok "Next: fund the EVM deployer, then 'cd contracts && ./deploy.sh --force'"
