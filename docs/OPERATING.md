# Operating Alamo

How to run the pool for real: nodes, install, configuration, service management, backups,
monitoring, and what to do when something looks wrong. For how the code is put together
see [ARCHITECTURE.md](ARCHITECTURE.md).

## What you need

- A Litecoin Core node (0.21 or newer) with RPC enabled. Litecoin is the parent chain.
- Optionally a Dogecoin Core node (1.14.6 or newer) with RPC enabled, for merge mining.
  Dogecoin needs at least one peer before it will hand out block templates.
- Optionally a Bitcoin Core node with RPC enabled, fully synced, for Bitcoin solo mining
  on a lane of its own (see [Bitcoin](#bitcoin)). The regtest setup and CI use Bitcoin
  Core 32.1.
- A Linux box for the daemon: x86-64 or arm64, a few hundred megabytes of RAM, and a
  little disk for the SQLite database. A Raspberry Pi next to the nodes is fine.
- Miners that speak stratum v1 and hash scrypt (Litecoin lane) or SHA-256d (Bitcoin lane).

The pool never holds funds. Every block pays the miner that found it directly from the
coinbase, so there is nothing to withdraw and no wallet to back up on the pool side.

## Node settings

Add to `litecoin.conf`:

```ini
server=1
rpcuser=alamo
rpcpassword=<a long random string>
rpcbind=127.0.0.1
rpcallowip=127.0.0.1
zmqpubhashblock=tcp://127.0.0.1:28332
```

And to `dogecoin.conf`:

```ini
server=1
rpcuser=alamo
rpcpassword=<another long random string>
rpcbind=127.0.0.1
rpcallowip=127.0.0.1
zmqpubhashblock=tcp://127.0.0.1:28333
```

And, for a Bitcoin lane, to `bitcoin.conf`:

```ini
server=1
rpcuser=alamo
rpcpassword=<a third long random string>
rpcbind=127.0.0.1
rpcallowip=127.0.0.1
zmqpubhashblock=tcp://127.0.0.1:28334
```

The `zmqpubhashblock` lines are optional but recommended: the pool learns about new
blocks within milliseconds instead of on its next poll. If the daemon runs on another
host, bind RPC and ZMQ to an interface it can reach and restrict `rpcallowip`
accordingly. `txindex` is not required.

## Install

### From a release tarball

Each release ships `alamo-<version>-<target>.tar.gz` for `x86_64-unknown-linux-musl` and
`aarch64-unknown-linux-musl`. The binaries are static; they run on any Linux with a
kernel from the last decade, no shared libraries needed.

```bash
tar xzf alamo-*.tar.gz
cd alamo-*/
sudo install -m 0755 alamo /usr/local/bin/alamo
sudo useradd --system --home /var/lib/alamo --shell /usr/sbin/nologin alamo
sudo mkdir -p /etc/alamo /var/lib/alamo
sudo chown alamo:alamo /var/lib/alamo
sudo cp alamo.example.toml /etc/alamo/alamo.toml
sudo chmod 0640 /etc/alamo/alamo.toml && sudo chgrp alamo /etc/alamo/alamo.toml
sudo cp alamo.service /etc/systemd/system/alamo.service
```

Edit `/etc/alamo/alamo.toml` (next section), then:

```bash
sudo alamo --config /etc/alamo/alamo.toml --check
sudo systemctl daemon-reload
sudo systemctl enable --now alamo
sudo journalctl -u alamo -f
```

The unit runs as the `alamo` user with a read-only filesystem apart from `/var/lib/alamo`,
and restarts the daemon if it exits with an error. Stopping the service sends SIGTERM,
which flushes pending accounting before exit.

### With Docker

The image `ghcr.io/slongstreet/alamo-mining-pool` is published for `linux/amd64` and
`linux/arm64`. It runs as an unprivileged user, reads `/etc/alamo/alamo.toml`, and writes
its database to `/var/lib/alamo`.

```bash
cp config/alamo.example.toml alamo.toml    # edit it
docker compose -f deploy/docker-compose.yml up -d
docker compose -f deploy/docker-compose.yml logs -f
```

The compose file uses host networking so the container can reach nodes on
`127.0.0.1`. Without it, point `rpc_url` and `zmq_hashblock` at an address the container
can reach (`host.docker.internal` on Docker Desktop) and publish ports 3333 and 8080.

### On Umbrel

`deploy/umbrel/longstreet-alamo/` is a ready-made app for a community app store. It
depends on the `longstreet-litecoin` and `longstreet-dogecoin` node apps and reads their
RPC and ZMQ endpoints from the variables those apps export, so no credential is ever
written into the store. This directory is the source of truth; the store repository holds
a copy of it.

The Umbrel app version, the image tag it pins, and the workspace version are one number:
an Alamo release is an Umbrel app release, and a store-only fix gets a patch release of
Alamo. `deploy/umbrel/check-version.sh` runs in CI and fails when they drift; once the
release image is on ghcr.io it also checks the pinned digest is that image's. To publish
a release:

1. In one PR, bump `version` in `Cargo.toml` and `umbrel-app.yml`, the image tag in
   `docker-compose.yml`, and rewrite `releaseNotes`. The digest stays stale for now; CI
   only warns until the image exists.
2. Tag `vX.Y.Z` on main. The release workflow builds the binaries and pushes the image.
3. In a second PR, set the digest in `docker-compose.yml` to the multi-arch index digest
   (`docker buildx imagetools inspect ghcr.io/slongstreet/alamo-mining-pool:X.Y.Z`, or
   the check script's error message prints it). CI now verifies it.
4. Copy the directory over the store's copy and push the store. Umbrel only offers an
   update when `version` changes, and the store's CI rejects a push that touches the
   directory without bumping it.

To install:

1. Install the app from the Umbrel UI. Umbrel copies the directory to the app's data
   directory and sources the node apps' exports before starting the container.
2. Set the two fallback payout addresses on the dashboard's Settings page, or edit the
   `fallback_address` values in `alamo.toml` under the app's data directory and restart
   the app.
3. Point miners at the Umbrel's LAN address on port 3333. The dashboard opens from the
   Umbrel home screen.

The container runs as the `umbrel` user so it can write the bind-mounted data directory.

An app update copies only a fixed list of files from the store (`docker-compose.yml`,
`umbrel-app.yml`, `exports.sh`, `*.template`, `torrc`, `hooks`) and leaves the rest of
`~/umbrel/app-data/longstreet-alamo/` alone, so an edited `alamo.toml` is kept, and a
newer release's `alamo.toml` never reaches an existing install. The `data/` directory
(mounted at `/var/lib/alamo`, the pool's `data_dir`) holds the database and is never
touched by an update; that is where anything you add belongs.

#### Bitcoin on Umbrel

The app depends only on the Litecoin and Dogecoin node apps. The Bitcoin lane is optional
and points at a `bitcoind` you name, typically elsewhere on your network, so its address
and credentials come from you rather than from Umbrel:

1. Create `~/umbrel/app-data/longstreet-alamo/data/alamo.local.toml` (over SSH on the
   Umbrel that runs Alamo) with your node's values:

   ```toml
   [coins.btc]
   enabled = true
   rpc_url = "http://192.168.1.20:8332"
   rpc_user = "umbrel"
   rpc_password = "the node's RPC password"
   # Only if the node's ZMQ hashblock port is reachable from this Umbrel:
   # zmq_hashblock = "tcp://192.168.1.20:28334"
   # A Bitcoin address you control, paid when a miner's username is not one.
   fallback_address = "bc1q..."
   coinbase_tag = "/alamo/"

   [coins.btc.stratum]
   listen = "0.0.0.0:3334"
   ```

   The file holds a password, so `chmod 600` it. It must stay readable by the container's
   user, `1000:1000` (the `umbrel` user), so create it as that user or `chown 1000:1000` it.
2. Restart the app. The log says `applying local config overlay` with the file's path,
   and the dashboard shows a Bitcoin lane and a Bitcoin node card ("Test connection"
   checks the RPC details).
3. Point SHA-256 miners at the Umbrel's LAN address on port 3334, with a Bitcoin address
   as the username (see [Bitcoin](#bitcoin)).

To remove the lane, delete the file (or set `enabled = false` in it) and restart.

##### The Bitcoin Node app on another Umbrel

- **RPC details.** In the Bitcoin Node app, the **Connect** button opens "Connect to
  Bitcoin Node"; its **RPC Details** tab, in Local mode, shows the username (`umbrel`),
  password, host, and port (8332). The password is generated once when that app is
  installed. The host is shown as that Umbrel's `.local` name; with two Umbrels on the
  network both may be `umbrel.local`, so use the node Umbrel's LAN IP address in
  `rpc_url`.
- **RPC from the LAN.** The app publishes RPC port 8332 on its Umbrel's LAN address, but
  the `bitcoind` configuration it generates only allows RPC from Umbrel's internal app
  network and localhost (`rpcallowip`), and the Connect dialog warns that a client on the
  LAN must be allowed by hand. If "Test connection" on Alamo's Settings page is refused,
  add `rpcallowip=<LAN IP of the Umbrel running Alamo>` to the custom `bitcoin.conf` in
  the Bitcoin Node app's advanced settings and restart that app.
- **ZMQ.** The app does not publish its ZMQ ports (hashblock is 28334) outside its own
  Umbrel, so leave `zmq_hashblock` out. The pool then polls the node for a new tip twice
  a second (`poll_interval_ms`, default 500), which costs a fraction of a second per
  block at most.

### From source

```bash
cd web && npm ci && npm run build && cd ..
cargo build --release -p alamo
```

The dashboard is embedded at build time, so build `web/dist` first.

## Configuration

`config/alamo.example.toml` documents every setting. The ones that matter:

| Setting | Meaning |
| --- | --- |
| `pool.name` | Shown on the dashboard. |
| `pool.data_dir` | Where `alamo.db` lives. Must be writable. |
| `stratum.listen` | Where miners connect. `0.0.0.0:3333` to accept from the LAN. |
| `stratum.vardiff.*` | Share difficulty range and target share interval, in the units miners display: for scrypt a share of difficulty 65536 equals one unit of network difficulty, as in cgminer and every Litecoin pool; for SHA-256d a share of difficulty 1 is one unit. Optional: without the table a lane on `[stratum]` gets its algorithm's built-in values (scrypt 65536 / 1024 to 16777216; SHA-256d 1024 / 0.001 to 4294967296; both 10 s target, 60 s retarget, 30 % variance). |
| `coins.<coin>.stratum.listen`, `.vardiff.*` | A parent coin's own listener and vardiff, instead of `[stratum]`. Each parent coin is its own lane; two parents cannot share an address. Not allowed on a merge-mined coin. A lane with its own section and no vardiff table gets its algorithm's built-in values, never `[stratum.vardiff]`. |
| `web.listen` | Dashboard and API. Bind to `127.0.0.1` and put a reverse proxy in front if the box is reachable from the internet; the API has no authentication. |
| `web.read_only` | Refuse every change from the dashboard (settings, share-count reset, worker removal). |
| `coins.<coin>.rpc_url`, `rpc_user`, `rpc_password` | Node RPC. The password is never logged. |
| `[coins.<coin>]` keys | Case does not matter (`[coins.LTC]` is `ltc`); two keys that differ only by case are refused. |
| `${NAME}` in any value | Replaced with the environment variable `NAME` when the file is read; startup fails naming the variable if it is unset. Comment lines are not expanded. |
| `coins.<coin>.zmq_hashblock` | The node's `zmqpubhashblock` endpoint, `tcp://host:port`. |
| `coins.<coin>.fallback_address` | Paid when a miner's username is not a valid address. |
| `coins.<coin>.template_stale_secs` | How long a node may be unreachable before its template is withdrawn (default 120). |

`alamo --config alamo.toml --check` validates the file and exits.

### Local overlay: `alamo.local.toml`

After reading the main file the daemon looks for `alamo.local.toml` in the main file's
`pool.data_dir` and, if it exists, merges it over the main file before validating. This
keeps settings you add yourself, and the secrets that go with them, out of a file that
someone else ships: the Umbrel app's `alamo.toml` comes from the app store, and the data
directory is the part of an install that is yours. On Docker the file goes in the
directory mounted at `/var/lib/alamo`.

- Tables merge key by key, at every depth: an overlay with only `[coins.ltc]
  fallback_address = "..."` changes that one value. A whole new table, such as
  `[coins.btc]` with `[coins.btc.stratum]`, is added.
- Any other value, arrays included, replaces the main file's value whole.
- `${NAME}` is expanded in the overlay exactly as in the main file.
- The overlay may not set `pool.data_dir` (it is found through it); doing so is a startup
  error.
- A missing overlay is fine. A present one is logged once by path, never by contents; an
  unreadable or invalid one stops startup with an error naming the file. Errors in either
  file give the line and column, never the line's text, so a password cannot end up in
  the log.

Precedence, lowest first: the main file, then `alamo.local.toml`, then the values stored
from the Settings page. The Settings page's "Effective configuration" and "revert to the
file's value" both mean the merged file. `--check` validates the merged result.

### Settings page

The dashboard's Settings page (the link in the header) changes a few things without a
restart: the pool name, the coinbase tag, each coin's fallback payout address, the vardiff
settings, and the log level. It also shows each node's endpoint and health with a "Test
connection" button, downloads the recent log and a consistent copy of the database, and
resets share counts. Changes are stored in `alamo.db` and override the file; each one is
marked on the page and can be reverted to the file's value. Overrides are stored under
the config path they override: `stratum.vardiff.<field>` for lanes on `[stratum]`,
`coins.<coin>.stratum.vardiff.<field>` for a parent with its own section. A stored value
that no longer validates (say, after switching a node to another network) is dropped with
a warning in the log. Through the API (`GET`/`PUT /api/settings`) a lane's vardiff is
`lanes[].vardiff` in the document and `lanes.<key>.vardiff` in a patch; the top-level
`vardiff` that stood for the primary lane is gone after 0.1.14, and a patch that still
sends it, or any other unknown top-level key, is refused with 422. A coin's fallback address is checked once that coin's node has
answered, so until then its stored override is kept and the page does not list the coin;
other values are checked at startup.

Node endpoints and credentials are deliberately not editable there: the API has no
authentication, so anyone who can open the dashboard could otherwise repoint the pool at
a host of their choosing. If people who should not operate the pool can reach the
dashboard, set `[web] read_only = true`; every change from the page, including the
share-count reset, is then refused with 403.

### Miner setup

Miners use their payout address as the stratum username, with an optional worker name
after a dot: `ltc1q....rig1`. The Dogecoin address goes in the password field, bare or as
`doge=D...`. A miner that supplies no valid address is paid at the fallback address and
the dashboard flags it.

Any miner can ask for a starting difficulty with `d=<number>` in the password, alone or
next to the Dogecoin address (`doge=D...,d=4096` or `D... d=4096`). It is clamped to the
lane's `min_difficulty` and `max_difficulty`, vardiff takes over from there, and a value
that is not a positive number is ignored.

The scrypt vardiff defaults suit ASICs in the hundreds of MH/s to GH/s range. For a small
test rig, lower `initial_difficulty` and `min_difficulty`, or use `d=`.

### Bitcoin

Bitcoin is a lane of its own: its own port, vardiff, statistics, and blocks, next to (or
instead of) the Litecoin lane. Add `[coins.btc]` and `[coins.btc.stratum]` as in the
commented example in `config/alamo.example.toml` (in `alamo.toml`, or in
[`alamo.local.toml`](#local-overlay-alamolocaltoml); on Umbrel, see
[Bitcoin on Umbrel](#bitcoin-on-umbrel)), pointing at a `bitcoind` that:

- has RPC enabled (`server=1`, credentials, `rpcallowip` for the pool's host);
- is fully synced. The lane opens its port once the node answers RPC, but a syncing
  node refuses `getblocktemplate`, so miners get no work (and the dashboard shows the
  node's error) until it catches up; the Litecoin lane is not affected;
- ideally publishes `zmqpubhashblock`, so the pool switches to a new tip at once;
- is on mainnet, testnet (`test`/`testnet4`), or regtest. Signet is refused at connect
  with a message saying so: signet templates need a rule and a coinbase solution the
  pool does not provide. The lane fails on its own; other lanes keep running.

Miners connect to the Bitcoin lane's port (3334 in the example) with a Bitcoin address as
the username (`bc1q...`, `bc1p...` taproot, `1...`, or `3...`; plus `.worker` if you like)
and any password, optionally with `d=<number>`. Without a vardiff table the lane starts
new connections at difficulty 1024, lets vardiff go as low as 0.001 for NerdMiner-class
devices, and jumps a large ASIC to its measured difficulty within seconds.

The Bitcoin lane offers BIP310 version rolling (ASICBoost) over BIP320's bits, mask
`1fffe000`. A miner that sends `mining.configure` with `version-rolling` gets the mask it
asked for ANDed with that one; it may then roll those header version bits and send them
as the sixth `mining.submit` parameter. A share that changes any bit outside the
negotiated mask is rejected as "Invalid version bits" (code 20). A shared mask with fewer
bits than the miner's `version-rolling.min-bit-count` is still granted (BIP310 treats the
count as diagnostic) and logged at info level; only a miner whose mask shares no bits
with the lane's is refused, and it then hashes without rolling. Litecoin lanes keep answering `version-rolling: false`.

## Upgrading

Stop the service, replace the binary, start it. Database migrations run at startup and
are forward-only, so take a backup first if you might roll back. Shares, workers,
hashrate history, and blocks survive; miners reconnect on their own.

## Backups

Everything the pool remembers is in one SQLite file, `alamo.db` in `data_dir`, plus its
`-wal` and `-shm` sidecars while the daemon runs. Copy it while the daemon is running with
SQLite's own tool so the copy is consistent:

```bash
sqlite3 /var/lib/alamo/alamo.db ".backup '/var/backups/alamo-$(date +%F).db'"
```

Losing the database loses history and statistics, nothing else: no funds, no pending
payouts.

## Monitoring

- **Dashboard** at `web.listen`: hashrate, odds, rounds and luck, workers, live shares,
  blocks, and a badge per coin when its node is unreachable or its template is stale.
- **`GET /api/health`** returns `{"status":"ok"}` while the daemon runs. Suitable for a
  liveness probe.
- **`GET /api/status`** is the full status document as JSON. `lanes` has one entry per
  parent coin, primary lane first (the parent on `[stratum]`, else the first by key), with
  its `state` (`connecting`, `running`, or `failed` with an `error`), port, chain,
  hashrate, share counts, best share, coins, and workers; `blocks` lists every lane's
  blocks. The top-level `coins`, `hashrate`, share counts, `total_work`,
  `share_multiplier`, best share, `scoring_since`, and `workers` that mirrored the primary
  lane are gone after 0.1.14: read them from `lanes[0]` (or the lane by `key`).
- **`GET /api/hashrate`** and **`GET /api/shares`** read one lane's history: the primary
  lane's unless `?lane=<key>` names another. An unknown lane is a 400 that lists the
  lanes there are.
- **`POST /api/stats/reset`** zeroes share counts and best share for every worker of a
  lane (the dashboard's `reset` button). **`DELETE /api/workers/{name}`** forgets an
  offline worker of a lane: its row, share log, and hashrate history go; blocks it found
  stay, and its accepted work is retired into the lane total so rounds and luck do not
  move. A worker connected in that lane is refused with 409. Both take `?lane=<key>`
  (default the primary lane) and are applied by the pool task within one publish
  interval.
- **`GET /metrics`** is a Prometheus text endpoint. Every series about a lane carries a
  `lane` label (`lane="ltc"`), next to `worker` or `coin` where those apply;
  `alamo_lane_state{lane,state}` is 1 for the state each lane is in. Useful alerts:
  - `alamo_lane_state{state="failed"} == 1`: a lane could not start (see below).
  - `alamo_node_up == 0` for more than a minute: a node is down.
  - `alamo_node_stale == 1`: a node has been down long enough that its template was
    withdrawn.
  - `alamo_workers{state="connected"} == 0`: nobody is mining in that lane; use
    `sum(alamo_workers{state="connected"}) == 0` for nobody in the whole pool.
  - `alamo_node_zmq_connected == 0` where ZMQ is configured: notifications are off and
    the pool is polling every 10 seconds.
  - `sum by (lane) (increase(alamo_shares_total{result="rejected"}[10m]))` climbing:
    check miner clocks and network.

  Expressions that compare a pool-level series with a constant fire once per lane. Any
  that matched a series without labels before (`alamo_work_total`, `alamo_hashrate_
  hashes_per_second`, ...) needs `sum(...)` or a `lane` matcher now. Do not sum hashrate
  across lanes of different algorithms.

Logs go to stdout in a structured format. `RUST_LOG=debug` is verbose;
`RUST_LOG=info,alamo_coins::zmq=debug` shows each block notification.

## Signals and shutdown

SIGINT and SIGTERM both stop the daemon cleanly: the stratum listener closes, miners are
disconnected, pending share accounting is written to the database, and the process exits,
normally within a few milliseconds and at most after five seconds. systemd's default stop
timeout and Docker's `stop_grace_period` in the compose file both allow for that.

## When something looks wrong

**`node unreachable` at startup.** The daemon keeps retrying with backoff and opens that
coin's stratum listener as soon as its nodes answer, so this is normal while nodes boot.
Each parent coin starts on its own: one coin's node being down does not keep another
coin's miners out. If it never connects, check
`rpc_url`, `rpc_user`, `rpc_password`, and the node's `rpcallowip`.

**A lane shows `failed`.** The lane gave up starting because of a configuration
problem it found once its node answered: the node is on another network than its
fallback address is for, the address is not valid for that coin, or the lane's stratum
listener could not bind (address in use, or a port below 1024 without the privilege).
The reason is in the lane's `error` on the dashboard and in `/api/status`, and logged as
`lane failed to start`. The other lanes keep running; the daemon exits only if no lane is
left running or connecting, so a single-coin pool stops as before. Fix the config (or the
node) and restart the daemon; a stored fallback-address override for that coin stays in
the database and is checked again then.

**`template fetch failed` while running.** The node stopped answering. The pool keeps
serving the last job to miners. After `template_stale_secs` it withdraws the template: an
aux chain (Dogecoin) drops out of merged work until its node returns; for the parent chain
miners keep working the last job, since stratum has no way to pause them, and any block
they find on it will be rejected as stale by the node when it is back. Either way the pool
refetches a template as soon as the node answers and issues a clean job if the tip moved.

**Dogecoin refuses templates (`Dogecoin is not connected`).** Dogecoin Core needs at least
one peer, even on regtest. Wait for it to connect, or add a node with `addnode`.

**`zmq connection failed`.** Check the node's `zmqpubhashblock` setting and that the port
is reachable. The pool keeps polling in the meantime.

**Rejected shares.** A burst right after a new block is normal: those shares were for the
previous job. A steady rate points at a miner submitting duplicate or low-difficulty
shares, or a miner with a wrong clock.

**A block shows as rejected.** The node refused it. The log line carries the node's
reason; the usual one is `stale` or `inconclusive` when another block arrived first.
`orphaned` appears later if the chain reorganized past a block that was accepted.
A reason starting with `rpc:` means the node never gave a verdict: the pool retries an
unreachable node for several minutes before giving up. Whenever a block is not accepted,
its hex stays in `<data_dir>/blocks/<COIN>-<height>-<hash>.hex` (also if the daemon stops
while still retrying), so it can be handed to the node by hand while the tip has not
moved: `<coin>-cli submitblock "$(cat <file>)"`. The pool does not resubmit these files
itself and never deletes them; clear them out once they are of no use.

**Dashboard says `polling` instead of `live`.** The WebSocket at `/api/ws` could not be
kept open, typically because a reverse proxy does not forward upgrades. The dashboard
still works, refreshing every five seconds.

**High `template_age_seconds`.** The template is refreshed every `template_refresh_secs`
(default 30) or on every new block; an age far beyond that means the node is not
answering.
