# Alamo Mining Pool

A self-hosted **solo** mining pool with beautiful visuals. Point your ASICs at it, run it
next to your own full nodes, and watch your odds of hitting the next block in real time.

- **Solo only.** Every block pays the miner who found it, straight from the coinbase.
- **Merge mining.** Litecoin (LTC) as the parent chain with Dogecoin (DOGE) as the aux chain
  out of the box. More coins later.
- **Single binary.** Rust daemon with the dashboard embedded. One executable, one config file.
- **Small footprint.** Built to sit quietly beside `litecoind` and `dogecoind` on a mini PC or
  a Raspberry Pi.
- **Odds visualizer.** Probability of a block in the next hour, day, week, and year, expected
  time to block, and luck versus expectation, all live.

> Status: Litecoin solo mining and Dogecoin merge mining work end to end on regtest,
> including Litecoin MWEB blocks. Shares, workers, hashrate samples, and blocks persist
> across restarts. The dashboard is next. See [docs/ROADMAP.md](docs/ROADMAP.md) for the waves.

## Layout

```
crates/
  alamo-core      Pure types and math: hashing, targets, difficulty, jobs, block odds
  alamo-coins     Coin definitions, node RPC, block templates, auxpow (LTC, DOGE)
  alamo-stratum   Stratum v1 TCP server, sessions, vardiff, job broadcast
  alamo-store     SQLite persistence (shares, hashrate samples, blocks, workers)
  alamo-web       HTTP/WebSocket API and the embedded dashboard
  alamo           The `alamo` binary: config, wiring, lifecycle
web/              Svelte + TypeScript dashboard, built by Vite and embedded into `alamo`
config/           Example configuration
deploy/           Dockerfile, docker-compose, systemd unit
docs/             Architecture and roadmap
```

## Quick start (development)

Prerequisites: Rust stable (via [rustup](https://rustup.rs)), Node 20+.

```bash
# Build the dashboard once so it can be embedded
cd web && npm install && npm run build && cd ..

# Build and run the pool with the example config
cargo run -p alamo -- --config config/alamo.example.toml
```

The dashboard is served at <http://localhost:8080> and stratum listens on port 3333.

Run the checks:

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

## Regtest

Throwaway Litecoin and Dogecoin regtest nodes live in `deploy/regtest` (Dogecoin runs as a
pair, because `dogecoind` refuses to hand out block templates without a peer):

```bash
docker compose -f deploy/regtest/docker-compose.yml up -d --build
./deploy/regtest/prepare-dogecoin.sh       # mine the 20 blocks Dogecoin needs before auxpow
./deploy/regtest/activate-mweb.sh          # optional: mine Litecoin past MWEB activation
cargo run -p alamo -- --config config/alamo.regtest.toml
```

The end-to-end test mines a real share through the stratum path and verifies that both
nodes accepted the resulting blocks and that each coinbase paid the worker's address:

```bash
ALAMO_REGTEST_RPC=http://alamo:alamo@127.0.0.1:19443 \
ALAMO_REGTEST_DOGE_RPC=http://alamo:alamo@127.0.0.1:18332 \
  cargo test -p alamo --test regtest -- --nocapture
```

Leave `ALAMO_REGTEST_DOGE_RPC` unset to test Litecoin alone.

## Configuration

Copy `config/alamo.example.toml` to `alamo.toml` and edit it. Miners connect with their
payout address as the stratum username (ckpool style), for example
`ltc1q...` for Litecoin. A worker name can be appended after a dot: `ltc1q....rig1`.

The Dogecoin payout address goes in the stratum password, either bare (`D...`) or tagged
(`doge=D...`, alongside anything else the miner sends there such as `d=1024`). Workers that
do not supply one are paid at the configured fallback address, and the dashboard says so.

## License

MIT. See [LICENSE](LICENSE).
