#!/usr/bin/env bash
#
# CURS3D — full coordinated rollout (binary + chain wipe + restart + verify).
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

WIPE=1
case "${1:-}" in
    --no-wipe) WIPE=0 ;;
    "") ;;
    -h|--help) sed -n '2,25p' "$0"; exit 0 ;;
    *) die "unknown arg: $1" ;;
esac

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
step "${BOLD}Waiting for chain to produce blocks${RESET}"
H_DEC=0
for i in $(seq 1 36); do
    sleep 10
    H_HEX="$(curl -sf --max-time 5 -X POST "$RPC_URL" -H 'Content-Type: application/json' \
        -d '{"jsonrpc":"2.0","method":"eth_blockNumber","params":[],"id":1}' \
        2>/dev/null | jq -r '.result // "0x0"' 2>/dev/null || echo "0x0")"
    H_DEC="$(printf '%d\n' "$H_HEX" 2>/dev/null || echo 0)"
    info "  [+$((i*10))s] height=$H_DEC"
    if [ "$H_DEC" -ge 3 ]; then
        ok "Chain producing (height=$H_DEC)."
        break
    fi
done

if [ "$H_DEC" -lt 3 ]; then
    warn "Chain still at height $H_DEC after 6 minutes."
    warn "Inspect: ssh curs3d-node1 'sudo journalctl -u curs3d -n 50 --no-pager'"
    exit 2
fi

# ─── 5. Final status ─────────────────────────────────────────────────────
echo
printf "%s%s━━━━ Rollout complete ━━━━%s\n" "$BOLD" "$GREEN" "$RESET"
curl -sf --max-time 5 https://api.curs3d.fr/api/status 2>/dev/null \
    | jq -r '.data | "  height=\(.height)  finalized=\(.finalized_height)  validators=\(.active_validators)  peers=\(.peer_count // "?")"' 2>/dev/null \
    || echo "  (could not fetch status)"
echo
ok "Next: fund the EVM deployer, then 'cd contracts && ./deploy.sh --force'"
