//! The `alamo` daemon: load config, start the stratum and web servers, run until signaled.

#![forbid(unsafe_code)]

mod config;

use anyhow::{bail, Context};
use clap::Parser;
use config::Config;
use std::path::PathBuf;
use tokio::task::{JoinError, JoinHandle};
use tokio_util::sync::CancellationToken;
use tracing_subscriber::EnvFilter;

/// Self-hosted solo mining pool.
#[derive(Parser, Debug)]
#[command(name = "alamo", version, about)]
struct Args {
    /// Path to the TOML configuration file.
    #[arg(short, long, env = "ALAMO_CONFIG", default_value = "alamo.toml")]
    config: PathBuf,

    /// Validate the configuration and exit.
    #[arg(long)]
    check: bool,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .with_target(false)
        .init();

    let args = Args::parse();
    let config = Config::load(&args.config)?;
    if args.check {
        println!("config ok: {}", args.config.display());
        return Ok(());
    }

    let enabled: Vec<&String> = config
        .coins
        .iter()
        .filter(|(_, c)| c.enabled)
        .map(|(k, _)| k)
        .collect();
    tracing::info!(
        pool = %config.pool.name,
        version = env!("CARGO_PKG_VERSION"),
        coins = ?enabled,
        "starting alamo"
    );

    let store = alamo_store::Store::open(&config.database_path())
        .await
        .context("opening database")?;
    tracing::info!(blocks_found = store.block_count().await?, "loaded state");

    let shutdown = CancellationToken::new();
    let state = alamo_web::AppState::new(config.pool.name.clone());

    let mut stratum = tokio::spawn(alamo_stratum::serve(
        config.stratum.clone(),
        shutdown.child_token(),
    ));
    let mut web = tokio::spawn(alamo_web::serve(
        config.web.clone(),
        state,
        shutdown.child_token(),
    ));

    let outcome = tokio::select! {
        _ = tokio::signal::ctrl_c() => {
            tracing::info!("received ctrl-c, shutting down");
            Ok(())
        }
        res = &mut stratum => exited(res, "stratum"),
        res = &mut web => exited(res, "web"),
    };

    shutdown.cancel();
    let drain = async {
        if !web.is_finished() {
            let _ = (&mut web).await;
        }
        if !stratum.is_finished() {
            let _ = (&mut stratum).await;
        }
    };
    if tokio::time::timeout(std::time::Duration::from_secs(5), drain)
        .await
        .is_err()
    {
        tracing::warn!("servers did not stop within 5s; exiting anyway");
    }

    outcome?;
    tracing::info!("bye");
    Ok(())
}

/// Turn a server task finishing before shutdown into an error that stops the daemon.
fn exited<E>(result: Result<Result<(), E>, JoinError>, name: &str) -> anyhow::Result<()>
where
    E: std::error::Error + Send + Sync + 'static,
{
    match result {
        Ok(Ok(())) => bail!("{name} server exited unexpectedly"),
        Ok(Err(err)) => Err(anyhow::Error::new(err).context(format!("{name} server failed"))),
        Err(err) => Err(anyhow::Error::new(err).context(format!("{name} task panicked"))),
    }
}

/// Unused for now; keeps the JoinHandle type in scope for future supervisors.
#[allow(dead_code)]
type ServerTask<E> = JoinHandle<Result<(), E>>;
