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

> Status: early scaffolding. See [docs/ROADMAP.md](docs/ROADMAP.md) for the implementation waves.

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

## Configuration

Copy `config/alamo.example.toml` to `alamo.toml` and edit it. Miners connect with their
payout address as the stratum username (ckpool style), for example
`ltc1q...` for Litecoin. A worker name can be appended after a dot: `ltc1q....rig1`.
For merge mining, DOGE payout addresses are provided per worker via the password field or
a configured fallback. Details are in the example config.

## License

MIT. See [LICENSE](LICENSE).
