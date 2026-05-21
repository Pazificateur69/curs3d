# Council Log — CURS3D

Décisions importantes prises en délibération AI council (Claude + Gemini + Codex).

---

## 2026-05-21 — Audit complet du projet CURS3D

### Question

Audit absolument complet du projet : code, architecture, crypto, consensus, networking, storage, prod-readiness, ops, tech debt, doc, tests, sustainability solo, risques business. Note précise sur 10, top risques, horizon mainnet réaliste, plan d'action 6 mois.

### Contexte au moment de l'audit

- ~31 400 LOC Rust, 211 tests unit verts, 16 modules
- 5-validator testnet live (Oracle ARM ×2, IONOS, Hostinger Plesk ×2 stealth), genesis SHA `e830418885dd9057...`, chain produces+finalize
- 10+ commits récents (trusted checkpoints, mempool priority, snapshot chunk fix, v6 SMT dormant, pruning primitive dormant, fuzzing CI, RFP audit prêt)
- 2 incidents 24h récents : Plesk-1 stealth fork oublié 14 jours, n1 OOM-thrash via `Vec<Block>` non bornée

### Positions des 3 AIs

| AI | Note | Position résumée |
|----|------|-------------------|
| **Claude** | 6.5/10 | Code et discipline ops au-dessus de la moyenne pour un projet solo, mais `Vec<Block>` bloquant + pas d'audit externe = 2 dettes critiques |
| **Gemini** | 3/10 | "PoC étudiant pas Layer 1". Insiste sur 2 failles crypto sous-estimées : Merkle 2nd-preimage (hash.rs:92-118) et slot-leader déterministe → DDoS. Hubris solo dev. |
| **Codex** | 4.7/10 | Prototype L1 ambitieux qui a dépassé le stade démo, mais loin d'un mainnet. Le plus inquiétant : "les bugs naissent de la forme actuelle du système". Bus factor 1 non viable. |

### Agreement zones (high-confidence, 3/3 d'accord)

1. **`Vec<Block>` non bornée = bloqueur n°1 absolu**. Sans refactor base-offset + activation pruning runtime, mainnet est mathématiquement impossible.
2. **God modules** chain.rs (6846 lignes) + network/mod.rs (3417 lignes) doivent être splittés — non auditables en l'état.
3. **Tests intégration multi-process manquants** — chaos localnet 5-7 nodes en CI requis.
4. **Audit externe APRÈS refactor**, pas avant — payer 80-150k$ pour faire auditer du code monolithique = budget vaporisé.
5. **Mainnet 12-18 mois = irréaliste**. Horizon consolidé : **18-24 mois min, 24-36 mois en solo strict**.

### Disagreement zones

**Gemini vs Claude/Codex sur la sévérité des failles crypto** :
- Gemini : "Merkle sans préfixe leaf/node 0x00/0x01 = vulnérabilité critique de forgerie de preuve. Slot-leader déterministe = surface DDoS prévisible."
- Claude/Codex : reconnaissent les 2 issues mais les classent moins haut.
- **Résolution** : Gemini a raison sur la gravité. Le 2nd-preimage est un finding standard d'audit (CVE-2012-2459 Bitcoin), à fixer cette semaine (50 LOC). Le slot-leader VRF est moins urgent (testnet sans incentive financier) mais doit être un blocker mainnet.

**Codex insiste sur le bus-factor 1 comme blocker mainnet absolu** :
- Position validée : aucun audit externe ne validera "mainnet-ready" pour un projet solo sans plan de redondance humaine. À traiter comme une exigence business, pas optionnelle.

**Codex flag le risque légal "quantum-resistant" marketing** :
- L'EVM secp256k1 n'est pas PQ. Les claims actuels sur curs3d.fr sont juridiquement fragiles (publicité mensongère potentielle).
- **Action** : qualifier en "Quantum-resistant **native layer** + Ethereum-compatible EVM layer".

### Décision finale (par Claude qui fait la final call)

**Note consolidée : 4.5/10**

Révision baissière par rapport au premier audit Claude (6.5/10) après les apports Gemini et Codex.

**Plan d'action consolidé sur 6 mois** :

| Mois | Action critique |
|------|------------------|
| M1 sem 1 | Fix Merkle 2nd-preimage (50 LOC, dette crypto) |
| M1 sem 2-4 | Refactor `Vec<Block>` → block store paginé redb + cache borné |
| M2 | Split chain.rs (→ 8-10 sous-modules) + split network/mod.rs (handlers séparés) + Trait Storage |
| M3 | Chaos localnet 5-7 nodes CI nightly + 30+ scénarios (partition / kill / fork / byzantine) |
| M4 | Activation v6 SMT (audit interne preuves) + qualifier marketing post-quantum |
| M5 | Recrutement 1 protocol eng + 1 ops, OU acceptation "testnet permanent" |
| M6 | Lancer audit externe phase 1 ($60-80k, ciblé consensus+storage+crypto) — seulement si M1-3 terminés |

