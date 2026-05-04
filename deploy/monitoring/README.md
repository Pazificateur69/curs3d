# CURS3D Public Monitoring Stack

Prometheus + Grafana exposed at **https://status.curs3d.fr** for public testnet observability.

## What you get

- Live block height, finalized height, validators, pending txs
- Block production rate over time
- Receipt/log totals
- Per-node uptime + reachability
- Anonymous read-only Grafana access (Viewer role) — admin login still required to edit

## Architecture

```
Validators (api.curs3d.fr/api/metrics ...)  ←─ Prometheus scrape (every 15s)
                                                       │
                                                  TSDB (90d retention)
                                                       │
                                                Grafana (auto-provisioned)
                                                       │
                                              nginx (status.curs3d.fr, TLS)
```

## Deploy

Pick a host with Docker installed (can be one of the validators or a dedicated VPS).

```bash
cd deploy/monitoring

# Set the Grafana admin password (strong passphrase!)
echo "GRAFANA_ADMIN_PASSWORD=$(openssl rand -base64 32)" > .env

# Start the stack
docker compose up -d
docker compose logs -f
```

Prometheus listens on `127.0.0.1:9090`, Grafana on `127.0.0.1:3001`.

## Expose at status.curs3d.fr

```bash
# Add an A record on Hostinger:  status.curs3d.fr  ->  <this-host-IP>

sudo cp deploy/monitoring/nginx-status.conf /etc/nginx/sites-available/curs3d-status
sudo ln -s /etc/nginx/sites-available/curs3d-status /etc/nginx/sites-enabled/
sudo certbot --nginx -d status.curs3d.fr
sudo systemctl reload nginx
```

## Adding more validators

When `api2.curs3d.fr`, `api3.curs3d.fr` ... come online, edit `prometheus.yml` and
uncomment the additional `targets:` entries, then:

```bash
curl -X POST http://127.0.0.1:9090/-/reload
```

## Updating the dashboard

The dashboard JSON lives at `grafana/dashboards/curs3d.json` and is reloaded every 30s
via the file provisioner. UI edits are disabled (read-only) — change the JSON in git
and the running Grafana picks it up automatically.

## Troubleshooting

- **Prometheus shows targets DOWN:** check that `/api/metrics` is publicly reachable
  over HTTPS and that CORS is not blocking the scrape (it shouldn't — Prometheus
  ignores CORS, but check connectivity with `curl https://api.curs3d.fr/api/metrics`).
- **Grafana shows "no data":** check `Configuration → Data sources → Prometheus` is
  green, then in `Explore` query `up`.
- **Anonymous viewers can't see anything:** verify `GF_AUTH_ANONYMOUS_ENABLED=true`
  and that the dashboard is in the default org.
