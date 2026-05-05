#!/usr/bin/env bash
#
# CURS3D contract portfolio — one-shot deploy.
#
# Usage:
#   ./deploy.sh                                 # uses ~/.curs3d/deployer.keystore at https://rpc.curs3d.fr/eth
#   ./deploy.sh --force                         # overwrites deployments/<chainId>.json
#   ./deploy.sh --rpc-url http://127.0.0.1:8080/eth
#   ./deploy.sh --keystore /path/to/keystore.json
#
# Environment:
#   CURS3D_KEYSTORE_PASSWORD   Password for the keystore (will prompt if unset)
#   CURS3D_DEPLOY_MIN_BALANCE  Required balance in wei (default 5e21 = 5000 CUR)
#   ARBITRATOR                 Optional 0x... arbitrator address (defaults to deployer)
#
# Flow:
#   1. Pre-flight: check forge / cast / jq, ping RPC, verify chain advancing
#   2. Keystore:   reuse or create ~/.curs3d/deployer.keystore
#   3. Balance:    check deployer has at least CURS3D_DEPLOY_MIN_BALANCE wei
#   4. Deploy:     forge script DeployPortfolio with --keystore + --sender
#   5. Verify:     eth_getCode on all 7 addresses + token.minters() on faucet & staking
#   6. Sync:       copy deployments/<chainId>.json into dapp/deployments.json
#   7. Summary:    print 7 addresses + explorer links
#
# Exit codes:
#   0 OK · 1 user error · 2 chain not healthy · 3 deploy failed · 4 verify failed

set -euo pipefail

# ─── Defaults ────────────────────────────────────────────────────────────
RPC_URL_DEFAULT="https://rpc.curs3d.fr/eth"
KEYSTORE_DEFAULT="$HOME/.curs3d/deployer.keystore"
EXPLORER_DEFAULT="https://explorer.curs3d.fr"
# CURS3D's EVM uses 1 wei == 1 microtoken (not the standard 1e18). Native CUR
# is microtokens internally, and eth_getBalance returns microtokens directly.
# 1_000_000_000 wei == 1_000 CUR, plenty for 7 contract deploys + 2 setMinter
# calls (each a few million gas at base_fee = 1).
MIN_BALANCE_WEI_DEFAULT="1000000000"

# ─── Style ───────────────────────────────────────────────────────────────
if [ -t 1 ] && [ -z "${NO_COLOR:-}" ]; then
    BOLD=$'\033[1m'; DIM=$'\033[2m'; RED=$'\033[31m'; GREEN=$'\033[32m'
    YELLOW=$'\033[33m'; BLUE=$'\033[34m'; CYAN=$'\033[36m'; RESET=$'\033[0m'
else
    BOLD=""; DIM=""; RED=""; GREEN=""; YELLOW=""; BLUE=""; CYAN=""; RESET=""
fi

step()  { printf "%s▸%s %s\n" "$CYAN" "$RESET" "$*"; }
ok()    { printf "%s✓%s %s\n" "$GREEN" "$RESET" "$*"; }
warn()  { printf "%s!%s %s\n" "$YELLOW" "$RESET" "$*"; }
die()   { printf "%s✗%s %s\n" "$RED" "$RESET" "$*" >&2; exit "${2:-1}"; }
info()  { printf "%s%s%s\n" "$DIM" "$*" "$RESET"; }

# ─── Args ────────────────────────────────────────────────────────────────
RPC_URL="$RPC_URL_DEFAULT"
KEYSTORE="$KEYSTORE_DEFAULT"
FORCE=0
EXPLORER="$EXPLORER_DEFAULT"

while [ $# -gt 0 ]; do
    case "$1" in
        --rpc-url)   RPC_URL="$2"; shift 2 ;;
        --keystore)  KEYSTORE="$2"; shift 2 ;;
        --explorer)  EXPLORER="$2"; shift 2 ;;
        --force)     FORCE=1; shift ;;
        -h|--help)
            sed -n '2,30p' "$0"; exit 0 ;;
        *) die "unknown argument: $1" 1 ;;
    esac
done

MIN_BALANCE="${CURS3D_DEPLOY_MIN_BALANCE:-$MIN_BALANCE_WEI_DEFAULT}"

cd "$(dirname "$0")"

# ─── 1. Pre-flight ───────────────────────────────────────────────────────
step "${BOLD}Pre-flight${RESET}"

for cmd in forge cast jq curl; do
    command -v "$cmd" >/dev/null || die "missing tool: $cmd" 1
done
info "  forge $(forge --version | head -1 | awk '{print $2}')   cast $(cast --version | head -1 | awk '{print $2}')   jq $(jq --version | sed 's/jq-//')"

rpc_call() {
    curl --fail --silent --show-error --max-time 10 -H 'Content-Type: application/json' \
        -X POST "$RPC_URL" -d "$1" 2>&1
}

