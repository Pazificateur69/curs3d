# CURS3D — Runbook de Deploiement Multi-Node

Derniere mise a jour: 2026-04-23

## Architecture

```
                    Internet
                       |
               api.curs3d.fr (DNS)
                       |
                 [node1 — bootstrap]
                 144.24.192.222
                 nginx + TLS + API
                       |
            +----------+----------+
            |                     |
      [node2]               [node3/4]
      84.235.238.213        en attente capacite ARM
```

- 4 validateurs dans le genesis (BFT 2/3 finality)
- node1 = bootstrap + API publique + explorer
- node2-4 = validateurs connectes a node1
- Tolerance de panne: 1 node down sur 4, le reseau continue

## Nodes

| Node | IP | Role | Validateur | SSH |
|------|-----|------|-----------|-----|
| node1 | 144.24.192.222 | Bootstrap + API + Explorer | CUR79c1D9E08D1b2347E27B31b1E8f8733c68E7c8D2 | `ssh curs3d-node1` |
| node2 | 84.235.238.213 | Validateur | CUR34BC42D53dD63FbFEE51778ADb6d568279eCE23A | `ssh curs3d-node2` |
| node3 | TBD | Validateur | CURfe35F718fB4939a80EeEb98F8F457fb434A89BCd | — |
| node4 | TBD | Validateur | CUR578e3E7d7C238cd1b32805013E385C032Df9bC4F | — |
| Faucet | — | — | CUR0207E1293cFCFfe0fF1517186b982E5DE6f61e8A | — |

## Infra

| Element | Valeur |
|---------|--------|
| Provider | Oracle Cloud (Always Free ARM) |
| Region | eu-marseille-1 |
| Shape | VM.Standard.A1.Flex (1 OCPU, 6 GB RAM par node) |
| OS | Ubuntu 22.04 ARM64 |
| Quota ARM total | 4 OCPU / 24 GB (Free Tier) |
| DNS | Hostinger (curs3d.fr) |
| Bootnode PeerId | 12D3KooWC6hEP7YRySmXNA4pTcTi4XMkch525jFWbHNDtwxW6K2K |

## Acces SSH

```bash
ssh curs3d-node1   # bootstrap + API
ssh curs3d-node2   # validateur 2
```

Config dans `~/.ssh/config`:
```
Host curs3d-node1
    HostName 144.24.192.222
    User ubuntu
    IdentityFile ~/.ssh/id_ed25519_server

Host curs3d-node2
    HostName 84.235.238.213
    User ubuntu
    IdentityFile ~/.ssh/id_ed25519_server
```

## Endpoints publics

| Service | URL |
|---------|-----|
| API | https://api.curs3d.fr/api/status |
| Explorer | https://explorer.curs3d.fr |
| WebSocket | wss://api.curs3d.fr/ws |
| Faucet | POST https://api.curs3d.fr/api/faucet/request |
| P2P Bootnode | 144.24.192.222:4337 |

## Ports (ouverts dans Oracle Security List)

| Port | Service | Expose |
|------|---------|--------|
| 22 | SSH | public |
| 80 | nginx (redirect HTTPS) | public (node1 only) |
| 443 | nginx (TLS) | public (node1 only) |
| 4337 | libp2p P2P | public (tous les nodes) |
| 8080 | API HTTP (hyper) | localhost only |
| 9545 | TCP RPC (CLI) | localhost only |

## Fichiers sur chaque serveur

```
/usr/local/bin/curs3d              # Binaire
/etc/curs3d/
  validator.json                   # Wallet validateur (Dilithium L5, AES-256-GCM)
  validator.password               # Password du wallet validateur
  faucet.json                      # Wallet faucet
  faucet.password                  # Password du wallet faucet
  genesis.public-testnet.json      # Config genesis (identique sur tous les nodes)
/var/lib/curs3d/                   # Data dir (sled DB, state)
/etc/systemd/system/curs3d.service # Service systemd
/usr/local/bin/curs3d-healthcheck.sh   # Healthcheck script
/etc/cron.d/curs3d-healthcheck         # Cron healthcheck (toutes les 2 min)
```

Node1 uniquement:
```
/var/www/curs3d/                   # Site web (explorer, docs)
/etc/nginx/sites-available/curs3d.conf # Config nginx
```

## Fichiers locaux importants

```
/tmp/curs3d-multinode/
  validator1.json + .password      # Wallet node1
  validator2.json + .password      # Wallet node2
  validator3.json + .password      # Wallet node3 (pour futur deploiement)
  validator4.json + .password      # Wallet node4 (pour futur deploiement)
  faucet.json + .password          # Wallet faucet
  genesis.json                     # Genesis avec 4 validateurs
  setup-node.sh                    # Script de setup d'un node
```

## Commandes utiles

