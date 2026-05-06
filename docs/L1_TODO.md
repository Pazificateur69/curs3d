# CURS3D L1 TODO

Etat **2026-05-04**: testnet public live sur https://curs3d.fr,
**1 validateur actif** (node1). Ce fichier distingue ce qui est execute
dans le code et ce qui reste pour approcher un niveau L1 serieux.

## Live

- Site: https://curs3d.fr
- API: https://api.curs3d.fr/api/status
- Explorer: https://explorer.curs3d.fr
- Faucet UI: https://curs3d.fr/faucet (Cloudflare Turnstile)
- API docs (OpenAPI 3.1): https://curs3d.fr/api
- WebSocket: wss://api.curs3d.fr/ws
- Status (Grafana): https://status.curs3d.fr/
- Status (Uptime-Kuma): https://status.curs3d.fr/status/
- P2P Bootnode: 144.24.192.222:4337
- TLS: Let's Encrypt, auto-renew (curs3d.fr + api + explorer + status)
- Hosting: Oracle Cloud ARM Free Tier (1 OCPU / 6 GB, Ubuntu 22.04), eu-marseille-1
- Node1 (bootstrap+API): 144.24.192.222 — `ssh curs3d-node1` — `CURe1Fa551B3f0524EfD8d0673cdBF9fD0e199458c5`
- Node2 (84.235.238.213, validateur `CURdC1ecceD4f12Cb3E34BD0d43E72d6D04fC4823dd`): **arrete** en attendant le fix consensus slot-leader
- Node3/4: reportes
- Faucet: `CUR34cafc74B750C0e0150877e99cd27D77C6c4fC44`
- Chain ID: `curs3d-public-testnet`
- Genesis hash: `8a58508589b2e0e2caf760eaed18500c262200fbdff3509faedfc1d9589efb18`
- Runbook complet: `deploy/DEPLOY_RUNBOOK.md`
- Toolchain: **Rust nightly** requis (multiaddr 0.18.2 vs stable >=1.94)
- IMPORTANT: les wallets doivent etre crees sur le serveur de production a cause de Argon2id

## Execute

- Consensus:
  - BFT PoS avec finalite 2/3 du stake total
  - slashing par preuve d equivocation liee a la meme hauteur
  - fork choice revalide contre l etat parent (heaviest chain)
  - reorg bloque sous finalite
  - votes de finalite rejetes pour hash inconnu ou non canonique
  - finalite evaluee sur validator set gele par epoch (32 blocs)
  - jail de 64 blocs apres slashing
- Protocole:
  - chain_id dans les transactions
  - version de protocole dans les blocs
  - topic reseau derive de chain_id + version
  - upgrade de version verifiee a la validation de bloc
  - filtrage des peers incompatibles par version
- Crypto:
  - CRYSTALS-Dilithium Level 5 (NIST FIPS 204)
  - SHA-3 Keccak-256 + double-hash blocs
  - Merkle root, proof, verify
  - AES-256-GCM + Argon2 wallets
- Etat:
  - persistance et rebuild de l etat canonique (accounts + contracts + receipts)
  - redb avec 10 tables, schema v4, auto-migration
  - snapshots complets avec chunks Merkle verifies
  - snapshots bases sur point finalise quand possible
  - preuves Merkle par chunk de snapshot
  - preuves d etat: account proof et storage proof
- VM:
  - execution Wasm reelle via Wasmer 5 + Cranelift
  - 11 host functions deterministes
  - metering instruction par instruction via fuel middleware
  - rejet des contrats avec boucles non fueles
  - gas schedule complet (base_tx, deploy, call, storage, logs, bytes, loop)
  - receipts enrichis avec gas details complets
- Fees et mempool:
  - EIP-1559 base_fee_per_gas dynamique (cible 50% gas target)
  - max_fee_per_gas / max_priority_fee_per_gas
  - base fee brulee, priority fee au proposeur
  - refunds de gas inutilise
  - block_gas_limit 10M applique
  - budget gas mempool global + par compte
  - remplacement strict, gap nonce 32, eviction
  - estimation dry-run via API et RPC
