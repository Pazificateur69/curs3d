# CURS3D — Runbook ops du testnet public

Derniere mise a jour: **2026-05-06 (redb storage migration deployed + 2-validator genesis regen + node3 temporarily out of cluster pending SSH recovery)**

## Architecture (etat actuel)

```
                              Internet
                                 |
                     curs3d.fr / api.curs3d.fr / explorer.curs3d.fr / status.curs3d.fr
                                 |
                  +--------------+---------------+---------------+
                  |                              |               |
       [node1 — bootstrap + API]      [node2 — validateur]   [node3 — out of cluster, 2026-05-06]
       144.24.192.222                  84.235.238.213          31.70.70.62
       Oracle ARM Free Tier            Oracle ARM Free Tier    IONOS VPS x86_64
       eu-marseille-1 (FR)             eu-marseille-1 (FR)     reset, SSH lockout pending diag
       6 GB RAM, 1 OCPU                6 GB RAM, 1 OCPU        (a re-add via Stake post-genesis)
       nginx + TLS Let's Encrypt       curs3d.service
       curs3d.service
       curs3d-captcha.service
       curs3d-backup.timer
       curs3d-healthcheck.cron
       Docker stack: prometheus + grafana + uptime-kuma + node-exporter
```

- **2 validateurs ACTIFS** depuis le 2026-05-06 (genesis regenere a 2 validators
  apres incident node3). Chaque validateur stake 50 000 CUR = 50 % du total
  online. Le scheduling slot-leader deterministe (`343a7a1`) elimine les forks
  multi-validateurs.
- **Genesis JSON file SHA-256 (regen 2026-05-06)** : `165c5f9d2a77719ecada5937753465806d83429588df06f0f25cea5c274bbf4e`.
  Wallets validateurs inchanges (n1 + n2 keypairs ML-DSA-87 preservees).
