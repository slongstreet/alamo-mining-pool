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
formats, the `getblocktemplate` rules and block version it needs, its coinbase maturity,
and any bytes its blocks carry after the transactions (Litecoin MWEB). One `TemplateSource`
per node polls the tip and publishes `Arc<WorkTemplate>` on a `watch` channel; `merge`
combines the parent's stream with the aux streams into `Arc<MergedWork>`. A parent tip
change is a clean job; an aux tip change is a non-clean job, so parent shares in flight
stay valid.

When a coin has `zmq_hashblock` configured, a `ZmqSubscriber` keeps a subscription to the
node's `hashblock` topic and every notification triggers an immediate poll. The subscriber
speaks just enough ZMTP 3.0 itself (greeting, NULL handshake, subscription, frames), so
there is no libzmq dependency. Polling never stops; it slows to every 10 s while the
subscription is up and is the fallback when it is not.

Each source reports `NodeHealth` on its own watch channel: whether the last RPC call
succeeded, consecutive failures, the last error, and the ZMQ state. At startup the daemon
retries an unreachable node with backoff instead of exiting, so it can boot before its
nodes do. If a node stays unreachable past `template_stale_secs`, the source withdraws its
template (`None` on the channel): a withdrawn aux template drops out of merged work on the
next job, while a withdrawn parent leaves miners on their last job because stratum v1 has
no way to pause them. When the node returns, the template is refetched whatever the tip.

### alamo-stratum
Tokio TCP server. One task per connection, line-delimited JSON-RPC. Each session derives
its own job from the shared `MergedWork` because every coinbase pays the addresses the
session authorized with: for each aux chain it builds the aux coinbase and header, then
commits the aux block hashes (an `AuxTree`) in the parent coinbase scriptSig between the
template prefix and the extranonce. Share validation rebuilds the parent header, hashes
it once, and compares against the share target, the parent target, and every aux target;
each target met yields a `BlockCandidate` (the aux ones wrapped in an `AuxPow`). Sessions
track extranonce1, authorized workers, current difficulty, the last few jobs (for late
shares), and per-job seen-share sets. Vardiff adjusts per-session difficulty toward a
target share interval. Session logic is pure (`Session::handle` returns effects), so it is
unit-tested without sockets.

### alamo-store
SQLite via sqlx with embedded migrations. Stores blocks found, hashrate samples, worker
metadata, and recent shares. Retention runs periodically to keep the file small: shares
are kept for a day, hashrate samples are folded from 1-minute to 5-minute to 1-hour
buckets (only complete buckets, so averages never drift) and expire after 30 days. On
startup the publisher restores worker counters and the hashrate window; jobs and sessions
are rebuilt from the current template when miners reconnect. Workers carry lifetime
accepted work and each block records the pool total at the moment it was found, which is
what round progress and luck are computed from.

### alamo-web
Axum router. `/api/status` is the snapshot document the publisher rebuilds every two
seconds; `/api/ws` pushes the same document on every rebuild through a watch channel.
`/api/hashrate`, `/api/shares`, and `/api/blocks` read history straight from the store.
`/metrics` renders the same snapshot in the Prometheus text format, so scraping costs no
database reads. Everything else is served from the embedded `web/dist`. The dashboard is a Svelte 5 SPA
that draws its charts as inline SVG.

### alamo
Config loading and validation, tracing setup, task supervision, graceful shutdown.
SIGINT and SIGTERM take the same path: cancel every task, flush pending accounting to
the store, and exit once the servers drain or after five seconds, whichever comes first.

## Payouts
Solo means no payout ledger. The coinbase of every job pays the address the connecting
worker used as its stratum username. When a worker's share meets the network target, the
resulting block already pays that worker. A configured fallback address is used if a
username fails address validation, with a warning surfaced on the dashboard. Aux chain
addresses come from the stratum password (`D...` or `doge=D...`) and fall back the same way.

## Merge mining
Dogecoin accepts a block whose proof of work is a Litecoin header, provided the Litecoin
coinbase commits to the Dogecoin block hash. The commitment is `fabe6d6d`, the aux merkle
root in display byte order, the tree size, and a nonce; the aux block then carries an
`AuxPow` (parent coinbase, its merkle branch, the chain branch, the parent header) between
its own header and transactions. `alamo-core::auxpow` owns these bytes and is verified
against Dogecoin mainnet block 371,337. Because the aux block hash depends on the aux
coinbase, and the aux coinbase pays the worker, the commitment is per session, like the
parent coinbase already was.

## Odds math
Given pool hashrate `H` (hashes/s) and network difficulty `D`, the expected hashes per block
is `D * 2^32`. Blocks arrive as a Poisson process with rate `λ = H / (D * 2^32)`.
`P(at least one block within t seconds) = 1 - exp(-λ t)`. Expected time to block is `1/λ`.
Luck is `expected shares / actual shares` since the last block, expressed as a percentage.
