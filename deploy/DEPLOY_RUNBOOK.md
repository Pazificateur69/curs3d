# CURS3D — Runbook ops du testnet public

Derniere mise a jour: **2026-05-04 (apres-midi — hardfork v4 deploye)**

## Architecture (etat actuel)

```
                       Internet
                          |
              curs3d.fr / api.curs3d.fr / explorer.curs3d.fr / status.curs3d.fr (DNS)
                          |
                  [node1 — bootstrap + API + site + status]
                  144.24.192.222 (Oracle ARM Free Tier, eu-marseille-1)
                  nginx + TLS Let's Encrypt
                          |              \
                  curs3d.service          [node2 — validateur]
                  curs3d-captcha.service  84.235.238.213
                  curs3d-backup.timer     curs3d.service
                  curs3d-healthcheck.cron (slot-leader v4)
                  Docker stack: prometheus + grafana + uptime-kuma + node-exporter
```

- **2 validateurs actifs** (node1 + node2) depuis le hardfork v4. Le
  scheduling slot-leader deterministe (`343a7a1`) elimine les forks
  multi-validateurs. La finalite progresse a la meme hauteur que le tip.
- node3/4 : reportes (capacite ARM Oracle a re-evaluer plus tard).

## Protocol v4 (current)

Le hardfork v4 ajoute :

1. **EVM dispatch** (revm 38, alongside Wasmer 5) — Solidity / MetaMask /
   Hardhat / Foundry. Endpoint `POST https://api.curs3d.fr/eth`.
