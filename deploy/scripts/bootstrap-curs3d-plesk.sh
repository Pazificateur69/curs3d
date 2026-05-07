#!/usr/bin/env bash
#
# CURS3D Plesk validator bootstrap — one-shot.
# Run as ROOT on the Plesk box (Hostinger srv1, IONOS srv2, ...).
# Idempotent: re-runnable if interrupted, will skip already-completed steps.
#
# Usage:
#   curl -fsSL https://raw.githubusercontent.com/Pazificateur69/curs3d/main/deploy/scripts/bootstrap-curs3d-plesk.sh | bash
# OR
#   git clone https://github.com/Pazificateur69/curs3d.git /tmp/curs3d && bash /tmp/curs3d/deploy/scripts/bootstrap-curs3d-plesk.sh
#
# Environment overrides:
#   CURS3D_PUBLIC_IP    public IP of THIS box (auto-detected via ip route by default)
#   CURS3D_API_PORT     local API HTTP port (default 8080; use 18080 if 8080 is taken, e.g. by crowdsec)
#   CURS3D_RPC_PORT     local TCP RPC port (default 9545)
#   CURS3D_REPO_DIR     where to clone curs3d source (default /opt/curs3d)
#   CURS3D_DATA_DIR     where to store chain data (default /var/lib/curs3d)
#   CURS3D_ETC_DIR      where to store wallet/genesis/secrets (default /etc/curs3d)
#   CURS3D_BIN          where to install the binary (default /usr/local/bin/curs3d)
#   CURS3D_SKIP_FIREWALL set to 1 to skip iptables rules (manage them manually)
#
# Coexistence with Plesk:
#   - never touches Plesk-managed paths (/var/www/vhosts, /etc/plesk, /etc/postfix, etc.)
#   - API binds 127.0.0.1 only (does NOT compete with Plesk's 80/443)
#   - P2P 4337 inbound restricted to known validator IPs only (n1 + n2)
#   - hardened systemd unit (NoNewPrivileges, ProtectSystem=strict, ProtectHome=yes,
#     CapabilityBoundingSet=, MemoryMax=1G, CPUQuota=100%) so curs3d cannot
#     interfere with hosted websites or other tenants

set -euo pipefail

BOLD=$'\033[1m'; DIM=$'\033[2m'; RED=$'\033[31m'; GREEN=$'\033[32m'
YELLOW=$'\033[33m'; CYAN=$'\033[36m'; RESET=$'\033[0m'
step() { printf "%s▸%s %s\n" "$CYAN" "$RESET" "$*"; }
ok()   { printf "%s✓%s %s\n" "$GREEN" "$RESET" "$*"; }
warn() { printf "%s!%s %s\n" "$YELLOW" "$RESET" "$*"; }
die()  { printf "%s✗%s %s\n" "$RED" "$RESET" "$*" >&2; exit "${2:-1}"; }
info() { printf "%s%s%s\n" "$DIM" "$*" "$RESET"; }

# ─── Args & defaults ────────────────────────────────────────────────────
[ "$(id -u)" = "0" ] || die "must run as root" 1

CURS3D_PUBLIC_IP="${CURS3D_PUBLIC_IP:-$(ip -4 -o route get 1.1.1.1 2>/dev/null | awk '{for(i=1;i<=NF;i++) if($i=="src") {print $(i+1); exit}}')}"
[ -n "$CURS3D_PUBLIC_IP" ] || die "could not auto-detect public IP — set CURS3D_PUBLIC_IP=x.x.x.x" 1

CURS3D_API_PORT="${CURS3D_API_PORT:-8080}"
CURS3D_RPC_PORT="${CURS3D_RPC_PORT:-9545}"
CURS3D_REPO_DIR="${CURS3D_REPO_DIR:-/opt/curs3d}"
CURS3D_DATA_DIR="${CURS3D_DATA_DIR:-/var/lib/curs3d}"
CURS3D_ETC_DIR="${CURS3D_ETC_DIR:-/etc/curs3d}"
CURS3D_BIN="${CURS3D_BIN:-/usr/local/bin/curs3d}"
CURS3D_SKIP_FIREWALL="${CURS3D_SKIP_FIREWALL:-0}"

