# Alamo Mining Pool — working notes for Claude

## What this is
Self-hosted solo mining pool in Rust. LTC parent chain, DOGE merge-mined aux chain first;
BTC (SHA-256d) as a lane of its own.
Single binary with an embedded Svelte dashboard. SQLite storage. Payouts go directly from the
coinbase to the address the miner used as its stratum username.

## Non-negotiables
- Block construction must be byte-exact. Any change to coinbase, merkle, auxpow, or header
  serialization needs a known-answer test against real chain data or a regtest round trip.
- Hot path (share validation) must not allocate per share beyond what is unavoidable and must
  never block the tokio runtime. Use `spawn_blocking` for scrypt if it ever matters.
- Never log or persist RPC credentials.
- Keep the daemon's idle memory small. Prefer bounded channels and fixed-size ring buffers
  over unbounded growth.

## Conventions
- Workspace crates: `alamo-core` (pure, no I/O), `alamo-coins`, `alamo-stratum`,
  `alamo-store`, `alamo-web`, `alamo` (binary).
- `alamo-core` must stay free of tokio, sqlx, axum, and reqwest.
- Errors: `thiserror` in library crates, `anyhow` only in the binary.
- Logging: `tracing`. Use structured fields, not formatted strings.
- Config: TOML, deserialized with serde, validated at startup with clear messages.
  Config: main file, then `<data_dir>/alamo.local.toml` merged over it (tables merge,
  other values replace; it may not set `data_dir`), then Settings-page overrides.
- Run `cargo fmt`, `cargo clippy --workspace --all-targets -- -D warnings`, and
  `cargo test --workspace` before declaring anything done.

## Regtest and verification
- `deploy/regtest/docker-compose.yml` runs litecoind (rpc alamo/alamo, port 19443) and
  dogecoind (rpc alamo/alamo, port 18332) plus a second dogecoind that exists only as a
  peer: Dogecoin Core refuses `getblocktemplate` with zero peers, even on regtest. It also
  runs bitcoind (Bitcoin Core 32.1, rpc alamo/alamo, port 18443, ZMQ hashblock 28334),
  which needs no peer. Its Dockerfile pins the tarball SHA256s from the release's
  SHA256SUMS; bump version and both hashes together.
- `deploy/regtest/prepare-dogecoin.sh` mines 30 Dogecoin blocks. Dogecoin regtest rejects
  auxpow blocks below height 20 and legacy blocks from 20 on; coinbase maturity is 60.
- `deploy/regtest/activate-mweb.sh` mines to 432 with a peg-in so MWEB activates; the first
  MWEB block needs a peg-in or the node cannot build it.
- `deploy/regtest/prepare-bitcoin.sh` mines to 110 (coinbase maturity is 100) into a
  wallet named `alamo` and, whenever the mempool has none, sends a segwit transaction; the
  BTC test consumes it, so rerun the script before each run of that test. Bitcoin regtest
  network difficulty is ~4.7e-10, below the SHA-256d vardiff floor of 0.001, so tests and
  `config/alamo.regtest.toml` set their own vardiff table (min 1e-9) to make every share a
  block. Bitcoin Core clears version bits of deployments not named in the gbt rules, so
  templates there are a plain `0x20000000`.
- `ALAMO_REGTEST_RPC=http://alamo:alamo@127.0.0.1:19443
  ALAMO_REGTEST_DOGE_RPC=http://alamo:alamo@127.0.0.1:18332 cargo test -p alamo --test regtest`
  is the proof that block construction is right. Run it before and after MWEB activation when
  touching coinbase, merkle, auxpow, template, or block assembly code. Add
  `ALAMO_REGTEST_BTC_RPC=http://alamo:alamo@127.0.0.1:18443` (after prepare-bitcoin.sh) for
  the Bitcoin lane test (`mines_bitcoin_blocks_with_version_rolling`); each regtest test
  skips cleanly when its variable is unset.
- Litecoin block serialization after MWEB: header, txs (HogEx last, as given by the
  template), then `0x01` + the template's `mweb` hex. Nothing is appended before activation.
- Dogecoin templates report version `0x00620004` (chain id 98 already in place); mined blocks
  are `0x00620104` (auxpow flag). Block = header, auxpow, varint(ntx), txs. The auxpow's
  coinbase is the non-witness Litecoin coinbase; its `hashBlock` field is not verified.
- `deploy/regtest/mine-shares.py --port 3333|3334 [--seconds N]` is a dev-only CPU miner that
  gives a lane workers, shares, and hashrate on regtest (most shares are also regtest blocks).
- `crates/alamo-core/testdata/doge-371337.hex` is a real mainnet auxpow block; the KAT in
  `auxpow.rs` must keep passing. `btc-699412.hex` is a real Bitcoin mainnet segwit block
  whose header has BIP320 bits rolled; the KATs in `bitcoin_kat.rs` check it authenticates
  by its own proof of work.
- Version rolling (BIP310) is a lane capability: `Coin::version_rolling_mask()` (BTC
  `0x1fffe000`, scrypt coins 0). Scrypt lanes must keep answering
  `{"version-rolling": false}` and ignoring a sixth submit parameter.

## Frontend
- `web/` is Svelte 5 + TypeScript + Vite. `npm run build` writes `web/dist`, which
  `alamo-web` embeds with `rust-embed`. A missing `web/dist` must not break `cargo build`.
- Dashboard talks to the daemon over `/api/*` and a WebSocket at `/api/ws`.

## Roadmap
See docs/ROADMAP.md. Work proceeds in waves; do not start a later wave's features inside an
earlier wave's change.