- Surface operateur:
  - **27 endpoints HTTP** + WebSocket (/ws) + Ethereum-compatible JSON-RPC (`POST /eth`),
    rate limiting IP (60 GET/min, 10 POST/min)
  - OpenAPI 3.1 publie (`website/api/openapi.json`, rendu Stoplight Elements a `/api`)
  - faucet POST /api/faucet/request (100 CUR, cooldown 1h address+IP) + UI a `/faucet`
    avec **Cloudflare Turnstile** (verifier `curs3d-captcha.service`)
  - auth optionnelle sur API (bearer token) et RPC
  - CORS configurable (bloque si absent)
  - body limit 1MB, 128 max connexions HTTP, 64 max WebSocket
  - CLI: node (avec `--reset-p2p-identity`), wallet, info, send, stake,
    unstake, deploy-token, token-transfer, status, genesis (multi
    `--validator-wallet`), bootnode-address
  - SDKs: JavaScript/TypeScript (`@curs3d/sdk`), Python (`curs3d`),
    Rust contract SDK (5 examples)
- Tokens:
  - Standard CUR-20 natif: deploy, transfer, approve, transferFrom
  - Token registry dans Blockchain struct
  - 3 endpoints API: /api/tokens, /api/token/:addr, /api/token/:addr/balance/:owner
- Gouvernance:
  - Propositions on-chain par validateurs
  - Vote pondéré par stake, quorum 50%, approbation 67%
  - Exécution automatique après délai
  - 2 endpoints API: /api/governance/proposals, /api/governance/proposal/:id
- Light client:
  - Module light/mod.rs: header-only sync, vérification Merkle proofs
- Reseau:
  - libp2p 0.54: Gossipsub + mDNS + noise + yamux
  - 10 types de messages (dont state sync)
  - sync batch 50 blocs, 15s timeout, 3 retries
  - state sync par snapshot chunke avec preuves Merkle
  - HeightAnnounce signe par validateurs
  - P2P rate limiting par peer avec bans escaladants
  - Peer scoring: reputation comportementale, decay, ban automatique sous seuil
  - WebSocket event broadcast (new_block, new_transaction, finality)
- Infra (live sur node1):
  - Mono-validateur production (cf. "Open" / Known bugs ci-dessous)
  - Docker multi-stage + docker-compose + healthcheck
  - nginx TLS + WebSocket reverse proxy + website serving (TLS Mozilla
    intermediate, OCSP stapling, headers durcis : CSP, HSTS preload,
    X-Frame-Options DENY, Referrer-Policy, Permissions-Policy)
  - systemd hardened (`User=curs3d`, `EnvironmentFile=/etc/curs3d/secrets.env`,
    `ProtectSystem=full`, `NoNewPrivileges`, `Restart=always`)
  - Healthcheck v2 cron */2 min avec **alertes Discord** via
    `/etc/curs3d/alerts.env`
  - Faucet captcha verifier (`curs3d-captcha.service`, 127.0.0.1:8090)
  - Backups off-host **restic → Backblaze B2** toutes les 6h
    (`curs3d-backup.timer`, notifications Discord)
  - SSH durci (no root, MaxAuthTries 3, key-only), `fail2ban` actif
  - Stack monitoring Docker a `status.curs3d.fr` : Prometheus +
    Grafana + Uptime-Kuma + node-exporter (4 services)
  - Scripts deploy : `deploy.sh`, `add-node.sh`, `setup-node.sh`,
    `init-localnet.sh`, `curs3d-healthcheck.sh`, `curs3d-backup.sh`,
    `curs3d-captcha-verify.py`
  - CI GitHub Actions sur **nightly**: check, test, clippy `-D warnings`,
    fmt, `cargo audit` (policy `.cargo/audit.toml`)
  - Benchmarks (criterion, 9 cibles) + fuzzing (cargo-fuzz, 5 cibles)
  - Site web : landing, docs, examples, whitepaper, explorer,
    governance, tokenomics, stack, faucet, run-validator, api (Stoplight)
- Crypto:
  - Domain separation (sha3_hash_domain) pour tous les usages
  - Adresses checksummed EIP-55 (checksum_address, verify_checksum_address)
- State:
  - Sparse Merkle Trie 256-bit (module pret, preuves O(log n))
  - Epoch settlement: rewards + inactivity penalties appliques a chaque epoch
