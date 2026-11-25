//! The pool runtime: node connections, template sources, stratum server, block submission,
//! confirmation tracking, and the status snapshot for the dashboard.

mod confirm;
mod lane;
mod node;
mod publisher;
mod submit;

pub use confirm::track_confirmations;
pub use node::{connect_lane, ChainNode, Chains};
pub use submit::{submit_candidate, submit_candidate_spooled, SubmitOutcome, BLOCK_SPOOL_DIR};

use crate::config::Config;
use crate::settings::{LiveSettings, LogSetup};
use alamo_core::time::now_unix;
use alamo_store::Store;
use alamo_stratum::PoolEvent;
use alamo_web::AppState;
use anyhow::{bail, Context};
use lane::{run_lane, LaneNotice, LaneTask};
use publisher::{publisher, Publisher};
use std::sync::Arc;
use tokio::sync::mpsc;
use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;

/// Run the pool until `shutdown` is cancelled or a component stops.
///
/// Each enabled parent coin is a lane with its own nodes, stratum listener, vardiff,
/// payout tables, submitter, and trackers. Lanes start independently: one whose node is
/// unreachable keeps retrying without holding any other lane's listener closed.
///
/// A lane that cannot start (its node is on another network than the config implies, a
/// fallback address does not fit that network, its listener cannot bind) fails on its
/// own: it is logged, shown as failed, and the other lanes keep running. The daemon exits
/// with that error only when no lane is left running or connecting, so a single-lane
/// pool with a bad config stops as it always did. A component of a running lane that
/// stops unexpectedly still stops the daemon, so its supervisor restarts it.
pub async fn run(
    config: Config,
    store: Store,
    state: AppState,
    log: LogSetup,
    shutdown: CancellationToken,
) -> anyhow::Result<()> {
    let lanes = config.lanes();
    if lanes.is_empty() {
        bail!("no enabled parent coin configured");
    }
    let primary = Config::primary_lane_index(&lanes);
    state.set_primary_lane(&lanes[primary].key);
    // Settings stored in the database override the file; the pool tasks below watch
    // the channels this owns, so a change from the dashboard reaches them live.
    let settings = LiveSettings::start(&config, store.clone(), state.clone(), log)
        .await
        .context("loading settings")?;
    state.install_operator(settings.clone());
    // Worker, node, and lane transitions are read off the published snapshots; blocks
    // are reported by each lane's submitter. Neither path waits for a webhook.
    let notifier = {
        let names = state.clone();
        crate::notify::start(
            &config.notify,
            Arc::new(move || names.pool_name()),
            state.subscribe(),
            shutdown.child_token(),
        )?
    };

    let config = Arc::new(config);
    let spool = config.pool.data_dir.join(BLOCK_SPOOL_DIR);
    let mut tasks: JoinSet<anyhow::Result<TaskEnd>> = JoinSet::new();
    let (notice_tx, notice_rx) = mpsc::channel::<LaneNotice>(lanes.len());
    // One event channel per lane: which channel an event arrives on is its lane, so
    // nothing is added to the per-share path.
    let mut events = Vec::with_capacity(lanes.len());
    for (index, lane) in lanes.iter().enumerate() {
        let (events_tx, events_rx) = mpsc::channel::<PoolEvent>(4096);
        events.push(Some(events_rx));
        tasks.spawn(run_lane(LaneTask {
            index,
            lane: lane.clone(),
            config: config.clone(),
            settings: settings.clone(),
            store: store.clone(),
            spool: spool.clone(),
            events: events_tx,
            notices: notice_tx.clone(),
            notifier: notifier.clone(),
            shutdown: shutdown.child_token(),
        }));
    }
    drop(notice_tx);

    let commands = state
        .take_commands()
        .context("web command receiver already taken")?;
    let publisher_state = Publisher::load(store.clone(), &lanes, primary, now_unix()).await;
    let token = shutdown.child_token();
    tasks.spawn(async move {
        publisher(
            publisher_state,
            events,
            notice_rx,
            state,
            commands,
            token.clone(),
        )
        .await;
        if token.is_cancelled() {
            Ok(TaskEnd::Done)
        } else {
            bail!("publisher stopped unexpectedly")
        }
    });

    // Lanes still running or connecting.
    let mut alive = lanes.len();
    let result = loop {
        tokio::select! {
            _ = shutdown.cancelled() => break Ok(()),
            finished = tasks.join_next() => match finished {
                // A task that saw the shutdown.
                Some(Ok(Ok(TaskEnd::Done))) => continue,
                Some(Ok(Ok(TaskEnd::LaneFailed(err)))) => {
                    alive -= 1;
                    if alive == 0 {
                        break Err(err.context("no lane is left running"));
                    }
                }
                Some(Ok(Err(err))) => break Err(err),
                Some(Err(join)) => break Err(anyhow::Error::new(join).context("pool task panicked")),
                None => break Ok(()),
            },
        }
    };
    shutdown.cancel();
    while tasks.join_next().await.is_some() {}
    result
}

/// How a pool task ended without stopping the daemon.
enum TaskEnd {
    /// It saw the shutdown.
    Done,
    /// A lane could not start; the others carry on.
    LaneFailed(anyhow::Error),
}