NODE1_IP=144.24.192.222
NODE1_PEERID=12D3KooWLttF4EJ1SjiLEiXvJ1yqmJawLafv47r55T5xzSt1GHn2
NODE2_IP=84.235.238.213
NODE2_PEERID=12D3KooWCL7dNFN2xz8yM65HDnNJUWF28K5qAd6d5ACT5ZCL1pb8

step "${BOLD}CURS3D Plesk bootstrap${RESET}"
info "  public IP   : $CURS3D_PUBLIC_IP"
info "  API port    : $CURS3D_API_PORT (bound to 127.0.0.1)"
info "  data dir    : $CURS3D_DATA_DIR"
info "  config dir  : $CURS3D_ETC_DIR"

# ─── 1. Swap (4 GB) ──────────────────────────────────────────────────────
step "1/10 swap"
if [ ! -f /swapfile ] && ! swapon --show | grep -q swap; then
    fallocate -l 4G /swapfile
    chmod 600 /swapfile
    mkswap /swapfile
    swapon /swapfile
    grep -q swapfile /etc/fstab || echo '/swapfile none swap sw 0 0' >> /etc/fstab
    ok "  4G swap added"
else
    ok "  swap already present"
fi

# ─── 2. ulimit / sysctl ─────────────────────────────────────────────────
step "2/10 limits + sysctl"
mkdir -p /etc/security/limits.d
cat > /etc/security/limits.d/99-curs3d.conf <<'EOF'
* soft nofile 65536
* hard nofile 65536
EOF
cat > /etc/sysctl.d/99-curs3d.conf <<'EOF'
net.core.somaxconn = 4096
net.ipv4.tcp_max_syn_backlog = 4096
vm.swappiness = 10
EOF
sysctl --system >/dev/null 2>&1 || true
ok "  limits.d + sysctl.d written"

# ─── 3. iptables: allow 4337 only from n1+n2 ────────────────────────────
step "3/10 firewall (4337 IP-allowlist)"
if [ "$CURS3D_SKIP_FIREWALL" = "0" ]; then
    iptables -C INPUT -p tcp --dport 4337 -s "$NODE1_IP" -j ACCEPT 2>/dev/null \
        || iptables -I INPUT -p tcp --dport 4337 -s "$NODE1_IP" -j ACCEPT
    iptables -C INPUT -p tcp --dport 4337 -s "$NODE2_IP" -j ACCEPT 2>/dev/null \
        || iptables -I INPUT -p tcp --dport 4337 -s "$NODE2_IP" -j ACCEPT
    iptables -C INPUT -p tcp --dport 4337 -j DROP 2>/dev/null \
        || iptables -A INPUT -p tcp --dport 4337 -j DROP
    if [ -d /etc/sysconfig ]; then
        iptables-save > /etc/sysconfig/iptables
    elif [ -d /etc/iptables ]; then
        iptables-save > /etc/iptables/rules.v4
    fi
    ok "  iptables: ACCEPT 4337 from $NODE1_IP, $NODE2_IP — DROP others"
else
    warn "  CURS3D_SKIP_FIREWALL=1 — assuming firewall managed externally"
fi

# ─── 4. Build deps ───────────────────────────────────────────────────────
step "4/10 build dependencies"
if command -v dnf >/dev/null; then
    dnf install -y -q git curl jq gcc gcc-c++ openssl-devel pkgconfig clang make >/dev/null
elif command -v yum >/dev/null; then
    yum install -y -q git curl jq gcc gcc-c++ openssl-devel pkgconfig clang make >/dev/null
elif command -v apt-get >/dev/null; then
    DEBIAN_FRONTEND=noninteractive apt-get install -y -q git curl jq build-essential libssl-dev pkg-config clang >/dev/null
else
    die "neither dnf, yum, nor apt-get found — install git/curl/build tools manually" 1
fi
ok "  build deps installed"

