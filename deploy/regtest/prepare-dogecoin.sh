#!/usr/bin/env bash
# Bring the regtest Dogecoin chain to a height where merge-mined (auxpow) blocks are
# accepted. Dogecoin regtest allows only legacy blocks below height 20 and only auxpow
# blocks from height 20 on, so a fresh chain needs a few node-mined blocks first.
set -euo pipefail

RPC_URL="${ALAMO_REGTEST_DOGE_RPC:-http://alamo:alamo@127.0.0.1:18332}"
TARGET_HEIGHT="${1:-30}"

rpc() {
  local method="$1"; shift
  local params="${1:-[]}"
  curl -sf --data "{\"method\":\"$method\",\"params\":$params}" "$RPC_URL" \
    | python3 -c 'import sys,json; r=json.load(sys.stdin); sys.exit(1) if r["error"] else print(json.dumps(r["result"]))'
}

for _ in $(seq 1 90); do
  curl -s --data '{"method":"getblockcount","params":[]}' "$RPC_URL" | grep -q result && break
  sleep 1
done

height=$(rpc getblockcount)
if [ "$height" -ge "$TARGET_HEIGHT" ]; then
  echo "dogecoin regtest already at height $height"
  exit 0
fi

# getblocktemplate needs a peer; the compose file runs one, wait until it is connected.
for _ in $(seq 1 60); do
  [ "$(rpc getconnectioncount)" -ge 1 ] && break
  sleep 1
done

addr=$(rpc getnewaddress | tr -d '"')
need=$((TARGET_HEIGHT - height))
echo "mining $need dogecoin regtest blocks to $addr"
rpc generatetoaddress "[$need, \"$addr\"]" >/dev/null
echo "dogecoin regtest at height $(rpc getblockcount), peers: $(rpc getconnectioncount)"
