# Refactor — Chaos localnet 5-7 nodes CI nightly

**Status**: Design.
**Tracking task**: #32.
**Estimated effort**: 2-3 weeks.
**Blocking**: confidence in consensus + sync under partition / loss.

## Motivation

Today's tests are entirely **single-process unit tests** (211+ of
them, all green). They prove individual functions behave; they do
*not* prove the cluster behaves under adverse conditions.

The 2026-05-20 incidents (Plesk-1 stealth fork + n1 OOM cascade)
would have been caught by a chaos CI:
- *"Run 5 nodes for 2 h with one node periodically force-restarted."*
- *"Run 5 nodes with 2-node-pair network partitions every 60 s."*
- *"Run 5 nodes with one node sending mutated blocks for 30 s."*

Without these, every consensus bug is found in production.

## Goal (P0)

Nightly GitHub Actions job that:
1. Boots a 5-node Docker Compose cluster.
2. Runs N predefined chaos scenarios sequentially.
3. After each, asserts the cluster has converged (same finalized
   height + state_root on all 5 nodes).
4. Uploads logs + Grafana exports on failure.

## Architecture

```
.github/workflows/chaos.yml          ← GitHub Actions definition
deploy/chaos/
├── docker-compose.yml               ← 5 curs3d nodes + chaos-proxy
├── chaos-proxy/Dockerfile           ← toxiproxy or pumba wrapper
├── scenarios/
│   ├── 01_random_kill.sh            ← kill -9 random node every 30 s for 10 min
│   ├── 02_network_partition.sh      ← split into {1,2} | {3,4,5} every 60 s
│   ├── 03_packet_loss.sh            ← 30 % loss on a random node
│   ├── 04_clock_skew.sh             ← skew one node's clock by +60 s
│   ├── 05_slow_link.sh              ← 500 ms latency on n1<->n2
│   ├── 06_disk_full.sh              ← fill / on n3, expect graceful degradation
│   ├── 07_double_sign.sh            ← inject equivocation evidence
│   ├── 08_snapshot_mid_sync.sh      ← trigger snapshot during cold sync
│   ├── 09_byzantine_block.sh        ← n5 produces blocks with bad merkle_root
│   ├── 10_fork_resolution.sh        ← n5 produces a 3-block fork
│   └── ...                           ← target ~30 scenarios
├── assertions/
│   ├── final_height_matches.sh      ← curl /api/status, diff heights
│   ├── final_hash_matches.sh        ← curl /api/block-by-height, diff hashes
│   ├── finality_progressed.sh       ← finalized_height should advance
│   └── no_panics_in_logs.sh         ← grep panic'd in docker logs
└── README.md                        ← how to run locally
```

## Tools

- **Toxiproxy** (Shopify) — TCP-level proxy to inject latency / drop / partition.
  Lightweight; runs as a sidecar Docker container per node.
- **Pumba** (alexei-led/pumba) — Docker-native chaos: kill/pause/network
  effects via Docker API. Used for kill/restart scenarios.
- **Both already containerized** — no host dependency beyond Docker.

## Scenario template

Every scenario script follows the same skeleton:

```bash
#!/bin/bash
set -euo pipefail
SCENARIO_NAME="$(basename "$0" .sh)"
LOG_DIR="/tmp/chaos-logs/${SCENARIO_NAME}"
mkdir -p "$LOG_DIR"

# 1. Wait for cluster baseline (all 5 nodes at finality lag <= 2).
./assertions/wait_for_baseline.sh

# 2. Inject the chaos.
INITIAL_HEIGHT=$(curl -s http://localhost:8080/api/status | jq .data.height)
inject_chaos &              # scenario-specific
CHAOS_PID=$!

# 3. Run for N seconds.
sleep ${CHAOS_DURATION:-300}

# 4. Stop the chaos.
kill $CHAOS_PID 2>/dev/null || true
recover_chaos               # scenario-specific cleanup

# 5. Give the cluster 60 s to recover.
sleep 60

# 6. Assertions.
./assertions/final_height_matches.sh   || { dump_logs; exit 1; }
./assertions/final_hash_matches.sh     || { dump_logs; exit 1; }
./assertions/finality_progressed.sh "$INITIAL_HEIGHT" || { dump_logs; exit 1; }
./assertions/no_panics_in_logs.sh      || { dump_logs; exit 1; }

echo "✅ $SCENARIO_NAME passed"
```

## Phases

### Phase A — Docker Compose baseline (3 days)

1. Write `deploy/chaos/docker-compose.yml` with 5 curs3d nodes
   (mutual bootnodes, port 8080N each).
2. Build curs3d Docker image (already exists at root `Dockerfile`).
3. Run locally: `docker-compose up` and confirm chain progresses for
   10 min.
4. Add `assertions/wait_for_baseline.sh`.

### Phase B — First 5 scenarios (1 week)

Start with the simplest (kill/restart, network loss, clock skew).
Each scenario is a separate PR so we can review one at a time.

### Phase C — Adversarial scenarios (1 week)

Byzantine block production, equivocation injection, snapshot races.
These require a "test-only" build flag (`--cfg test_chaos`) that
exposes hooks to inject malformed blocks.

### Phase D — CI wiring (3 days)

1. `.github/workflows/chaos.yml` runs nightly on cron.
2. Uses `docker-in-docker` runner (or a self-hosted runner with
   Docker access).
3. On failure: upload logs as artifact, post a Discord webhook alert.
4. Job timeout: 90 min (worst case: 30 scenarios × 5 min each).

### Phase E — Tuning (ongoing)

Inevitably some scenarios will be flaky. Move them to a "quarantine"
matrix lane that warns but doesn't fail the build, until rooted out.

## Risks

| Risk | Severity | Mitigation |
|------|----------|------------|
| Flaky scenarios block CI permanently | High | Quarantine lane; assertions tolerate small timing windows |
| GitHub Actions runners can't sustain 5-node Docker for 90 min | Medium | Move to self-hosted runner ($5/mo Hetzner VM) if needed |
| Scenarios reveal real bugs we can't fix quickly | Catastrophic | That's the *point* — accept the noise; mainnet would expose them anyway |
| Cluster requires too much config to bring up | Medium | Compose file is the single source of truth; no per-machine config |

## Acceptance criteria

1. `deploy/chaos/docker-compose.yml` brings up a 5-node mesh in <60 s.
2. At least 10 scenario scripts exist and pass on a clean build.
3. CI workflow runs nightly + on workflow_dispatch.
4. Failed runs upload logs as artifacts.
5. No false positives over a 7-day rolling window.

## Out-of-scope

- Stress tests (sustained throughput); separate concern, handled by
  benches/ + criterion.
- Mainnet-style real-world conditions (geographic latency, ISP
  failures); covered by actual testnet operations, not CI.
