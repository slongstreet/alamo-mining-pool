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

## Wave 2 — DOGE merge mining
- `dogecoind` template fetching and auxpow block submission.
- Aux merkle tree commitment in the LTC coinbase (`fabe6d6d` magic, chain id slot).
- AuxPow serialization: coinbase tx, coinbase branch, blockchain branch, parent header.
- Per-worker DOGE payout address (password field or fallback).
- Regtest verification of a merge-mined DOGE block.

## Wave 3 — Persistence and stats
- Share accounting, hashrate samples (per worker, per pool), blocks found, worker last-seen.
- Retention and downsampling so the database stays small.
- Restart safety: jobs and sessions rebuild cleanly.

## Wave 4 — Dashboard
- Live hashrate, worker table, share log.
- Odds visualizer: P(block) in the next hour/day/week/year, expected time to block,
  luck vs expectation, best share vs network target.
- Block history with confirmations.
- Polished visuals, dark and light themes.

## Wave 5 — Hardening and packaging
- ZMQ `hashblock` notifications instead of polling.
- Node reconnect and template staleness handling.
- Prometheus metrics endpoint.
- Release builds for x86-64 and arm64 Linux, multi-arch Docker image.
- Operator docs.

## Later
- Additional coins via the `Coin` abstraction (BTC/BCH sha256d solo are cheap).
- Stratum v2.
- Notifications (webhook, email) on block found.
