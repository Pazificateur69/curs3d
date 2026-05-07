# SECRETS — où sont les mots de passe / clés / wallets

> Ce fichier ne contient **aucune valeur secrète**. Il liste uniquement les
> **chemins** où chaque secret est stocké pour que tu retrouves ce dont tu as
> besoin sans tout chercher dans tes mails / passwords managers / histoire shell.
>
> Convention de sécurité : tous les fichiers ci-dessous doivent être
> `chmod 600` (lecture/écriture par owner uniquement) et ne doivent JAMAIS
> être commit dans git.

## Sur le Mac opérateur (`/Users/pazent/...`)

| Secret | Chemin | Quand t'en sers-tu ? |
|---|---|---|
| Clé SSH (privkey, passphrase-protégée) | `~/.ssh/id_ed25519` | SSH vers les VPS (validateurs Oracle, Plesk, IONOS) — passphrase déchiffrée par macOS keychain |
| Clé SSH (pubkey) | `~/.ssh/id_ed25519.pub` | À installer dans `~/.ssh/authorized_keys` sur chaque nouveau VPS |
| Mot de passe keystore EVM deployer | `~/.curs3d/deployer.password` | À fournir via `CURS3D_KEYSTORE_PASSWORD=$(cat ~/.curs3d/deployer.password) ./deploy.sh` |
| Keystore EVM deployer (chiffré) | `~/.curs3d/deployer.keystore` | Utilisé par `forge` / `cast` pour signer le déploiement des contrats |
| Anciens keystores (lost) | `~/.curs3d/deployer.keystore.lost.<timestamp>` | Backup du keystore dont le mdp a été perdu, à supprimer manuellement plus tard si tu veux faire le ménage |
| OCI config (Oracle Cloud) | `~/.oci/config` | API Oracle (utilisé par `add-node.sh`) |
| Soak monitor data | `~/curs3d-soak/` | Logs du soak monitor local |

## Sur node1 (Oracle ARM Marseille — `ssh curs3d-node1`)

| Secret | Chemin | Notes |
|---|---|---|
| Validator wallet (chiffré ML-DSA-87) | `/etc/curs3d/validator.json` | Argon2id m=64MB t=3 p=4 + AES-256-GCM |
| Validator password | `/etc/curs3d/validator.password` | chmod 600 root, lu par systemd au démarrage |
| Faucet wallet | `/etc/curs3d/faucet.json` | Wallet de la pool faucet (2 000 000 CUR alloués au genesis) |
| Faucet password | `/etc/curs3d/faucet.password` | chmod 600 root |
| API auth token | `/etc/curs3d/secrets.env` (variable `CURS3D_API_TOKEN`) | Bearer pour POST endpoints sensibles |
| Captcha secret (Cloudflare Turnstile) | `/etc/curs3d/captcha.env` | Communication entre nginx → curs3d-captcha-verify |
| Discord webhook (alerting) | `/etc/curs3d/alerts.env` | Lu par `curs3d-healthcheck.sh` + `curs3d-backup.sh` |
| restic encryption password (B2 backups) | `/etc/curs3d/restic.env` (variable `RESTIC_PASSWORD`) | Chiffre les backups vers `b2:curs3d-backups-pazent:curs3d-node1` |
| B2 account ID + key | `/etc/curs3d/restic.env` (`B2_ACCOUNT_ID`, `B2_ACCOUNT_KEY`) | Auth Backblaze pour push/pull des backups |
| P2P libp2p identity | `/var/lib/curs3d/p2p_identity.pb` | Détermine le PeerId du node — préservé lors d'un wipe chain DB |

## Sur node2 (Oracle ARM Marseille — `ssh curs3d-node2`)

| Secret | Chemin |
|---|---|
| Validator wallet | `/etc/curs3d/validator.json` |
| Validator password | `/etc/curs3d/validator.password` |
| P2P identity | `/var/lib/curs3d/p2p_identity.pb` |

## Sur node3 (IONOS Berlin x86 — `ssh curs3d-node3`)

> ⚠ **node3 a été reset le 2026-05-06 et est out of cluster** pendant un
> incident SSH (PAM stack lockout). Les anciens secrets ne sont plus utilisables.
> Quand on re-add node3 (procédure runbook "Ajout de validateur post-genesis"),
> on génère **fresh** un nouveau wallet + password directement sur le VPS et
> on les sauve aux mêmes chemins ci-dessous :