2. **Slot-leader stake-weighted scheduling** dans `src/consensus/mod.rs`.
3. **Transactions EVM-flavored** : RLP, secp256k1, recovery du sender,
   nouveaux variants `TransactionKind::DeployEvmContract` /
   `CallEvmContract` (appendus en fin d'enum, bincode-compat).

Le genesis n'inclut pas d'`upgrades` explicite : tout le monde demarre
directement en `protocol_version = 4`. Une chain DB pre-v4 est
**incompatible** : wipe `/var/lib/curs3d/` requis lors de la migration
(garder `validator.json` + `validator.password` + `p2p_identity.pb`).

## Nodes

| Node | IP | Role | Validateur | Etat | SSH |
|------|-----|------|------------|------|-----|
| node1 | 144.24.192.222 | Bootstrap + API + site + status | `CURe1Fa551B3f0524EfD8d0673cdBF9fD0e199458c5` | **actif** | `ssh curs3d-node1` |
| node2 | 84.235.238.213 | Validateur | `CURdC1ecceD4f12Cb3E34BD0d43E72d6D04fC4823dd` | **actif** | `ssh curs3d-node2` |
| Faucet | — | Wallet faucet | `CUR34cafc74B750C0e0150877e99cd27D77C6c4fC44` | — | — |

## Endpoints publics

| Service | URL |
|---------|-----|
| Site | https://curs3d.fr |
| API | https://api.curs3d.fr/api/status |
| **Ethereum-compatible JSON-RPC** | **https://api.curs3d.fr/eth** |
| Explorer | https://explorer.curs3d.fr |
| Wallet UI (read-only — voir Known issues #1) | https://curs3d.fr/wallet |
| Bundle WASM wallet | https://curs3d.fr/wallet-wasm/curs3d_wallet_wasm.js |
| Developers hub | https://curs3d.fr/developers |
| Security | https://curs3d.fr/security |
| Community | https://curs3d.fr/community |
| Faucet UI | https://curs3d.fr/faucet (Cloudflare Turnstile) |
| OpenAPI 3.1 / Stoplight | https://curs3d.fr/api |
| WebSocket | wss://api.curs3d.fr/ws |
| Status (Grafana) | https://status.curs3d.fr/ |
| Status (Uptime-Kuma) | https://status.curs3d.fr/status/ |
| Sitemap | https://curs3d.fr/sitemap.xml |
| security.txt (RFC 9116) | https://curs3d.fr/.well-known/security.txt |
| P2P Bootnode | 144.24.192.222:4337 |

Chain ID (string): `curs3d-public-testnet`
Chain ID (EVM, decimal): `1800329576`  ·  hex: `0x6b4ed968`  ·  Symbol: `CUR`
Genesis hash (v4): `daeb2e6ac802c182ab737d11211339a3d8c3a8da8f2dfd56595e319dbc938b23`
Protocol version: `v4`

## Infra (node1)

| Element | Valeur |
|---------|--------|
| Provider | Oracle Cloud Always Free ARM |
| Shape | VM.Standard.A1.Flex (1 OCPU, 6 GB RAM) |
| Region | eu-marseille-1 |
| OS | Ubuntu 22.04 ARM64 |
| Reverse proxy | nginx 1.18 (TLS Let's Encrypt, OCSP stapling, `ssl_session_tickets off`) |
| TLS profile | Mozilla intermediate |
| Headers | CSP, HSTS preload, `X-Frame-Options: DENY`, `Referrer-Policy: strict-origin-when-cross-origin`, `Permissions-Policy` |
| Pare-feu / IDS | `ufw` + `fail2ban` (jail `sshd`) |
| SSH | port 22, `PermitRootLogin no`, `MaxAuthTries 3`, key-only |
| DNS | Hostinger (curs3d.fr, api, explorer, status → 144.24.192.222) |

## Ports (Oracle Security List + ufw)

| Port | Service | Expose |
|------|---------|--------|
| 22 | SSH | public |
| 80 | nginx (HTTP→HTTPS) | public |
| 443 | nginx (TLS) | public |
| 4337 | libp2p P2P | public |
| 8080 | API HTTP (hyper) | localhost only |
| 8090 | curs3d-captcha-verify | localhost only |
| 9100 | node-exporter | localhost only |
| 9090 | prometheus | localhost only |
| 3001 | grafana | localhost only |
| 3002 | uptime-kuma | localhost only |
| 9545 | TCP RPC (CLI) | localhost only |

## Layout fichiers (node1)

```
/usr/local/bin/curs3d                       # binaire validateur
/usr/local/bin/curs3d-healthcheck.sh        # healthcheck v2 + Discord
/usr/local/bin/curs3d-backup.sh             # restic → B2
/usr/local/bin/curs3d-captcha-verify.py     # verifier Turnstile

/etc/curs3d/
  validator.json + validator.password       # wallet validateur (Dilithium L5, AES-256-GCM)
  faucet.json + faucet.password             # wallet faucet
  genesis.public-testnet.json               # genesis (immuable)
  secrets.env                               # EnvironmentFile= pour curs3d.service
  alerts.env                                # webhook Discord (lu par healthcheck + backup)
  captcha.env                               # secrets Cloudflare Turnstile

/etc/systemd/system/
  curs3d.service                            # node validateur (User=curs3d, hardened)
  curs3d-captcha.service                    # python verifier
  curs3d-backup.service + .timer            # restic 6h
/etc/cron.d/
  curs3d-healthcheck                        # */2 min

/var/lib/curs3d/                            # data dir (sled, p2p_identity.pb)
/var/log/curs3d-healthcheck.log
/var/www/curs3d/                            # site statique (index, faucet, api docs, wallet, ...)
/var/www/curs3d/wallet-wasm/                # bundle wasm-pack (curs3d_wallet_wasm.js + .wasm)
/etc/nginx/sites-available/
  curs3d.conf                               # site (curs3d.fr) + api (api.curs3d.fr/api/, /eth, /ws) + explorer
  curs3d-status                             # status.curs3d.fr (grafana + uptime-kuma)
```

## nginx — endpoints critiques (api.curs3d.fr)

`/etc/nginx/sites-available/curs3d.conf` doit exposer **deux** locations
qui proxifient vers le node :

```nginx
# REST + WebSocket existants
location /api/ { proxy_pass http://127.0.0.1:8080/api/; ... }
location /ws   { proxy_pass http://127.0.0.1:8080/ws;  proxy_http_version 1.1; Upgrade ...; }

# Ethereum-compatible JSON-RPC (v4) — MetaMask / Hardhat / Foundry / ethers.js
location /eth  { proxy_pass http://127.0.0.1:8080/eth; }
```

CORS / rate-limit suivent les memes regles que `/api/`. La location `/eth`
est requise par MetaMask : ne jamais la fermer derriere auth Bearer
(les wallets externes ne savent pas l'envoyer).

Sur le vhost `curs3d.fr`, la `Content-Security-Policy` autorise
`wasm-unsafe-eval` dans `script-src` pour permettre l'instanciation du
bundle WASM du wallet.

## Monitoring

Stack Docker dans `deploy/monitoring/` (4 services):

- `prometheus` (`127.0.0.1:9090`) — scrape `https://api.curs3d.fr/api/metrics`,
  retention TSDB 90j.
- `grafana` (`127.0.0.1:3001`) — provisionne via fichiers, anonymous Viewer
  active, edition UI desactivee.
- `uptime-kuma` (`127.0.0.1:3002`) — moniteurs HTTP + TCP custom (api,
  explorer, p2p 4337). Servi sous `/status/` par nginx.
- `node-exporter` (`127.0.0.1:9100`, `network_mode: host`) — CPU, RAM,
  disque, FS, reseau du host.

```bash
ssh curs3d-node1
cd /home/ubuntu/curs3d/deploy/monitoring
docker compose ps
docker compose logs -f
```

`/etc/nginx/sites-available/curs3d-status` route :
- `/` → grafana
- `/status/` → uptime-kuma

## Alerting

`/etc/curs3d/alerts.env` contient (au moins) `DISCORD_WEBHOOK_URL=...`.

- `curs3d-healthcheck.sh` (cron `*/2 min`) verifie `/api/healthz` et
  detecte les boucles de restart systemd ; il poste sur le webhook
  Discord en cas de probleme.
- `curs3d-backup.sh` reporte aussi (succes / echec) sur le meme webhook.
- Le script est self-contained : il sourse `alerts.env` lui-meme — pas
  besoin de l'injecter dans la cron.

## Faucet captcha (Cloudflare Turnstile)

```
client (curs3d.fr/faucet) ──> nginx (auth_request /captcha-verify)
                                         │
                                         ▼
                      curs3d-captcha.service (127.0.0.1:8090)
                                         │   accepte GET (auth_request) + POST
                                         ▼
                       https://challenges.cloudflare.com/turnstile/v0/siteverify
                                         │
                          ok → nginx pose X-Captcha-Verified: 1
                                  + X-Captcha-Secret: $CURS3D_FAUCET_CAPTCHA_SECRET
                                         │
                                         ▼
              curs3d.service (POST /api/faucet/request) — verif constant-time
              -> applique 100 CUR + cooldown 1h address+IP
```

Variables (dans `/etc/curs3d/secrets.env` cote node + `/etc/curs3d/captcha.env` cote verifier) :

- `CURS3D_FAUCET_REQUIRE_CAPTCHA=1`
- `CURS3D_FAUCET_CAPTCHA_SECRET=<long random>`
- `TURNSTILE_SECRET_KEY=...`

## Backups

`curs3d-backup.timer` declenche `curs3d-backup.service` toutes les 6 h.
Le script `curs3d-backup.sh`:

1. snapshot `/etc/curs3d/`, `/var/lib/curs3d/`, `/var/www/curs3d/`,
   `/etc/nginx/sites-available/`
2. push vers Backblaze B2 : `b2:curs3d-backups-pazent:curs3d-node1`
   via restic
3. restic forget/prune (retention configuree dans le script)
4. notifie Discord en succes / echec

Restore (recovery path):

```bash
# variables d'env: B2_ACCOUNT_ID, B2_ACCOUNT_KEY, RESTIC_PASSWORD
restic -r b2:curs3d-backups-pazent:curs3d-node1 snapshots
restic -r b2:curs3d-backups-pazent:curs3d-node1 restore latest --target /tmp/recover
# puis copier /etc/curs3d, /var/lib/curs3d, regenerer p2p_identity si besoin
sudo systemctl start curs3d
```

Le wallet validateur etant chiffre AES-256-GCM + Argon2id, le password
file `validator.password` est obligatoire pour redemarrer — restaurer
les deux.

## Acces SSH

```bash
ssh curs3d-node1
```

`~/.ssh/config` :
```
Host curs3d-node1
    HostName 144.24.192.222
    User ubuntu
    IdentityFile ~/.ssh/id_ed25519_server
```

SSH du serveur durci :
- `PermitRootLogin no`
- `PasswordAuthentication no`
- `MaxAuthTries 3`
- `AllowUsers ubuntu`
- fail2ban actif (`bantime` 1h, `findtime` 10min, `maxretry` 5).

## Commandes utiles

### Status
```bash
ssh curs3d-node1 "curl -s http://localhost:8080/api/status | jq '{height: .data.height, validators: .data.active_validators, finalized: .data.finalized_height, epoch: .data.epoch}'"
```

### Logs
```bash
ssh curs3d-node1 "sudo journalctl -u curs3d -f"
ssh curs3d-node1 "sudo journalctl -u curs3d-captcha -f"
ssh curs3d-node1 "sudo journalctl -u curs3d-backup --since '24h ago'"
ssh curs3d-node1 "tail -50 /var/log/curs3d-healthcheck.log"
```

### Redemarrage
```bash
ssh curs3d-node1 "sudo systemctl restart curs3d"
```

### TLS / nginx
```bash
ssh curs3d-node1 "sudo nginx -t && sudo certbot certificates"
```

### Connectivite P2P (depuis le Mac)
```bash
nc -z -w5 144.24.192.222 4337 && echo OK || echo BLOCKED
```

## Redeploy (mise a jour du code)

```bash
ssh curs3d-node1
cd ~/curs3d
git pull
RUSTUP_TOOLCHAIN=nightly cargo build --release
sudo cp target/release/curs3d /usr/local/bin/curs3d
sudo systemctl restart curs3d
curl -s http://localhost:8080/api/status | jq .data.height
```

> **Heads-up build time :** depuis le hardfork v4, `revm 38` ajoute ~200
> deps. Premier build clean sur Oracle ARM Free Tier : 5–8 min ;
> incrementaux : ~1 min.

Mettre a jour le site web (statique + bundle WASM wallet) :

```bash
# Site statique
ssh curs3d-node1 "sudo cp -r /home/ubuntu/curs3d/website/* /var/www/curs3d/"

# Bundle wallet WASM — uniquement quand sdk/wasm a change
cd ~/curs3d/sdk/wasm
wasm-pack build --target web --release   # voir Known issues #2 si wasm-opt manque
ssh curs3d-node1 "sudo mkdir -p /var/www/curs3d/wallet-wasm"
scp pkg/curs3d_wallet_wasm.js pkg/curs3d_wallet_wasm_bg.wasm \
    curs3d-node1:/tmp/wallet-wasm/
ssh curs3d-node1 "sudo mv /tmp/wallet-wasm/* /var/www/curs3d/wallet-wasm/"
```

## Hardfork v3 → v4 (procedure deja jouee, pour reference)

1. **Coordonner l'arret simultane des deux nodes** (peers en versions
   melangees divergent silencieusement).
2. Compiler le binaire v4 sur chaque node :
   `RUSTUP_TOOLCHAIN=nightly cargo build --release` (5–8 min en clean).
3. Regenerer le genesis avec **les deux** `--validator-wallet` :
   ```bash
   ./target/release/curs3d genesis \
     --output deploy/genesis.public-testnet.json \
     --chain-id curs3d-public-testnet \
     --chain-name "CURS3D Public Testnet" \
     --validator-wallet deploy/secrets/validator.json \
     --validator-password-file deploy/secrets/validator.password \
     --validator-wallet deploy/secrets/validator2.json \
     --validator-password-file deploy/secrets/validator2.password \
     --faucet-wallet deploy/secrets/faucet.json \
     --faucet-password-file deploy/secrets/faucet.password
   ```
4. **Wipe `/var/lib/curs3d/`** sur node1 et node2 (chain DB pre-v4
   incompatible). **Conserver** `validator.json`, `validator.password`,
   `p2p_identity.pb`. Push le nouveau `genesis.public-testnet.json` sur
   les deux machines (meme bytes).
5. Deployer le binaire v4 et redemarrer les deux nodes.
6. Verifier :
   ```bash
   curl -s https://api.curs3d.fr/api/status | jq '.data | {protocol_version, active_validators, height, finalized_height, genesis_hash}'
   # protocol_version=4, active_validators=2, finalized_height ~= height
   ```
7. Tester `/eth` :
   ```bash
   curl -s -X POST https://api.curs3d.fr/eth \
     -H 'Content-Type: application/json' \
     -d '{"jsonrpc":"2.0","method":"eth_chainId","id":1}'
   # -> {"jsonrpc":"2.0","result":"0x6b4ed968","id":1}
   ```

## Troubleshooting

### Le node ne demarre pas
```bash
ssh curs3d-node1 "sudo journalctl -u curs3d --since '10 min ago' --no-pager | tail -80"
# Verifier secrets.env et que validator.password est lisible par le user `curs3d`.
```

### Faucet renvoie 403
- Le verifier captcha n'est pas accessible : `systemctl status curs3d-captcha`
- `CURS3D_FAUCET_CAPTCHA_SECRET` desynchronise entre nginx et node.

### Sync timeout (RequestBlocks)
Bug connu — `src/network/mod.rs` BlockResponse path. Plus declenche en
operation normale depuis le slot-leader v4 (`343a7a1`), mais le code
sous-jacent n'est pas corrige.

### MetaMask refuse la chain
- Verifier `curl -s -X POST https://api.curs3d.fr/eth -d '{"jsonrpc":"2.0","method":"eth_chainId","id":1}'` retourne bien `0x6b4ed968`.
- Si `502/504` : la location nginx `/eth` n'est pas configuree (cf. section nginx ci-dessus) ou le node ecoute sur autre chose que `127.0.0.1:8080`.
- CORS : la reponse doit inclure `Access-Control-Allow-Origin` (`CURS3D_API_ALLOW_ORIGIN`).

### Wallet UI affiche "signature rejected"
Comportement attendu aujourd'hui — la wallet UI est read-only tant que la
migration `pqcrypto-dilithium` -> `ml-dsa` cote node n'est pas faite.
Voir `CLAUDE.md` -> Known issues #1.

### `wasm-opt` manquant (build du bundle wallet)
`brew install binaryen` (macOS) / `apt install binaryen` (Debian/Ubuntu),
ou ajouter dans `sdk/wasm/Cargo.toml` :
```toml
[package.metadata.wasm-pack.profile.release]
wasm-opt = false
```

### Disque plein
```bash
ssh curs3d-node1 "df -h && sudo journalctl --vacuum-size=500M && docker system prune -f"
```

### Certificat TLS expire
```bash
ssh curs3d-node1 "sudo certbot renew --force-renewal && sudo systemctl reload nginx"
```

### Oracle reclaim (Free Tier)
Restaurer depuis restic (`b2:curs3d-backups-pazent:curs3d-node1`),
puis rejouer `deploy/scripts/setup-node.sh`.
