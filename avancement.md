# Avancement CURS3D

Etat au 2026-04-14.

## Positionnement reel

- Projet actuel: `prototype L1 avance`
- Niveau estime:
  - `70%` d un proto L1 serieux
  - `35%` d une L1 publique `mainnet-ready`
  - `15-20%` d une L1 tres haut niveau

## Ce qui est deja en place

### Consensus et securite protocolaire

- slashing par preuve d equivocation liee a la meme hauteur
- fork choice revalide contre l etat parent
- reorg bloque sous finalite
- finalite basee sur validator set gele par epoch
- votes de finalite rejetes pour hash inconnu ou non canonique
- version de protocole verifiee dans les blocs

### Etat, persistance et preuves

- persistance de l etat canonique `accounts + contracts + receipts`
- rebuild correct au redemarrage
- snapshots complets avec verification de chunks
- snapshots bases sur point finalise quand possible
- preuves d etat `account proof` et `storage proof`
- export des preuves par RPC et API HTTP

### VM, execution et gas

- execution Wasm reelle
- host functions deterministes pour storage, logs et input
- metering Wasmer par middleware
- injection de fuel instruction par instruction quand le contrat expose un hook
- rejet des contrats avec boucle non fuelee
- receipts enrichis avec `gas_used`, `effective_gas_price`, `priority_fee_paid`, `base_fee_burned`, `gas_refunded`

### Fee market et mempool

- `block_gas_limit` applique en validation et en production
- `base_fee_per_gas` dynamique type EIP-1559
- separation `max_fee_per_gas` / `max_priority_fee_per_gas`
- refunds sur gas inutilise
- admission mempool sous pression avec budget gas global
- budget gas par compte
- remplacement de tx plus strict sur `total_fee_cap + max_fee + priority_fee`
- limite de gap de nonce
- eviction des tx devenues non competitives

### Surfaces node / integrateur

- API HTTP avec auth possible, body limit et CORS non permissif par defaut
- RPC TCP avec auth possible
- endpoint d estimation de transaction via API
- estimation de transaction via coeur de chaine et RPC
- propagation reseau des transactions soumises

## Ce qui a ete execute dans le dernier lot

- export RPC/API des preuves d etat
- metering instruction par instruction plus fin dans la VM
- mempool EIP-1559 durci
- endpoint et logique de `dry-run / estimate` pour les transactions

## Validation

- suite de tests locale verte:
  - `79/79` sur `lib`
  - `79/79` sur `main`
- commande utilisee:
  - `CARGO_TARGET_DIR=/tmp/curs3d-target cargo test -j1`

## Risque principal restant

Le projet est techniquement credible, mais pas encore une L1 publique prete pour Internet. Les principaux ecarts restants sont:

- VM encore trop simple face a une vraie prod
- sync d etat encore insuffisant pour gros reseau permissionless
- couche reseau encore trop peu dure contre DoS / peers malveillants
- absence d observabilite et d outillage ops de niveau mainnet
- pas d audits externes ni de campagne de fuzzing / long-run / chaos
