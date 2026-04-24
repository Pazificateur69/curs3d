#!/bin/bash
set -euo pipefail

# CURS3D — Script pour ajouter un nouveau node au réseau
# Usage: ./add-node.sh <node_number> <bootnode_ip> <bootnode_peerid>
# Example: ./add-node.sh 3 144.24.192.222 12D3KooWC6hEP7YRySmXNA4pTcTi4XMkch525jFWbHNDtwxW6K2K

if [ $# -lt 3 ]; then
    echo "Usage: $0 <node_number> <bootnode_ip> <bootnode_peerid>"
    exit 1
fi

NODE_NUM=$1
BOOTNODE_IP=$2
BOOTNODE_PEERID=$3
MULTINODE_DIR="/tmp/curs3d-multinode"

# Oracle Cloud config
COMPARTMENT="ocid1.tenancy.oc1..aaaaaaaalu3mxlfrfi3nb5lqazygqtj6um2epn4ra2s2hp4kblbrs3zbxmva"
AD="RaWf:EU-MARSEILLE-1-AD-1"
IMAGE="ocid1.image.oc1.eu-marseille-1.aaaaaaaas4ufge4zdy45ve4kwlqpqpbafllx32galvxljjiclvkui2ksczlq"
SUBNET="ocid1.subnet.oc1.eu-marseille-1.aaaaaaaajggxyny36gmrjp2mb2dqhfedunmpg4w6bdjhrtgbxeqfpghs3idq"
SSH_KEY="/tmp/curs3d_ssh_pub.pub"

echo "=== Creating curs3d-node${NODE_NUM} ==="

# 1. Launch instance
RESULT=$(oci compute instance launch \
    --compartment-id "$COMPARTMENT" \
    --availability-domain "$AD" \
    --shape "VM.Standard.A1.Flex" \
    --shape-config '{"ocpus": 1, "memoryInGBs": 6}' \
    --display-name "curs3d-node${NODE_NUM}" \
    --image-id "$IMAGE" \
    --subnet-id "$SUBNET" \
    --assign-public-ip true \
    --ssh-authorized-keys-file "$SSH_KEY" \
    --query "data.id" \
    --raw-output 2>/dev/null)

if [ -z "$RESULT" ]; then
    echo "FAILED: Oracle ARM capacity not available. Try again later."
    exit 1
fi

echo "Instance ID: $RESULT"

# 2. Wait for RUNNING
echo "Waiting for instance to start..."
while true; do
    STATE=$(oci compute instance get --instance-id "$RESULT" --query "data.\"lifecycle-state\"" --raw-output 2>/dev/null)
    if [ "$STATE" = "RUNNING" ]; then break; fi
    if [ "$STATE" = "TERMINATED" ]; then echo "Instance terminated by Oracle. Retry later."; exit 1; fi
    sleep 10
done

# 3. Get IP
VNIC_ID=$(oci compute vnic-attachment list --compartment-id "$COMPARTMENT" --instance-id "$RESULT" --query "data[0].\"vnic-id\"" --raw-output 2>/dev/null)
IP=$(oci network vnic get --vnic-id "$VNIC_ID" --query "data.\"public-ip\"" --raw-output 2>/dev/null)
echo "IP: $IP"

# 4. Wait for SSH
echo "Waiting for SSH..."
for i in $(seq 1 30); do
    ssh -o StrictHostKeyChecking=accept-new -o ConnectTimeout=5 -i ~/.ssh/id_ed25519_server ubuntu@$IP "echo ok" 2>/dev/null && break
    sleep 5
done

# 5. Deploy
echo "Deploying..."
rsync -az --exclude 'target' --exclude '.git' -e "ssh -i ~/.ssh/id_ed25519_server -o StrictHostKeyChecking=no" ~/Desktop/Web3/curs3d/ ubuntu@$IP:/home/ubuntu/curs3d/

scp -i ~/.ssh/id_ed25519_server \
    ${MULTINODE_DIR}/validator${NODE_NUM}.json \
    ${MULTINODE_DIR}/validator${NODE_NUM}.password \
    ${MULTINODE_DIR}/faucet.json \
    ${MULTINODE_DIR}/faucet.password \
    ${MULTINODE_DIR}/genesis.json \
    ubuntu@$IP:/home/ubuntu/

# 6. Install deps + build
echo "Installing deps + building (this takes ~10 min)..."
ssh -i ~/.ssh/id_ed25519_server ubuntu@$IP "sudo apt-get update -qq && sudo DEBIAN_FRONTEND=noninteractive apt-get install -y --no-install-recommends build-essential pkg-config libssl-dev nginx curl jq ca-certificates 2>&1 | tail -1 && curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y 2>&1 | tail -1 && source ~/.cargo/env && cd /home/ubuntu/curs3d && cargo build --release 2>&1 | tail -3"

# 7. Setup
scp -i ~/.ssh/id_ed25519_server ${MULTINODE_DIR}/setup-node.sh ubuntu@$IP:/home/ubuntu/
ssh -i ~/.ssh/id_ed25519_server ubuntu@$IP "bash /home/ubuntu/setup-node.sh ${NODE_NUM} ${BOOTNODE_IP}"

# 8. Fix bootnode flag in systemd
ssh -i ~/.ssh/id_ed25519_server ubuntu@$IP "sudo sed -i 's|--boot-nodes /ip4/${BOOTNODE_IP}/tcp/4337|--bootnode /ip4/${BOOTNODE_IP}/tcp/4337/p2p/${BOOTNODE_PEERID}|' /etc/systemd/system/curs3d.service && sudo systemctl daemon-reload"

# 9. Start
ssh -i ~/.ssh/id_ed25519_server ubuntu@$IP "sudo systemctl enable curs3d && sudo systemctl start curs3d"
sleep 5

echo ""
echo "=== curs3d-node${NODE_NUM} deployed ==="
echo "IP: $IP"
echo "SSH: ssh -i ~/.ssh/id_ed25519_server ubuntu@$IP"
echo ""
echo "Add to ~/.ssh/config:"
echo "Host curs3d-node${NODE_NUM}"
echo "    HostName $IP"
echo "    User ubuntu"
echo "    IdentityFile ~/.ssh/id_ed25519_server"