### Status de tous les nodes
```bash
for n in 1 2; do
  echo "=== Node $n ==="
  ssh curs3d-node$n "curl -s http://localhost:8080/api/status | jq '{height: .data.height, validators: .data.active_validators, finalized: .data.finalized_height, epoch: .data.epoch}'"
done
```

### Logs en temps reel
```bash
ssh curs3d-node1 "sudo journalctl -u curs3d -f"
ssh curs3d-node2 "sudo journalctl -u curs3d -f"
```

### Redemarrer un node
```bash
ssh curs3d-node1 "sudo systemctl restart curs3d"
ssh curs3d-node2 "sudo systemctl restart curs3d"
```

### Verifier la connectivite P2P
```bash
ssh curs3d-node2 "nc -z -w5 144.24.192.222 4337 && echo OK || echo BLOCKED"
```

### Verifier nginx + TLS (node1)
```bash
ssh curs3d-node1 "sudo nginx -t && sudo certbot certificates"
```

### Logs du healthcheck
```bash
ssh curs3d-node1 "tail -20 /var/log/curs3d-healthcheck.log"
ssh curs3d-node2 "tail -20 /var/log/curs3d-healthcheck.log"
```

## Haute disponibilite

Chaque node est configure pour ne JAMAIS s'arreter:

1. **systemd Restart=always** — relance en 3s apres un crash
2. **WatchdogSec=120** — kill + relance si le process freeze pendant 2 min
3. **StartLimitBurst=10 / StartLimitAction=reboot** — 10 crashes en 5 min = reboot complet
4. **Healthcheck cron (*/2)** — toutes les 2 min, restart si l'API ne repond plus
5. **Certbot auto-renew** — renouvellement TLS automatique (node1)

## Ajouter node3 ou node4

Quand la capacite ARM Oracle se libere:
```bash
cd ~/Desktop/Web3/curs3d
./deploy/scripts/add-node.sh 3 144.24.192.222 12D3KooWC6hEP7YRySmXNA4pTcTi4XMkch525jFWbHNDtwxW6K2K
```

## Redeploy (mise a jour du code)

Depuis le Mac local, pour tous les nodes:
```bash
for n in 1 2; do
  echo "=== Deploying to node$n ==="
  rsync -az --exclude 'target' --exclude '.git' ~/Desktop/Web3/curs3d/ curs3d-node$n:/home/ubuntu/curs3d/
  ssh curs3d-node$n "source ~/.cargo/env && cd ~/curs3d && cargo build --release && sudo cp target/release/curs3d /usr/local/bin/curs3d && sudo systemctl restart curs3d"
  ssh curs3d-node$n "curl -s http://localhost:8080/api/status | jq .data.height"
done
```

Mettre a jour le site web (node1 uniquement):
```bash
ssh curs3d-node1 "sudo cp -r /home/ubuntu/curs3d/website/* /var/www/curs3d/"
```

## Oracle Cloud CLI

Configure sur le Mac local (`~/.oci/config`).

```bash
# Lister les instances
oci compute instance list --compartment-id ocid1.tenancy.oc1..aaaaaaaalu3mxlfrfi3nb5lqazygqtj6um2epn4ra2s2hp4kblbrs3zbxmva --lifecycle-state RUNNING --query "data[*].{Name:\"display-name\", State:\"lifecycle-state\"}" --output table

# Security list (port 4337 ouvert)
# ID: ocid1.securitylist.oc1.eu-marseille-1.aaaaaaaacsxi5gj43pfndqjyjjz3wag5mg7eipkqf6ktugjqcldtjnmqzjya
```

## DNS (Hostinger)

Domaine: curs3d.fr
Enregistrements A:
- `@` → 144.24.192.222 (node1)
- `api` → 144.24.192.222 (node1)
- `explorer` → 144.24.192.222 (node1)

## Troubleshooting

### Les nodes ne se connectent pas entre eux
```bash
# Verifier que le port 4337 est ouvert dans la security list Oracle
ssh curs3d-node2 "nc -z -w5 144.24.192.222 4337 && echo OK || echo BLOCKED"
# Si BLOCKED: verifier la security list dans la console Oracle
```

### Un node ne produit plus de blocs
```bash
ssh curs3d-nodeN "sudo journalctl -u curs3d --since '10 min ago' --no-pager | tail -50"
ssh curs3d-nodeN "sudo systemctl restart curs3d"
```

### Permission denied SSH
```bash
ssh -vvv -i ~/.ssh/id_ed25519_server ubuntu@<IP>
```

### Certificat TLS expire (node1)
```bash
ssh curs3d-node1 "sudo certbot renew --force-renewal && sudo systemctl reload nginx"
```

### Disque plein
```bash
ssh curs3d-nodeN "df -h && sudo journalctl --vacuum-size=500M"
```

### Oracle a reclaim une instance (Free Tier)
Utiliser le script add-node.sh pour recreer le node.
