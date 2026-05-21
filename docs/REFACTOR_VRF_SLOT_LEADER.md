# Refactor — VRF slot-leader selection

**Status**: Design.
**Tracking task**: #35.
**Estimated effort**: 2 weeks focused (incl. cryptographic review).
**Blocking**: mainnet (DDoS mitigation).

## Motivation

The current slot-leader function (`consensus::slot_leader`, see
[`SPEC_CONSENSUS.md`](SPEC_CONSENSUS.md) §6) computes the producer for
a height `h` as:

```
seed = sha3(h.to_le_bytes() || parent_hash_at(h-1))
target = u64::from_le_bytes(seed[0..8]) % total_stake
leader = walk validators sorted by address, pick the one whose
         cumulative stake range covers `target`
```

This is **fully deterministic**. The moment block `h-1` is finalized,
every observer knows the leader for h, h+1, h+2, …, every block out
into the future where the validator set is stable.

Threat model that breaks here:

1. **DDoS the upcoming leader.** Attacker computes the next 10
   validators 10 minutes in advance, floods the IP of each with traffic
   right before its slot. Result: every slot mis-produced; chain stalls.
2. **Selective gossip starvation.** Attacker peers with the upcoming
   leader and refuses to gossip the previous block to it, delaying its
   awareness of `h-1` past the production deadline.
3. **Front-running on MEV.** Once the leader is known, MEV searchers
   can wire transactions directly to it and bypass the public mempool.

