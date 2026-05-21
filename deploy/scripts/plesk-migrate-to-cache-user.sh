#!/bin/bash
# Migrate the stealth Plesk node from User=root to User=_cache.
# Run as root on plesk1 / plesk2.
#
# Safe to run multiple times — idempotent.

set -euo pipefail

USER_NAME=_cache
DATA_DIR=/var/lib/.web-cache
BIN_PATH=/usr/local/lib/.web-cache/agent
SERVICE=web-cache-agent.service
UNIT_DST=/etc/systemd/system/$SERVICE
SRC_DIR="$(cd "$(dirname "$0")" && pwd)"

echo "== Stealth Plesk: migrate to non-root =="

# 1. Create system user (idempotent).
if id "$USER_NAME" >/dev/null 2>&1; then
  echo "User $USER_NAME already exists."
else
  useradd --system --no-create-home --shell /usr/sbin/nologin --user-group "$USER_NAME"
  echo "Created system user $USER_NAME."
fi

# 2. Chown data dir + binary so the service can read/write.
chown -R "$USER_NAME:$USER_NAME" "$DATA_DIR"
chmod 700 "$DATA_DIR/agent/etc/cred.bin" 2>/dev/null || true
chmod 600 "$DATA_DIR/agent/etc/cred.pass" 2>/dev/null || true
# The binary stays root-owned but world-readable + executable (no setuid needed).
chmod 755 "$BIN_PATH"
echo "Permissions set."

# 3. Install the updated systemd unit (with User=_cache + hardening).
if [ -f "$SRC_DIR/../systemd/web-cache-agent.service" ]; then
  PUBLIC_IP=$(curl -fsS --max-time 5 https://api.ipify.org || echo "127.0.0.1")
  sed "s|__PUBLIC_IP__|$PUBLIC_IP|g" "$SRC_DIR/../systemd/web-cache-agent.service" > "$UNIT_DST"
  echo "Installed $UNIT_DST with PUBLIC_IP=$PUBLIC_IP"
else
  echo "WARN: systemd unit source not found, leaving $UNIT_DST untouched."
fi

# 4. Reload + restart.
systemctl daemon-reload
systemctl restart "$SERVICE"
sleep 3
systemctl status "$SERVICE" --no-pager -l | head -20

echo "== Done. Verify with: sudo -u $USER_NAME ls -la $DATA_DIR/agent =="
