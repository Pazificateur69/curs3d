# CURS3D Public VPS Deployment

Last updated: **2026-05-04 (afternoon — protocol v4)**.

This guide is for a public bootstrap node on a VPS so external users can connect, query the API (REST + WebSocket + **`/eth` Ethereum-compatible JSON-RPC**), and use a faucet backed by a real signed transaction. From v4 onwards, the same node also serves MetaMask / Hardhat / Foundry traffic via revm 38.

## Prerequisites: Rust nightly

The codebase requires the **nightly** Rust toolchain. `multiaddr 0.18.2` (a
transitive dep of libp2p) fails to compile on stable ≥ 1.94 due to a
type-inference regression we have not patched out.

```bash
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --default-toolchain none
. "$HOME/.cargo/env"
rustup install nightly --profile minimal
# Build everything with nightly:
RUSTUP_TOOLCHAIN=nightly cargo build --release
```

CI (`.github/workflows/`) also runs on nightly.

> **Build-time heads-up (v4):** revm 38 was added in commit `c34b366` and
> brings ~200 transitive crates with it. On Oracle ARM Free Tier
> (`VM.Standard.A1.Flex`, 1 OCPU, 6 GB RAM) the **first clean release
> build takes 5–8 minutes**. Subsequent incremental builds are ~1 minute.
> Plan accordingly if you build directly on the VPS.

## What This Deploys

- Persistent P2P identity stored in the node data directory
- Bootstrap validator wallet loaded non-interactively from a password file
- Public bootnode addresses generated from `--public-addr`
- HTTP API behind Nginx + TLS (REST + WebSocket + `/eth` JSON-RPC)
- TCP RPC kept private on localhost by default
- Optional faucet backed by a real wallet and signed transfer transactions
- Optional browser wallet bundle served from `/wallet-wasm/` (built via `wasm-pack`)

## Ports

- `4337/tcp`: libp2p P2P
- `8080/tcp`: internal HTTP API
- `9545/tcp`: internal RPC, keep private
- `80/tcp`: Nginx + ACME
- `443/tcp`: Nginx + TLS

## 1. Prepare Wallets

Create password files first:

```bash
mkdir -p deploy/secrets
printf '%s\n' 'change-this-validator-password' > deploy/secrets/validator.password
printf '%s\n' 'change-this-faucet-password' > deploy/secrets/faucet.password
chmod 600 deploy/secrets/*.password
```

Create the validator and faucet wallets:

```bash
RUSTUP_TOOLCHAIN=nightly cargo build --release
./target/release/curs3d wallet \
  --output deploy/secrets/validator.json \
  --password-file deploy/secrets/validator.password

./target/release/curs3d wallet \
  --output deploy/secrets/faucet.json \
  --password-file deploy/secrets/faucet.password
```

> **IMPORTANT:** Argon2id (m=64MB) makes wallet derivation host-specific
> from a memory-pressure standpoint. Wallets work cross-host, but for
> production, generate them ON the VPS that will run the validator.

Inspect the wallets:

```bash
./target/release/curs3d info \
  --wallet deploy/secrets/validator.json \
  --password-file deploy/secrets/validator.password \
  --json
```

## 2. Generate the Official Public Genesis

Generate a real `genesis.json` from the wallets you will actually operate. The
flags `--validator-wallet`, `--validator-password-file`, `--validator-balance-cur`
and `--validator-stake-cur` can be repeated in lock-step to seed several
validators in the same genesis (positional pairing — pass them in the same
order for each validator):

```bash
./target/release/curs3d genesis \
  --output deploy/genesis.public-testnet.json \
  --chain-id curs3d-public-testnet \
  --chain-name "CURS3D Public Testnet" \
  --validator-wallet deploy/secrets/validator.json \
  --validator-password-file deploy/secrets/validator.password \
  --validator-balance-cur 1500000 \
  --validator-stake-cur 50000 \
  --validator-wallet deploy/secrets/validator2.json \
  --validator-password-file deploy/secrets/validator2.password \
  --validator-balance-cur 1500000 \
  --validator-stake-cur 50000 \
  --faucet-wallet deploy/secrets/faucet.json \
  --faucet-password-file deploy/secrets/faucet.password \
  --faucet-balance-cur 2000000
```

> **Note (2026-05-04, v4):** deterministic stake-weighted slot-leader
> scheduling is now implemented in `src/consensus/mod.rs` (commit
> `343a7a1`), so multi-validator genesis is supported in production. The
> current public testnet runs 2 validators (node1 + node2) on this binary
> with finalized height matching the tip.

Publish `deploy/genesis.public-testnet.json` somewhere public and keep the exact same file on every node.

## 3. Publish the Bootnode Address

Pick the public address you want peers to use:

```bash
./target/release/curs3d bootnode-address \
  --data-dir deploy/node-data \
  --public-addr /dns4/node.example.com/tcp/4337
```

This writes the stable address list to `deploy/node-data/bootnode.addrs`. Publish the resulting `/dns4/.../tcp/.../p2p/...` address to users and other nodes.

If you ever need to wipe the persistent libp2p PeerId (e.g. cloning a host
or recovering from a corrupted `p2p_identity.pb`), pass
`--reset-p2p-identity` to the `node` subcommand once. The flag deletes the
existing identity and lets the node regenerate a fresh one on the next
start. Re-publish the new bootnode address afterwards.

## 4. Configure Environment

Copy and edit the example environment:

```bash
cp deploy/env/node.env.example deploy/env/node.env
```

Set at minimum:

- `CURS3D_PUBLIC_ADDR`
- `CURS3D_API_ALLOW_ORIGIN`
- `CURS3D_API_TOKEN`
- `CURS3D_RPC_TOKEN`
- `CURS3D_FAUCET_WALLET`
- `CURS3D_FAUCET_PASSWORD_FILE`
- `CURS3D_FAUCET_COOLDOWN_FILE`

