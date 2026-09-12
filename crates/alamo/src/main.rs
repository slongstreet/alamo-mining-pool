//! The `alamo` daemon: load config, start the pool and web servers, run until signaled.

#![forbid(unsafe_code)]

use alamo::Config;
use anyhow::Context;
use clap::Parser;
use std::path::PathBuf;
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
    tracing::info!(pool = %config.pool.name, version = env!("CARGO_PKG_VERSION"), coins = ?enabled, "starting alamo");

    let store = alamo_store::Store::open(&config.database_path())
        .await
        .context("opening database")?;
    tracing::info!(blocks_found = store.block_count().await?, "loaded state");

    let shutdown = CancellationToken::new();
    let state = alamo_web::AppState::with_store(config.pool.name.clone(), store.clone());

    let web = tokio::spawn(alamo_web::serve(
        config.web.clone(),
        state.clone(),
        shutdown.child_token(),
    ));
    let pool = tokio::spawn(alamo::pool::run(
        config,
        store,
        state,
        shutdown.child_token(),
    ));
    tokio::pin!(web, pool);

    let outcome = tokio::select! {
        _ = tokio::signal::ctrl_c() => {
            tracing::info!("received ctrl-c, shutting down");
            Ok(())
        }
        res = &mut pool => match res {
            Ok(Ok(())) => Ok(()),
            Ok(Err(err)) => Err(err.context("pool failed")),
            Err(join) => Err(anyhow::Error::new(join).context("pool task panicked")),
        },
        res = &mut web => match res {
            Ok(Ok(())) => Err(anyhow::anyhow!("web server exited unexpectedly")),
            Ok(Err(err)) => Err(anyhow::Error::new(err).context("web server failed")),
            Err(join) => Err(anyhow::Error::new(join).context("web task panicked")),
        },
    };

    shutdown.cancel();
    let drain = async {
        if !web.is_finished() {
            let _ = (&mut web).await;
        }
        if !pool.is_finished() {
            let _ = (&mut pool).await;
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
