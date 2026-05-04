#!/usr/bin/env sh
set -eu

SHARED_DIR="${LOCALNET_SHARED_DIR:-/localnet}"
NODE1_DIR="$SHARED_DIR/node1"
NODE2_DIR="$SHARED_DIR/node2"
FAUCET_DIR="$SHARED_DIR/faucet"
GENESIS_PATH="$SHARED_DIR/genesis.localnet.json"

mkdir -p "$NODE1_DIR" "$NODE2_DIR" "$FAUCET_DIR"

ensure_password_file() {
    path="$1"
    value="$2"
    if [ ! -f "$path" ]; then
        printf '%s\n' "$value" > "$path"
        chmod 600 "$path"
    fi
}

ensure_wallet() {
    wallet_path="$1"
    password_path="$2"
    if [ ! -f "$wallet_path" ]; then
        curs3d wallet --output "$wallet_path" --password-file "$password_path"
    fi
}

ensure_password_file "$NODE1_DIR/validator.password" "localnet-validator-1"
ensure_password_file "$NODE2_DIR/validator.password" "localnet-validator-2"
ensure_password_file "$FAUCET_DIR/faucet.password" "localnet-faucet"

ensure_wallet "$NODE1_DIR/validator.json" "$NODE1_DIR/validator.password"
ensure_wallet "$NODE2_DIR/validator.json" "$NODE2_DIR/validator.password"
ensure_wallet "$FAUCET_DIR/faucet.json" "$FAUCET_DIR/faucet.password"

if [ ! -f "$GENESIS_PATH" ]; then
    curs3d genesis \
        --output "$GENESIS_PATH" \
        --chain-id "curs3d-localnet" \
        --chain-name "CURS3D Localnet" \
        --validator-wallet "$NODE1_DIR/validator.json" \
        --validator-password-file "$NODE1_DIR/validator.password" \
        --validator-wallet "$NODE2_DIR/validator.json" \
        --validator-password-file "$NODE2_DIR/validator.password" \
        --validator-balance-cur 1500000 \
        --validator-balance-cur 1500000 \
        --validator-stake-cur 50000 \
        --validator-stake-cur 50000 \
        --faucet-wallet "$FAUCET_DIR/faucet.json" \
        --faucet-password-file "$FAUCET_DIR/faucet.password" \
        --faucet-balance-cur 2000000
fi
