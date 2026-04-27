# CURS3D — Runbook de Deploiement Multi-Node

Derniere mise a jour: 2026-04-27

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

- 2 validateurs actifs (node1 + node2), extensible a 4
- node1 = bootstrap + API publique + explorer + site web
- node2 = validateur connecte a node1
- node3/4 = en attente de capacite ARM Oracle

**Security hardening (2026-04-24):**
- Governance: snapshot-based voting (anti double-vote)
- P2P: bounded deserialization 16MB (anti OOM)
- Fee market: base_fee >= 1 (anti free spam)
- Mempool: nonce floor check
- Chain: reorg depth limit 64 blocks
- Wallet: Argon2id m=64MB, t=3, p=4
- Zero Box::leak memory leaks

**IMPORTANT:** Les wallets doivent etre crees sur le serveur (pas cross-compiles) a cause de Argon2id.

## Nodes

| Node | IP | Role | Validateur | SSH |
|------|-----|------|-----------|-----|
| node1 | 144.24.192.222 | Bootstrap + API + Explorer | CURe1Fa551B3f0524EfD8d0673cdBF9fD0e199458c5 | `ssh curs3d-node1` |
| node2 | 84.235.238.213 | Validateur | CURdC1ecceD4f12Cb3E34BD0d43E72d6D04fC4823dd | `ssh curs3d-node2` |
| node3 | TBD | Validateur | TBD (wallet a creer sur le serveur) | — |
| node4 | TBD | Validateur | TBD (wallet a creer sur le serveur) | — |
| Faucet | — | — | CUR34cafc74B750C0e0150877e99cd27D77C6c4fC44 | — |

## Infra

| Element | Valeur |
|---------|--------|
| Provider | Oracle Cloud (Always Free ARM) |
| Region | eu-marseille-1 |
| Shape | VM.Standard.A1.Flex (1 OCPU, 6 GB RAM par node) |
| OS | Ubuntu 22.04 ARM64 |
| Quota ARM total | 4 OCPU / 24 GB (Free Tier) |
| DNS | Hostinger (curs3d.fr) |
| Bootnode PeerId | 12D3KooWGy9BLopUe6CmnxuFgk5pCou8Kj5R1DXa9XLga9MD63va |

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
./deploy/scripts/add-node.sh 3 144.24.192.222 12D3KooWGy9BLopUe6CmnxuFgk5pCou8Kj5R1DXa9XLga9MD63va
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
