# Alamo Mining Pool — working notes for Claude

## What this is
Self-hosted solo mining pool in Rust. LTC parent chain, DOGE merge-mined aux chain first.
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
- Run `cargo fmt`, `cargo clippy --workspace --all-targets -- -D warnings`, and
  `cargo test --workspace` before declaring anything done.

## Regtest and verification
- `deploy/regtest/docker-compose.yml` runs litecoind on regtest (rpc alamo/alamo, port 19443).
- `deploy/regtest/activate-mweb.sh` mines to 432 with a peg-in so MWEB activates; the first
  MWEB block needs a peg-in or the node cannot build it.
- `ALAMO_REGTEST_RPC=http://alamo:alamo@127.0.0.1:19443 cargo test -p alamo --test regtest`
  is the proof that block construction is right. Run it before and after MWEB activation when
  touching coinbase, merkle, template, or block assembly code.
- Litecoin block serialization after MWEB: header, txs (HogEx last, as given by the
  template), then `0x01` + the template's `mweb` hex. Nothing is appended before activation.

## Frontend
- `web/` is Svelte 5 + TypeScript + Vite. `npm run build` writes `web/dist`, which
  `alamo-web` embeds with `rust-embed`. A missing `web/dist` must not break `cargo build`.
- Dashboard talks to the daemon over `/api/*` and a WebSocket at `/api/ws`.

## Roadmap
See docs/ROADMAP.md. Work proceeds in waves; do not start a later wave's features inside an
earlier wave's change.
