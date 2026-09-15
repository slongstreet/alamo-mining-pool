# Operating Alamo

How to run the pool for real: nodes, install, configuration, service management, backups,
monitoring, and what to do when something looks wrong. For how the code is put together
see [ARCHITECTURE.md](ARCHITECTURE.md).

## What you need

- A Litecoin Core node (0.21 or newer) with RPC enabled. Litecoin is the parent chain.
- Optionally a Dogecoin Core node (1.14.6 or newer) with RPC enabled, for merge mining.
  Dogecoin needs at least one peer before it will hand out block templates.
- A Linux box for the daemon: x86-64 or arm64, a few hundred megabytes of RAM, and a
  little disk for the SQLite database. A Raspberry Pi next to the nodes is fine.
- Miners that speak stratum v1 and hash scrypt.

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
a copy of it. To publish a release:

1. Update the image tag and digest in `docker-compose.yml` to the new release. The digest
   is the multi-arch index digest, which `docker buildx imagetools inspect` prints.
2. Bump `version` in `umbrel-app.yml` and rewrite `releaseNotes`. This is the Umbrel app
   version, independent of the Alamo release: Umbrel only offers an update when it changes,
   and the store's CI rejects a push that touches the directory without bumping it.
3. Copy the directory over the store's copy and push the store.

To install:

1. Install the app from the Umbrel UI. Umbrel copies the directory to the app's data
   directory and sources the node apps' exports before starting the container.
3. Edit the two `fallback_address` values in `alamo.toml` under the app's data directory
   and restart the app.
4. Point miners at the Umbrel's LAN address on port 3333. The dashboard opens from the
   Umbrel home screen.

The container runs as the `umbrel` user so it can write the bind-mounted data directory.

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
| `stratum.vardiff.*` | Share difficulty range and target share interval, in the units miners display: for scrypt a share of difficulty 65536 equals one unit of network difficulty, as in cgminer and every Litecoin pool. |
| `web.listen` | Dashboard and API. Bind to `127.0.0.1` and put a reverse proxy in front if the box is reachable from the internet; the API has no authentication. |
| `coins.<coin>.rpc_url`, `rpc_user`, `rpc_password` | Node RPC. The password is never logged. |
| `${NAME}` in any value | Replaced with the environment variable `NAME` when the file is read; startup fails naming the variable if it is unset. Comment lines are not expanded. |
| `coins.<coin>.zmq_hashblock` | The node's `zmqpubhashblock` endpoint, `tcp://host:port`. |
| `coins.<coin>.fallback_address` | Paid when a miner's username is not a valid address. |
| `coins.<coin>.template_stale_secs` | How long a node may be unreachable before its template is withdrawn (default 120). |

`alamo --config alamo.toml --check` validates the file and exits.

### Miner setup

Miners use their payout address as the stratum username, with an optional worker name
after a dot: `ltc1q....rig1`. The Dogecoin address goes in the password field, bare or as
`doge=D...`. A miner that supplies no valid address is paid at the fallback address and
the dashboard flags it.

Vardiff defaults suit ASICs in the hundreds of MH/s to GH/s range. For a small test rig,
lower `initial_difficulty` and `min_difficulty`.

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
- **`GET /api/status`** is the full status document as JSON.
- **`GET /metrics`** is a Prometheus text endpoint. Useful alerts:
  - `alamo_node_up == 0` for more than a minute: a node is down.
  - `alamo_node_stale == 1`: a node has been down long enough that its template was
    withdrawn.
  - `alamo_workers{state="connected"} == 0`: nobody is mining.
  - `alamo_node_zmq_connected == 0` where ZMQ is configured: notifications are off and
    the pool is polling every 10 seconds.
  - `increase(alamo_shares_total{result="rejected"}[10m])` climbing: check miner clocks
    and network.

Logs go to stdout in a structured format. `RUST_LOG=debug` is verbose;
`RUST_LOG=info,alamo_coins::zmq=debug` shows each block notification.

## Signals and shutdown

SIGINT and SIGTERM both stop the daemon cleanly: the stratum listener closes, miners are
disconnected, pending share accounting is written to the database, and the process exits,
normally within a few milliseconds and at most after five seconds. systemd's default stop
timeout and Docker's `stop_grace_period` in the compose file both allow for that.

## When something looks wrong

**`node unreachable` at startup.** The daemon keeps retrying with backoff and starts as
soon as the node answers, so this is normal while nodes boot. If it never connects, check
`rpc_url`, `rpc_user`, `rpc_password`, and the node's `rpcallowip`.

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

**Dashboard says `polling` instead of `live`.** The WebSocket at `/api/ws` could not be
kept open, typically because a reverse proxy does not forward upgrades. The dashboard
still works, refreshing every five seconds.

**High `template_age_seconds`.** The template is refreshed every `template_refresh_secs`
(default 30) or on every new block; an age far beyond that means the node is not
answering.
