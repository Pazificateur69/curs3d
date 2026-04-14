#!/usr/bin/env bash
set -euo pipefail

# CURS3D Deployment Script
# Usage: sudo ./deploy.sh [--domain api.example.com] [--explorer explorer.example.com]

DOMAIN="${1:-api.example.com}"
EXPLORER_DOMAIN="${2:-explorer.example.com}"
CURS3D_USER="curs3d"
INSTALL_DIR="/opt/curs3d"
DATA_DIR="/var/lib/curs3d"
CONFIG_DIR="/etc/curs3d"
WEBSITE_DIR="/var/www/curs3d"

echo "=== CURS3D Deployment ==="
echo "API Domain: $DOMAIN"
echo "Explorer Domain: $EXPLORER_DOMAIN"
echo ""

# 1. Create curs3d user
if ! id "$CURS3D_USER" &>/dev/null; then
    echo "[1/8] Creating user $CURS3D_USER..."
    useradd --system --shell /usr/sbin/nologin --home-dir "$DATA_DIR" "$CURS3D_USER"
else
    echo "[1/8] User $CURS3D_USER already exists"
fi

# 2. Create directories
echo "[2/8] Creating directories..."
mkdir -p "$INSTALL_DIR" "$DATA_DIR" "$CONFIG_DIR" "$WEBSITE_DIR"
chown "$CURS3D_USER:$CURS3D_USER" "$DATA_DIR"

# 3. Build and install binary
echo "[3/8] Building CURS3D..."
if [ -f "../Cargo.toml" ]; then
    cd ..
    cargo build --release
    cp target/release/curs3d /usr/local/bin/curs3d
    chmod 755 /usr/local/bin/curs3d
    cd deploy
else
    echo "ERROR: Run this script from the deploy/ directory"
    exit 1
fi

# 4. Copy website
echo "[4/8] Deploying website..."
cp -r ../website/* "$WEBSITE_DIR/"

# 5. Install systemd service
echo "[5/8] Installing systemd service..."
sed "s/example.com/${DOMAIN}/g" systemd/curs3d.service > /etc/systemd/system/curs3d.service
systemctl daemon-reload

# 6. Install nginx config
echo "[6/8] Configuring nginx..."
if command -v nginx &>/dev/null; then
    sed -e "s/api.example.com/${DOMAIN}/g" \
        -e "s/explorer.example.com/${EXPLORER_DOMAIN}/g" \
        nginx/curs3d.conf > /etc/nginx/sites-available/curs3d.conf
    ln -sf /etc/nginx/sites-available/curs3d.conf /etc/nginx/sites-enabled/curs3d.conf
    nginx -t && systemctl reload nginx
else
    echo "WARNING: nginx not installed. Install it and re-run."
fi

# 7. TLS certificates
echo "[7/8] Setting up TLS..."
if command -v certbot &>/dev/null; then
    certbot certonly --nginx -d "$DOMAIN" -d "$EXPLORER_DOMAIN" --non-interactive --agree-tos --email "admin@${DOMAIN}" || true
    systemctl reload nginx || true
else
    echo "WARNING: certbot not installed. Run: apt install certbot python3-certbot-nginx"
fi

# 8. Firewall
echo "[8/8] Configuring firewall..."
if command -v ufw &>/dev/null; then
    ufw allow 22/tcp    # SSH
    ufw allow 80/tcp    # HTTP (redirect)
    ufw allow 443/tcp   # HTTPS
    ufw allow 4337/tcp  # P2P
    # Block direct access to node ports
    ufw deny 8080/tcp   # API (only via nginx)
    ufw deny 9545/tcp   # RPC (local only)
    echo "Firewall configured. Run 'ufw enable' if not already active."
else
    echo "WARNING: ufw not installed."
fi

echo ""
echo "=== Deployment Complete ==="
echo ""
echo "Next steps:"
echo "  1. Create wallets:"
echo "     curs3d wallet --output $CONFIG_DIR/validator.json"
echo "     curs3d wallet --output $CONFIG_DIR/faucet.json"
echo ""
echo "  2. Generate genesis:"
echo "     curs3d genesis --validator-wallet $CONFIG_DIR/validator.json \\"
echo "       --faucet-wallet $CONFIG_DIR/faucet.json \\"
echo "       --output $CONFIG_DIR/genesis.public-testnet.json"
echo ""
echo "  3. Set passwords:"
echo "     echo 'your-password' > $CONFIG_DIR/validator.password"
echo "     echo 'your-password' > $CONFIG_DIR/faucet.password"
echo "     chmod 600 $CONFIG_DIR/*.password"
echo "     chown $CURS3D_USER:$CURS3D_USER $CONFIG_DIR/*"
echo ""
echo "  4. Update API tokens in /etc/systemd/system/curs3d.service"
echo ""
echo "  5. Start the node:"
echo "     systemctl enable --now curs3d"
echo "     journalctl -u curs3d -f"
