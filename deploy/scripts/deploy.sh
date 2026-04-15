#!/usr/bin/env bash
set -euo pipefail

# ═══════════════════════════════════════════════════════════════════
# CURS3D VPS Deployment Script
# Usage: sudo ./deploy.sh <api-domain> <explorer-domain>
# Example: sudo ./deploy.sh api.curs3d.fr explorer.curs3d.fr
# ═══════════════════════════════════════════════════════════════════

if [ $# -lt 2 ]; then
    echo "Usage: sudo $0 <api-domain> <explorer-domain>"
    echo "Example: sudo $0 api.curs3d.fr explorer.curs3d.fr"
    exit 1
fi

DOMAIN="$1"
EXPLORER_DOMAIN="$2"
CURS3D_USER="curs3d"
INSTALL_DIR="/opt/curs3d"
DATA_DIR="/var/lib/curs3d"
CONFIG_DIR="/etc/curs3d"
WEBSITE_DIR="/var/www/curs3d"
SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
REPO_DIR="$(cd "$SCRIPT_DIR/../.." && pwd)"

echo "=== CURS3D Deployment ==="
echo "API Domain:      $DOMAIN"
echo "Explorer Domain: $EXPLORER_DOMAIN"
echo "Repo:            $REPO_DIR"
echo ""

# ─── 1. System packages ─────────────────────────────────────────
echo "[1/9] Installing system packages..."
apt-get update -qq
apt-get install -y --no-install-recommends \
    nginx certbot python3-certbot-nginx \
    curl jq ca-certificates build-essential pkg-config libssl-dev

# ─── 2. Install Rust (if not present) ───────────────────────────
echo "[2/9] Checking Rust..."
if ! command -v cargo &>/dev/null; then
    echo "Installing Rust..."
    curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y
    source "$HOME/.cargo/env"
fi

# ─── 3. Create user and directories ─────────────────────────────
echo "[3/9] Creating user and directories..."
if ! id "$CURS3D_USER" &>/dev/null; then
    useradd --system --shell /usr/sbin/nologin --home-dir "$DATA_DIR" "$CURS3D_USER"
fi
mkdir -p "$INSTALL_DIR" "$DATA_DIR" "$CONFIG_DIR" "$WEBSITE_DIR"
chown "$CURS3D_USER:$CURS3D_USER" "$DATA_DIR"

# ─── 4. Build and install binary ─────────────────────────────────
echo "[4/9] Building CURS3D (this may take a few minutes)..."
cd "$REPO_DIR"
cargo build --release
cp target/release/curs3d /usr/local/bin/curs3d
chmod 755 /usr/local/bin/curs3d
echo "Binary installed: $(curs3d --version 2>/dev/null || echo 'ok')"

# ─── 5. Deploy website ──────────────────────────────────────────
echo "[5/9] Deploying website..."
cp -r "$REPO_DIR/website/"* "$WEBSITE_DIR/"

# ─── 6. Install systemd service ─────────────────────────────────
echo "[6/9] Installing systemd service..."
sed -e "s/node.example.com/${DOMAIN}/g" \
    -e "s/explorer.example.com/${EXPLORER_DOMAIN}/g" \
    "$REPO_DIR/deploy/systemd/curs3d.service" > /etc/systemd/system/curs3d.service
systemctl daemon-reload

# ─── 7. TLS certificates (BEFORE nginx TLS config) ──────────────
echo "[7/9] Obtaining TLS certificates..."

# First install a temporary HTTP-only nginx config for certbot
cat > /etc/nginx/sites-available/curs3d.conf <<NGINX_TEMP
server {
    listen 80;
    server_name $DOMAIN $EXPLORER_DOMAIN;
    location / { return 200 'ok'; add_header Content-Type text/plain; }
}
NGINX_TEMP
ln -sf /etc/nginx/sites-available/curs3d.conf /etc/nginx/sites-enabled/curs3d.conf
rm -f /etc/nginx/sites-enabled/default
nginx -t && systemctl restart nginx

# Obtain separate certs for each domain
certbot certonly --nginx -d "$DOMAIN" \
    --non-interactive --agree-tos --email "admin@${DOMAIN}" || true
certbot certonly --nginx -d "$EXPLORER_DOMAIN" \
    --non-interactive --agree-tos --email "admin@${DOMAIN}" || true

# ─── 8. Install full nginx config (with TLS) ────────────────────
echo "[8/9] Configuring nginx with TLS..."

# Check if certs were obtained
if [ -f "/etc/letsencrypt/live/$DOMAIN/fullchain.pem" ] && \
   [ -f "/etc/letsencrypt/live/$EXPLORER_DOMAIN/fullchain.pem" ]; then
    # Full TLS config
    sed -e "s/api.example.com/${DOMAIN}/g" \
        -e "s/explorer.example.com/${EXPLORER_DOMAIN}/g" \
        "$REPO_DIR/deploy/nginx/curs3d.conf" > /etc/nginx/sites-available/curs3d.conf
    echo "TLS certificates found, full HTTPS config installed."
else
    # HTTP-only fallback (no TLS)
    cat > /etc/nginx/sites-available/curs3d.conf <<NGINX_HTTP
# CURS3D - HTTP only (run certbot manually for TLS)
server {
    listen 80;
    server_name $DOMAIN;

    location /ws {
        proxy_pass http://127.0.0.1:8080;
        proxy_http_version 1.1;
        proxy_set_header Upgrade \$http_upgrade;
        proxy_set_header Connection "upgrade";
        proxy_set_header Host \$host;
        proxy_set_header X-Forwarded-For \$proxy_add_x_forwarded_for;
        proxy_read_timeout 86400;
    }

    location /api/ {
        proxy_pass http://127.0.0.1:8080;
        proxy_http_version 1.1;
        proxy_set_header Host \$host;
        proxy_set_header X-Forwarded-For \$proxy_add_x_forwarded_for;
    }

    location / { return 404; }
}

server {
    listen 80;
    server_name $EXPLORER_DOMAIN;
    root /var/www/curs3d;
    index index.html;
    location / { try_files \$uri \$uri/ =404; }
}
NGINX_HTTP
    echo "WARNING: TLS certificates not obtained. Running HTTP-only mode."
    echo "Run certbot manually later: certbot --nginx -d $DOMAIN -d $EXPLORER_DOMAIN"
fi

nginx -t && systemctl reload nginx

# ─── 9. Firewall ────────────────────────────────────────────────
echo "[9/9] Configuring firewall..."
if command -v iptables &>/dev/null; then
    # Oracle Cloud uses iptables, not ufw
    iptables -I INPUT -p tcp --dport 80 -j ACCEPT 2>/dev/null || true
    iptables -I INPUT -p tcp --dport 443 -j ACCEPT 2>/dev/null || true
    iptables -I INPUT -p tcp --dport 4337 -j ACCEPT 2>/dev/null || true
    # Save rules
    if command -v netfilter-persistent &>/dev/null; then
        netfilter-persistent save 2>/dev/null || true
    fi
    echo "iptables rules added for ports 80, 443, 4337."
elif command -v ufw &>/dev/null; then
    ufw allow 22/tcp
    ufw allow 80/tcp
    ufw allow 443/tcp
    ufw allow 4337/tcp
    echo "UFW rules added."
fi

echo ""
echo "=========================================="
echo "  CURS3D Deployment Complete"
echo "=========================================="
echo ""
echo "Next steps:"
echo ""
echo "  1. Create wallets:"
echo "     curs3d wallet --output $CONFIG_DIR/validator.json"
echo "     curs3d wallet --output $CONFIG_DIR/faucet.json"
echo ""
echo "  2. Generate genesis:"
echo "     curs3d genesis \\"
echo "       --validator-wallet $CONFIG_DIR/validator.json \\"
echo "       --faucet-wallet $CONFIG_DIR/faucet.json \\"
echo "       --output $CONFIG_DIR/genesis.public-testnet.json"
echo ""
echo "  3. Set passwords:"
echo "     echo 'YOUR_SECURE_PASSWORD' > $CONFIG_DIR/validator.password"
echo "     echo 'YOUR_SECURE_PASSWORD' > $CONFIG_DIR/faucet.password"
echo "     chmod 600 $CONFIG_DIR/*.password"
echo "     chown $CURS3D_USER:$CURS3D_USER $CONFIG_DIR/*"
echo ""
echo "  4. Start the node:"
echo "     systemctl enable --now curs3d"
echo "     journalctl -u curs3d -f"
echo ""
echo "  API:      http://$DOMAIN/api/status"
echo "  Explorer: http://$EXPLORER_DOMAIN"
echo "  P2P:      $DOMAIN:4337"