- **node3** : VPS reset le 2026-05-06 apres outage reseau. Cloud-init applique
  via `deploy/scripts/cloud-init-node3.yaml`, mais SSH par cle est rejete
  apres "Server accepts key" → `userauth_pubkey: authenticated 0` (signature
  verification echoue cote serveur). Diagnostic en cours, hypothese probable
  Ubuntu 24.04 + OpenSSH 10.2p1 PAM stack interaction. node3 sera re-ajoute
  comme validateur dynamique post-genesis (procedure "Ajout de validateur
  post-genesis" plus bas) une fois SSH recupere.
- Historique : la chain a ete redemarree le 2026-05-05 lors du bump wasmer
  5 -> 7 (fix __rust_probestack sur x86_64), genesis regenere une seconde fois
  apres un fork resolu en incluant les 3 validateurs directement, puis sled
  -> redb migre le 2026-05-06 (fix deadlock IoBufs::write_to_log) et
  finalement regenere a 2 validators apres incident node3. Anciens hashes
  preserves pour tracabilite (CLAUDE.md).
- node4 : reporte (capacite ARM Oracle a re-evaluer plus tard, ou autre provider).
- Diversification geo + provider + arch : node1+node2 sur Oracle ARM Marseille,
  node3 sur IONOS x86_64 Berlin. Reduit le risque de panne provider/region/arch.

## Protocol v5 (current)

Le hardfork v5 (2026-04-30) migre la signature post-quantique :

1. **`pqcrypto-dilithium 0.5.0` (NIST round 3, C bindings) → `ml-dsa = 0.1.0-rc.9`
   (FIPS-204 ML-DSA-87, pure Rust).** Meme crate que `sdk/wasm` → les transactions
   signees dans le navigateur sont byte-pour-byte verifiables sur le node.
2. **Wallet UI write-capable** depuis v5 (interop browser <-> node).
3. **Toutes les addresses changent** (cles publiques differentes -> SHA3 different).
4. **Tous les blocs/finality-votes pre-v5 invalides** : wipe complet de la chain DB
   et regen wallets requis lors de la migration.

Le hardfork v4 (2026-05-04, conserve dans v5) ajoute :

1. **EVM dispatch** (revm 38, alongside Wasmer — Wasmer 5 a l'epoque, bumped
   a Wasmer 7 le 2026-05-05) — Solidity / MetaMask / Hardhat / Foundry.
   Endpoint `POST https://api.curs3d.fr/eth`.
2. **Slot-leader stake-weighted scheduling** dans `src/consensus/mod.rs`.
3. **Transactions EVM-flavored** : RLP, secp256k1, recovery du sender,
   nouveaux variants `TransactionKind::DeployEvmContract` /
   `CallEvmContract` (appendus en fin d'enum, bincode-compat).

Le genesis n'inclut pas d'`upgrades` explicite : tout le monde demarre
directement en `protocol_version = 5`. Une chain DB pre-v5 est
**incompatible** : wipe `/var/lib/curs3d/` requis lors de la migration
(garder `p2p_identity.pb` ; les wallets v4 ne sont PAS reutilisables — keypairs
incompatibles entre `pqcrypto-dilithium` et `ml-dsa`).

## Nodes

| Node | IP | Role | Validateur | Etat | SSH |
|------|-----|------|------------|------|-----|
| node1 | 144.24.192.222 | Bootstrap + API + site + status | `CURA770bE29d4C0066263855Ea5ADE6387d503f1Cea` | **actif** | `ssh curs3d-node1` |
| node2 | 84.235.238.213 | Validateur | `CURd5E78C78FF164fb4eAC641d5a2802134B8A2D836` | **actif** | `ssh curs3d-node2` |
| node3 | 31.70.70.62 | _(ex-validateur — out of cluster 2026-05-06)_ | _(wallet a regenerer sur la nouvelle install)_ | **down** (SSH lockout post-reset, voir "node3 temporarily out of cluster" dans CLAUDE.md) | `ssh curs3d-node3` (pending fix) |
| Faucet | — | Wallet faucet | _regen v5, voir `/etc/curs3d/faucet.json` sur node1_ | — | — |

Note : les addresses des validateurs ont change au hardfork v5 (les cles
ML-DSA-87 different des anciennes Dilithium round 3, donc l'address derivee
de SHA3(pubkey)[..20] est differente).

## Endpoints publics

| Service | URL |
|---------|-----|
| Site | https://curs3d.fr |
| API | https://api.curs3d.fr/api/status |
| **RPC public** (alias /eth + /v1/* + landing page) | **https://rpc.curs3d.fr** |
| **Ethereum-compatible JSON-RPC** | **https://rpc.curs3d.fr/eth** OR **https://api.curs3d.fr/eth** |
| Explorer | https://explorer.curs3d.fr |
| Wallet UI (write-capable depuis v5) | https://curs3d.fr/wallet |
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
Genesis hash (chain block, v5 — regen 2026-05-05 with 3 validators in genesis): `81420887fb59cd7c4837b2195bedbbb78291bd835e5b72162337f10d26f315d6`
Genesis JSON file SHA-256: `702be65951ec6b29efb157fe96f8aba0baf14fc24bfab3926976d2b8e25ca1c1`
Bootnode multiaddr: `/dns4/api.curs3d.fr/tcp/4337/p2p/12D3KooWLttF4EJ1SjiLEiXvJ1yqmJawLafv47r55T5xzSt1GHn2`
  (libp2p `dns4` resolution OK on node2 ; node3 dial via `/ip4/144.24.192.222/tcp/4337/...` car certains transports libp2p ne resolvent pas dns4)
Protocol version: `v5`

## Infra (node1 — Oracle ARM Marseille)

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

## Infra (node2 — Oracle ARM Marseille)

| Element | Valeur |
|---------|--------|
| Provider | Oracle Cloud Always Free ARM |
| Shape | VM.Standard.A1.Flex (1 OCPU, 6 GB RAM) |
| Region | eu-marseille-1 |
| OS | Ubuntu 22.04 ARM64 |
| Role | Validateur uniquement (pas d'API/site publics) |
| Pare-feu | `ufw` + `fail2ban` |

## Infra (node3 — IONOS Berlin x86_64)

| Element | Valeur |
|---------|--------|
| Provider | IONOS Cloud VPS |
| Shape | VPS 2-2-80 (2 vCore, 2 GB RAM, 80 GB NVMe SSD) |
| Region | Allemagne (Berlin) — eu-de-berlin |
| OS | Ubuntu 24.04.4 LTS x86_64 |
| Role | Validateur uniquement (pas d'API/site publics) |
| Swap | **6 GB obligatoire** (sinon OOM au build cargo et runtime instable) |
| Pare-feu IONOS | TCP entrant 22/80/443/4337 (panel "My firewall policy") |
| Pare-feu VM | `ufw` (22/80/443/4337) + `fail2ban` (jail `sshd`) |
| SSH | port 22, `PermitRootLogin prohibit-password`, password auth desactive, key-only |
| User systemd | `ubuntu` (sudo NOPASSWD), comme node1/node2 |

> **Heads-up x86_64 vs ARM** : le binaire `curs3d` n'est PAS portable entre
> ARM (node1/2) et x86_64 (node3). Build natif sur chaque architecture.
> Compilation initiale sur node3 : ~15-25 min (vs 5-8 min sur Oracle ARM 6 GB)
> a cause des 2 GB de RAM + swap. Privilegier les builds incrementaux
> (`cargo build --release` apres un `git pull`).

> **Linker quirk historique x86_64** (resolu le 2026-05-05) : wasmer 5.0.6 +
> rustc recent declenchait `rust-lld: undefined symbol __rust_probestack`
> sur x86_64. Resolu en bumpant wasmer 5 -> 7.1.0 (commit `759d600`). Migration
> API mineure : 6 items deplaces vers `wasmer::sys::*`, et wasmparser 0.246
> (bundled dans wasmer 7) yields `Imports<'a>` (group compact) au lieu de
> `Import` direct. Tous les nodes (ARM + x86_64) tournent en wasmer 7.

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

/var/lib/curs3d/                            # data dir (redb, p2p_identity.pb)
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
ssh curs3d-node2
ssh curs3d-node3
```

`~/.ssh/config` :
```
Host curs3d-node1
    HostName 144.24.192.222
    User ubuntu
    IdentityFile ~/.ssh/id_ed25519_server

Host curs3d-node2
    HostName 84.235.238.213
    User ubuntu
    IdentityFile ~/.ssh/id_ed25519_server

Host curs3d-node3
    HostName 31.70.70.62
    User ubuntu
    IdentityFile ~/.ssh/id_ed25519
```

> **Note clefs SSH** : node1 et node2 utilisent `id_ed25519_server` (clef Oracle
> historique). node3 utilise `id_ed25519` (clef ed25519 par defaut du Mac).
> Les deux clefs sont sur les machines : `id_ed25519` est aussi sur node1 / node2
> via `authorized_keys` pour faciliter les operations cross-host.

SSH du serveur durci (identique sur les 3 nodes) :
- `PermitRootLogin no` (ou `prohibit-password` sur node3, root par cle uniquement)
- `PasswordAuthentication no`
- `MaxAuthTries 3`
- `AllowUsers ubuntu` (+ `root` sur node3 pour ops d'urgence)
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

> **Recommandation par defaut**: la voie longue-duree, zero-downtime, c'est
> **(1) cross-compile depuis le Mac → (2) staggered rollout via
> `deploy/scripts/rollout-staggered.sh`**. Voir les deux sections ci-dessous.
> Les anciennes voies (`ssh + git pull + cargo build` sur chaque VPS, ou
> `full-rollout.sh` qui restart les 3 nodes en simultane) restent documentees
> pour les cas particuliers (hardfork, wipe, premier bootstrap).

### Voie 1 — Cross-compile depuis le Mac (recommandee)

```bash
# One-time setup (Mac)
brew install --cask orbstack    # ou Docker Desktop
cargo install cross
rustup target add aarch64-unknown-linux-gnu x86_64-unknown-linux-gnu

# A chaque release (ces 2 commandes peuvent tourner en parallele)
cross build --release --target aarch64-unknown-linux-gnu   # node1 + node2 (Oracle ARM)
cross build --release --target x86_64-unknown-linux-gnu    # node3 (IONOS x86)
```

Avantages :
- Pas besoin de garder une copie du source code synchronisee sur chaque VPS.
- Pas de toolchain Rust a maintenir sur les VPS (libere ~3 GB et evite que
  les builds OOM sur les 2 GB de RAM de node3).
- Build deterministe : meme binaire, meme empreinte, deployable plusieurs fois.
- `curs3d` ARM et x86 dans le meme commit, traceable dans `target/`.

### Voie 2 — Staggered rollout (zero downtime)

`deploy/scripts/rollout-staggered.sh` push les binaires pre-build (Voie 1)
puis redemarre **un seul node a la fois**, dans l'ordre `node3 → node2 → node1`,
avec une fenetre d'observation de 10 min entre chaque (configurable).

Le mesh mutuel (`deploy/systemd/curs3d-node{1,2,3}.service` listent les 2 autres
comme `--bootnode`) garantit que pendant qu'un node redemarre, les 2 autres
continuent a produire et finaliser des blocs (la finalite BFT tient avec 2/3
de stake online). Si le redemarrage d'un node echoue, le script avorte AVANT
de toucher les nodes suivants.

```bash
# Apres cross-compile :
./deploy/scripts/rollout-staggered.sh

# Variantes :
OBSERVE_SECS=300 ./deploy/scripts/rollout-staggered.sh                       # observation plus courte (5 min)
ORDER='curs3d-node3 curs3d-node2 curs3d-node1' ./deploy/scripts/rollout-staggered.sh   # ordre custom
```

Health gates entre chaque node :
1. La chain doit continuer a avancer sur les **deux autres nodes** via leurs
   `/api/status` locaux SSH — preuve que le cluster produit toujours meme si
   le RPC public node1 est temporairement redemarre.
2. Le node redemarre doit repondre `/api/status` avec `height>0` ET
   `peer_count>=2` — preuve qu'il a rejoint le mesh.
3. Si l'un des deux echoue, `die` et arret immediat avant de continuer.

**Quand NE PAS utiliser staggered** :
- Storage format change (sled <-> redb, redb v1 <-> v2, etc.). Les nodes
  ne peuvent pas lire la DB de l'ancienne version, donc une migration
  coordonnee + wipe est requise → utiliser `full-rollout.sh --wipe`.
- Hardfork du protocole (consensus, gossipsub topic, signature scheme,
  block format). Mixed-version peers diverge silencieusement → coordonner
  un cold restart.

### Voie 2b — Full rollout coordonne avec wipe

Utiliser uniquement pour storage-format change, hardfork, genesis regen, ou
bootstrap initial. Le script stoppe les 3 nodes, installe le binaire deja build
sur chaque VPS, wipe `/var/lib/curs3d` en preservant `p2p_identity*`, puis
redemarre les 3 nodes ensemble.

```bash
./deploy/scripts/full-rollout.sh --wipe
```

Gates obligatoires du script :
1. Les 3 `/api/status` locaux doivent converger au meme `height` + meme
   `latest_hash` a `h >= 4`, avec `peer_count >= 2` partout.
2. Le RPC public doit servir cette nouvelle chain (`eth_blockNumber >= 4`).
3. La finalite doit s'activer sur les 3 nodes apres le premier epoch boundary.

Apres un wipe, toutes les adresses EVM de testnet sont a redeployer :
```bash
cd contracts
./deploy.sh --force
```

### Voie 3 — Old school (build par VPS — fallback)

Conserve pour les cas de debug ou quand cross-compile n'est pas dispo.

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
> incrementaux : ~1 min. Sur node3 IONOS x86 (2 GB RAM + swap 6 GB) : 15–25 min.

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

## Ajout de validateur post-genesis (procedure node3, deja jouee)

CURS3D supporte l'**activation dynamique de validateur** : un wallet quelconque
qui detient au moins `DEFAULT_MIN_STAKE = 1000 CUR` (1_000_000_000 microtokens)
et envoie une transaction `Stake` devient validateur a partir de l'epoch
suivante (cf. `src/consensus/mod.rs::active_validators` ligne 469 et
`src/core/chain.rs` ligne 2927). **Pas besoin de hardfork** pour ajouter un
3eme validateur.

### Discovery long terme (plus d'edition manuelle de tous les nodes)

Depuis le patch peerstore/sync-gate, l'ajout d'un validateur suit le modele des
chains matures : seed/bootnode minimal, peer exchange, peerstore persistant,
puis state catch-up avant production.

Concretement :

- Un nouveau node doit connaitre **au moins un** bootnode reachable au premier
  demarrage (`--bootnode /ip4/.../tcp/4337/p2p/...` ou `/dns4/...`).
- Le nouveau node doit publier son adresse WAN stable avec `--public-addr`; le
  binaire convertit cette adresse en `/p2p/<PeerId>` et l'annonce dans les
  `HeightAnnounce` signes.
- Les peers qui verifient l'annonce (genesis identique, protocol version
  identique, signature ML-DSA valide) enregistrent ces adresses dans
  `/var/lib/curs3d/peerstore.json`.
- Au prochain restart, chaque node relit `peerstore.json` et redial les peers
  connus automatiquement. Il n'est donc plus necessaire de SSH sur node1/node2
  pour ajouter manuellement `--bootnode` a chaque nouveau validator.
- Le sync gate bloque la production tant que le node n'a pas observe un tip
  verifie stable pendant 3 ticks de production. Un node late-join ou restart ne
  doit donc plus proposer un bloc sur un tip stale avant d'etre catch-up.

Implication ops : conserver 2-3 bootnodes stables dans les units systemd reste
utile pour le cold start, mais le mesh vivant s'auto-entretient ensuite via
`peerstore.json`.

### Procedure complete (executee le 2026-05-05 pour node3)

#### 1. Provisionner le VPS IONOS

VPS commande chez IONOS (compte `agencenetstrategy@gmail.com`) :
- Shape VPS 2-2-80 (2 vCore, 2 GB RAM, 80 GB NVMe)
- Ubuntu 24.04 x86_64
- Centre de calcul Allemagne (Berlin)
- Stratégie de pare-feu IONOS : autoriser TCP entrant 22, 80, 443, 4337

#### 2. Bootstrap du VPS (cloud-init ou script manuel)

Le bootstrap configure : hostname, timezone, user `ubuntu` sudo NOPASSWD,
clefs SSH, swap 6 GB, ufw, fail2ban, hardening sshd, paquets de base.
Voir le script `/tmp/curs3d-vps-bootstrap.sh` (sauvegarde sur le Mac
operateur, a re-executer si besoin de re-provisionner).

```bash
# Apres reinstall image IONOS, login KVM en root puis :
bash /tmp/curs3d-vps-bootstrap.sh   # ou via cloud-init "Donnees d'utilisateur"
```

#### 3. Build le binaire sur node3 (x86_64 native)

```bash
ssh curs3d-node3
# Rsync depuis le Mac operateur :
# rsync -az --exclude target ~/Desktop/Web3/curs3d/ curs3d-node3:/home/ubuntu/curs3d/

# Sur node3 :
. ~/.cargo/env
RUSTUP_TOOLCHAIN=nightly cargo build --release   # ~15-25 min sur 2 GB + swap
```

#### 4. Recuperer le genesis depuis node1

```bash
# Depuis le Mac (relais) :
ssh curs3d-node1 "sudo cat /etc/curs3d/genesis.public-testnet.json" > /tmp/genesis.json
scp /tmp/genesis.json curs3d-node3:/tmp/
ssh curs3d-node3 "sudo install -m 644 /tmp/genesis.json /etc/curs3d/genesis.public-testnet.json"

# Verifier hash identique :
ssh curs3d-node1 "sudo sha256sum /etc/curs3d/genesis.public-testnet.json"
ssh curs3d-node3 "sha256sum /etc/curs3d/genesis.public-testnet.json"
# Doit afficher 702be65951ec6b29efb157fe96f8aba0baf14fc24bfab3926976d2b8e25ca1c1
```

#### 5. Generer le wallet validateur SUR node3

**IMPORTANT : Argon2id m=64 MB t=3 p=4 -> generer le wallet sur la machine cible
pour eviter les soucis de performance/memoire au demarrage. Ne JAMAIS
cross-compiler le wallet.**

```bash
ssh curs3d-node3
sudo install -d /etc/curs3d
PASSWORD="$(openssl rand -base64 32)"
echo "$PASSWORD" | sudo tee /etc/curs3d/validator.password >/dev/null
sudo chmod 600 /etc/curs3d/validator.password

cd /home/ubuntu/curs3d
./target/release/curs3d wallet \
  --output /tmp/validator.json \
  --password-file /etc/curs3d/validator.password
sudo install -m 644 /tmp/validator.json /etc/curs3d/validator.json
rm /tmp/validator.json

# Recuperer l'address pour la suite :
./target/release/curs3d info \
  --wallet /etc/curs3d/validator.json \
  --password-file /etc/curs3d/validator.password \
  --json | jq -r .address
# Format : CUR... (40 hex avec checksum EIP-55-like)
```

#### 6. Configurer systemd + demarrer node3 en mode validateur

Adapter `deploy/scripts/setup-node.sh` (qui hardcode `User=ubuntu`, OK pour
node3) avec :

```bash
NODE_NUM=3
PUBLIC_IP=31.70.70.62
BOOTNODE='/dns4/api.curs3d.fr/tcp/4337/p2p/12D3KooWLttF4EJ1SjiLEiXvJ1yqmJawLafv47r55T5xzSt1GHn2'

# Sur node3 :
bash /home/ubuntu/curs3d/deploy/scripts/setup-node.sh ${NODE_NUM} ${PUBLIC_IP} ${BOOTNODE}
sudo systemctl enable --now curs3d
sudo journalctl -u curs3d -f
```

Le node3 va se synchroniser depuis node1 (telecharger tous les blocs +
state). Le sync prend quelques minutes selon la taille de la chain.

#### 7. Verifier la sync

```bash
ssh curs3d-node3 "curl -s http://localhost:8080/api/status | jq '.data | {height, finalized_height, active_validators}'"
# Doit afficher la meme height que node1 / node2 (a quelques blocs pres)
```

#### 8. Funder + staker l'address de node3 (depuis le faucet ou un wallet user)

Le wallet de node3 est cree avec balance = 0. Pour devenir validateur,
il faut au minimum `DEFAULT_MIN_STAKE = 1000 CUR` + des fees + une marge
de sec. Recommande : funder avec **2000 CUR** depuis le faucet (qui en a
2_000_000), puis stake **1500 CUR**.

Depuis le Mac operateur (qui a l'access node1) :

```bash
NODE3_ADDR="<address renvoyee a l'etape 5>"
ssh curs3d-node1 "sudo /usr/local/bin/curs3d send \
  --wallet /etc/curs3d/faucet.json \
  --password-file /etc/curs3d/faucet.password \
  --to ${NODE3_ADDR} \
  --amount 2000000000 \
  --rpc 127.0.0.1:9545"  # 2000 CUR en microtokens
```

Sur node3, lancer la transaction Stake (signee par le wallet validateur) :

```bash
ssh curs3d-node3 "/usr/local/bin/curs3d stake \
  --wallet /etc/curs3d/validator.json \
  --password-file /etc/curs3d/validator.password \
  --amount 1500000000 \
  --rpc 127.0.0.1:9545"  # 1500 CUR de stake
```

#### 9. Attendre l'epoch boundary

`epoch_length = 32` blocs (env. 5 minutes a 10s/bloc). Le champ
`validator_active_from_height` est positionne a la prochaine `epoch_start_height`
quand le stake passe au-dessus du minimum. Verifier :

```bash
curl -s https://api.curs3d.fr/api/status | jq '.data | {epoch, epoch_start_height, height}'
curl -s https://api.curs3d.fr/api/validators | jq 'length'
# Apres l'epoch boundary, validators count passe de 2 a 3
curl -s https://api.curs3d.fr/api/account/${NODE3_ADDR} | jq '.data | {staked_balance, validator_active_from_height}'
```

#### 10. Verifier que node3 produit des blocs

Apres activation, node3 va proposer des blocs aux hauteurs ou
`slot_leader(h, validators) == node3_address`. Verifier :

```bash
ssh curs3d-node3 "sudo journalctl -u curs3d -n 100 | grep -i 'produced\|proposed'"
```

## Hardfork v4 → v5 (procedure deja jouee 2026-04-30, pour reference)

Le hardfork v5 swap `pqcrypto-dilithium 0.5.0` (round 3, C bindings) ->
`ml-dsa = 0.1.0-rc.9` (FIPS-204 final, pure Rust). Memes addresses cle pub
(2592 B), signing key passe de 4864 B a 32 B (seed FIPS-204).

1. Coordonner l'arret simultane des nodes (`systemctl stop curs3d`).
2. Build le binaire v5 sur chaque node : `RUSTUP_TOOLCHAIN=nightly cargo build --release`.
3. Wipe complet de la chain DB sur chaque node :
   `sudo rm -f /var/lib/curs3d/curs3d.redb`
   (garder `p2p_identity.pb`).
4. Regenerer wallets validateurs ET faucet sous v5 (les anciens wallets
   v4 ont des keypairs incompatibles avec ML-DSA-87) :
   ```bash
   curs3d wallet --output /etc/curs3d/validator.json --password-file /etc/curs3d/validator.pass
   curs3d wallet --output /etc/curs3d/faucet.json    --password-file /etc/curs3d/faucet.pass
   ```
5. Regenerer le genesis avec les nouveaux wallets (sur la machine operateur),
   le rsync sur tous les nodes (`/etc/curs3d/genesis.public-testnet.json`).
6. Deployer le binaire v5 et redemarrer les nodes.
7. Verifier :
   ```bash
   curl -s https://api.curs3d.fr/api/status | jq '.data | {protocol_version, active_validators, height, finalized_height, genesis_hash}'
   # protocol_version=5
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

### Sync timeout / forks au boot (`RequestBlocks`)
Corrige dans le code courant : le node ne produit plus pendant la fenetre de
demarrage/sync, les broadcasts echoues sont remis en file puis retentes, les
`BlockResponse` stale mais contigus sont acceptes, et une divergence de
checkpoint escalade vers state snapshot au lieu de rester bloquee en retry.

### MetaMask refuse la chain
- Verifier `curl -s -X POST https://api.curs3d.fr/eth -d '{"jsonrpc":"2.0","method":"eth_chainId","id":1}'` retourne bien `0x6b4ed968`.
- Si `502/504` : la location nginx `/eth` n'est pas configuree (cf. section nginx ci-dessus) ou le node ecoute sur autre chose que `127.0.0.1:8080`.
- CORS : la reponse doit inclure `Access-Control-Allow-Origin` (`CURS3D_API_ALLOW_ORIGIN`).

### Wallet UI affiche "signature rejected"
Ce n'est plus le comportement attendu depuis le hardfork v5 : la wallet UI signe
en ML-DSA-87 via le bundle WASM et le node verifie les memes bytes. Si ce message
revient, verifier que le node deploye est bien en protocol v5+, que le bundle
`/wallet-wasm/curs3d_wallet_wasm.js` est celui du build courant, puis tester une
transaction native via `POST /api/tx/submit` avec la payload exacte envoyee par
le navigateur.

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

### Cross-compile : `cross build` echoue avec `detected conflict: lib/rustlib/.../libaddr2line-*.rlib`

Symptome (vu depuis le Mac):

```
error: failed to install component: 'rust-std-aarch64-unknown-linux-gnu',
detected conflict: 'lib/rustlib/aarch64-unknown-linux-gnu/lib/libaddr2line-XXXX.rlib'
Error:
   1: `rustup target add aarch64-unknown-linux-gnu --toolchain nightly-x86_64-unknown-linux-gnu` failed
```

Cause : la toolchain `nightly-x86_64-unknown-linux-gnu` du Mac contient
des fichiers `rust-std` orphelins (sur disque mais plus dans le manifest
rustup), typiquement laisses par un `rustup update` partiellement avorte
ou une rotation rapide de la rolling nightly.

**Recovery propre (a essayer d'abord)** :

```bash
rustup toolchain uninstall nightly-x86_64-unknown-linux-gnu
rustup toolchain install   nightly-x86_64-unknown-linux-gnu --force-non-host --profile minimal
rustup target add aarch64-unknown-linux-gnu --toolchain nightly-x86_64-unknown-linux-gnu
rustup target add x86_64-unknown-linux-gnu --toolchain nightly-x86_64-unknown-linux-gnu
RUSTUP_TOOLCHAIN=nightly cross build --release --target aarch64-unknown-linux-gnu
```

**Last resort (si la voie propre echoue toujours avec le meme conflit)** :
si rustup retourne "does not have target X installed" mais le fichier
`libaddr2line-*.rlib` existe encore sur disque, le manifest et le filesystem
sont desynchronises. Supprimer le repertoire orphelin manuellement :

```bash
# A reserver aux cas ou rustup uninstall/install ne suffit pas
rm -rf ~/.rustup/toolchains/nightly-x86_64-unknown-linux-gnu/lib/rustlib/aarch64-unknown-linux-gnu
rustup target add aarch64-unknown-linux-gnu --toolchain nightly-x86_64-unknown-linux-gnu
```

Cette etape ne doit pas devenir routine ; si tu y reviens souvent c'est
qu'une autre cause sous-jacente (autre outil ecrivant dans `~/.rustup`,
filesystem en read-only intermittent, etc.) merite une investigation.
