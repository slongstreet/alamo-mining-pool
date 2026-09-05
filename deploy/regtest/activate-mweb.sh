#!/usr/bin/env bash
# Bring the regtest Litecoin chain past MWEB activation so block templates carry the
# HogEx transaction and `mweb` payload that mainnet blocks require.
#
# MWEB is a BIP9 deployment on regtest (period 144, activates at height 432). The first
# MWEB block must contain a peg-in, otherwise the node cannot build it. This script mines
# to a wallet, pegs one coin into MWEB, and mines the activation block.
set -euo pipefail

RPC_URL="${ALAMO_REGTEST_RPC:-http://alamo:alamo@127.0.0.1:19443}"
rpc() {
  local method="$1"; shift
  local params="${1:-[]}"
  curl -sf --data "{\"method\":\"$method\",\"params\":$params}" "$RPC_URL/wallet/mweb" \
    | python3 -c 'import sys,json; r=json.load(sys.stdin); sys.exit(1) if r["error"] else print(json.dumps(r["result"]))'
}
rpc_noresult() {
  local method="$1"; shift
  curl -s --data "{\"method\":\"$method\",\"params\":${1:-[]}}" "$RPC_URL" >/dev/null
}

for _ in $(seq 1 60); do
  curl -s --data '{"method":"getblockcount","params":[]}' "$RPC_URL" | grep -q result && break
  sleep 1
done

rpc_noresult createwallet '["mweb"]'
rpc_noresult loadwallet '["mweb"]'

height=$(rpc getblockcount)
active=$(curl -s --data '{"method":"getblockchaininfo","params":[]}' "$RPC_URL" | python3 -c 'import sys,json; print(json.load(sys.stdin)["result"]["softforks"]["mweb"]["active"])')
if [ "$active" = "True" ]; then
  echo "MWEB already active at height $height"
  exit 0
fi

addr=$(rpc getnewaddress '["", "bech32"]' | tr -d '"')
need=$((431 - height))
if [ "$need" -gt 0 ]; then
  echo "mining $need blocks to $addr"
  rpc generatetoaddress "[$need, \"$addr\"]" >/dev/null
fi

mweb_addr=$(rpc getnewaddress '["", "mweb"]' | tr -d '"')
echo "pegging in 1 LTC to $mweb_addr"
rpc sendtoaddress "[\"$mweb_addr\", 1]" >/dev/null
echo "mining the MWEB activation block"
rpc generatetoaddress "[1, \"$addr\"]" >/dev/null
echo "height $(rpc getblockcount), mweb active: $(curl -s --data '{"method":"getblockchaininfo","params":[]}' "$RPC_URL" | python3 -c 'import sys,json; print(json.load(sys.stdin)["result"]["softforks"]["mweb"]["active"])')"
