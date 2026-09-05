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

## Frontend
- `web/` is Svelte 5 + TypeScript + Vite. `npm run build` writes `web/dist`, which
  `alamo-web` embeds with `rust-embed`. A missing `web/dist` must not break `cargo build`.
- Dashboard talks to the daemon over `/api/*` and a WebSocket at `/api/ws`.

## Roadmap
See docs/ROADMAP.md. Work proceeds in waves; do not start a later wave's features inside an
earlier wave's change.