## 5. Docker Compose Deploy

```bash
docker compose -f deploy/docker-compose.public.yml --env-file deploy/env/node.env up -d --build
docker compose -f deploy/docker-compose.public.yml ps
curl http://127.0.0.1:8080/api/healthz
curl http://127.0.0.1:8080/api/metrics
```

The validator wallet password is loaded from a file, so restarts are non-interactive.
The node now fails fast if the validator wallet cannot be loaded or if the P2P stack does not start; `/api/healthz` returns `503` when the node is stale or not network-ready.

## 6. Nginx + TLS

Install Nginx and Certbot on the VPS, then use `deploy/nginx/curs3d.conf` as a base vhost.

Recommended exposure:

- Public: `4337`, `80`, `443`
- Private or localhost only: `8080`, `9545`

The `api.curs3d.fr` vhost MUST proxy three locations to the node:

```nginx
location /api/ { proxy_pass http://127.0.0.1:8080/api/; ... }
location /ws   { proxy_pass http://127.0.0.1:8080/ws;   proxy_http_version 1.1;
                 proxy_set_header Upgrade $http_upgrade;
                 proxy_set_header Connection "upgrade"; ... }
# v4: Ethereum-compatible JSON-RPC, used by MetaMask / Hardhat / Foundry / ethers.js
location /eth  { proxy_pass http://127.0.0.1:8080/eth; }
```

Do not require `Authorization: Bearer ...` on `/eth` — external EVM
wallets cannot send a custom auth header. Keep `/eth` rate-limited the
same way `/api/` is.

If you serve the static site (`curs3d.fr`) from the same nginx, the
Content-Security-Policy on that vhost needs `wasm-unsafe-eval` in
`script-src` so the browser wallet bundle (`/wallet-wasm/...`) can
instantiate.

## 6b. (Optional) Build and ship the browser wallet bundle

The browser wallet UI (`website/wallet.html` + `wallet.js`) loads a WASM
crypto bundle from `/wallet-wasm/`. The bundle lives in the standalone
crate `sdk/wasm` (excluded from the root workspace).

```bash
rustup target add wasm32-unknown-unknown   # one-off
cargo install wasm-pack                    # one-off (if missing)
brew install binaryen                      # macOS — provides wasm-opt
# or: apt install binaryen                  # Debian/Ubuntu

cd sdk/wasm
wasm-pack build --target web --release
# Output:
#   sdk/wasm/pkg/curs3d_wallet_wasm.js          (~24 KB)
#   sdk/wasm/pkg/curs3d_wallet_wasm_bg.wasm     (~100 KB optimized,
#                                                ~236 KB if wasm-opt fails)
#   sdk/wasm/pkg/curs3d_wallet_wasm.d.ts        TypeScript types
```

If `wasm-opt` is missing, `wasm-pack` will warn and emit an unoptimised
bundle; that's the current production state. Either install `binaryen`,
or set `wasm-opt = false` in
`sdk/wasm/Cargo.toml [package.metadata.wasm-pack.profile.release]` to
silence the warning.

Deploy the two output files under the public site:

```bash
# Replace with your nginx site root
sudo mkdir -p /var/www/curs3d/wallet-wasm
sudo cp sdk/wasm/pkg/curs3d_wallet_wasm.js \
       sdk/wasm/pkg/curs3d_wallet_wasm_bg.wasm \
       /var/www/curs3d/wallet-wasm/
```

> **Read-only today.** ML-DSA-87 (FIPS-204, in this bundle) is not
> byte-compatible with `pqcrypto-dilithium 0.5.0` (NIST round 3, in the
> node). Browser-signed txs are silently rejected by `/api/tx/submit`.
> The wallet UI displays balance / nonce / staked / history but cannot
> send. See `CLAUDE.md` → "Known bugs / open issues" for the migration
> path (node side switches to `pqcrypto-mldsa` or `ml-dsa`).

## 7. Firewall

Example UFW rules:

```bash
ufw allow 22/tcp
ufw allow 80/tcp
ufw allow 443/tcp
ufw allow 4337/tcp
ufw deny 8080/tcp
ufw deny 9545/tcp
ufw enable
```

## 8. Health and Operations

Useful checks:

```bash
curl http://127.0.0.1:8080/api/status
curl http://127.0.0.1:8080/api/validators
curl http://127.0.0.1:8080/api/healthz
curl http://127.0.0.1:8080/api/metrics
sudo tail -20 /var/log/curs3d-healthcheck.log

# v4: smoke-test the EVM JSON-RPC
curl -s -X POST http://127.0.0.1:8080/eth \
  -H 'Content-Type: application/json' \
  -d '{"jsonrpc":"2.0","method":"eth_chainId","id":1}'
# Expect: {"jsonrpc":"2.0","result":"0x6b4ed968","id":1}

# Public: same call but through TLS / nginx
curl -s -X POST https://api.curs3d.fr/eth \
  -H 'Content-Type: application/json' \
  -d '{"jsonrpc":"2.0","method":"eth_blockNumber","id":1}'
```

Generated files to keep:

- `deploy/genesis.public-testnet.json`
- `deploy/secrets/validator.json`
- `deploy/secrets/validator.password`
- `deploy/secrets/faucet.json`
- `deploy/secrets/faucet.password`
- `deploy/node-data/p2p_identity.pb`
- `deploy/node-data/bootnode.addrs`

## 9. Backups

Back up these paths regularly:

- `deploy/genesis.public-testnet.json`
- `deploy/secrets/`
- `deploy/node-data/`

At minimum, treat wallet files, password files, and `p2p_identity.pb` as critical.