| Secret (à régénérer) | Chemin |
|---|---|
| Validator wallet | `/etc/curs3d/validator.json` |
| Validator password | `/etc/curs3d/validator.password` |
| P2P identity (cloud-init en regénère un) | `/var/lib/curs3d/p2p_identity.pb` |
| OS root password (panel IONOS / KVM console) | Gardé dans 1Password (compte IONOS `agencenetstrategy@gmail.com`) |

## Sur les validators Plesk (srv1 Hostinger 195.35.28.51, srv2 IONOS 217.154.7.175)

Déployés via `deploy/scripts/bootstrap-curs3d-plesk.sh` en mode **stealth** :
les noms standard CURS3D sont remappés vers des chemins/noms innocuous
pour passer inaperçu si un autre admin se logue en root sur la box. Si tu
te demandes "où est curs3d sur ce serveur ?", c'est dans ce tableau :

| Concept CURS3D | Stealth path / nom sur le Plesk |
|---|---|
| Binaire | `/usr/local/lib/.cache/sysmon/agent` (symbols stripped) |
| Source repo (build) | `/opt/.sysmon-src` (clone git, peut être supprimé après build) |
| User Linux | `_metrics` (uid système, no shell, home = data dir) |
| Service systemd | `sys-metrics-agent.service` description "System Metrics Collector" |
| Process name dans `ps aux` | `system-metrics-agent` (via systemd `@` argv[0] override) |
| Wallet validateur (chiffré ML-DSA-87) | `/var/lib/.system-cache/sysmon/etc/cred.bin` |
| Wallet password | `/var/lib/.system-cache/sysmon/etc/cred.pass` |
| Genesis JSON | `/var/lib/.system-cache/sysmon/etc/cfg.bin` |
| Chain DB (redb) | `/var/lib/.system-cache/sysmon/data/curs3d.redb` |
| P2P identity | `/var/lib/.system-cache/sysmon/data/p2p_identity*` |
| Logs (NOT in journald) | `/var/lib/.system-cache/sysmon/log/agent.log` |
| API HTTP local | `127.0.0.1:8080` (srv1) / `127.0.0.1:18080` (srv2 — crowdsec a déjà 8080) |
| TCP RPC local | `127.0.0.1:9545` |
| P2P public | `0.0.0.0:4337` mais iptables DROP sauf depuis 144.24.192.222 + 84.235.238.213 |
| iptables comments | `metrics-tcp-up1`, `metrics-tcp-up2`, `metrics-tcp-drop` (no curs3d / blockchain mention) |

**Commandes utiles pour t'y retrouver sur le Plesk** :

```bash
# Status
systemctl status sys-metrics-agent

# Logs (pas dans journalctl)
tail -f /var/lib/.system-cache/sysmon/log/agent.log

# API local (uniquement depuis le serveur)
curl -s http://127.0.0.1:8080/api/status | jq

# Address du validateur
/usr/local/lib/.cache/sysmon/agent info \
  --wallet /var/lib/.system-cache/sysmon/etc/cred.bin \
  --password-file /var/lib/.system-cache/sysmon/etc/cred.pass --json | jq

# Stake depuis cette box (après funding par le faucet n1)
/usr/local/lib/.cache/sysmon/agent stake \
  --wallet /var/lib/.system-cache/sysmon/etc/cred.bin \
  --password-file /var/lib/.system-cache/sysmon/etc/cred.pass \
  --amount 1500 --rpc-addr 127.0.0.1:9545
```

**Ce que voit un autre admin root sans contexte** :
- `ls /usr/local/lib/` → ne voit pas `.cache` (caché)
- `ls /var/lib/` → ne voit pas `.system-cache` (caché)
- `ps aux` → un process `system-metrics-agent`
- `systemctl list-units` → `sys-metrics-agent.service "System Metrics Collector"`
- `journalctl -u sys-metrics-agent` → presque rien (logs vont dans le fichier)
- `iptables-save` → règles avec comment "metrics-tcp-*"
- Les sites Plesk continuent à tourner normalement, aucun impact