The mitigation in mainstream PoS chains (Algorand, Cardano, Filecoin,
Ethereum's RANDAO + VRF) is to make the leader unpredictable until the
moment the leader itself reveals "yes, that's me" with a proof.

## Goal (P0)

Replace deterministic slot-leader with a **VRF (Verifiable Random
Function)**:

- Each validator computes `VRF(secret_key, height || epoch_seed) ->
  (output, proof)`.
- The validator with the lowest `output` (within an eligibility
  threshold) for height `h` is the leader.
- The proof is included in the block header; any observer verifies
  `VRF_verify(pubkey, height || epoch_seed, output, proof)`.

Now an observer cannot predict the leader until that leader publishes
the block. DDoS surface shrinks from O(blocks_ahead) to O(1).

## Goal (P1)

Backup leader rotation also via VRF (rank by `output`, second-lowest
is backup if primary doesn't produce within timeout).

## Crate choice

Two candidates in pure Rust:

- **`schnorrkel`** (Substrate / Polkadot): well-audited, ed25519-style
  curves, includes VRF as a first-class feature. Already used by Kusama
  / Polkadot for BABE slot-leader.
- **`vrf` crate** (Filecoin ecosystem): BLS-based, lighter API.

**Recommendation: `schnorrkel`**. Battle-tested at Polkadot scale,
clean API for `(output, proof)` pairs, signature verification matches
our existing ed25519 mental model.

Cost: each validator stores an additional VRF keypair (32 + 32 bytes).
The chain's primary signing key stays ML-DSA-87 (post-quantum); the
VRF key is classical ed25519. This is fine — the VRF output is a
*selection* mechanism, not a value-bearing signature. A quantum
adversary that broke ed25519 could grief the network (predict leaders
again) but couldn't forge txs or finality.

## Design sketch

```rust
// In consensus/vrf.rs (new module)
use schnorrkel::{Keypair as VrfKeypair, vrf::{VRFInOut, VRFProof}};

pub struct VrfSlotLeader {
    keypair: VrfKeypair,
}

impl VrfSlotLeader {
    pub fn compute(&self, height: u64, epoch_seed: &[u8; 32]) -> VrfBallot {
        let input = vrf_input(height, epoch_seed);
        let (io, proof, _) = self.keypair.vrf_sign(input);
        VrfBallot {
            output: io.output.to_bytes(),
            proof,
            public_key: self.keypair.public,
        }
    }

    pub fn verify(
        public_key: &PublicKey,
        height: u64,
        epoch_seed: &[u8; 32],
        output: &[u8; 32],
        proof: &VRFProof,
    ) -> Result<(), VrfError> {
        let input = vrf_input(height, epoch_seed);
        let io = VRFInOut::from_bytes(public_key, input, output)?;
        public_key.vrf_verify(input, &io, proof)?;
        Ok(())
    }
}

fn vrf_input(height: u64, epoch_seed: &[u8; 32]) -> &[u8] {
    // schnorrkel uses a transcript abstraction; build one keyed on
    // chain_id + height + epoch_seed.
}

// In core/block.rs, BlockHeader gains:
pub vrf_output: [u8; 32],
pub vrf_proof: VRFProof,
pub vrf_pubkey: PublicKey,
```

Leader eligibility: `vrf_output < threshold`, where `threshold` is
derived from stake share. The lower `output`, the more likely the
producer is the canonical leader. Ties broken by stake then address.

The `epoch_seed` is the VRF output of the *last* block in the previous
epoch — chained, so an attacker needs control over an entire epoch to
manipulate future seeds. (Same construction as Polkadot BABE.)

## Phases

### Phase 1 — schnorrkel integration (3 days)

1. Add `schnorrkel = "0.11"` to deps.
2. Implement `consensus::vrf` module.
3. Add VRF keygen to validator wallet creation (separate from ML-DSA).
4. Persist VRF pubkey alongside validator address on chain.

### Phase 2 — Header changes (2 days)

1. Extend `BlockHeader` with `vrf_output`, `vrf_proof`, `vrf_pubkey`.
2. Header is a `protocol_version` v7 change (hardfork). All blocks
   before the activation height keep using sha3-deterministic.
3. Add `V7_HARDFORK_HEIGHT_TESTNET = u64::MAX` (dormant), wire the
   dispatch in `slot_leader`.

### Phase 3 — Block validation (2 days)

In `validate_block`:
- If `protocol_version >= 7`: require `vrf_output` + `vrf_proof`;
  verify via `VrfSlotLeader::verify`.
- Reject if `vrf_output >= eligibility_threshold` (proposer not
  actually eligible).
- Compute next epoch_seed from this block's `vrf_output`.

### Phase 4 — Backup leader rotation (2 days)

If no primary block arrives within `BACKUP_LEADER_TIMEOUT` (30 s), the
next-lowest-vrf-output validator may produce a backup block. Both
backups carry their VRF output for verifiability.

### Phase 5 — Testnet activation (1 week soak)

1. Set `V7_HARDFORK_HEIGHT_TESTNET = current_head + 1000` (give 1000
   blocks of warning).
2. Roll out updated binary to all 5 validators ahead of activation.
3. Soak. Watch for missed slots, fork rate, finality lag.
4. If healthy after 7 days, ready for mainnet.

## Risks

| Risk | Severity | Mitigation |
|------|----------|------------|
| Two validators produce simultaneously because both VRF outputs are below threshold | High | Threshold tuned so P(>=2 eligible at same height) < 1 % per slot; backup-leader path handles the rare 2-block-h case via fork choice |
| Validator without VRF key (legacy) tries to produce after activation | Catastrophic | Hard fail at validate_block; node won't propagate; pre-activation runbook step "generate VRF key for every validator" |
| schnorrkel API breakage on minor version bump | Medium | Pin to exact version; never auto-update |
| VRF proof verification is too slow at scale | Low | schnorrkel verify is ~50 µs on x86_64; 10 verifies per slot is trivial |

## Acceptance criteria

1. After v7 activation, no observer can predict the next leader from
   parent_hash alone — only the leader themselves can compute their
   own `vrf_output`.
2. Block validation rejects a block whose `vrf_output` does not match
   the proof.
3. Eligibility threshold is calibrated so ~95 % of slots have exactly
   one eligible validator.
4. Localnet 5-node soak: 24 h, missed-slot rate < 1 %, no forks.
5. Reverting `V7_HARDFORK_HEIGHT_TESTNET` to `u64::MAX` restores the
   old deterministic behaviour cleanly (kill-switch).
