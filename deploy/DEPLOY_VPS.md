# CURS3D Public VPS Deployment

This guide is for a single public bootstrap node on a VPS so external users can connect, query the API, and use a faucet backed by a real signed transaction.

## What This Deploys

- Persistent P2P identity stored in the node data directory
- Bootstrap validator wallet loaded non-interactively from a password file
- Public bootnode addresses generated from `--public-addr`
- HTTP API behind Nginx + TLS
- RPC kept private on localhost by default
- Optional faucet backed by a real wallet and signed transfer transactions

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
cargo build --release
./target/release/curs3d wallet \
  --output deploy/secrets/validator.json \
  --password-file deploy/secrets/validator.password

./target/release/curs3d wallet \
  --output deploy/secrets/faucet.json \
  --password-file deploy/secrets/faucet.password
```

Inspect the wallets:

```bash
./target/release/curs3d info \
  --wallet deploy/secrets/validator.json \
  --password-file deploy/secrets/validator.password \
  --json
```

## 2. Generate the Official Public Genesis

Generate a real `genesis.json` from the wallets you will actually operate:

```bash
./target/release/curs3d genesis \
  --output deploy/genesis.public-testnet.json \
  --chain-id curs3d-public-testnet \
  --chain-name "CURS3D Public Testnet" \
  --validator-wallet deploy/secrets/validator.json \
  --validator-password-file deploy/secrets/validator.password \
  --validator-balance-cur 1500000 \
  --validator-stake-cur 50000 \
  --faucet-wallet deploy/secrets/faucet.json \
  --faucet-password-file deploy/secrets/faucet.password \
  --faucet-balance-cur 2000000
```

Publish `deploy/genesis.public-testnet.json` somewhere public and keep the exact same file on every node.

## 3. Publish the Bootnode Address

Pick the public address you want peers to use:

```bash
./target/release/curs3d bootnode-address \
  --data-dir deploy/node-data \
  --public-addr /dns4/node.example.com/tcp/4337
```

This writes the stable address list to `deploy/node-data/bootnode.addrs`. Publish the resulting `/dns4/.../tcp/.../p2p/...` address to users and other nodes.

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

## 6. Nginx + TLS

Install Nginx and Certbot on the VPS, then use `deploy/nginx/curs3d.conf` as a base vhost.

Recommended exposure:

- Public: `4337`, `80`, `443`
- Private or localhost only: `8080`, `9545`

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
