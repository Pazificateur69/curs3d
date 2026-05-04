#!/usr/bin/env bash
set -euo pipefail

if [ $# -lt 3 ]; then
    echo "Usage: $0 <node_number> <public_ip> <bootnode_multiaddr>"
    exit 1
fi

NODE_NUM="$1"
PUBLIC_IP="$2"
BOOTNODE_MULTIADDR="$3"

REPO_DIR="/home/ubuntu/curs3d"
CONFIG_DIR="/etc/curs3d"
DATA_DIR="/var/lib/curs3d"
SERVICE_PATH="/etc/systemd/system/curs3d.service"
HEALTHCHECK_BIN="/usr/local/bin/curs3d-healthcheck.sh"
HEALTHCHECK_CRON="/etc/cron.d/curs3d-healthcheck"

sudo install -d -m 755 "$CONFIG_DIR" "$DATA_DIR"
sudo install -m 755 "$REPO_DIR/target/release/curs3d" /usr/local/bin/curs3d
sudo install -m 755 "$REPO_DIR/deploy/scripts/curs3d-healthcheck.sh" "$HEALTHCHECK_BIN"
sudo install -m 644 "$REPO_DIR/deploy/systemd/curs3d-healthcheck.cron" "$HEALTHCHECK_CRON"

sudo mv "/home/ubuntu/validator${NODE_NUM}.json" "$CONFIG_DIR/validator.json"
sudo mv "/home/ubuntu/validator${NODE_NUM}.password" "$CONFIG_DIR/validator.password"
sudo mv /home/ubuntu/genesis.json "$CONFIG_DIR/genesis.public-testnet.json"
if [ -f /home/ubuntu/faucet.json ]; then
    sudo mv /home/ubuntu/faucet.json "$CONFIG_DIR/faucet.json"
fi
if [ -f /home/ubuntu/faucet.password ]; then
    sudo mv /home/ubuntu/faucet.password "$CONFIG_DIR/faucet.password"
fi

sudo chmod 600 "$CONFIG_DIR/validator.password"
if [ -f "$CONFIG_DIR/faucet.password" ]; then
    sudo chmod 600 "$CONFIG_DIR/faucet.password"
fi

cat <<EOF | sudo tee "$SERVICE_PATH" >/dev/null
[Unit]
Description=CURS3D validator node ${NODE_NUM}
After=network-online.target
Wants=network-online.target
StartLimitIntervalSec=300
StartLimitBurst=10

[Service]
Type=simple
User=ubuntu
Group=ubuntu
WorkingDirectory=$REPO_DIR
ExecStart=/usr/local/bin/curs3d node --port 4337 --data-dir $DATA_DIR --validator-wallet $CONFIG_DIR/validator.json --validator-password-file $CONFIG_DIR/validator.password --bootnode $BOOTNODE_MULTIADDR --public-addr /ip4/$PUBLIC_IP/tcp/4337 --rpc-addr 127.0.0.1:9545 --http-addr 127.0.0.1:8080 --genesis-config $CONFIG_DIR/genesis.public-testnet.json
Restart=always
RestartSec=5
LimitNOFILE=65536
NoNewPrivileges=yes
PrivateTmp=yes
ProtectSystem=full
ProtectHome=yes
ReadOnlyPaths=$CONFIG_DIR
ReadWritePaths=$DATA_DIR

[Install]
WantedBy=multi-user.target
EOF

sudo systemctl daemon-reload
