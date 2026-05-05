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
