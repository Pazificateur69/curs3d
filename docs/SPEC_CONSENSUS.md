# CURS3D Consensus Specification (v5)

Draft, 2026-05-21. This document is a normative reference for the
consensus rules implemented by `curs3d` at protocol version **v5**. It
is also the document an external auditor reads first; please file PRs
when reality diverges from spec.

For the *why* behind these choices, see [`docs/council-log.md`](council-log.md)
and the project [`CLAUDE.md`](../CLAUDE.md). For governance (rather than
consensus) rules, see [`docs/SPEC_GOVERNANCE.md`](SPEC_GOVERNANCE.md) (TBD).

## 1. Scope

This spec covers:

- Block structure and validity
- Fork choice and finality
- Validator set, staking, slot-leader selection
- Slashing and inactivity penalties
- Epoch settlement
- Snapshot and state-sync rules
- Hardfork protocol version handling

It does **not** cover: smart-contract VMs (Wasmer/revm), the wire format
of P2P messages (see [`src/network/`](../src/network/) for the canonical
bincode definitions), the HTTP API, or the wallet format.

## 2. Notation

- `H(x)` ≡ SHA-3-256 of `x`.
- `Hd(domain, x)` ≡ `H(len(domain) || domain || x)` (domain-separated).
- `||` is byte concatenation.
- All multi-byte integers are little-endian.
- `Address := first 20 bytes of Hd("curs3d-address", pubkey)`.
- `BlockHash(header) := H(H(bincode(header)))` (double SHA-3).
- `TxHash(tx) := Hd("curs3d-tx", bincode(tx))`.
- One CUR = 1_000_000 microtokens. All on-chain amounts are microtokens.

## 3. Block structure

```text
Block {
    header: BlockHeader,
    transactions: Vec<Transaction>,
    proposer_signature: Vec<u8>,        // ML-DSA-87 over BlockHash(header)
}

BlockHeader {
    height: u64,
    parent_hash: [u8; 32],
    state_root: [u8; 32],               // see §5 (v5: merkle root; v6: SMT)
    receipts_root: [u8; 32],
    merkle_root: [u8; 32],              // merkle root over TxHash(tx) leaves
    timestamp: i64,                     // unix seconds, monotone w.r.t. parent
    proposer: Address,
    protocol_version: u32,
    chain_id: String,
}
```

The merkle root construction applies leaf/node domain separation
(RFC 6962 style): leaves are hashed as `H(0x00 || tx_hash)`, internal
nodes as `H(0x01 || left || right)`. See [`src/crypto/hash.rs`](../src/crypto/hash.rs).

## 4. Block validity

A block `B` at height `h` is **valid** iff all of the following hold:

1. `B.header.height = h = parent.header.height + 1`.
2. `B.header.parent_hash = BlockHash(parent.header)`.
3. `B.header.chain_id = genesis.chain_id`.
4. `B.header.protocol_version = protocol_version_at_height(h)`.
5. `B.header.timestamp >= parent.header.timestamp` and
   `B.header.timestamp <= now + 60s` (clock-skew tolerance).
6. `B.header.proposer = slot_leader(h, validator_set_at(h - 1))`
   (see §6).
7. `B.proposer_signature` verifies under the public key of
   `B.header.proposer` over `BlockHash(B.header)`.
8. `B.header.merkle_root` equals the merkle root over
   `[TxHash(tx) for tx in B.transactions]`.
9. Every `tx in B.transactions` is shape-valid (see §11) and applies
   without error against the state after the parent's `state_root`.
10. The post-apply `state_root` equals `B.header.state_root`.
11. The post-apply `receipts_root` equals `B.header.receipts_root`.
12. `B.transactions[0]` is the Coinbase tx paying the block reward to
    `B.header.proposer`.

## 5. State root

- **Protocol v5 (current)**: `state_root` is the Merkle root over all
  `(address, AccountState)` leaves plus all `(contract_address, ContractState)`
  leaves, in canonical order (lexicographic by address), with the
  domain-separated merkle scheme of §3.
- **Protocol v6 (wired but dormant since 2026-05-19, commit `0f0ab2c`):**
  state_root is the `SparseMerkleTrie` root from `src/trie/mod.rs`. Activated
  by changing `V6_HARDFORK_HEIGHT_TESTNET` from `u64::MAX` to a coordinated
  height.

## 6. Slot-leader selection (v4+)

