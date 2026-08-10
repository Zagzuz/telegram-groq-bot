use std::{net::SocketAddr, sync::Arc};

use anyhow::Context;

use crate::{
    config::Config,
    groq::{GroqClient, ModelRouter},
    http::{HttpState, router},
    persistence::Database,
    telegram::TelegramClient,
    worker::{self, Processor},
};

pub async fn serve(config: Config) -> anyhow::Result<()> {
    let config = Arc::new(config);
    let database = Database::connect(&config.database_url).await?;
    database.migrate().await?;

    let telegram = TelegramClient::new(&config.telegram_bot_token)?;
    if config.auto_register_webhook {
        let public_base_url = config
            .public_base_url
            .as_deref()
            .context("PUBLIC_BASE_URL is required when AUTO_REGISTER_WEBHOOK=true")?;
        telegram
            .set_webhook(public_base_url, &config.telegram_webhook_secret)
            .await
            .context("failed to register Telegram webhook at startup")?;
        tracing::info!("Telegram webhook registered at startup");
    }
    let groq = GroqClient::new(config.groq_api_key.clone())?;
    let model_router = ModelRouter::new(config.clone(), database.clone());
    let processor = Processor::new(
        config.clone(),
        database.clone(),
        telegram,
        groq,
        model_router,
    );

    let app = router(HttpState {
        config: config.clone(),
        database,
    });
    let address = SocketAddr::from(([0, 0, 0, 0], config.port));
    let listener = tokio::net::TcpListener::bind(address)
        .await
        .with_context(|| format!("failed to bind to {address}"))?;

    let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);
    let worker_handle = tokio::spawn(worker::run(processor, shutdown_rx));
    tracing::info!(%address, "service started");

    let server_result = axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal(shutdown_tx.clone()))
        .await;
    let _ = shutdown_tx.send(true);
    let _ = tokio::time::timeout(std::time::Duration::from_secs(5), worker_handle).await;
    server_result.context("HTTP server failed")
}

async fn shutdown_signal(shutdown_tx: tokio::sync::watch::Sender<bool>) {
    if let Err(error) = tokio::signal::ctrl_c().await {
        tracing::error!(%error, "failed to install shutdown signal handler");
    }
    tracing::info!("shutdown requested");
    let _ = shutdown_tx.send(true);
}
