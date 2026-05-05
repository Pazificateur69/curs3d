#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
BIN="${CURS3D_BIN:-$ROOT_DIR/target/debug/curs3d}"
DATA_DIR="$(mktemp -d)"
LOG_FILE="${TMPDIR:-/tmp}/curs3d-rpc-smoke.log"
HTTP_ADDR="127.0.0.1:18080"
RPC_ADDR="127.0.0.1:19545"

cleanup() {
    if [ -n "${NODE_PID:-}" ] && kill -0 "$NODE_PID" 2>/dev/null; then
        kill "$NODE_PID" 2>/dev/null || true
        wait "$NODE_PID" 2>/dev/null || true
    fi
    rm -rf "$DATA_DIR"
}
trap cleanup EXIT

if [ ! -x "$BIN" ]; then
    echo "missing curs3d binary at $BIN" >&2
    exit 1
fi

"$BIN" node \
    --port 0 \
    --data-dir "$DATA_DIR" \
    --http-addr "$HTTP_ADDR" \
    --rpc-addr "$RPC_ADDR" >"$LOG_FILE" 2>&1 &
NODE_PID="$!"

for _ in $(seq 1 40); do
    if curl -fsS --max-time 2 "http://$HTTP_ADDR/api/status" >/dev/null 2>&1; then
        break
    fi
    if ! kill -0 "$NODE_PID" 2>/dev/null; then
        cat "$LOG_FILE" >&2 || true
        exit 1
    fi
    sleep 0.5
done

STATUS="$(curl -fsS --max-time 5 "http://$HTTP_ADDR/api/status")"
echo "$STATUS" | jq -e '.ok == true and (.data.chain_id | type == "string")' >/dev/null

CHAIN_ID="$(
    curl -fsS --max-time 5 -X POST "http://$HTTP_ADDR/eth" \
        -H 'Content-Type: application/json' \
        -d '{"jsonrpc":"2.0","method":"eth_chainId","params":[],"id":1}' \
    | jq -r '.result'
)"
[[ "$CHAIN_ID" =~ ^0x[0-9a-fA-F]+$ ]]

BLOCK_NUMBER="$(
    curl -fsS --max-time 5 -X POST "http://$HTTP_ADDR/eth" \
        -H 'Content-Type: application/json' \
        -d '{"jsonrpc":"2.0","method":"eth_blockNumber","params":[],"id":2}' \
    | jq -r '.result'
)"
[[ "$BLOCK_NUMBER" =~ ^0x[0-9a-fA-F]+$ ]]

echo "RPC smoke ok: chainId=$CHAIN_ID blockNumber=$BLOCK_NUMBER"
