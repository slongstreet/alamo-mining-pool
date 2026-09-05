# Roadmap

Work proceeds in waves. Each wave ends with something runnable and tested.

## Wave 0 — Scaffolding (done)
- Cargo workspace with six crates, pinned toolchain, fmt/clippy/test in CI.
- `alamo-core`: hashing (sha256d, scrypt), target/difficulty math, block odds math, with tests.
- Stratum protocol types and a listener that accepts connections and parses messages.
- Config format and example file.
- SQLite store that opens and migrates.
- Axum server with `/api/health` and embedded dashboard placeholder.
- Svelte dashboard skeleton.
- Dockerfile, compose file, systemd unit.

## Wave 1 — LTC solo, end to end on regtest (done)
- JSON-RPC client for `litecoind` (`getblocktemplate`, `submitblock`, `getblock`, tip polling).
- Template source: polls the tip twice a second, refreshes every 30s, publishes work over a
  watch channel with `clean_jobs` on a new tip.
- Coinbase builder: BIP34 height, tag, extranonce, payout to the worker's address, witness
  commitment output; split into coinb1/coinb2 for stratum.
- Stratum v1: subscribe, authorize, submit, notify, set_difficulty, configure,
  suggest_difficulty, extranonce.subscribe. Per-session jobs so every worker's coinbase pays
  its own address. Stale, duplicate, ntime, and extranonce checks.
- Share validation with scrypt, share and network targets, block assembly.
- Vardiff with bounded steps and idle lowering.
- Block submission, status recording, and confirmation tracking (orphan detection, maturity).
- Litecoin MWEB: templates are requested with the `mweb` rule; the HogEx transaction comes
  in the transaction list and the `mweb` payload is appended after the transactions with a
  presence byte. Verified on regtest before and after activation.
- Regtest harness: `deploy/regtest` runs `litecoind`, `activate-mweb.sh` brings the chain
  past MWEB activation, and `cargo test -p alamo --test regtest` mines a real block through
  the stratum path and checks the coinbase pays the worker's address. CI runs it.
- Status API (`/api/status`) with workers, hashrate, blocks, and block odds.

## Wave 2 — DOGE merge mining (done)
- `dogecoind` template source (same poller as Litecoin) and `submitblock` of auxpow blocks.
  Dogecoin's template version already carries chain id 98; the auxpow flag is added.
- Aux merkle tree (`alamo-core::auxpow`): `fabe6d6d` magic, root in display order, size
  and nonce, chain slots from `getExpectedIndex`. One aux chain today, but the tree builder
  handles several.
- AuxPow serialization: parent coinbase (non-witness), parent hash, coinbase branch, chain
  branch, chain index, parent header. Known-answer test round-trips Dogecoin mainnet block
  371,337 byte for byte and re-verifies every check `CAuxPow::check` performs.
- Merged work: parent and aux templates combine into one `MergedWork`; an aux tip change
  produces a non-clean job so parent shares stay valid, and the old job's aux block is
  marked stale. Sessions build the aux coinbase and header first, commit the aux block
  hash in the parent coinbase, and a share that meets any chain's target yields a block
  candidate for that chain.
- Per-worker DOGE payout address from the stratum password (`D...` or `doge=D...`), with
  the configured fallback otherwise; reported on the status API.
- Regtest harness runs `dogecoind` plus a peer (Dogecoin requires one for templates),
  `prepare-dogecoin.sh` mines past the auxpow start height, and the end-to-end test submits
  the same share as a Litecoin block and a merge-mined Dogecoin block. CI runs it.

## Wave 3 — Persistence and stats (done)
- Share accounting, hashrate samples (per worker, per pool), blocks found, worker last-seen.
- Retention and downsampling so the database stays small.
- Restart safety: jobs and sessions rebuild cleanly.

## Wave 4 — Dashboard (done)
- `/api/ws` streams every status snapshot; the dashboard falls back to polling `/api/status`
  while the socket is down and reconnects with backoff. `/api/hashrate`, `/api/shares`, and
  `/api/blocks` serve history from the store.
- Per-coin odds: P(block) in the next hour/day/week/30 days/year and expected time to block
  at the pool's hashrate. Rounds: workers accumulate lifetime accepted work and every block
  banks the pool total at the moment it was found, so the current round's work, its progress
  against the network difficulty, and lifetime luck survive share retention. Best share is
  shown against the network target on a log scale.
- Live hashrate chart (pool or one worker; 1h/24h/7d/30d) drawn as inline SVG from the
  downsampled samples, worker table with payout addresses and fallback flags, live share
  log, and block history with reward, share difficulty, and confirmations.
- Dark and light themes follow the system with a remembered manual override. No chart
  library; the built dashboard is about 70 kB before compression.

## Wave 5 — Hardening and packaging
- [x] SIGTERM triggers the same graceful shutdown and accounting flush as SIGINT, so
  systemd and Docker stops no longer drop the last batch of shares.
- [x] ZMQ `hashblock` notifications with a built-in ZMTP subscriber (no libzmq); polling
  stays on as the fallback.
- [x] Node reconnect and template staleness: retry unreachable nodes at startup, report
  node health on the dashboard and in metrics, withdraw templates from nodes that stay
  unreachable past `template_stale_secs`, refetch when they return.
- [x] Prometheus metrics at `/metrics`.
- Release builds for x86-64 and arm64 Linux, multi-arch Docker image.
- Operator docs.

## Later
- Additional coins via the `Coin` abstraction (BTC/BCH sha256d solo are cheap).
- Stratum v2.
- Notifications (webhook, email) on block found.
