# Refactor — Gossipsub peer scoring + config

**Status**: Design.
**Tracking task**: #36.
**Estimated effort**: 1 week.
**Blocking**: resistance to a misbehaving / malicious peer floodink the
mesh.

## Motivation

Today's libp2p Gossipsub config in `src/network/mod.rs` uses defaults.
Defaults are permissive: a peer that repeatedly sends invalid blocks,
or floods us with messages, or has high latency, is treated like any
other. The only resistance comes from:

- Our application-level `PeerScorer` (rep-based ban on invalid blocks).
- Our application-level `PeerRateLimiter` (per-peer msg/sec).

Both operate *after* the gossipsub layer has already accepted the
message and forwarded it. Gossipsub itself doesn't punish bad peers —
so a malicious peer can keep getting blocks routed to / from it in the
mesh, even after we've decided we don't trust it.

Libp2p Gossipsub supports a peer-scoring system (the
`PeerScoreParams` / `PeerScoreThresholds` API). With it configured:

- Bad peers are *graylist*ed at the mesh level (still receive forwards
  but don't get added to mesh).
- Worse peers are *publish-rejected* (we don't forward to them).
- Worst peers are *disconnected* automatically.

## Goal (P0)

Wire up gossipsub peer scoring with sane defaults derived from
upstream's Eth2 reference (filecoin / lighthouse use similar values).

## Surface

```rust
// In src/network/mod.rs::build_swarm()

use libp2p::gossipsub::{
    PeerScoreParams, PeerScoreThresholds, TopicScoreParams,
};

let topic_params = TopicScoreParams {
    topic_weight: 1.0,
    time_in_mesh_weight: 0.0_f64,
    time_in_mesh_quantum: Duration::from_secs(1),
    time_in_mesh_cap: 3600.0,
    first_message_deliveries_weight: 1.0,
    first_message_deliveries_decay: 0.5,
    first_message_deliveries_cap: 100.0,
    mesh_message_deliveries_weight: -16.0,    // negative = penalty
    mesh_message_deliveries_decay: 0.97,
    mesh_message_deliveries_cap: 100.0,
    mesh_message_deliveries_threshold: 8.0,
    mesh_message_deliveries_window: Duration::from_secs(2),
    mesh_message_deliveries_activation: Duration::from_secs(30),
    mesh_failure_penalty_weight: -16.0,
    mesh_failure_penalty_decay: 0.997,
    invalid_message_deliveries_weight: -2000.0,  // hard penalty
    invalid_message_deliveries_decay: 0.997,
};

let score_params = PeerScoreParams {
    topics: hashmap! { our_topic_hash => topic_params },
    topic_score_cap: 3200.0,
    app_specific_weight: 1.0,
    ip_colocation_factor_weight: -35.0,
    ip_colocation_factor_threshold: 5.0,
    ip_colocation_factor_whitelist: Default::default(),
    behaviour_penalty_weight: -10.0,
    behaviour_penalty_threshold: 6.0,
    behaviour_penalty_decay: 0.986,
    decay_interval: Duration::from_secs(1),
    decay_to_zero: 0.01,
    retain_score: Duration::from_secs(3600),
};

let thresholds = PeerScoreThresholds {
    gossip_threshold: -4000.0,    // below = no gossip
    publish_threshold: -8000.0,   // below = no publish
    graylist_threshold: -16000.0, // below = graylist
    accept_px_threshold: 100.0,
    opportunistic_graft_threshold: 5.0,
};

gossipsub.with_peer_score(score_params, thresholds)
    .map_err(|e| anyhow::anyhow!("peer scoring: {}", e))?;
```

Then in `handle_new_block` / `handle_new_vote` etc., when we detect
an invalid message, call:

```rust
gossipsub.report_message_validation_result(
    &message_id,
    &source_peer,
    MessageAcceptance::Reject,
);
```

This is what feeds the `invalid_message_deliveries_weight` penalty.

Currently the code calls `report_message_validation_result` only in a
few places — extend to every reject path (bad signature, bad height,
bad merkle root, etc.).

## Phases

### Phase A — Wire the scoring config (1 day)

Add the `PeerScoreParams` / `PeerScoreThresholds` to `build_swarm()`.
Pick defaults from Lighthouse / filecoin-project (they're battle-tested
on real PoS networks). Document each constant inline.

### Phase B — Wire `MessageAcceptance::Reject` everywhere (2 days)

For every reject path in `handle_new_block`, `handle_new_vote`,
`handle_new_transaction`, `handle_snapshot_chunk`, etc., add a call to
`gossipsub.report_message_validation_result(..., Reject)`. Wrap in a
helper `reject_msg(reason: &str, peer: PeerId, msg_id: MessageId)` for
consistency + logging.

### Phase C — Tests (2 days)

1. Unit test: feeding an "invalid block" repeatedly causes
   `peer_score < graylist_threshold` after N attempts.
2. Localnet integration test: a 4-node mesh where node 4 is the
   troublemaker (sends randomly mutated blocks). After 60 s, nodes 1-3
   should have node 4 graylist-ed or disconnected.

### Phase D — Metrics + alerting (1 day)

Add a Prometheus gauge `curs3d_gossipsub_peer_score{peer_id="..."}`
exporting the current peer-score for every connected peer. Grafana
alert when any peer score drops below `gossip_threshold`.

## Risks

| Risk | Severity | Mitigation |
|------|----------|------------|
| Too-aggressive thresholds cause healthy peers to be graylist-ed during transient network issues | High | Start with Lighthouse defaults (proven on Ethereum mainnet). Tune via metrics, not theory. |
| `ip_colocation_factor` penalizes legitimate validators behind the same NAT | Medium | The whitelist field lets us exempt known validators by IP |
| `invalid_message_deliveries_weight` -2000 is so harsh that a single bug in our reject path bans every peer | High | Roll out behind a feature flag the first month; monitor `peer_score` p99 |

## Acceptance criteria

1. A node that receives 10+ provably-invalid blocks from peer X
   disconnects peer X within 60 s.
2. `peer_score` Prometheus gauge live.
3. No false positives on a 5-node mesh under normal load (24 h soak).
4. Lighthouse-reference defaults documented inline; deviations
   justified.
