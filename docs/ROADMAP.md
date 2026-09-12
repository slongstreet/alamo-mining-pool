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
- `/api/ws` pushes the status document on every publish and each share in between;
  the dashboard falls back to polling `/api/status` when the socket is down.
- History endpoints: `/api/hashrate?range=1h|6h|24h|7d|30d&worker=`, `/api/shares`,
  `/api/blocks`, read from the store.
- Per-coin rounds (`rounds` table): work, shares, and best share since the last block
  found on that coin, reset by the submitter. Luck is expected work over round work.
- Status document carries per-coin odds, round, maturity, and the pool's best share.
- Dashboard: stat tiles, per-coin odds visualizer (horizon probabilities, expected time,
  round progress and luck, best share on a log scale against the network target),
  hashrate chart with range and worker selection and a table view, worker table, live
  share log, block history with confirmation progress. Dark, light, and system themes.

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
