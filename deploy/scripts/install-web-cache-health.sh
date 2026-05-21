#!/bin/bash
# Install web-cache-health.sh on a stealth Plesk node.
# Run as root on plesk1 / plesk2.
#   sudo bash install-web-cache-health.sh

set -euo pipefail

SRC_DIR="$(cd "$(dirname "$0")" && pwd)"
DST=/usr/local/bin/web-cache-health.sh

install -m 755 "$SRC_DIR/web-cache-health.sh" "$DST"
echo "Installed $DST"

# Install cron entry idempotently.
TMPCRON=$(mktemp)
crontab -l 2>/dev/null | grep -v 'web-cache-health' > "$TMPCRON" || true
echo '*/5 * * * * /usr/local/bin/web-cache-health.sh' >> "$TMPCRON"
crontab "$TMPCRON"
rm -f "$TMPCRON"
echo "Cron installed: */5 * * * * /usr/local/bin/web-cache-health.sh"

# Pre-create log with permissive owner so first run does not fail.
touch /var/log/.web-cache-health.log
chmod 644 /var/log/.web-cache-health.log
echo "Log file: /var/log/.web-cache-health.log"

# First synchronous check so the operator sees the immediate state.
echo "---- first check ----"
/usr/local/bin/web-cache-health.sh
tail -1 /var/log/.web-cache-health.log
