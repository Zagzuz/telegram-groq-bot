mod app;
mod config;
mod domain;
mod groq;
mod http;
mod persistence;
mod telegram;
mod telemetry;
mod worker;

use anyhow::Context;
use clap::{Parser, Subcommand};
use config::Config;
use persistence::Database;
use telegram::TelegramClient;

#[derive(Debug, Parser)]
#[command(author, version, about)]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Run database migrations and start the HTTP service and job worker.
    Serve,
    /// Run database migrations and exit.
    Migrate,
    /// Register the configured public URL as the Telegram webhook.
    SetWebhook,
    /// Remove the Telegram webhook.
    DeleteWebhook,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    dotenvy::dotenv().ok();
    telemetry::init();

    let cli = Cli::parse();
    let config = Config::from_env().context("invalid configuration")?;

    match cli.command.unwrap_or(Command::Serve) {
        Command::Serve => app::serve(config).await,
        Command::Migrate => {
            let database = Database::connect(&config.database_url).await?;
            database.migrate().await?;
            tracing::info!("database migrations completed");
            Ok(())
        }
        Command::SetWebhook => {
            let public_base_url = config
                .public_base_url
                .as_deref()
                .context("PUBLIC_BASE_URL is required to set the webhook")?;
            let client = TelegramClient::new(&config.telegram_bot_token)?;
            client
                .set_webhook(public_base_url, &config.telegram_webhook_secret)
                .await?;
            tracing::info!("Telegram webhook registered");
            Ok(())
        }
        Command::DeleteWebhook => {
            let client = TelegramClient::new(&config.telegram_bot_token)?;
            client.delete_webhook().await?;
            tracing::info!("Telegram webhook removed");
            Ok(())
        }
    }
}
