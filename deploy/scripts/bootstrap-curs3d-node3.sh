#!/usr/bin/env bash
#
# CURS3D node3 — post-cloud-init bootstrap.
# Run this from the operator Mac AFTER cloud-init has finished on the new
# node3 VPS (cloud-init takes ~3-5 min after VPS boot; check with
# `ssh curs3d-node3 'cloud-init status'`).
#
# What this does:
#   1. Verifies SSH connectivity to curs3d-node3.
#   2. Installs Rust nightly + targets via rustup on the VPS.
#   3. Pulls the curs3d source from origin/main into ~/curs3d-new.
#   4. Builds the release binary (background, 15-25 min on 2 GB + swap).
#   5. Returns immediately with the build PID; you can come back later.
#
# Wallet generation, genesis distribution, systemd unit setup are NOT done
# here — those depend on whether you're rejoining as a 3rd validator
# post-genesis (DEPLOY_RUNBOOK.md "Ajout de validateur post-genesis") or
# being included in a fresh genesis.

set -euo pipefail

BOLD=$'\033[1m'; DIM=$'\033[2m'; RED=$'\033[31m'; GREEN=$'\033[32m'
YELLOW=$'\033[33m'; CYAN=$'\033[36m'; RESET=$'\033[0m'

step() { printf "%s▸%s %s\n" "$CYAN" "$RESET" "$*"; }
ok()   { printf "%s✓%s %s\n" "$GREEN" "$RESET" "$*"; }
warn() { printf "%s!%s %s\n" "$YELLOW" "$RESET" "$*"; }
die()  { printf "%s✗%s %s\n" "$RED" "$RESET" "$*" >&2; exit "${2:-1}"; }
info() { printf "%s%s%s\n" "$DIM" "$*" "$RESET"; }

HOST="${HOST:-curs3d-node3}"

step "${BOLD}Pre-flight: SSH reachability${RESET}"
ssh -o ConnectTimeout=10 -o BatchMode=yes "$HOST" "true" \
    || die "Cannot reach $HOST over SSH. Check the VPS is up, firewall allows 22, and ~/.ssh/config has a Host entry for $HOST." 2
ok "SSH OK to $HOST"

step "${BOLD}Verifying cloud-init has finished${RESET}"
CI_STATUS="$(ssh "$HOST" "cloud-init status 2>/dev/null | tail -1")"
info "  cloud-init: $CI_STATUS"
case "$CI_STATUS" in
    *done*|*disabled*) ok "cloud-init finished" ;;
    *running*) warn "cloud-init still running — sleep a bit and re-run this script" ;;
    *) warn "cloud-init status unclear ($CI_STATUS) — proceeding anyway" ;;
esac

step "${BOLD}Installing Rust nightly on $HOST${RESET}"
ssh "$HOST" 'bash -lc "
    if ! command -v rustup >/dev/null; then
        curl --proto =https --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --default-toolchain nightly --profile minimal
    fi
    . ~/.cargo/env
    rustup toolchain install nightly --profile minimal --component rust-src
    rustup target add wasm32-unknown-unknown --toolchain nightly
    rustc --version
"' || die "Rust install failed on $HOST" 3
ok "Rust nightly installed"

step "${BOLD}Cloning curs3d source into ~/curs3d-new${RESET}"
ssh "$HOST" 'bash -lc "
    if [ ! -d ~/curs3d-new/.git ]; then
        rm -rf ~/curs3d-new
        git clone https://github.com/Pazificateur69/curs3d.git ~/curs3d-new
    else
        cd ~/curs3d-new && git fetch origin main && git reset --hard origin/main
    fi
    cd ~/curs3d-new && git log --oneline -1
"' || die "Source pull failed" 4
ok "curs3d-new at origin/main"

step "${BOLD}Starting cargo build --release in background${RESET}"
BUILD_PID="$(ssh "$HOST" 'bash -lc "
    cd ~/curs3d-new
    nohup bash -lc \"RUSTUP_TOOLCHAIN=nightly cargo build --release\" > /tmp/curs3d-build.log 2>&1 < /dev/null &
    echo \$!
"')"
ok "Build started (PID $BUILD_PID on $HOST)"

cat <<EOF

${DIM}--- Next steps ---${RESET}

1. Watch the build (15-25 min on 2 GB + swap):
     ssh $HOST 'tail -f /tmp/curs3d-build.log'

2. Once finished:
     ssh $HOST 'ls -la ~/curs3d-new/target/release/curs3d'

3. To re-add node3 as a validator post-genesis (option B path):
     - Generate validator wallet on $HOST (do NOT cross-compile — Argon2id
       memory cost requires the target machine).
     - Fund the new address from the faucet (>= 1500 CUR recommended).
     - Submit a Stake transaction.
     - Wait one epoch boundary (32 blocks ≈ 5 min).
   Full procedure: deploy/DEPLOY_RUNBOOK.md "Ajout de validateur post-genesis".

EOF
