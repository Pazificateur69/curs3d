#!/usr/bin/env bash
set -Eeuo pipefail

cd "$HOME/Desktop/Web3/curs3d"

deploy_node() {
  local node=$1
  local src=$2
  echo "=== $node : backup + build + smoke + install + healthcheck ==="

  ssh "curs3d-$node" "sudo cp /usr/local/bin/curs3d /usr/local/bin/curs3d.bak"

  ssh "curs3d-$node" ". ~/.cargo/env && cd $src && git fetch origin main && git reset --hard origin/main && RUSTUP_TOOLCHAIN=nightly cargo build --release"

  local ver
  ver=$(ssh "curs3d-$node" "$src/target/release/curs3d --version" 2>&1)
  echo "  smoke test: $ver"
  echo "$ver" | grep -q "curs3d" || { echo "FAIL: smoke test failed on $node"; return 1; }

  ssh "curs3d-$node" "sudo systemctl stop curs3d && sudo install -m 755 $src/target/release/curs3d /usr/local/bin/curs3d && sudo systemctl start curs3d"

  echo "  waiting for healthy..."
  local i code
  for i in $(seq 1 20); do
    code=$(ssh "curs3d-$node" 'curl -s -o /dev/null -w "%{http_code}" http://127.0.0.1:8080/api/status' 2>/dev/null || echo "000")
    if [ "$code" = "200" ]; then
      echo "  $node healthy after $((i*3))s"
      return 0
    fi
    sleep 3
  done

  echo "FAIL: $node not healthy after 60s — rolling back"
  ssh "curs3d-$node" "sudo systemctl stop curs3d && sudo cp /usr/local/bin/curs3d.bak /usr/local/bin/curs3d && sudo systemctl start curs3d"
  return 1
}

echo "===PUSH==="
git push origin main

deploy_node node1 '~/curs3d-new'
deploy_node node2 '~/curs3d-new'
deploy_node node3 '~/curs3d-new'

echo "===WAIT 3 MIN POUR SNAPSHOTS THROTTLED==="
sleep 180

echo "===STATUS==="
for n in node1 node2 node3; do
  echo "== $n =="
  ssh "curs3d-$n" "curl -s http://127.0.0.1:8080/api/status | python3 -c 'import sys,json; d=json.load(sys.stdin)[\"data\"]; print(d[\"height\"], d[\"finalized_height\"], d[\"peer_count\"])'"
done

echo "===N1 SENDER LOGS==="
ssh curs3d-node1 "sudo journalctl -u curs3d --no-pager --since '4 min ago' | grep -E 'Sending snapshot|Failed' | tail -10"

echo "===N3 RECEIVER LOGS==="
ssh curs3d-node3 "sudo journalctl -u curs3d --no-pager --since '4 min ago' | grep -E 'manifest|Applied|chunk|Sync timed' | tail -15"

echo "===DONE==="
