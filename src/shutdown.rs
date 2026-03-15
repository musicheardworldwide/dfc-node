use tokio_util::sync::CancellationToken;
use tracing::info;

/// Wait for SIGTERM or SIGINT, then trigger graceful shutdown.
pub async fn wait_for_signal(cancel: CancellationToken) {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{signal, SignalKind};
        let mut sigterm = signal(SignalKind::terminate()).expect("Failed to register SIGTERM handler");
        let mut sigint = signal(SignalKind::interrupt()).expect("Failed to register SIGINT handler");

        tokio::select! {
            _ = sigterm.recv() => info!("Received SIGTERM"),
            _ = sigint.recv() => info!("Received SIGINT"),
        }
    }

    #[cfg(not(unix))]
    {
        tokio::signal::ctrl_c()
            .await
            .expect("Failed to register ctrl-c handler");
        info!("Received Ctrl+C");
    }

    info!("Initiating graceful shutdown...");
    cancel.cancel();
}
