# CURS3D 24-72h Soak Runbook

The `Final 10/10 Gate` from `21-10-out-of-10-execution-checklist.md` requires a
72h public soak with no manual intervention. This document describes how to run
that soak, what the monitoring catches, and how to read the result.

## Tools

Three artefacts make the soak self-documenting:

| Tool | Where | Purpose |
|------|-------|---------|
| `deploy/scripts/soak-monitor.sh` | runs on your Mac (or any operator host) | continuous polling of all 3 nodes, CSV log, alert log, pass/fail summary on exit |
| `deploy/scripts/soak-status.sh` | runs on your Mac | one-shot spot check — ASCII table + verdict line |
| `/api/metrics` (Prometheus) | each node | scrape-friendly metrics including `curs3d_latest_block_age_seconds`, `curs3d_finality_lag`, `curs3d_peer_count`, `curs3d_protocol_version` |

## What the monitor catches

`soak-monitor.sh` polls every 30s by default and writes one CSV line per node
per poll to `./soak.csv`. It emits an alert line to `./soak.alerts` whenever
any of these thresholds trip:

| Alert kind | Trigger |
|------------|---------|
| `STALL` | a node's `height` did not advance for `--stall` seconds (default 120). |
| `DIVERGENCE` | two nodes report the same `height` with different `latest_hash`. |
| `FINALITY_LAG` | `height - finalized_height > --finality-lag` (default 50 blocks). |
| `FINALITY_REGRESS` | a node's `finalized_height` decreased between polls (should never happen). |
| `PEER_DROP` | a node had `peer_count = 0` for `--peer-drop` seconds (default 120). |
| `API_DROP` | `/api/status` failed to respond on a node. |
| `API_RECOVER` | `/api/status` came back online after an `API_DROP`. |

Pass = `SOAK PASSED` printed on exit, with `0 stalls, 0 divergences, 0 api drops`.

## Running a soak

### Option A — full 72h pass

```bash
# Mac (assumes brew bash 5+):
brew install bash       # one-time
/opt/homebrew/bin/bash deploy/scripts/soak-monitor.sh \
    --duration 259200   \
    --interval 30       \
    --csv soak-72h.csv  \
    --alerts soak-72h.alerts
```

Leave it running. When it exits at the 72h mark:

- 0 stalls / 0 divergences / 0 api drops → `✓ SOAK PASSED` printed → tag `v0.4.0`
  (or `v1.0` if every other 10/10 gate is also met).
- Any alert → `✗ SOAK FAILED` printed → inspect `soak-72h.alerts`, fix root
  cause, redeploy via `deploy/scripts/full-rollout.sh`, restart the soak.

### Option B — 24h smoke before full soak

Same command, `--duration 86400`. Use this to catch obvious regressions before
committing to the full 72h.

### Option C — overnight on a remote host

The monitor only needs SSH access to the validators and `jq`. To run it on a
small VPS or Raspberry Pi instead of your laptop, copy the script over and
launch under `tmux` / `screen`:

```bash
scp deploy/scripts/soak-monitor.sh ops-host:~
ssh ops-host
tmux new -s soak
~/soak-monitor.sh --duration 259200
# Ctrl+B then D to detach. Reattach with: tmux attach -t soak
```

## Spot checks during a soak

```bash
deploy/scripts/soak-status.sh
```

Example output:

```
━━━━ CURS3D soak status ━━━━
NODE              HEIGHT    FINAL      LAG  PEERS     AGE  HASH
------------------------------------------------------------------------------
node1               4823     4820        3      2      8s  c41f15e9215a4a3e
node2               4823     4820        3      1      9s  c41f15e9215a4a3e
node3               4823     4820        3      1      7s  c41f15e9215a4a3e

✓ HEALTHY — all 3 nodes converged, finalizing, latest block <30s old.
```

The verdict line evaluates DIVERGENCE → ISOLATION → STALL → NO FINALITY →
HEALTHY in priority order.

## Reading the CSV

```
ts_utc,node,height,finalized,latest_hash,peer_count,age_secs,validators,protocol_version,api_ok
2026-05-05T18:00:00Z,curs3d-node1,4823,4820,c41f15e9...,2,8,3,5,1
```

A few useful one-liners:

```bash
# Per-node max height seen during soak
awk -F, 'NR>1 && $10==1 {if ($3>m[$2]) m[$2]=$3} END {for (h in m) print h": "m[h]}' soak.csv

# All divergence events (collapsed)
awk -F, 'NR>1 && $10==1 {print $1","$3","$5}' soak.csv | sort -u | awk -F, '{c[$2","$3]++} END {for (k in c) if (c[k]>1) print k}'

# Time series of finality lag on node1
awk -F, '$2=="curs3d-node1" && $10==1 {print $1, $3-$4}' soak.csv
```

## Prometheus integration (optional)

If `/api/metrics` is exposed publicly (or via a private VPN), point Prometheus
at each node and graph in Grafana:

```yaml
- job_name: curs3d
  scrape_interval: 15s
  static_configs:
    - targets:
      - 'curs3d-node1.internal:8080'
      - 'curs3d-node2.internal:8080'
      - 'curs3d-node3.internal:8080'
  metrics_path: /api/metrics
```

Recommended Grafana alerts:

- `curs3d_latest_block_age_seconds > 30 for 2m` → page on stall
- `curs3d_finality_lag > 30 for 1m` → page on finality drift
- `curs3d_peer_count < 1 for 2m` → page on isolation
- `up == 0 for 1m` → page on API drop

## Rolling restart during a soak

