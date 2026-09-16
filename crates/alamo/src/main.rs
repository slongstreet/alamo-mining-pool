//! The `alamo` daemon: load config, start the pool and web servers, run until signaled.

#![forbid(unsafe_code)]

use alamo::settings::LogSetup;
use alamo::Config;
use alamo_web::LogBuffer;
use anyhow::Context;
use clap::Parser;
use std::path::PathBuf;
use std::sync::Arc;
use tokio_util::sync::CancellationToken;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::{reload, EnvFilter};

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
    // The filter can be changed from the dashboard; the log buffer feeds its download.
    let filter = std::env::var(EnvFilter::DEFAULT_ENV).unwrap_or_else(|_| "info".to_string());
    let (filter_layer, filter_handle) =
        reload::Layer::new(EnvFilter::try_new(&filter).unwrap_or_else(|_| EnvFilter::new("info")));
    let logs = LogBuffer::default();
    tracing_subscriber::registry()
        .with(filter_layer)
        .with(tracing_subscriber::fmt::layer().with_target(false))
        .with(logs.layer())
        .init();
    let log = LogSetup {
        filter,
        control: Arc::new(move |directive: &str| {
            let filter = EnvFilter::try_new(directive).map_err(|e| e.to_string())?;
            filter_handle.reload(filter).map_err(|e| e.to_string())
        }),
    };

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
    let state = alamo_web::AppState::new(
        config.pool.name.clone(),
        config.stratum.listen.port(),
        store.clone(),
    );
    state.install_logs(logs);
    state.set_read_only(config.web.read_only);
    if config.web.read_only {
        tracing::info!("dashboard is read-only; settings and resets are refused");
    }

    let web = tokio::spawn(alamo_web::serve(
        config.web.clone(),
        state.clone(),
        shutdown.child_token(),
    ));
    let pool = tokio::spawn(alamo::pool::run(
        config,
        store,
        state,
        log,
        shutdown.child_token(),
    ));
    tokio::pin!(web, pool);

    let outcome = tokio::select! {
        signal = shutdown_signal() => {
            tracing::info!(signal, "shutting down");
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

/// Resolve to the name of the first termination signal received.
///
/// systemd and Docker stop the daemon with SIGTERM; a terminal sends SIGINT. Both must
/// run the same graceful path so the final accounting flush is not skipped.
#[cfg(unix)]
async fn shutdown_signal() -> &'static str {
    use tokio::signal::unix::{signal, SignalKind};

    let mut terminate = match signal(SignalKind::terminate()) {
        Ok(stream) => stream,
        Err(err) => {
            tracing::warn!(%err, "could not listen for SIGTERM; only SIGINT will stop the daemon");
            let _ = tokio::signal::ctrl_c().await;
            return "SIGINT";
        }
    };
    tokio::select! {
        _ = tokio::signal::ctrl_c() => "SIGINT",
        _ = terminate.recv() => "SIGTERM",
    }
}

#[cfg(not(unix))]
async fn shutdown_signal() -> &'static str {
    let _ = tokio::signal::ctrl_c().await;
    "ctrl-c"
}
