# TODO CURS3D

Objectif: transformer CURS3D en vraie L1 publique, robuste, programmable, auditable et exploitable.

## Priorite 0: mainnet blockers absolus

- VM:
  - host functions plus completes: iteration storage, logs multi-topics, events indexables, return data memoire complete
  - limites plus strictes sur memoire, tables, pages et croissance memoire
  - modele de gas plus fin sur appels, memory growth, host overhead et ABI
  - politique deterministe de trap / revert / abort
- Etat:
  - arbre d etat explicite et stable pour comptes, contrats et storage
  - preuves d etat exportables a grande echelle avec format versionne
  - state sync reel avec reprise, resume, checkpoints connus et verif locale forte
  - snapshots incrementaux et mode pruning / archival
- Reseau:
  - peer scoring
  - banlist / quarantine
  - limites anti-spam par peer et par message
  - sync pipelinee par plages et backpressure reseau
- Consensus / PoS:
  - checkpoints d epoch explicites
  - rewards par epoch
  - penalties de liveness / inactivity leak
  - slashing pour cas byzantins supplementaires
- Ops:
  - observabilite `Prometheus + healthchecks + structured logs`
  - crash recovery et verification offline
  - benchmarks de sync, mempool, VM et stockage
  - fuzzing, soak tests et tests de partition reseau
- Assurance:
  - audit externe consensus
  - audit externe VM
  - audit externe crypto / stockage / reseau

## Priorite 1: execution et integrateur

- simulation `dry-run` plus riche:
  - simulation sans signature forcee si mode local autorise
  - estimation du prochain `base_fee`
  - estimation de succes / revert avec return data
- API / RPC:
  - pagination robuste
  - filtres de logs
  - lookup de receipts indexes
  - preuves d etat en format compact
  - endpoints de debug / trace optionnels
- Wallet / UX:
  - estimation automatique des fee caps
  - protection contre nonces bloques
  - resubmission / replacement policy cote client

## Priorite 2: economie et marche des fees

- base fee plus sophistiquee selon cible de bloc
- fee estimation multi-percentiles
- mempool avec classes de priorite
- eviction plus agressive sur tx peu rentables
- anti-spam economique par compte / peer / IP
- politique de replacement configurable et versionnee

## Priorite 3: gouvernance et upgrades

- matrice de compatibilite protocole / reseau / storage
- activation d upgrade a hauteur fixee
- signaux pre-fork et refus des peers incompatibles
- migrations d etat versionnees et testees
- procedure de rollback / replay / recovery documentee

## Priorite 4: securite systemique

- revue de domain separation partout
- adresses checksummed
- modeles de menace documentes
- campagnes de chaos testing
- CI securite plus dure: fuzz, MIRI si utile, sanitizers, benches de regression

## Priorite 5: performance

- profiling CPU / memoire
- batch verification signatures si applicable
- meilleur pipeline sync / import blocs
- compaction stockage
- caches explicites et invalidation claire

## Execute deja

- consensus et finalite nettement durcis
- persistance et rebuild d etat canonique
- preuves Merkle de snapshots
- preuves d etat comptes / storage
- execution Wasm reelle
- host functions deterministes
- metering Wasmer par middleware
- `base_fee_per_gas` dynamique
- `max_fee_per_gas` / `max_priority_fee_per_gas`
- refunds de gas
- mempool plus agressif
- estimation de transaction
- export RPC/API de preuves d etat

## Execute maintenant dans ce lot

- `avancement.md`
- `todo.md`
- estimation de transaction via coeur de chaine
- exposition de cette estimation via RPC/API

## Prochain lot recommande

1. receipts indexes + filtres de logs
2. observabilite node `metrics + health + audit logs`
3. state sync plus industrialise avec resume/checkpoints connus