If you need to push a binary upgrade without ending the soak (e.g. a hotfix),
use `deploy/scripts/full-rollout.sh` and **let the monitor keep running**. The
restart will produce a brief `API_DROP` and `STALL` alert per node — those are
expected and don't fail the soak as long as everything recovers within the
threshold windows.

For the 10/10 gate, however, the canonical 72h soak must be **without** any
binary upgrade in the middle. Plan upgrades for between soak runs.

## Real incident log — 2026-05-05

The first 72h soak attempt was launched at `2026-05-05T23:02:34Z` and
**immediately surfaced two pre-existing issues** that no static test had
found. Logged here so the next operator does not waste hours rediscovering
them.

### Symptom 1 — `API_DROP` on node1, sustained
```
2026-05-05T21:02:34Z [API_DROP] curs3d-node1 /api/status unreachable
```
- `systemctl is-active curs3d` → `active`
- Process at 0.0% CPU, 12s of accumulated CPU time after 1h+ of running
- All worker threads parked in `futex_wait` (`/proc/<pid>/task/*/stack`)
- Only the libp2p IO thread in `epoll_wait` (idle, no events)

**Diagnosis**: chain-mutex deadlock. Some async path acquired
`chain.lock().await` and is awaiting on something that will never fire
(channel receive, condvar, or another lock). All API tasks pile up
behind the lock, never progress, libp2p sees no new state to gossip.

**Fix**: needs a stack trace from `gdb -p <pid>` or
`RUST_LOG=debug,tokio=trace` capture during a hang. Pending.

### Symptom 2 — `DIVERGENCE` between node2 and node3 at h=341+
```
2026-05-05T21:02:36Z [DIVERGENCE] height=351:
  curs3d-node2=b04bdcdf95f42d41138ba53b96799fbaecb82b63c0b89acd4531bb96d52b69c8
  VS
  curs3d-node3=9adc68fee3cbacc4ae41c0ef56c4aa8952bae0280e1666f4a74aed71a469dc38
```
Last finalized block on both nodes: h=340 hash `b8db70a3...` (agreed).

Per-node logs:
- node2 produced #341 at `20:29:14Z` (9s after parent → primary).
- node3 produced #341 at `20:30:05Z` (60s after parent → rank-2 backup).

The 51-second gap between productions is the smoking gun. With
`BACKUP_LEADER_TIMEOUT_SECS = 30s` (already bumped from 12s for this
exact class of issue), node3 only became eligible as backup *after* 30s.
That is correct — the bug is that node3 did not receive node2's #341
block during those 30s. Gossip propagation between node2 and node3
silently failed.

**Diagnosis**: the 3-node mesh relies on node1 as a relay. node2 and
node3 only configure node1 as `--bootnode`; libp2p gossipsub forwards
between them via node1's peer connection. When node1's consensus task
deadlocked (Symptom 1), its gossipsub task was starved of CPU and
stopped relaying. node2 and node3 partitioned. The
BACKUP_LEADER_TIMEOUT eventually elapsed and node3 produced a
competing block.

**Fix**: each node's systemd unit must add the *other two* as
`--bootnode`, not just node1. The peer IDs are now stable across
restarts (the new wipe in `full-rollout.sh` preserves
`/var/lib/curs3d/p2p_identity*`).

```
node1 PeerId: 12D3KooWLttF4EJ1SjiLEiXvJ1yqmJawLafv47r55T5xzSt1GHn2 (144.24.192.222)
node2 PeerId: 12D3KooWCL7dNFN2xz8yM65HDnNJUWF28K5qAd6d5ACT5ZCL1pb8 (84.235.238.213)
node3 PeerId: 12D3KooWPxvzCmTjDK4pn4E4gPnWdoVTr1wS8z3pdo7yM1oMZrGY (31.70.70.62)
```

Each unit needs the two other-node bootnodes appended to ExecStart, then
`systemctl daemon-reload && systemctl restart curs3d`. After the change,
verify with `soak-status.sh`: every node's `peer_count` should reach 2
within ~30s of all 3 starting.

### Resolution (commit `0476981`, 2026-05-05 23:50 UTC)

Both root causes fixed and verified on the live testnet.

**Layer 1 — sled deadlock, first mitigation:** `add_block` stopped
calling `persist_full_state` on every block and kept only `put_block`
per block plus a full `replace_*` pass at epoch boundaries. This reduced
write pressure by ~32×, but the 2026-05-06 overnight soak proved it was
not sufficient: sled 0.34 deadlocked again at later heights.

**Layer 1b — long-term storage fix:** persistent storage moved from
sled 0.34 to redb. The live node still uses async persistence as a
second line of defense: `add_block`, finality, mempool admission,
snapshot application, and slashing enqueue bounded persistence jobs and
return without disk IO while `Mutex<Blockchain>` is held. If storage ever
stalls, only the persistence worker is affected; consensus, RPC, and
gossipsub keep running. Restart recovery depends on the latest completed
background snapshot plus peer/snapshot sync for any missing tail.

**Layer 2 — mesh topology:** each systemd unit was patched to
list the OTHER two nodes as `--bootnode` (peer IDs above). With
direct connections in place, no single node is a gossipsub
relay SPOF.

Post-fix verification:
- All 3 nodes converged on identical hash by h=4
- Finality lag = 0 since h=33
- 7 Solidity contracts redeployed cleanly
- Soak monitor still running; no new DIVERGENCE / STALL alerts

### Lesson
The soak monitor doing exactly what it was built for in under 60 seconds
of runtime is the strongest evidence that this kind of observability has
to be the *default* during testnet operations, not an afterthought. The
pre-soak chain looked perfectly healthy in `/api/status` snapshots; the
problem was only visible *across* nodes and *over time*. gdb on the
hung node — not source review — was what isolated the sled deadlock.
