use std::sync::Arc;

use axum::{
    Json, Router,
    extract::{DefaultBodyLimit, State},
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
    routing::{get, post},
};
use tower_http::trace::TraceLayer;

use crate::{
    config::Config,
    persistence::Database,
    telegram::{Update, extract_work_item},
};

#[derive(Clone)]
pub struct HttpState {
    pub config: Arc<Config>,
    pub database: Database,
}

pub fn router(state: HttpState) -> Router {
    Router::new()
        .route("/healthz", get(health))
        .route("/readyz", get(ready))
        .route("/telegram/webhook", post(webhook))
        .layer(DefaultBodyLimit::max(1024 * 1024))
        .layer(TraceLayer::new_for_http())
        .with_state(state)
}

async fn health() -> &'static str {
    "ok"
}

async fn ready(State(state): State<HttpState>) -> impl IntoResponse {
    if state.database.ready().await {
        (StatusCode::OK, "ready")
    } else {
        (StatusCode::SERVICE_UNAVAILABLE, "database unavailable")
    }
}

async fn webhook(
    State(state): State<HttpState>,
    headers: HeaderMap,
    Json(update): Json<Update>,
) -> impl IntoResponse {
    let supplied_secret = headers
        .get("x-telegram-bot-api-secret-token")
        .and_then(|value| value.to_str().ok());
    if supplied_secret != Some(state.config.telegram_webhook_secret.as_str()) {
        return StatusCode::UNAUTHORIZED;
    }

    let Some(item) = extract_work_item(&update, &state.config.telegram_bot_username) else {
        return StatusCode::OK;
    };
    match state.database.enqueue(&item).await {
        Ok(inserted) => {
            if inserted {
                tracing::info!(
                    update_id = item.update_id,
                    kind = item.kind.as_str(),
                    "Telegram job queued"
                );
            } else {
                tracing::debug!(
                    update_id = item.update_id,
                    "duplicate Telegram update ignored"
                );
            }
            StatusCode::OK
        }
        Err(error) => {
            tracing::error!(
                update_id = item.update_id,
                chat_id = item.chat_id,
                %error,
                "failed to persist Telegram update"
            );
            StatusCode::INTERNAL_SERVER_ERROR
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn health_is_static() {
        assert_eq!(health().await, "ok");
    }
}
