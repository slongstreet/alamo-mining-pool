# Roadmap

Work proceeds in waves. Each wave ends with something runnable and tested.

## Wave 0 — Scaffolding (current)
- Cargo workspace with six crates, pinned toolchain, fmt/clippy/test in CI.
- `alamo-core`: hashing (sha256d, scrypt), target/difficulty math, block odds math, with tests.
- Stratum protocol types and a listener that accepts connections and parses messages.
- Config format and example file.
- SQLite store that opens and migrates.
- Axum server with `/api/health` and embedded dashboard placeholder.
- Svelte dashboard skeleton.
- Dockerfile, compose file, systemd unit.

## Wave 1 — LTC solo, end to end on regtest
- JSON-RPC client for `litecoind` (`getblocktemplate`, `submitblock`, `getblockchaininfo`).
- Job builder: coinbase with payout to the worker's address, merkle branch, header fields.
- Stratum v1: `mining.subscribe`, `mining.authorize`, `mining.submit`, `mining.notify`,
  `mining.set_difficulty`, extranonce handling, clean-jobs on new template.
- Share validation: scrypt PoW hash, share target, block target, duplicate detection.
- Vardiff.
- Block submission and confirmation tracking.
- Regtest compose with `litecoind` and a CPU miner for integration tests.

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
