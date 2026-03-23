use crate::error::Result;
use crate::handler::ServiceHandler;
use futures::Future;
use std::sync::Arc;

pub struct GracefulShutdown {
    shutdown_tx: tokio::sync::broadcast::Sender<()>,
    shutdown_rx: Arc<tokio::sync::Mutex<tokio::sync::broadcast::Receiver<()>>>,
}

impl GracefulShutdown {
    pub fn new() -> Self {
        let (shutdown_tx, shutdown_rx) = tokio::sync::broadcast::channel(1);
        Self {
            shutdown_tx,
            shutdown_rx: Arc::new(tokio::sync::Mutex::new(shutdown_rx)),
        }
    }

    pub fn shutdown_tx(&self) -> tokio::sync::broadcast::Sender<()> {
        self.shutdown_tx.clone()
    }

    pub async fn wait(&self) {
        let mut rx = self.shutdown_rx.lock().await;
        let _ = rx.recv().await;
    }

    pub fn trigger(&self) {
        let _ = self.shutdown_tx.send(());
    }
}

impl Default for GracefulShutdown {
    fn default() -> Self {
        Self::new()
    }
}

pub async fn shutdown_signal() {
    tokio::signal::ctrl_c()
        .await
        .expect("failed to install CTRL+C handler");
}

pub async fn run_with_graceful_shutdown<F, Fut, H>(_handler: Arc<H>, future: F) -> Result<()>
where
    F: Future<Output = Result<()>>,
    H: ServiceHandler + 'static,
{
    let graceful = GracefulShutdown::new();
    let shutdown_rx = graceful.shutdown_rx.clone();

    let server = async {
        if let Err(e) = future.await {
            tracing::error!("Server error: {}", e);
        }
    };

    tokio::select! {
        _ = server => {}
        _ = graceful.wait() => {
            tracing::info!("Shutdown signal received, closing connections...");
        }
        _ = shutdown_signal() => {
            tracing::info!("Ctrl+C received, initiating graceful shutdown...");
            graceful.trigger();
            let _ = shutdown_rx.lock().await.recv().await;
        }
    }

    Ok(())
}
