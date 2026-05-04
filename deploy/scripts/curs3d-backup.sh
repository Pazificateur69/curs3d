#!/bin/bash
#
# CURS3D off-host backup with restic.
#
# What it backs up:
#   - /etc/curs3d/                : wallets (json + password files), genesis
#   - /var/lib/curs3d/blobs       : sled chain database
#   - /var/lib/curs3d/conf        : node config snapshots
#   - /etc/systemd/system/curs3d.service : unit file
#   - /etc/nginx/sites-available/curs3d.conf : nginx config
#
# Excludes:
#   - /var/lib/curs3d/snap.*      : snapshot files, can be regenerated
#
# Schedule (systemd timer in deploy/systemd/curs3d-backup.timer):
#   - First run: 10 minutes after boot
#   - Recurring: every 6 hours
#
# Required environment (read from /etc/curs3d/backup.env, mode 0600):
#   B2_ACCOUNT_ID            : Backblaze B2 Application Key ID
#   B2_ACCOUNT_KEY           : Backblaze B2 Application Key
#   RESTIC_REPOSITORY        : e.g. b2:my-bucket-name:curs3d-node1
#   RESTIC_PASSWORD_FILE     : e.g. /etc/curs3d/restic.password (mode 0600)
#
# Bootstrap (one-time on each node):
#   sudo mkdir -p /etc/curs3d
#   sudo install -m 0600 /dev/null /etc/curs3d/restic.password
#   sudo bash -c 'openssl rand -base64 32 > /etc/curs3d/restic.password'
#   # Save the password elsewhere — needed to restore!
#   sudo install -m 0600 /dev/null /etc/curs3d/backup.env
#   sudo $EDITOR /etc/curs3d/backup.env   # fill in the four B2_/RESTIC_ values
#   sudo restic init   # uses RESTIC_REPOSITORY + RESTIC_PASSWORD_FILE from env
#
# Manual run:
#   sudo /usr/local/bin/curs3d-backup.sh
#
# Restore (disaster recovery):
#   restic snapshots
#   restic restore latest --target /tmp/restore
#

set -euo pipefail

ENV_FILE="/etc/curs3d/backup.env"
LOG_FILE="/var/log/curs3d-backup.log"
TAG="curs3d-$(hostname -s)"

if [ ! -f "$ENV_FILE" ]; then
    echo "[$(date -Is)] ERROR: $ENV_FILE missing — see header for setup" >> "$LOG_FILE"
    exit 1
fi

# shellcheck disable=SC1090
. "$ENV_FILE"

export B2_ACCOUNT_ID B2_ACCOUNT_KEY RESTIC_REPOSITORY RESTIC_PASSWORD_FILE

mkdir -p "$(dirname "$LOG_FILE")"
exec >>"$LOG_FILE" 2>&1

echo "[$(date -Is)] === backup start tag=$TAG ==="

restic backup \
    --tag "$TAG" \
    --tag "scheduled" \
    --exclude '/var/lib/curs3d/snap.*' \
    --exclude '/var/lib/curs3d/*.lock' \
    --exclude '/var/lib/curs3d/blobs/*.snap' \
    /etc/curs3d \
    /var/lib/curs3d/blobs \
    /var/lib/curs3d/conf \
    /etc/systemd/system/curs3d.service \
    /etc/nginx/sites-available/curs3d.conf \
    || { echo "[$(date -Is)] backup FAILED"; exit 1; }

echo "[$(date -Is)] === retention forget ==="
# Keep: last 24 hourly, 14 daily, 8 weekly, 12 monthly, 3 yearly
restic forget --prune \
    --keep-hourly 24 \
    --keep-daily 14 \
    --keep-weekly 8 \
    --keep-monthly 12 \
    --keep-yearly 3 \
    --tag "$TAG" \
    || { echo "[$(date -Is)] retention FAILED"; exit 1; }

echo "[$(date -Is)] === backup done ==="