- Tests:
  - **150 tests**: consensus (15), block (2), blocktree (6), chain (28), transaction (5), dilithium (2), hash (7), governance (8), light (3), network (9), storage (7), token (10), trie (9), vm (10), wallet (5)
- Securite (audit interne 2026-04-23 + audit pass 2026-05-04 — commit `c015ee3`):
  - Gouvernance: vote par stake snapshot a la creation de la proposition (anti double-vote)
  - Deserialisation bornee sur tous les messages P2P (anti OOM, limite 16 MB)
  - Elimination de tous les Box::leak (zero memory leak)
  - Fee market: base_fee >= 1 en permanence (anti spam gratuit)
  - Timestamp strictement monotone entre blocs
  - Nonce floor: rejet des transactions avec nonce < account.nonce
  - Argon2 renforce: m=64MB, t=3, p=4 (wallet encryption)
  - Limite de profondeur de reorg: max 64 blocs
  - Validation des index de chunks snapshot (anti storage bloat)

## Bugs ouverts (priorite haute)

- **Consensus slot-leader manquant** — `src/consensus/mod.rs` n'elit
  pas un proposeur unique par slot ; ajouter
  `slot_leader(height, validator_set)` deterministe et pondere stake,
  gater la production dans `src/network/mod.rs`. Bloque le retour de
  node2 (sinon forks toutes les 10 s).
- **RequestBlocks sync timeout** — receive loop dans
  `src/network/mod.rs` time out avant l'arrivee des batches malgre une
  connectivite peer valide. A investiguer.
- **State-root divergence apres certains restarts** — logging diagnostique
  en place (dump des leaves), root cause non identifiee.
- **Cross-compile Mac → ARM** — `cross` installe, requiert Docker
  Desktop / OrbStack actif.

## Priorite 1

- Audit externe: consensus, VM, crypto, reseau
- Re-activer node2 puis provisionner node3 (Hetzner) une fois le
  slot-leader fix.
- ~~Peer scoring, banlist, anti-spam par peer/message~~ FAIT: PeerRateLimiter + PeerScorer avec reputation et bans comportementaux
- ~~Arbre d etat explicite (MPT ou Verkle)~~ FAIT: SparseMerkleTrie 256-bit (module pret, migration state root planifiee via protocol upgrade)
- State sync avec reprise partielle et checkpoints connus
- Pruning, snapshots incrementaux, mode archival
- ~~Epoch rewards et inactivity leak~~ FAIT: EpochSettlement avec rewards proportionnels au stake, inactivity penalties avec grace period de 2 epochs
- Observabilite: Prometheus endpoint /api/metrics deja present, structured logs via tracing

## Priorite 2

- ~~Receipts indexes + filtres de logs~~ FAIT: receipts + IndexedLogEntry + LogFilter + GET /api/logs
- VM: limites memoire/pages, politique trap/revert/abort
- Contract SDK (Rust + AssemblyScript)
- ~~Light client protocol~~ FAIT: module light/mod.rs
- API: pagination robuste, debug/trace endpoints

## Priorite 3

- Fee estimation multi-percentiles
- Mempool avec classes de priorite
- ~~Activation d upgrade avec compat matrix~~ FAIT: gouvernance on-chain avec execution automatique
- Signaux pre-fork
- Migrations d etat versionnees

## Priorite 4

- ~~Domain separation partout~~ FAIT: sha3_hash_domain() avec prefixe unique par usage
- ~~Adresses checksummed~~ FAIT: EIP-55 style via checksum_address() + verify_checksum_address()
- Batch verification signatures
- ~~Fuzzing, soak tests, chaos testing~~ FAIT (fuzzing): 5 cibles cargo-fuzz
- ~~CI: fuzz, MIRI, sanitizers~~ PARTIEL: fuzzing targets prêts, pas encore en CI
- ~~Benchmarks publics~~ FAIT: 9 benchmarks criterion

## Non-negociable avant mainnet

- audit externe consensus + VM + crypto
- ~~peer scoring et anti-spam~~ FAIT
- arbre d etat explicite (pas juste sort+hash)
- tests reseau de longue duree
- tests de reorg/finalite sous partitions
- fuzzing tx, block, snapshot et RPC
- benchmarks sync, mempool et execution