# ─── 5. Rust nightly ─────────────────────────────────────────────────────
step "5/10 Rust nightly"
export CARGO_HOME=/root/.cargo
export RUSTUP_HOME=/root/.rustup
if [ ! -x /root/.cargo/bin/cargo ]; then
    curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs \
        | sh -s -- -y --default-toolchain nightly --profile minimal
    ok "  rustup installed nightly"
else
    /root/.cargo/bin/rustup toolchain install nightly --profile minimal --component rust-src >/dev/null 2>&1 || true
    ok "  rust nightly already present"
fi
. /root/.cargo/env

# ─── 6. Clone + build curs3d ────────────────────────────────────────────
step "6/10 clone + cargo build --release (5-15 min)"
if [ ! -d "$CURS3D_REPO_DIR/.git" ]; then
    rm -rf "$CURS3D_REPO_DIR"
    git clone https://github.com/Pazificateur69/curs3d.git "$CURS3D_REPO_DIR"
fi
cd "$CURS3D_REPO_DIR"
git fetch origin main
git reset --hard origin/main
RUSTUP_TOOLCHAIN=nightly cargo build --release
[ -x "$CURS3D_REPO_DIR/target/release/curs3d" ] || die "cargo build did not produce the binary" 1
install -m 755 "$CURS3D_REPO_DIR/target/release/curs3d" "$CURS3D_BIN"
ok "  binary installed at $CURS3D_BIN ($("$CURS3D_BIN" --version 2>&1 | head -1))"

# ─── 7. Create curs3d user + dirs ───────────────────────────────────────
step "7/10 user + directories"
id -u curs3d >/dev/null 2>&1 || useradd -r -s /usr/sbin/nologin -d "$CURS3D_DATA_DIR" curs3d
mkdir -p "$CURS3D_DATA_DIR" "$CURS3D_ETC_DIR"
chown curs3d:curs3d "$CURS3D_DATA_DIR"
chmod 700 "$CURS3D_DATA_DIR" "$CURS3D_ETC_DIR"
ok "  user 'curs3d' + $CURS3D_DATA_DIR ($DIM 700 curs3d:curs3d $RESET) + $CURS3D_ETC_DIR ($DIM 700 root:root $RESET)"

# ─── 8. Wallet (generate if missing) ────────────────────────────────────
step "8/10 validator wallet"
if [ ! -f "$CURS3D_ETC_DIR/validator.json" ]; then
    openssl rand -base64 32 > "$CURS3D_ETC_DIR/validator.password"
    chmod 600 "$CURS3D_ETC_DIR/validator.password"
    "$CURS3D_BIN" wallet \
        --output "$CURS3D_ETC_DIR/validator.json" \
        --password-file "$CURS3D_ETC_DIR/validator.password"
    chmod 600 "$CURS3D_ETC_DIR/validator.json"
    ok "  fresh wallet generated (Argon2id m=64MB, AES-256-GCM)"
else
    ok "  wallet already exists at $CURS3D_ETC_DIR/validator.json"
fi

# ─── 9. Genesis (copy from repo) ────────────────────────────────────────
step "9/10 genesis"
if [ -f "$CURS3D_REPO_DIR/deploy/genesis.public-testnet.json" ]; then
    install -m 644 "$CURS3D_REPO_DIR/deploy/genesis.public-testnet.json" \
        "$CURS3D_ETC_DIR/genesis.public-testnet.json"
    GENESIS_HASH="$(sha256sum "$CURS3D_ETC_DIR/genesis.public-testnet.json" | awk '{print $1}')"
    ok "  genesis installed (sha256=${GENESIS_HASH:0:16}...)"
else
    die "genesis file not found in repo — operator must commit deploy/genesis.public-testnet.json first" 1
fi

# ─── 10. Systemd unit (hardened) ────────────────────────────────────────
step "10/10 systemd unit"
cat > /etc/systemd/system/curs3d.service <<EOF
[Unit]
Description=CURS3D Validator Node
After=network-online.target
Wants=network-online.target

