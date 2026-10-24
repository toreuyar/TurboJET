mod access;
pub mod auth;
pub mod delivery;
pub mod diagnostics;
pub mod handlers;
pub mod log_handlers;
pub mod logging;
pub mod secrets;
pub mod ssh_validation;

// Re-export commonly used types
pub use handlers::AppState;

use anyhow::Result;
use tj_config::ConfigLoader;
use tj_manager::spawn_manager;
use tokio::signal::unix::{SignalKind, signal};

pub async fn run(config_path: &str) -> Result<()> {
    let path = std::path::Path::new(config_path);
    let config = ConfigLoader::from_file(path)?;
    let sys = config.config();
    let listen_addr = sys.listen_addr.clone();
    // Hold process ownership for the whole server lifetime. `tj-ops restore`
    // checks the same lock and refuses to swap a live database/artifact tree.
    let _database_lock = tj_db::acquire_for_database(&sys.database_path)?;

    let spawn_result = spawn_manager(config).await?;
    let manager = spawn_result.handle.clone();

    let app = handlers::build_app(spawn_result).await?;

    let listener = tokio::net::TcpListener::bind(&listen_addr).await?;
    tracing::info!("TurboJet listening on {}", listen_addr);

    let mut sigterm = signal(SignalKind::terminate())?;
    let shutdown_signal = async move {
        tokio::select! {
            _ = sigterm.recv() => {},
            _ = tokio::signal::ctrl_c() => {},
        }
        tracing::info!("Received shutdown signal, stopping active executions");
        if let Err(error) = manager.shutdown().await {
            tracing::error!(%error, "Execution shutdown incomplete; affected workspaces preserved");
        }
    };

    let result = axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal)
        .await;

    match &result {
        Ok(()) => tracing::info!("TurboJet server shut down cleanly"),
        Err(e) => tracing::error!(error = %e, "TurboJet server error"),
    }
    result?;
    Ok(())
}
