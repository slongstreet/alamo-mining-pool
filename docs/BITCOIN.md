# Bitcoin solo mining — design

Status: implemented. This was Wave 7 in [ROADMAP.md](ROADMAP.md), released as 0.1.14:
lanes, the Bitcoin coin, version rolling, vardiff, the proof, the dashboard and settings
UI, and packaging are all in. The document is kept as the design record; where it and the
code disagree, the code and [OPERATING.md](OPERATING.md) are right.

Adding Bitcoin is mostly a runtime-shape problem, not a hashing problem.
`Algorithm::Sha256d` already exists, but the daemon assumes exactly one parent chain
everywhere. This design adds a second independent *lane* (a parent chain plus its aux
chains) and three Bitcoin-specific pieces: the coin definition, BIP310 version rolling, and
a vardiff that copes with ASIC-scale hashrate.

## Where the code stands

### Already Bitcoin-ready
- `Algorithm::Sha256d` with share multiplier 1, the 80-byte header, the merkle branch, the
  BIP34 coinbase, the witness commitment, and block assembly are chain-agnostic.
- Address parsing handles base58, bech32, and bech32m, so `bc1q…` and `bc1p…` need only an
  `AddressParams` entry.
- Template polling, ZMQ, node health, confirmation tracking, and the odds math work
  unchanged against a `bitcoind`.

### What blocks Bitcoin today
- ~~**One parent only.**~~ Done: every enabled parent is a lane with its own listener and
  vardiff (`[coins.<key>.stratum]`), started independently.
- ~~**No version rolling.**~~ Done: a Bitcoin lane negotiates BIP310 (section 4).
- ~~**Stats are global.**~~ Done: one `Stats` per lane, each with its own
  `share_multiplier`, and the API and metrics show every lane (section 7).
- ~~**Storage has no lane.**~~ Done: migration 0008 (section 6).

### Prework before real BTC hashrate is pointed at it
- [x] **Block submission retries.** An unreachable node is retried for several minutes
  instead of the block being recorded as rejected and dropped, and a copy of every block
  not yet accepted is kept under `<data_dir>/blocks/`.
- [x] **Submissions run independently.** A slow or down node no longer delays a block
  for another chain.
- [x] **Vardiff fast start.** A worker measured at 8x the target share rate or more jumps
  straight to its measured difficulty after five seconds instead of doubling once a
  minute.
- [x] **Startup is all-or-nothing.** Stratum did not listen until every node answered, so
  a syncing `bitcoind` would have held LTC miners off. Lanes now start independently
  (below).

## Technical design

### 1. Lanes
A lane is one parent coin, its aux coins, its own stratum listener, vardiff, `Stats`, and
`Persistence`. It is keyed by the parent's config key (`ltc`, `btc`).

- `pool::run` builds one lane per enabled parent and starts each independently, so one
  lane's unreachable node never holds another lane's listener closed.
- Block submission already retries, keeps a copy of the block, and runs per candidate
  (prework above); lanes only add their nodes to it.
- The existing publisher composes one snapshot from all lanes.

This step is a refactor: with only LTC and DOGE configured, behaviour does not change.

### 2. Configuration
```toml
[coins.btc]
enabled = true
rpc_url = "http://127.0.0.1:8332"
rpc_user = "alamo"
rpc_password = "${BTC_RPC_PASSWORD}"
zmq_hashblock = "tcp://127.0.0.1:28332"
fallback_address = "bc1q..."

[coins.btc.stratum]
listen = "0.0.0.0:3334"

[coins.btc.stratum.vardiff]   # optional; if present, every field
initial_difficulty = 1024
min_difficulty = 0.001
max_difficulty = 4294967296
target_share_seconds = 10
retarget_seconds = 60
variance_percent = 30
```

- `[stratum]` stays as the default for a parent without its own `[coins.<key>.stratum]`
  section, so existing files and a BTC-only file both work.
- Two parents resolving to the same listen address is a startup error with a message that
  names both coins.
- Coin keys are lower-cased when the file is loaded; keys differing only by case are an
  error. A `stratum` section on a merge-mined coin is an error.
- Stored vardiff overrides mirror the config path: `stratum.vardiff.<field>` applies to
  every lane on `[stratum]`, `coins.<key>.stratum.vardiff.<field>` to a lane with its own
  section, so existing rows keep working.
- Separate ports rather than routing one port by address prefix: a username that is not a
  valid address would have no unambiguous fallback lane.

### 3. Bitcoin coin
`crates/alamo-coins/src/btc.rs`, registered as `btc`:

| Property | Value |
| --- | --- |
| Symbol / name | `BTC` / Bitcoin |
| Algorithm | `Sha256d` (share multiplier 1) |
| Aux chain id | none |
| Template rules | `["segwit"]` |
| Coinbase maturity | 100 |
| Addresses, main | P2PKH 0, P2SH 5, hrp `bc` |
| Addresses, test | P2PKH 111, P2SH 196, hrp `tb` |
| Addresses, regtest | P2PKH 111, P2SH 196, hrp `bcrt` |
| Version rolling mask | `0x1fffe000` (BIP320) |

Bech32m (taproot, `bc1p…`) payouts work through the existing witness v1+ handling; the
tests use BIP350's and BIP86's vectors. Testnet3 (`test`) and testnet4 (`testnet4`) share
the `tb` parameters. Signet is refused: `Coin::unsupported_network` makes a node reporting
`chain = "signet"` fail the BTC lane at connect with a message that says why (signet
templates need the `signet` rule and a signet solution in the coinbase). Like any lane
start failure it fails that lane alone.

### 4. BIP310 version rolling (SHA-256 lanes only)
The capability comes from the coin: `Coin::version_rolling_mask()` (default 0) is handed to
the lane's `StratumServer`, and a session offers version rolling only when it is non-zero.