CHAIN_ID_HEX="$(rpc_call '{"jsonrpc":"2.0","method":"eth_chainId","params":[],"id":1}' | jq -r '.result // empty')"
[ -n "$CHAIN_ID_HEX" ] || die "RPC at $RPC_URL did not return a chain ID. Is the network up?" 2
CHAIN_ID_DEC="$(printf '%d\n' "$CHAIN_ID_HEX")"
ok "RPC reachable: chainId=$CHAIN_ID_DEC ($CHAIN_ID_HEX)"

H1_HEX="$(rpc_call '{"jsonrpc":"2.0","method":"eth_blockNumber","params":[],"id":1}' | jq -r '.result // empty')"
H1_DEC="$(printf '%d\n' "$H1_HEX")"
[ "$H1_DEC" -gt 0 ] || die "chain at height 0 — wait for block production" 2
info "  current height: $H1_DEC"

step "Waiting 12s to confirm chain is advancing..."
sleep 12
H2_HEX="$(rpc_call '{"jsonrpc":"2.0","method":"eth_blockNumber","params":[],"id":1}' | jq -r '.result // empty')"
H2_DEC="$(printf '%d\n' "$H2_HEX")"
if [ "$H2_DEC" -le "$H1_DEC" ]; then
    die "chain not advancing: was $H1_DEC, still $H2_DEC after 12s. Investigate node health before deploying." 2
fi
ok "Chain advancing: $H1_DEC → $H2_DEC"

# ─── 2. Keystore ─────────────────────────────────────────────────────────
step "${BOLD}Keystore${RESET}"

mkdir -p "$(dirname "$KEYSTORE")"

if [ ! -f "$KEYSTORE" ]; then
    warn "No keystore at $KEYSTORE — creating a fresh one."
    if [ -z "${CURS3D_KEYSTORE_PASSWORD:-}" ]; then
        printf "Password for new keystore: "; read -rs CURS3D_KEYSTORE_PASSWORD; echo
        printf "Confirm password:          "; read -rs CONFIRM; echo
        [ "$CURS3D_KEYSTORE_PASSWORD" = "$CONFIRM" ] || die "passwords do not match" 1
    fi
    cast wallet new \
        "$(dirname "$KEYSTORE")" \
        "$(basename "$KEYSTORE")" \
        --unsafe-password "$CURS3D_KEYSTORE_PASSWORD" >/dev/null
    [ -f "$KEYSTORE" ] || die "cast wallet new did not produce $KEYSTORE" 1
    ok "Created $KEYSTORE"
fi

if [ -z "${CURS3D_KEYSTORE_PASSWORD:-}" ]; then
    printf "Keystore password: "; read -rs CURS3D_KEYSTORE_PASSWORD; echo
fi

SENDER="$(cast wallet address --keystore "$KEYSTORE" --password "$CURS3D_KEYSTORE_PASSWORD")"
ok "Deployer: $SENDER"

# ─── 3. Balance ──────────────────────────────────────────────────────────
step "${BOLD}Balance${RESET}"

BAL_HEX="$(rpc_call "$(printf '{"jsonrpc":"2.0","method":"eth_getBalance","params":["%s","latest"],"id":1}' "$SENDER")" | jq -r '.result // empty')"
[ -n "$BAL_HEX" ] || die "eth_getBalance failed for $SENDER" 2

# Compare hex strings as decimal via cast
BAL_DEC="$(cast to-dec "$BAL_HEX" 2>/dev/null || printf '%d\n' "$BAL_HEX")"
BAL_CUR="$(cast from-wei "$BAL_DEC" 2>/dev/null || echo "$BAL_DEC")"
info "  balance: $BAL_CUR CUR ($BAL_DEC wei)"

# Use shell arithmetic for big integer compare via python (portable on macOS + Linux)
python3 - "$BAL_DEC" "$MIN_BALANCE" <<'PY' || die "deployer underfunded — top up $SENDER then retry. Need $MIN_BALANCE wei." 1
import sys
bal, need = int(sys.argv[1]), int(sys.argv[2])
sys.exit(0 if bal >= need else 1)
PY
ok "Balance OK (>= $(cast from-wei "$MIN_BALANCE") CUR)"

# ─── 4. Deploy ───────────────────────────────────────────────────────────
step "${BOLD}Deploy${RESET}"

DEPLOY_FILE="deployments/${CHAIN_ID_DEC}.json"
mkdir -p deployments

if [ -f "$DEPLOY_FILE" ] && [ "$FORCE" -eq 0 ]; then
    info "  $DEPLOY_FILE already exists — running in verify-only mode (use --force to redeploy)."
    DEPLOY_SKIPPED=1
