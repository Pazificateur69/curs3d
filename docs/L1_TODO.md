# CURS3D L1 TODO

Etat: vivant. Ce fichier distingue ce qui est deja execute dans le code et ce qui reste pour approcher un niveau L1 serieux.

## Execute

- Consensus:
  - slashing par preuve liee a la meme hauteur
  - fork choice revalide contre l etat parent
  - reorg bloque sous finalite
  - votes de finalite rejetes pour hash inconnu ou non canonique
  - finalite evaluee sur validator set gele par epoch
- Protocole:
  - `chain_id` dans les transactions
  - version de protocole dans les blocs
  - topic reseau derive de `chain_id + version`
  - upgrade de version verifiee a la validation de bloc
- Etat:
  - persistance et rebuild de l etat canonique `accounts + contracts + receipts`
  - snapshots complets applicables sur redemarrage
  - snapshots bases sur point finalise quand possible
  - preuves Merkle par chunk de snapshot
- VM:
  - execution Wasm reelle
  - host functions deterministes pour storage et logs
  - ABI memoire pour `input`, `storage`, `logs`
  - estimation de gas par operateurs Wasm
  - rejet des contrats avec boucles non fueles via `consume_gas` ou `loop_tick`
- Fees et mempool:
  - `block_gas_limit` applique
  - budget de gas mempool
  - eviction des tx faibles sous pression
  - tri par densite de frais
  - `base_fee_per_gas` dynamique type EIP-1559 adaptee
  - burn implicite de la base fee, seule la priority fee va au producteur
- Surface operateur:
  - fallback memoire dangereux supprime
  - quotas HTTP/RPC
  - auth optionnelle sur API/RPC

## Priorite 1

- VM:
  - metering runtime injection-level reel par middleware ou fuel natif, pas seulement garde statique + host metering
  - host functions pour storage iteratif, logs multi-topics, events indexables, return data memoire complete
  - isolation plus stricte des ressources memoire/pages
- Etat:
  - arbre d etat explicite avec preuves de comptes et de storage
  - state sync chunked avec reprise partielle et verif independante du manifest par checkpoint connu
  - pruning, snapshots incrementaux et mode archival
- Fees:
  - separation explicite `max_fee_per_gas` / `max_priority_fee_per_gas`
  - refunds partiels sur gas non consomme
  - estimation RPC de gas et fee market

## Priorite 2

- Consensus / PoS:
  - checkpoints d epoch explicites
  - rewards par epoch
  - inactivity leak / penalties liveness
  - slashing et jailing pour plus de cas byzantins
- Reseau:
  - peer scoring
  - anti-spam gossipsub
  - sync par plages et pipeline parallele
  - banlist / quarantine des pairs incoherents
- Storage:
  - migrations versionnees robustes
  - checksum d integrite offline
  - compaction / GC

## Priorite 3

- Smart contracts:
  - receipts indexes
  - logs filtrables
  - appels internes
  - gestion du storage trie / canonical encoding
- API / RPC:
  - endpoints de preuve
  - simulation dry-run
  - pagination robuste
  - rate limiting par IP/peer/account
- Ops:
  - metrics Prometheus
  - structured logs avec correlation ids
  - healthchecks et alerting
  - outils d inspection d epoch/finalite/reorg

## Priorite 4

- Gouvernance:
  - upgrades actives a hauteur fixee avec compat matrix
  - signals reseau avant hard fork
  - process de migration d etat
- Crypto:
  - revue formelle de domain separation
  - adresses checksummed
  - rotation de schemas et audits externes

## Non-negociable avant mainnet

- audit externe consensus
- audit externe VM
- tests reseau de longue duree
- tests de reorg/finalite sous partitions
- fuzzing tx, block, snapshot et RPC
- benchs sur sync, mempool et execution