- `mining.configure` may arrive before or after subscribe and authorize. Every requested
  extension gets an answer: `version-rolling` gets `true` with `version-rolling.mask` =
  miner mask AND `0x1fffe000` as 8 hex digits, every other extension `false`. A missing
  miner mask means `ffffffff` (BIP310's default). If the shared mask is empty the answer is
  `false` and the miner hashes without rolling. `version-rolling.min-bit-count` is
  diagnostic (BIP310): a non-empty mask with fewer bits than the miner asked for is still
  granted, and the shortfall is logged at info level. A malformed mask gets an error
  string, as BIP310 allows. Scrypt lanes
  answer `{"version-rolling": false}` to every configure, exactly as before.
- `mining.submit` takes an optional sixth parameter, the version bits (hex). Any bit
  outside the session's negotiated mask (zero before a successful configure) rejects the
  share with `invalid_version_bits` ("Invalid version bits", code 20). The header version
  is `(job_version & !mask) | (bits & mask)` (`alamo_core::header::rolled_version`); no
  sixth parameter hashes the job's version. A scrypt lane ignores a sixth parameter.
- The per-job duplicate key is `(extranonce2, ntime, nonce, version)`, and the candidate
  block is assembled from the rolled header. Nothing is allocated per share for this.
- No `mining.set_version_mask`: the lane's mask never changes.

Reference implementations differ from the BIP on the header formula: ckpool ORs the bits
into the job version and public-pool XORs them. All three agree when the job version has
no bits inside the mask, which is what Bitcoin Core templates carry today (BIP320 keeps
deployments out of bits 13-28); the BIP's form is used here. Neither ckpool nor
public-pool ANDs with the miner's mask or looks at `min-bit-count`.

### 5. Vardiff
- Fast start is done (prework above).
- Built-in defaults depend on the lane's algorithm when no table is configured for it
  (its own section's, or `[stratum.vardiff]` for a lane on `[stratum]`): scrypt keeps
  65536 initial, 1024 to 16777216; SHA-256d starts at 1024 within 0.001 to 4294967296.
  Both aim for a share every 10 s, retarget every 60 s, and allow 30 % variance.
  `[stratum].vardiff` is now optional in the file so "absent" is distinguishable from a
  table that happens to spell out the scrypt defaults; the settings page shows the
  effective file values either way.
- `d=<n>` in the stratum password sets a session's starting difficulty on any lane,
  clamped to the lane's min and max, next to aux addresses (`doge=D…,d=4096`,
  `D… d=4096`); values that are not positive numbers are ignored.
- Fractional difficulties work end to end (`mining.set_difficulty`, share target, clamp);
  `Target::from_difficulty` keeps 32 fractional bits.

### 6. Storage (migration 0008)
- Add `lane` to `workers` (primary key becomes `(lane, name)`), `shares`, and
  `hashrate_samples`; key `retired_work` per lane in `pool_counters`.
- Existing rows default to `ltc`. That is safe because no other parent has ever existed.

### 7. API and metrics (done)
- ~~The snapshot gains `lanes[]`.~~ Done: one entry per configured lane, primary first,
  with key, algorithm, stratum port, `state` (`connecting`/`running`/`failed`) and
  `error`, chain, share multiplier, hashrate, share counts, work, best share,
  `scoring_since`, coins, and workers. It is published from startup.
- ~~Top-level fields mirror the first lane.~~ Done: they mirror the primary lane while it
  runs (defaults otherwise), for one release, marked deprecated. `blocks` stays global,
  each row converted with its own lane's share multiplier; rounds are keyed by lane and
  coin.
- ~~`lane` parameters.~~ Done: `/api/hashrate`, `/api/shares`, the stats reset, and
  worker removal take `?lane=` (default the primary lane; unknown is a 400).
- ~~Prometheus `lane` label.~~ Done on every per-lane series, plus
  `alamo_lane_state{lane,state}`.
- ~~Per-lane settings.~~ Done: `SettingsDoc.lanes[]` with each lane's listener and
  vardiff, `SettingsPatch.lanes.<key>.vardiff`, and every configured coin listed with its
  lane and whether it has connected. The coinbase tag stays global, with the byte budget
  taken from the tightest lane (41 bytes when a lane merge-mines).
- A lane that cannot start fails alone and is shown as failed; the daemon exits only when
  no lane is left running or connecting.

### 8. Proof
Block construction must be byte-exact (see `CLAUDE.md`), so:

- Known-answer tests in `alamo-core` (`src/bitcoin_kat.rs`) against real mainnet data.
  Block 699,412 (`crates/alamo-core/testdata/btc-699412.hex`, 7 transactions, 5 segwit):
  header round trip, block hash, proof of work against its own bits, merkle root from the
  coinbase branch, the coinbase's witness serialization rebuilt from stratum-style parts,
  the witness commitment, and version rolling (`0x20000004` template version, bits
  `0x0fffe000`, mask `0x1fffe000` give the mined `0x2fffe004`). Block 759,892's header
  repeats the version-rolling check over a plain `0x20000000`.
- Regtest: `bitcoind` (Bitcoin Core 32.1, SHA256-verified tarball) in
  `deploy/regtest/docker-compose.yml`, `deploy/regtest/prepare-bitcoin.sh` to mature coins
  and put a segwit transaction in the mempool, and `mines_bitcoin_blocks_with_version_rolling`
  in `crates/alamo/tests/regtest.rs`: the whole pool with a BTC lane, `mining.configure`,
  `d=` in the password, two CPU-mined blocks with non-zero rolled bits, checked in
  bitcoind for the rolled version, the mempool segwit transaction, and coinbases paying a
  bech32 and a bech32m address. CI runs it after the LTC/DOGE runs.

## User interface design

### Dashboard
- **Lane cards.** A strip of one card per lane under the header: name, algorithm, port,
  node health, hashrate, workers, and expected time to block. Selecting a card scopes the
  page to that lane; the choice lives in the hash (`#/btc`) and is remembered. With one
  lane configured the strip is hidden and the dashboard looks as it does today.
- **Scoped panels.** Stat tiles, block odds, hashrate chart, workers, and the share log
  follow the selected lane. "Pool hashrate" becomes "Lane hashrate".
- **Block history stays global** with its coin column, since blocks are rare.
- **Odds for long shots.** At home-miner Bitcoin hashrate the percentage bars would all
  read 0.00%. Below 1%, show "1 in N" and lead with the yearly chance.
- **Miner setup.** Per lane: the lane's port, a BTC address (plus optional `.worker`) as
  the username, any password, and a note that version rolling is supported.

### Settings
- A Bitcoin node card using the existing `NodeCard`, including "Test connection".
- Vardiff as one card per lane, titled by algorithm and port.
- The BTC fallback address alongside the LTC and DOGE ones.
- When `[coins.btc]` is absent, a "Bitcoin not configured" card with the TOML snippet to
  add. Endpoints and credentials stay in the file because the API has no authentication.

## Packaging decision
**One Umbrel app; the Bitcoin lane is optional and its node is not a dependency.** The
app keeps depending only on `longstreet-litecoin` and `longstreet-dogecoin`. An operator
who wants Bitcoin points the pool at a `bitcoind` of their choosing, typically elsewhere on
the LAN (for example the Bitcoin Node app on a second Umbrel), by writing `[coins.btc]` and
`[coins.btc.stratum]` into `data/alamo.local.toml`, which the daemon merges over
`alamo.toml` at startup. The compose file publishes 3334 for it; nothing listens there
until the lane exists. Why not the options considered before:

- Umbrel `dependencies` are mandatory, and an unset `${VAR}` stops the daemon, so
  `${APP_BITCOIN_*}` references in the shipped `alamo.toml` would make Bitcoin required.
  A required third node app (or a second "Alamo BTC" store app) is more than most
  operators want for an optional lane.
- A node on another device cannot come from any app's `exports.sh`: exports describe apps
  on the same Umbrel, on its internal network.
- So the endpoint and credentials must be the operator's, kept on the device and never in
  the store. Umbrel updates copy only a fixed list of files (`docker-compose.yml`,
  `umbrel-app.yml`, `exports.sh`, `*.template`, ...) and never touch `data/`. An edited
  `alamo.toml` happens to survive too, but it is the store's file (a later release could
  ship it as a template), and newer releases of it never reach an existing install. The
  data directory is the one place that is plainly the operator's and is already
  persisted, so the overlay lives there. See docs/OPERATING.md, "Bitcoin on Umbrel".