else
    DEPLOY_SKIPPED=0
    [ -f "$DEPLOY_FILE" ] && rm -f "$DEPLOY_FILE"

    # ARBITRATOR is read by the Solidity script via vm.envOr, so export it here.
    export ARBITRATOR="${ARBITRATOR:-}"
    # forge needs --legacy on chains that don't expose EIP-1559 properly.
    forge script script/DeployPortfolio.s.sol:DeployPortfolio \
        --rpc-url "$RPC_URL" \
        --broadcast --slow --legacy \
        --keystore "$KEYSTORE" \
        --password "$CURS3D_KEYSTORE_PASSWORD" \
        --sender "$SENDER" \
        || die "forge script failed (broadcasts/run-latest.json may have details)" 3

    [ -f "$DEPLOY_FILE" ] || die "deploy script ran but $DEPLOY_FILE was not produced" 3
    ok "All 7 contracts deployed; addresses written to $DEPLOY_FILE"
fi

# ─── 5. Verify ───────────────────────────────────────────────────────────
step "${BOLD}Verify${RESET}"

TOKEN="$(jq -r '.token' "$DEPLOY_FILE")"
FAUCET="$(jq -r '.faucet' "$DEPLOY_FILE")"
STAKING="$(jq -r '.staking' "$DEPLOY_FILE")"
GOVERNANCE="$(jq -r '.governance' "$DEPLOY_FILE")"
ATTESTATIONS="$(jq -r '.attestations' "$DEPLOY_FILE")"
VAULT="$(jq -r '.vault' "$DEPLOY_FILE")"
ESCROW="$(jq -r '.escrow' "$DEPLOY_FILE")"

verify_code() {
    local label="$1" addr="$2"
    local code
    code="$(rpc_call "$(printf '{"jsonrpc":"2.0","method":"eth_getCode","params":["%s","latest"],"id":1}' "$addr")" | jq -r '.result // "0x"')"
    if [ "${#code}" -le 4 ]; then
        die "$label at $addr has no code (eth_getCode → $code)" 4
    fi
    info "  $label  $addr  code=${#code}c"
}

verify_code Token         "$TOKEN"
verify_code Faucet        "$FAUCET"
verify_code Staking       "$STAKING"
verify_code Governance    "$GOVERNANCE"
verify_code Attestations  "$ATTESTATIONS"
verify_code Vault         "$VAULT"
verify_code Escrow        "$ESCROW"

verify_minter() {
    local label="$1" addr="$2"
    local result
    result="$(cast call "$TOKEN" "minters(address)(bool)" "$addr" --rpc-url "$RPC_URL" 2>/dev/null || echo "")"
    if [ "$result" != "true" ]; then
        die "Token.minters($label=$addr) → $result (expected true). setMinter wiring broken." 4
    fi
    info "  Token.minters($label) = true"
}

verify_minter Faucet  "$FAUCET"
verify_minter Staking "$STAKING"
ok "All 7 contracts have code; both minter wires set."

# ─── 6. Sync dApp ────────────────────────────────────────────────────────
step "${BOLD}Sync dApp${RESET}"

DAPP_FILE="dapp/deployments.json"
cp "$DEPLOY_FILE" "$DAPP_FILE"
ok "Copied $DEPLOY_FILE → $DAPP_FILE (dApp will pick this up on next reload)."

# ─── 7. Summary ──────────────────────────────────────────────────────────
echo
printf "%s%s━━━━ CURS3D portfolio ready ━━━━%s\n" "$BOLD" "$GREEN" "$RESET"
printf "%schain%s        %s (%s)\n"        "$DIM" "$RESET" "$CHAIN_ID_DEC" "$CHAIN_ID_HEX"
printf "%sdeployer%s     %s\n"             "$DIM" "$RESET" "$SENDER"
printf "%stoken%s        %s%s/address/%s%s\n" "$DIM" "$RESET" "$BLUE" "$EXPLORER" "$TOKEN" "$RESET"
printf "%sfaucet%s       %s%s/address/%s%s\n" "$DIM" "$RESET" "$BLUE" "$EXPLORER" "$FAUCET" "$RESET"
printf "%sstaking%s      %s%s/address/%s%s\n" "$DIM" "$RESET" "$BLUE" "$EXPLORER" "$STAKING" "$RESET"
printf "%sgovernance%s   %s%s/address/%s%s\n" "$DIM" "$RESET" "$BLUE" "$EXPLORER" "$GOVERNANCE" "$RESET"
printf "%sattestations%s %s%s/address/%s%s\n" "$DIM" "$RESET" "$BLUE" "$EXPLORER" "$ATTESTATIONS" "$RESET"
printf "%svault%s        %s%s/address/%s%s\n" "$DIM" "$RESET" "$BLUE" "$EXPLORER" "$VAULT" "$RESET"
printf "%sescrow%s       %s%s/address/%s%s\n" "$DIM" "$RESET" "$BLUE" "$EXPLORER" "$ESCROW" "$RESET"
echo
if [ "$DEPLOY_SKIPPED" -eq 1 ]; then
    info "Verify-only run. Pass --force to redeploy."
else
    ok "Open dapp/index.html (or visit https://curs3d.fr/dapp) to interact."
fi