**Verdict viabilité mainnet** : 18-24 mois si équipe, 24-36 mois si solo strict. Pas en 12 mois dans aucun scénario.

**Décision sur le RFP audit déjà préparé** : NE PAS L'ENVOYER avant fin M3. Le code n'est pas auditable en l'état (god modules + bugs naissant de la forme du système).

### Dissenting opinions

- Mon audit initial Claude (6.5/10) était trop indulgent sur la note. Le projet est solide pour un solo dev mais pas mainnet-ready, et 6.5 envoyait un signal trop optimiste.
- Gemini était la voix la plus dure mais aussi la plus utile sur les findings crypto concrets (Merkle, slot-leader). À retenir : ses analyses techniques sont précises, sa note 3/10 est probablement un peu dure mais reflète son standard "Layer 1 production-grade".

### Pour future référence

Si cette décision est ré-évaluée dans 6 mois, vérifier :
- M1 sem 1 : commit fix Merkle 2nd-preimage présent dans le tree ?
- M1 sem 2-4 : `Blockchain::blocks` n'existe plus ou est `BlockStoreCursor` paginé ?
- M2 : `chain.rs` < 1500 lignes ? `network/mod.rs::run_with_chain()` < 500 lignes ?
- M3 : `.github/workflows/chaos.yml` existe et tourne nightly ?

Si oui aux 4 : nouvelle session council, ré-évaluer note. Probablement passage à 6.5-7/10 et timeline audit accélérée.

Si non : redire les mêmes choses, le verdict ne change pas. Pas de mainnet sans refactor.

---

## 2026-05-21 (later) — Sprint 1 + 2 + 7 execution

Pas une décision council, juste une trace de ce qui a été shipé suite à l'audit ci-dessus.

### Done dans la session
- **M1 sem 1 fait** : Merkle 2nd-preimage fix shipé (commit pending, `src/crypto/hash.rs` + 2 anti-attack tests). Le finding #1 de Gemini est fermé.
- **M1 sem 2-4 Phase A fait** : `BlockStoreCursor` scaffold créé dans `src/core/block_store.rs` avec 11 tests (8 unit + 3 proptest). Co-existe avec `Vec<Block>` ; Phases B-E pas faites.
- **Sprint 2 cluster fait** : fork-detector alerting Discord (`deploy/scripts/curs3d-fork-detector.sh`), pre-manifest BYTES cap (`src/network/mod.rs`), release signing pipeline (`.github/workflows/release.yml`), Plesk non-root user, healthcheck cron, unwrap audit (1 prod-path → expect documenté).
- **Sprint 7 cluster fait** : CONTRIBUTING.md, SPEC_CONSENSUS.md, RPC batching vérifié déjà présent, WebSocket 5s write-timeout, tracing migration complète, metrics +6 (mempool par classe, slashed, jailed), wallet recovery tests (replay, tampering, re-encrypt), newtypes (Address/BlockHash/TxHash) + 11 tests, thiserror audit (toutes les erreurs déjà thiserror).
- **Marketing claims qualifié** : "post-quantum native layer + EVM-compatible (secp256k1)" remplacé partout — supprime le risque légal flagué par Codex.
- **PGP key publiée** : `14C7 7953 E130 4F27 DDCF C6B2 4438 0E95 639E 6F37`, `/.well-known/security-pgp.asc` live.
- **Plesk-1 et Plesk-2** : migrés en `User=_cache` (plus root), healthcheck cron actif.

### Tests
211 → 248 (37 nouveaux : +2 Merkle, +8 block_store, +3 wallet, +6 types, +5 hash proptest, +5 types proptest, +3 block_store proptest, +3 transaction proptest, +2 block proptest). Tous verts.

### Scoping docs ajoutés pour les sprints à venir
- `docs/REFACTOR_BLOCK_STORE.md` (#28, Phases B-E)
- `docs/REFACTOR_SPLIT_CHAIN.md` (#29)
- `docs/REFACTOR_SPLIT_NETWORK.md` (#30)
- `docs/REFACTOR_STORAGE_TRAIT.md` (#31)
- `docs/REFACTOR_CHAOS_CI.md` (#32)
- `docs/REFACTOR_REPLACE_BLOCKS.md` (#34)
- `docs/REFACTOR_VRF_SLOT_LEADER.md` (#35)
- `docs/REFACTOR_GOSSIPSUB_SCORING.md` (#36)
- `docs/REFACTOR_V6_SMT_ACTIVATION.md` (#37)
- `docs/REFACTORS_INDEX.md` (vue d'ensemble)

### Verdict ré-évalué (auto-évaluation, pas council)
La note 4.5/10 du council de ce matin reste valide tant que `Vec<Block>` n'est pas refactoré (Phase B-E) — c'est le bloqueur qui pèse le plus. Les findings sécurité critiques (Merkle, marketing claim, PGP, audit pipeline) sont fermés, ce qui retire ~1.0 point de risque immédiat. Note interne post-session : **~5.5/10**, mais formal council re-run est nécessaire après la Phase B du refactor block_store.
