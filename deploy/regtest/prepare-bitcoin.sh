#!/usr/bin/env bash
# Bring the regtest Bitcoin chain to a state the BTC regtest test needs: spendable coins
# (coinbase maturity is 100, so at least 101 blocks) and at least one segwit transaction
# in the mempool, so the next block template carries a witness commitment over a real
# witness transaction. Safe to run repeatedly: it only mines when the chain is short and
# only sends when the mempool holds no segwit transaction.
set -euo pipefail

RPC_URL="${ALAMO_REGTEST_BTC_RPC:-http://alamo:alamo@127.0.0.1:18443}"
WALLET="alamo"
MIN_HEIGHT="${1:-110}"

# Call a node RPC (or, with WALLET_RPC=1, a wallet RPC) and print its JSON result.
rpc() {
  local method="$1"; shift
  local params="${1:-[]}"
  local url="$RPC_URL"
  [ "${WALLET_RPC:-0}" = 1 ] && url="$RPC_URL/wallet/$WALLET"
  curl -sf --data "{\"method\":\"$method\",\"params\":$params}" "$url" \
    | python3 -c 'import sys,json; r=json.load(sys.stdin); sys.exit(1) if r["error"] else print(json.dumps(r["result"]))'
}
wallet() { WALLET_RPC=1 rpc "$@"; }

# Wait until the node answers getblockcount without an error. While it starts up it
# answers with an error (-28, "Loading block index...") that still carries a "result" key,
# so only a null error counts as ready.
ready() {
  curl -s --data '{"method":"getblockcount","params":[]}' "$RPC_URL" 2>/dev/null \
    | python3 -c 'import sys,json; r=json.load(sys.stdin); sys.exit(0 if r.get("error") is None and "result" in r else 1)' \
      2>/dev/null
}
for _ in $(seq 1 90); do
  ready && break
  sleep 1
done
ready || { echo "bitcoind at $RPC_URL did not become ready" >&2; exit 1; }

# Create the wallet on first use and load it after a restart; either may already be done.
curl -s --data "{\"method\":\"createwallet\",\"params\":[\"$WALLET\"]}" "$RPC_URL" >/dev/null || true
curl -s --data "{\"method\":\"loadwallet\",\"params\":[\"$WALLET\"]}" "$RPC_URL" >/dev/null || true

height=$(rpc getblockcount)
if [ "$height" -lt "$MIN_HEIGHT" ]; then
  addr=$(wallet getnewaddress '["", "bech32"]' | tr -d '"')
  need=$((MIN_HEIGHT - height))
  echo "mining $need bitcoin regtest blocks to $addr"
  rpc generatetoaddress "[$need, \"$addr\"]" >/dev/null
fi

# Does the mempool already hold a transaction with witness data?
has_segwit() {
  local txid
  for txid in $(rpc getrawmempool | python3 -c 'import sys,json; print(" ".join(json.load(sys.stdin)))'); do
    if rpc getrawtransaction "[\"$txid\", true]" \
      | python3 -c 'import sys,json; t=json.load(sys.stdin); sys.exit(0 if any("txinwitness" in i for i in t["vin"]) else 1)'; then
      return 0
    fi
  done
  return 1
}

if has_segwit; then
  echo "bitcoin regtest mempool already holds a segwit transaction"
else
  # Spending the wallet's P2WPKH coinbase outputs makes a segwit transaction.
  to=$(wallet getnewaddress '["", "bech32"]' | tr -d '"')
  txid=$(wallet sendtoaddress "[\"$to\", 1.5]" | tr -d '"')
  echo "sent segwit transaction $txid"
fi
echo "bitcoin regtest at height $(rpc getblockcount), mempool: $(rpc getmempoolinfo | python3 -c 'import sys,json; print(json.load(sys.stdin)["size"])') transaction(s)"
