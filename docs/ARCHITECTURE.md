# Architecture

## Overview

```
 ASIC miners ──stratum v1 (TCP)──▶ alamo-stratum ──shares──▶ validator (alamo-core)
                                        ▲                        │
                                   jobs │                        │ block found
                                        │                        ▼
                    alamo-coins ◀──templates/submit──▶ litecoind / dogecoind (JSON-RPC, ZMQ)
                                        │
                                        ▼
                                  alamo-store (SQLite) ◀──▶ alamo-web (axum, WS) ◀──▶ dashboard
```

The `alamo` binary wires these together. Everything lives in one process.

## Crates

### alamo-core
Pure computation, no I/O, no async. Hashing (sha256d, scrypt N=1024 r=1 p=1), 256-bit
targets and compact bits, share and block difficulty, job and share types, and the block
odds math used by the dashboard. This crate is where the byte-exact code lives and where the
known-answer tests are.

### alamo-coins
The `Coin` abstraction and its implementations. A coin knows its PoW algorithm, its address
formats, how to fetch a block template from its node, how to build a coinbase paying a given
address, and how to submit a block. Merge mining is modeled as a parent coin that carries
one or more aux coins: the parent's coinbase commits to the aux merkle root, and a found
share that meets an aux target is assembled into an auxpow block for that aux chain.

### alamo-stratum
Tokio TCP server. One task per connection, line-delimited JSON-RPC. The template source
publishes `Arc<WorkTemplate>` on a `watch` channel; each session derives its own job from it
because the coinbase pays the address the session authorized with. Sessions track
extranonce1, authorized workers, current difficulty, the last few jobs (for late shares),
and per-job seen-share sets. Vardiff adjusts per-session difficulty toward a target share
interval. Session logic is pure (`Session::handle` returns effects), so it is unit-tested
without sockets.

### alamo-store
SQLite via sqlx with embedded migrations. Stores blocks found, hashrate samples, worker
metadata, and recent shares. Retention runs periodically to keep the file small.

### alamo-web
Axum router. `/api/*` JSON endpoints, `/api/ws` for live updates, and everything else served
from the embedded `web/dist`. The dashboard is a Svelte SPA.

### alamo
Config loading and validation, tracing setup, task supervision, graceful shutdown.

## Payouts
Solo means no payout ledger. The coinbase of every job pays the address the connecting
worker used as its stratum username. When a worker's share meets the network target, the
resulting block already pays that worker. A configured fallback address is used if a
username fails address validation, with a warning surfaced on the dashboard.

## Odds math
Given pool hashrate `H` (hashes/s) and network difficulty `D`, the expected hashes per block
is `D * 2^32`. Blocks arrive as a Poisson process with rate `λ = H / (D * 2^32)`.
`P(at least one block within t seconds) = 1 - exp(-λ t)`. Expected time to block is `1/λ`.
Luck is `expected shares / actual shares` since the last block, expressed as a percentage.