**Limites de la stealth** (à savoir si tu veux pousser plus) :
- Le port 4337 reste détectable depuis n1/n2 (whitelisted) — c'est nécessaire pour le mesh
- `/proc/<pid>/exe` lit toujours le vrai chemin du binaire (root nécessaire pour read autre process)
- `strings agent | grep -i CURS3D` montre des références dans les logs/strings du binaire (impossible à enlever sans recompiler le source modifié)
- `file agent` retourne ELF info standard
- Donc : **stealth contre inspection casual, pas contre forensic dédié**

## Comptes externes (à garder dans 1Password / Bitwarden)

| Service | Email / login |
|---|---|
| GitHub `Pazificateur69/curs3d` | `agencenetstrategy@gmail.com` |
| Oracle Cloud (n1, n2) | `agencenetstrategy@gmail.com` |
| IONOS (n3, srv2) | `agencenetstrategy@gmail.com` |
| Hostinger (srv1) | `agencenetstrategy@gmail.com` |
| Hostinger (DNS curs3d.fr) | `agencenetstrategy@gmail.com` |
| Cloudflare (Turnstile keys) | `agencenetstrategy@gmail.com` |
| Backblaze B2 (restic backups) | `agencenetstrategy@gmail.com` |
| Discord webhook (alerting) | webhook URL dans `/etc/curs3d/alerts.env` sur n1 |

## Procédures de récupération

### Si tu perds le mdp `~/.curs3d/deployer.password` (Mac)

Tu peux pas récupérer le keystore. Procédure de re-génération :
1. `mv ~/.curs3d/deployer.keystore ~/.curs3d/deployer.keystore.lost.$(date +%s)`
2. `openssl rand -base64 24 > ~/.curs3d/deployer.password && chmod 600 ~/.curs3d/deployer.password`
3. `cd contracts && CURS3D_KEYSTORE_PASSWORD="$(cat ~/.curs3d/deployer.password)" ./deploy.sh --force`
4. Le script crée un nouveau keystore + tu choppes la nouvelle address `0x...`
5. Funder cette address depuis le faucet n1 (`ssh curs3d-node1 "stop curs3d → curs3d send → start curs3d"`)
6. Relancer `./deploy.sh --force` → déploie les 7 contrats à de nouvelles addresses
7. Le JSON `contracts/deployments/1800329576.json` est auto-mis-à-jour ; commit + push
8. Update les frontends qui utilisent les anciennes addresses (dApp si déployée publiquement)

### Si tu perds un validator password (n1, n2, ou Plesk)

Tu peux pas re-décrypter le wallet. Procédure :
1. SSH le node concerné, archive l'ancien wallet : `mv /etc/curs3d/validator.json /etc/curs3d/validator.json.lost.$(date +%s)`
2. Stop curs3d : `systemctl stop curs3d`
3. Génère un nouveau : `curs3d wallet --output /etc/curs3d/validator.json --password-file /etc/curs3d/validator.password`
4. **Le node n'est plus le même validator** — il faut soit régénérer le genesis avec ce nouveau wallet (gros boulot), soit re-ajouter dynamiquement post-genesis (procédure runbook)
5. Note que tu **perds le stake** de l'ancien validator (50 000 CUR), qui devient inaccessible jusqu'à ce qu'on retrouve son password ou via une tx de slashing manuelle

### Si tu perds la clé SSH `~/.ssh/id_ed25519`

1. Génère une nouvelle paire : `ssh-keygen -t ed25519 -f ~/.ssh/id_ed25519`
2. Sur chaque VPS, ajouter la nouvelle pub à `~/.ssh/authorized_keys` (via console KVM si plus accessible par SSH)
3. Update les Plesk panels (Hostinger, IONOS) si tu y avais des clés enregistrées

## Que tu ne dois JAMAIS pousser sur git

- `~/.curs3d/deployer.keystore`
- `~/.curs3d/deployer.password`
- `~/.curs3d/deployer.keystore.lost.*`
- `/etc/curs3d/*.json` (wallets)
- `/etc/curs3d/*.password`
- `/etc/curs3d/secrets.env`, `alerts.env`, `captcha.env`, `restic.env`
- `~/.ssh/id_*` (privkey)
- Tout fichier qui termine en `.password`, `.key`, `.keystore`, ou contient le mot `secret`

`.gitignore` a déjà des règles pour ça mais vérifie avec `git status` avant chaque commit.