```text
fn slot_leader(h: u64, validators: &[ValidatorEntry]) -> Address {
    let seed = H(h.to_le_bytes() || parent_hash_at(h - 1));
    let total_stake = sum(v.stake for v in validators);
    let seed_int = u64::from_le_bytes(seed[0..8]);
    let target = seed_int % total_stake;
    let mut cumulative = 0;
    for v in validators.iter().sorted_by_key(|v| v.address) {
        cumulative += v.stake;
        if target < cumulative {
            return v.address;
        }
    }
    unreachable!()
}
```

Properties:

- **Deterministic given the validator set + parent hash.** Every honest
  node computes the same leader.
- **Stake-weighted.** Probability of being leader at any height is
  proportional to share of total stake.
- **Single leader per height.** Eliminates simultaneous block-production
  forks at the same height (commit `343a7a1`).

Known weakness: deterministic ⇒ an attacker can predict leaders far
ahead and time targeted DDoS. The replacement is a VRF (see task #35
in the internal roadmap), gated on v6+.

If the slot-leader does not produce a block within `BACKUP_LEADER_TIMEOUT`
(30s after the parent's timestamp + the normal 10s production interval),
any other validator may produce a "backup" block. Backup blocks count
for fork choice with the same weight; finality eventually resolves
duplicates.

## 7. Fork choice

Implemented in [`src/core/blocktree.rs`](../src/core/blocktree.rs).

- Each block stored in the BlockTree carries `cumulative_stake_weight`,
  defined as `sum(stake_of(b.header.proposer) for b in chain(genesis, b))`.
- The **canonical head** is the leaf with maximum `cumulative_stake_weight`.
- Ties broken by lower `BlockHash` (deterministic).
- Blocks below the latest finalized height (see §8) are pruned from
  the tree; reorgs cannot cross the finalized boundary.

## 8. Finality (BFT, 2/3 threshold)

```text
FinalityVote {
    block_hash: [u8; 32],
    height: u64,
    epoch: u64,
    voter: Address,
    signature: Vec<u8>,                 // ML-DSA-87 over (block_hash || height || epoch)
}
```

Rules:

- Validators sign one `FinalityVote` per height per epoch. A second
  vote for a different `block_hash` at the same `(height, epoch)` is
  **equivocation evidence** (see §9).
- A block `B` at height `h` is **finalized** once
  `sum(stake(v) for v with FinalityVote at (h, e, BlockHash(B)))
  >= 2/3 * total_stake_in_epoch(e)`.
- Finalized blocks may not be reorged. Their state is irreversible.
- The protocol does not prescribe vote-aggregation timing; validators
  broadcast votes on every block they accept, with rate-limited dedup.

A 5-validator chain with 5 equal stakes finalizes when 4/5 vote
(`ceil(5 * 2/3) = 4`), tolerating 1 down. With unequal stakes the
threshold is stake-weighted.

## 9. Slashing

Two slashable offenses:

### 9.1 Equivocation
Two different blocks at the same height signed by the same proposer,
OR two different `FinalityVote`s at the same `(height, epoch)` from the
same voter. Submit-tx: any validator may submit
`EquivocationEvidence { evidence_a, evidence_b }` to be included in a
future block.

**Penalty:** burn `33%` of the offending validator's `staked_balance`,
jail for `64` blocks (`DEFAULT_JAIL_DURATION_BLOCKS`). Jailed validators
do not participate in slot-leader selection nor in finality.

### 9.2 Inactivity (epoch-end)
At each epoch boundary, every validator that produced **0** blocks
during the epoch and missed all finality votes incurs an inactivity
penalty proportional to elapsed inactive epochs (grace period: 2 epochs).
See `consensus::compute_epoch_settlement`.

## 10. Epoch settlement

- Epoch length: `DEFAULT_EPOCH_LENGTH = 32` blocks.
- At every height `h` where `h % epoch_length == 0`, an epoch settlement
  is applied **synchronously inside the block apply step** (this is
  load-bearing; the prior boot-replay desync was the root cause of
  `invalid state root` errors fixed in commit `f461aa4`).
- Settlement consists of:
  1. Distribute rewards: each validator earns
     `epoch_reward_rate * blocks_produced * stake_share`. Default rate
     is `100` microtokens per CUR staked per block produced.
  2. Apply inactivity penalties (§9.2).
  3. Snapshot the new validator set for the next epoch
     (`EpochSnapshot::freeze`). Once frozen, the set cannot change
     mid-epoch even if new `Stake` / `Unstake` transactions land.

## 11. Transaction validity (shape only)

Shape validation runs before state application; failures reject the
block. See [`src/core/transaction.rs`](../src/core/transaction.rs).

- Every tx has a `sender_public_key` and signature; signature verifies
  over `bincode(tx_without_signature)`.
- `tx.from = first 20 bytes of Hd("curs3d-address", sender_public_key)`.
- `tx.nonce = state.account[tx.from].nonce` (strict equality,
  prevents replay and gaps).
- Gas: `tx.gas_limit <= block.gas_limit`,
  `tx.max_fee_per_gas >= block.base_fee_per_gas`,
  `tx.max_priority_fee_per_gas <= tx.max_fee_per_gas`.
- Type-specific shape: Transfer requires `to + amount`, Stake requires
  `amount >= MINIMUM_STAKE`, etc.

## 12. State machine (informal)

For each tx in block order:

1. Charge `intrinsic_gas + len(data) * gas_per_byte` upfront from
   `state.account[tx.from].balance`.
2. Dispatch by `tx.kind`:
   - `Transfer`: debit sender, credit recipient.
   - `Stake`: debit sender liquid, credit sender staked; if
     `staked >= MINIMUM_STAKE` and not jailed, mark validator active
     from height + 1.
   - `Unstake`: pull from staked → `pending_unstakes[h + UNSTAKE_DELAY_BLOCKS]`;
     mature unstakes paid out via Coinbase-like ledger update.
   - `DeployContract` / `CallContract`: hand off to Wasmer VM (native)
     or revm (EVM) depending on `TransactionKind::DeployEvmContract /
     CallEvmContract`.
   - `DeployToken` / `TokenTransfer` / ...: token-registry mutation.
   - `SubmitProposal` / `GovernanceVote`: governance module.
3. Refund unused gas (subject to EVM rules for `CallEvmContract`).
4. Append `Receipt { tx_hash, success, gas_used, logs }`.

## 13. Hardfork protocol version

- `protocol_version_at_height(h: u64) -> u32` returns the active version
  for blocks at height `h`. Encoded as a static const ladder
  (`V4_HARDFORK_HEIGHT_TESTNET`, `V5_…`, `V6_…`).
- Genesis is created at the latest known version; chains with
  mixed-version peers diverge silently. **Coordinate restarts.**

## 14. Snapshot and state sync

- Snapshot manifest: `(height, chunk_count, total_size, chunk_root)`.
- Chunk root: merkle root (§3) over per-chunk `H(chunk_bytes)`.
- Each chunk carries `proof: Vec<[u8; 32]>` verifying its index against
  the manifest's `chunk_root`.
- Receiver rules (current tree):
  - Reject `manifest.height <= our_height` (prevents mutual h=0 loop).
  - Buffer chunks that arrive before their manifest in a bounded
    pre-manifest buffer (cap by count AND bytes — see
    `MAX_BUFFERED_PRE_MANIFEST_CHUNKS / BYTES`).
- Sender rules:
  - Throttle 50 ms between chunks (prevents gossipsub outbound overflow).
  - Skip serving if our height is 0.

## 15. Trusted checkpoints (optional safety net)

`src/core/checkpoints.rs` lets the binary embed hardcoded
`(height, hash, state_root)` anchors. Blocks at a checkpoint height
that don't match are rejected at validation time (Bitcoin Core
pattern). Currently the `curs3d-public-testnet` slice is **empty**;
checkpoints will be added after the first external audit cycle.

## 16. Open questions / acknowledged weaknesses

- **Slot-leader VRF migration** (§6): the deterministic scheme is a
  predictable DDoS surface. Migration is gated on v6+ activation.
- **Gossipsub peer scoring**: default config is permissive; custom scoring
  is in the roadmap.
- **No HotStuff-style pipelined voting**: each finality vote is one
  network round-trip per height. Throughput is sufficient for current
  testnet load (~10s blocks) but won't scale to subsecond finality.
- **No fork-choice rule formalization in a model checker** (Coq /
  Lean / TLA+). Currently spec-tested only via Rust unit tests +
  proptest (in progress).

---

*Spec maintained by the project. Patches welcome via
[CONTRIBUTING.md](../CONTRIBUTING.md). Audit findings against the spec
itself are welcomed via security@curs3d.fr (PGP key in
[security.txt](../website/.well-known/security.txt)).*