[Service]
Type=simple
User=curs3d
Group=curs3d
ExecStart=$CURS3D_BIN node \\
    --port 4337 \\
    --data-dir $CURS3D_DATA_DIR \\
    --validator-wallet $CURS3D_ETC_DIR/validator.json \\
    --validator-password-file $CURS3D_ETC_DIR/validator.password \\
    --rpc-addr 127.0.0.1:$CURS3D_RPC_PORT \\
    --http-addr 127.0.0.1:$CURS3D_API_PORT \\
    --genesis-config $CURS3D_ETC_DIR/genesis.public-testnet.json \\
    --bootnode /ip4/$NODE1_IP/tcp/4337/p2p/$NODE1_PEERID \\
    --bootnode /ip4/$NODE2_IP/tcp/4337/p2p/$NODE2_PEERID \\
    --public-addr /ip4/$CURS3D_PUBLIC_IP/tcp/4337
Restart=always
RestartSec=5
LimitNOFILE=65536

# Hardening — sandbox the validator from Plesk websites/customers
NoNewPrivileges=yes
ProtectSystem=strict
ProtectHome=yes
ProtectKernelTunables=yes
ProtectKernelModules=yes
ProtectControlGroups=yes
PrivateTmp=yes
PrivateDevices=yes
RestrictNamespaces=yes
LockPersonality=yes
RestrictRealtime=yes
RestrictAddressFamilies=AF_INET AF_INET6 AF_UNIX AF_NETLINK
SystemCallArchitectures=native
ReadWritePaths=$CURS3D_DATA_DIR
ReadOnlyPaths=$CURS3D_ETC_DIR
CapabilityBoundingSet=
AmbientCapabilities=
RemoveIPC=yes

# Resource caps — protect Plesk websites from any curs3d resource spike
MemoryMax=1G
CPUQuota=100%

[Install]
WantedBy=multi-user.target
EOF
systemctl daemon-reload
systemctl enable curs3d >/dev/null 2>&1
systemctl restart curs3d
ok "  systemd unit installed and started"

# ─── Output ──────────────────────────────────────────────────────────────
sleep 5
ADDR_HEX="$("$CURS3D_BIN" info \
    --wallet "$CURS3D_ETC_DIR/validator.json" \
    --password-file "$CURS3D_ETC_DIR/validator.password" \
    --json 2>/dev/null | jq -r '.address // empty' 2>/dev/null || true)"

if [ -z "$ADDR_HEX" ]; then
    ADDR_HEX="$("$CURS3D_BIN" info \
        --wallet "$CURS3D_ETC_DIR/validator.json" \
        --password-file "$CURS3D_ETC_DIR/validator.password" 2>/dev/null \
        | grep -i -oE '(CUR|0x)[a-fA-F0-9]+' | head -1)"
fi

echo
printf "%s%s━━━━ Bootstrap complete ━━━━%s\n" "$BOLD" "$GREEN" "$RESET"
printf "  validator address: %s%s%s\n" "$BOLD" "$ADDR_HEX" "$RESET"
printf "  systemd: %s\n" "$(systemctl is-active curs3d)"
printf "  local API: %s\n" "http://127.0.0.1:$CURS3D_API_PORT/api/status"
printf "  P2P bind: 0.0.0.0:4337 (IP-allowlisted to $NODE1_IP, $NODE2_IP)\n"
echo
echo "${BOLD}NEXT — paste this address back to the operator:${RESET}"
echo "    VALIDATOR_ADDRESS = $ADDR_HEX"
echo
echo "Operator will then:"
echo "  1. Fund this address with >= 1500 CUR from the n1 faucet"
echo "  2. Wait for fund tx to land (~30s)"
echo "  3. SSH back here and run: $CURS3D_BIN stake \\"
echo "       --wallet $CURS3D_ETC_DIR/validator.json \\"
echo "       --password-file $CURS3D_ETC_DIR/validator.password \\"
echo "       --amount 1500 --rpc-addr 127.0.0.1:$CURS3D_RPC_PORT"
echo "  4. Wait for next epoch boundary (~5 min) — node becomes active validator"
