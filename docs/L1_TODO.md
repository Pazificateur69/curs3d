# CURS3D L1 TODO

Etat: **testnet public live** sur https://api.curs3d.fr. Ce fichier distingue ce qui est execute dans le code et ce qui reste pour approcher un niveau L1 serieux.

## Live

- Site: https://curs3d.fr
- API: https://api.curs3d.fr/api/status
- Explorer: https://explorer.curs3d.fr
- WebSocket: wss://api.curs3d.fr/ws
- Faucet: POST https://api.curs3d.fr/api/faucet/request
- P2P Bootnode: 144.24.192.222:4337
- TLS: Let's Encrypt, auto-renew (api + explorer + curs3d.fr)
- Hosting: Oracle Cloud ARM Free Tier (2x 1 OCPU, 6 GB RAM, Ubuntu 22.04), eu-marseille-1
- Node1 (bootstrap+API): 144.24.192.222 — `ssh curs3d-node1` — CURe1Fa551B3f0524EfD8d0673cdBF9fD0e199458c5
- Node2 (validator): 84.235.238.213 — `ssh curs3d-node2` — CURdC1ecceD4f12Cb3E34BD0d43E72d6D04fC4823dd
- Faucet: CUR34cafc74B750C0e0150877e99cd27D77C6c4fC44
- Node3-4: en attente capacite ARM Oracle (wallets a creer sur le serveur)
- Bootnode PeerId: 12D3KooWGy9BLopUe6CmnxuFgk5pCou8Kj5R1DXa9XLga9MD63va
- SSH key: id_ed25519_server
- Runbook complet: deploy/DEPLOY_RUNBOOK.md
- IMPORTANT: les wallets doivent etre crees sur le serveur (pas cross-compiles) a cause de Argon2id

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
  - sled avec 10 arbres, schema v4, auto-migration
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
  - 20 endpoints HTTP + WebSocket (/ws) avec rate limiting IP (60 GET/min, 10 POST/min)
  - faucet testnet POST /api/faucet/request (100 CUR, cooldown 1h par adresse, persistant)
  - auth optionnelle sur API (bearer token) et RPC
  - CORS configurable (bloque si absent)
  - body limit 1MB, 128 max connexions HTTP, 64 max WebSocket
  - CLI complet: node, wallet, info, send, stake, unstake, deploy-token, token-transfer, status, genesis, bootnode-address
  - SDKs: JavaScript/TypeScript (@curs3d/sdk) et Python (curs3d)
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
- Infra:
  - Multi-node testnet: 4 validateurs BFT sur Oracle Cloud ARM (2 actifs, 2 en attente capacite)
  - Consensus multi-validateur avec propagation P2P et sync automatique
  - Docker multi-stage + docker-compose + healthcheck
  - nginx TLS + WebSocket reverse proxy + website serving
  - systemd hardened: Restart=always, WatchdogSec=120, StartLimitAction=reboot
  - Healthcheck cron toutes les 2 min avec auto-restart
  - Script de deploiement deploy/scripts/deploy.sh + deploy/scripts/add-node.sh
  - CI GitHub Actions: check, test, clippy (0 warnings), fmt
  - Benchmarks (criterion) + fuzzing targets (cargo-fuzz, 5 cibles)
  - Site web: 8 pages (landing, docs, exemples, whitepaper, explorer, governance, tokenomics, stack)
- Crypto:
  - Domain separation (sha3_hash_domain) pour tous les usages
  - Adresses checksummed EIP-55 (checksum_address, verify_checksum_address)
- State:
  - Sparse Merkle Trie 256-bit (module pret, preuves O(log n))
  - Epoch settlement: rewards + inactivity penalties appliques a chaque epoch
- Tests:
  - 129 tests: consensus (15), block (2), blocktree (6), chain (28), transaction (5), dilithium (2), hash (7), governance (8), light (3), network (9), storage (7), token (10), trie (9), vm (10), wallet (5)
- Securite (audit interne 2026-04-23, fixes 2026-04-24):
  - Gouvernance: vote par stake snapshot a la creation de la proposition (anti double-vote)
  - Deserialisation bornee sur tous les messages P2P (anti OOM, limite 16 MB)
  - Elimination de tous les Box::leak (zero memory leak)
  - Fee market: base_fee >= 1 en permanence (anti spam gratuit)
  - Timestamp strictement monotone entre blocs
  - Nonce floor: rejet des transactions avec nonce < account.nonce
  - Argon2 renforce: m=64MB, t=3, p=4 (wallet encryption)
  - Limite de profondeur de reorg: max 64 blocs
  - Validation des index de chunks snapshot (anti storage bloat)

## Priorite 1

- Audit externe: consensus, VM, crypto, reseau
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
