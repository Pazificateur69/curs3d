#!/usr/bin/env bash
#
# CURS3D coordinated rolling restart.
#
# Pushes pre-built binaries to all 3 nodes, stops them simultaneously, installs,
# starts them within a tight window so the gossip mesh forms before any node
# starts producing alone (which would create boot forks).
#
# Pre-requisites:
#   - target/aarch64-unknown-linux-gnu/release/curs3d  (for node1, node2)
#   - target/x86_64-unknown-linux-gnu/release/curs3d   (for node3)
#   - SSH config aliases curs3d-node1 / curs3d-node2 / curs3d-node3
#   - sudo NOPASSWD: systemctl restart curs3d (or run via root login)
#
# Usage:
#   ./deploy/scripts/rollout.sh           # safe rolling: no chain wipe
#   ./deploy/scripts/rollout.sh --wipe    # also clears chain DB before restart
#                                         # (use after a hardfork or known divergence)

set -euo pipefail

BOLD=$'\033[1m'; DIM=$'\033[2m'; RED=$'\033[31m'; GREEN=$'\033[32m'
YELLOW=$'\033[33m'; CYAN=$'\033[36m'; RESET=$'\033[0m'

step() { printf "%s▸%s %s\n" "$CYAN" "$RESET" "$*"; }
ok()   { printf "%s✓%s %s\n" "$GREEN" "$RESET" "$*"; }
warn() { printf "%s!%s %s\n" "$YELLOW" "$RESET" "$*"; }
die()  { printf "%s✗%s %s\n" "$RED" "$RESET" "$*" >&2; exit "${2:-1}"; }
info() { printf "%s%s%s\n" "$DIM" "$*" "$RESET"; }

WIPE=0
case "${1:-}" in
    --wipe) WIPE=1 ;;
    -h|--help) sed -n '1,25p' "$0"; exit 0 ;;
    "") ;;
    *) die "unknown argument: $1" ;;
esac

ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
ARM_BIN="$ROOT/target/aarch64-unknown-linux-gnu/release/curs3d"
X86_BIN="$ROOT/target/x86_64-unknown-linux-gnu/release/curs3d"

NODES=(
    "curs3d-node1:$ARM_BIN"
    "curs3d-node2:$ARM_BIN"
    "curs3d-node3:$X86_BIN"
)

step "${BOLD}Pre-flight${RESET}"
[ -x "$ARM_BIN" ] || die "missing ARM binary: $ARM_BIN — run cross build first"
[ -x "$X86_BIN" ] || die "missing x86 binary: $X86_BIN — run cross build first"
ok "ARM binary: $(stat -f%z "$ARM_BIN" 2>/dev/null || stat -c%s "$ARM_BIN") bytes"
ok "x86 binary: $(stat -f%z "$X86_BIN" 2>/dev/null || stat -c%s "$X86_BIN") bytes"

# ─── 1. Push binaries (parallel) ─────────────────────────────────────────
step "${BOLD}Pushing binaries to all nodes (parallel)${RESET}"
PIDS=()
for entry in "${NODES[@]}"; do
    HOST="${entry%%:*}"
    BIN="${entry#*:}"
    (
        scp -q "$BIN" "$HOST:/tmp/curs3d.new" \
            && ssh "$HOST" "chmod +x /tmp/curs3d.new"
        echo "  pushed → $HOST"
    ) &
    PIDS+=($!)
done
for pid in "${PIDS[@]}"; do wait "$pid"; done
ok "All binaries pushed."

# ─── 2. Coordinated stop ─────────────────────────────────────────────────
step "${BOLD}Stopping all nodes simultaneously${RESET}"
PIDS=()
for entry in "${NODES[@]}"; do
    HOST="${entry%%:*}"
    (ssh "$HOST" "sudo systemctl stop curs3d" && echo "  stopped: $HOST") &
    PIDS+=($!)
done
for pid in "${PIDS[@]}"; do wait "$pid"; done
ok "All 3 services stopped."

# ─── 3. Install binary + optionally wipe chain ───────────────────────────
step "${BOLD}Installing new binary${RESET}"
PIDS=()
for entry in "${NODES[@]}"; do
    HOST="${entry%%:*}"
    if [ "$WIPE" -eq 1 ]; then
        SCRIPT="sudo install -m 755 /tmp/curs3d.new /usr/local/bin/curs3d \
            && sudo rm -rf /var/lib/curs3d/blocks /var/lib/curs3d/state /var/lib/curs3d/accounts /var/lib/curs3d/*.sled \
            && sudo chown -R curs3d:curs3d /var/lib/curs3d"
    else
        SCRIPT="sudo install -m 755 /tmp/curs3d.new /usr/local/bin/curs3d"
    fi
    (ssh "$HOST" "$SCRIPT" && echo "  installed: $HOST") &
    PIDS+=($!)
done
for pid in "${PIDS[@]}"; do wait "$pid"; done
ok "All binaries installed${WIPE:+ + chain DB wiped}."

# ─── 4. Coordinated start ────────────────────────────────────────────────
step "${BOLD}Starting all nodes within a tight window${RESET}"
PIDS=()
for entry in "${NODES[@]}"; do
    HOST="${entry%%:*}"
    (ssh "$HOST" "sudo systemctl start curs3d" && echo "  started: $HOST") &
    PIDS+=($!)
done
for pid in "${PIDS[@]}"; do wait "$pid"; done
ok "All 3 services started."

# ─── 5. Healthcheck loop ─────────────────────────────────────────────────
step "${BOLD}Waiting for chain to start producing${RESET}"
RPC="https://rpc.curs3d.fr/eth"
for i in $(seq 1 30); do
    sleep 10
    H_HEX="$(curl -sf --max-time 5 -X POST "$RPC" -H 'Content-Type: application/json' \
        -d '{"jsonrpc":"2.0","method":"eth_blockNumber","params":[],"id":1}' \
        | jq -r '.result // "0x0"' 2>/dev/null || echo "0x0")"
    H_DEC="$(printf '%d\n' "$H_HEX" 2>/dev/null || echo 0)"
    info "  [+$((i*10))s] height=$H_DEC"
    if [ "$H_DEC" -ge 3 ]; then
        ok "Chain producing (height=$H_DEC)."
        break
    fi
done

if [ "$H_DEC" -lt 3 ]; then
    warn "Chain still at height $H_DEC after 5 minutes."
    warn "Check: ssh curs3d-node1 'sudo journalctl -u curs3d -n 50 --no-pager'"
    exit 2
fi

# ─── 6. Final summary ────────────────────────────────────────────────────
echo
printf "%s%s━━━━ Rollout complete ━━━━%s\n" "$BOLD" "$GREEN" "$RESET"
curl -sf --max-time 5 https://api.curs3d.fr/api/status | jq -r \
    '.data | "  height=\(.height)  finalized=\(.finalized_height)  validators=\(.active_validators)  peers=\(.peer_count)  age=\(.latest_block_age_secs)s"'
echo
ok "Now run: cd contracts && ./deploy.sh --force"
