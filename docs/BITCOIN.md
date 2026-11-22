# Bitcoin solo mining — design

Status: proposed (Wave 7 in [ROADMAP.md](ROADMAP.md)). Nothing here is implemented yet.

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
- **No version rolling.** `mining.configure` answers `version-rolling: false`
  (`crates/alamo-stratum/src/session.rs`). Modern SHA-256 ASICs expect BIP310.
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
initial_difficulty = 2048
min_difficulty = 0.001
max_difficulty = 1073741824
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
`crates/alamo-coins/src/btc.rs`:

| Property | Value |
| --- | --- |
| Symbol / name | `BTC` / Bitcoin |
| Algorithm | `Sha256d` |
| Aux chain id | none |
| Template rules | `["segwit"]` |
| Coinbase maturity | 100 |
| Addresses, main | P2PKH 0, P2SH 5, hrp `bc` |
| Addresses, test | P2PKH 111, P2SH 196, hrp `tb` |
| Addresses, regtest | P2PKH 111, P2SH 196, hrp `bcrt` |

Signet needs the extra `signet` template rule and is left out of the first cut.

### 4. BIP310 version rolling (SHA-256 lanes only)
- `mining.configure` answers `version-rolling: true` with
  `version-rolling.mask = miner_mask & 0x1fffe000`. Scrypt lanes keep answering `false`.
- `mining.submit` takes the optional sixth parameter (version bits). Bits outside the
  negotiated mask reject the share; the header version is
  `(job_version & !mask) | (bits & mask)`.
- The per-job duplicate key gains the version, and the rolled version goes into the
  assembled block.

### 5. Vardiff
- Fast start is done (prework above).
- Honour `d=<n>` in the stratum password as a starting difficulty. The miner setup hint
  already mentions it; the code currently ignores it.
- Fractional minimums already work in `Target::from_difficulty`, so very small devices
  (NerdMiner class) are served by a low `min_difficulty`.

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

- Known-answer tests in `alamo-core` against real mainnet data: a segwit block's header
  hash, merkle root, and coinbase txid, plus a version-rolled header.
- Regtest: add `bitcoind` to `deploy/regtest/docker-compose.yml` and a test that mines a
  block through the stratum path with a rolled version and a segwit transaction in the
  template, then checks the coinbase pays the worker's address. CI runs it alongside the
  existing LTC/DOGE test.

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

## Open decision
**Umbrel packaging.** The app's `dependencies` are mandatory and an unset `${VAR}` stops
the daemon, so one app cannot make Bitcoin optional. Options:

1. A second store app, "Alamo BTC": the same image with a BTC-only config on port 3334,
   depending only on the Bitcoin node app. (Recommended.)
2. One app that requires the Litecoin, Dogecoin, and Bitcoin node apps.
