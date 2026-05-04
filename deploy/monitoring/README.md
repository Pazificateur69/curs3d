# CURS3D Public Monitoring Stack

Last updated: **2026-05-04**.

Four-service Docker stack exposed at **https://status.curs3d.fr** :

| Service | Local port | Image | Role |
|---------|-----------|-------|------|
| `prometheus` | `127.0.0.1:9090` | `prom/prometheus:v2.55.0` | Scrapes `https://api.curs3d.fr/api/metrics` (and node-exporter) every 15 s, 90-day TSDB retention. |
| `grafana` | `127.0.0.1:3001` | `grafana/grafana:11.3.0` | Auto-provisioned dashboard, anonymous Viewer enabled. Served at `/`. |
| `uptime-kuma` | `127.0.0.1:3002` | `louislam/uptime-kuma:1` | HTTP/TCP/keyword monitors (api, explorer, p2p, captcha verifier). Served at `/status/`. |
| `node-exporter` | `127.0.0.1:9100` | `prom/node-exporter:v1.9.0` | Host CPU/RAM/disk/FS/network metrics. `network_mode: host` so prometheus can hit it via localhost. |

## What you get

- Live block height, finalized height, validator count, pending tx count, block production rate, receipt/log totals (Grafana via Prometheus).
- HTTP/TCP probes with on-page status badges and historical uptime (Uptime-Kuma at `/status/`).
- Host metrics: CPU, memory, disk fill, file-system pressure, network throughput (node-exporter scraped by Prometheus).
- Anonymous read-only Grafana access (Viewer role); admin login still required to edit. Edits via UI are disabled — modify `grafana/dashboards/*.json` in git and the file provisioner reloads in 30 s.

## Architecture

```
┌──────────────────────────┐
│  api.curs3d.fr/api/metrics │──┐
└──────────────────────────┘  │
                              ▼ scrape /15s
                       ┌──────────────┐
                       │  prometheus  │── TSDB (90d) ──┐
                       └──────────────┘                ▼
┌────────────────┐        scrape /15s             ┌─────────┐
│  node-exporter │──────────────────────────────▶│ grafana  │
│  (host metrics)│                                │  /       │
└────────────────┘                                └─────────┘
                                                       ▲
                                                       │
                                          nginx ───────┤
                                                       │
                                                ┌─────────────┐
                                                │ uptime-kuma │── /status/
                                                └─────────────┘
```

## Deploy

```bash
cd deploy/monitoring

# Strong random Grafana admin password
echo "GRAFANA_ADMIN_PASSWORD=$(openssl rand -base64 32)" > .env

docker compose up -d
docker compose ps
docker compose logs -f
```

## Expose at status.curs3d.fr

```bash
# DNS (Hostinger): A record  status.curs3d.fr  ->  144.24.192.222

sudo cp deploy/monitoring/nginx-status.conf /etc/nginx/sites-available/curs3d-status
sudo ln -s /etc/nginx/sites-available/curs3d-status /etc/nginx/sites-enabled/
sudo certbot --nginx -d status.curs3d.fr
sudo systemctl reload nginx
```

The vhost serves:
- `/` → Grafana (proxy to `127.0.0.1:3001`)
- `/status/` → Uptime-Kuma (proxy to `127.0.0.1:3002`, with WebSocket upgrade for real-time pings)

## Uptime-Kuma monitors (current)

| Monitor | Type | Target |
|---------|------|--------|
| API status | HTTPS keyword | `https://api.curs3d.fr/api/status` — keyword `"ok":true` |
| Explorer | HTTPS | `https://explorer.curs3d.fr` |
| Site | HTTPS | `https://curs3d.fr` |
| P2P | TCP | `144.24.192.222:4337` |
| Captcha verifier (internal) | HTTP | `http://127.0.0.1:8090/health` |

Uptime-Kuma is configured manually on first boot. After deploy, log in
once at `https://status.curs3d.fr/status/` to set the admin user, add the
monitors above, then the dashboard becomes public.

## Adding more validators

When `api2.curs3d.fr`, `api3.curs3d.fr` ... come online, edit
`prometheus.yml` and uncomment the additional `targets:` entries, then:

```bash
curl -X POST http://127.0.0.1:9090/-/reload
```

Add matching HTTPS-keyword + TCP monitors in Uptime-Kuma.

## Updating the dashboard

The dashboard JSON lives at `grafana/dashboards/curs3d.json` and is reloaded
every 30 s via the file provisioner. UI edits are disabled — change the
JSON in git and Grafana picks it up automatically.

## Troubleshooting

- **Prometheus shows targets DOWN:** `curl https://api.curs3d.fr/api/metrics` from the host. CORS does not affect Prometheus, but firewalling or a misnamed target will.
- **Grafana shows "no data":** `Configuration → Data sources → Prometheus` should be green; test it from `Explore` with `up`.
- **Anonymous viewers can't see anything:** verify `GF_AUTH_ANONYMOUS_ENABLED=true` and that the dashboard is in the default org.
- **Uptime-Kuma 502 behind nginx:** the `/status/` location must forward `Upgrade` and `Connection` headers (already in `nginx-status.conf`).
- **node-exporter unreachable from prometheus:** the service uses `network_mode: host` and listens on `127.0.0.1:9100`. Prometheus must scrape `127.0.0.1:9100`, not the container name.
