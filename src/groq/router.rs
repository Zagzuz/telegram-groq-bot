use std::{collections::HashMap, sync::Arc, time::Duration};

use tokio::sync::RwLock;

use crate::{config::Config, persistence::Database};

use super::{ChatMessage, RateHeaders};

#[derive(Clone, Debug)]
pub struct ModelChoice {
    pub model: String,
    pub max_completion_tokens: u32,
    pub is_fallback: bool,
}

#[derive(Clone, Debug)]
pub enum RouteDecision {
    Use(ModelChoice),
    Wait(Duration),
}

#[derive(Clone)]
pub struct ModelRouter {
    config: Arc<Config>,
    database: Database,
    snapshots: Arc<RwLock<HashMap<String, Snapshot>>>,
}

#[derive(Clone, Debug, Default)]
struct Snapshot {
    limit_requests: Option<i64>,
    remaining_requests: Option<i64>,
    limit_tokens: Option<i64>,
    remaining_tokens: Option<i64>,
    cooldown_until: Option<tokio::time::Instant>,
}

impl ModelRouter {
    pub fn new(config: Arc<Config>, database: Database) -> Self {
        Self {
            config,
            database,
            snapshots: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    pub async fn choose(
        &self,
        auto_switch: bool,
        estimated_prompt_tokens: i64,
    ) -> anyhow::Result<RouteDecision> {
        let primary = self.primary_choice();
        if !auto_switch {
            return Ok(RouteDecision::Use(primary));
        }

        let estimated = estimated_prompt_tokens + i64::from(primary.max_completion_tokens);
        if self
            .has_capacity(
                &primary.model,
                estimated,
                self.config.primary_daily_token_budget,
                self.config.primary_daily_request_budget,
                i64::from(self.config.primary_daily_switch_percent),
                true,
            )
            .await?
        {
            return Ok(RouteDecision::Use(primary));
        }

        self.choose_fallback(estimated_prompt_tokens).await
    }

    pub async fn choose_fallback(
        &self,
        estimated_prompt_tokens: i64,
    ) -> anyhow::Result<RouteDecision> {
        let fallback = self.fallback_choice();
        let estimated = estimated_prompt_tokens + i64::from(fallback.max_completion_tokens);
        if self
            .has_capacity(
                &fallback.model,
                estimated,
                self.config.fallback_daily_token_budget,
                self.config.fallback_daily_request_budget,
                98,
                false,
            )
            .await?
        {
            Ok(RouteDecision::Use(fallback))
        } else {
            Ok(RouteDecision::Wait(Duration::from_secs(60)))
        }
    }

    pub async fn observe(&self, model: &str, headers: &RateHeaders, rate_limited: bool) {
        let mut snapshots = self.snapshots.write().await;
        let snapshot = snapshots.entry(model.to_owned()).or_default();
        snapshot.limit_requests = headers.limit_requests.or(snapshot.limit_requests);
        snapshot.remaining_requests = headers.remaining_requests.or(snapshot.remaining_requests);
        snapshot.limit_tokens = headers.limit_tokens.or(snapshot.limit_tokens);
        snapshot.remaining_tokens = headers.remaining_tokens.or(snapshot.remaining_tokens);
        if rate_limited {
            snapshot.cooldown_until = Some(
                tokio::time::Instant::now()
                    + headers.retry_after.unwrap_or(Duration::from_secs(10)),
            );
        }
    }

    pub fn primary_choice(&self) -> ModelChoice {
        ModelChoice {
            model: self.config.primary_model.clone(),
            max_completion_tokens: self.config.primary_answer_max_tokens,
            is_fallback: false,
        }
    }

    fn fallback_choice(&self) -> ModelChoice {
        ModelChoice {
            model: self.config.fallback_model.clone(),
            max_completion_tokens: self.config.fallback_answer_max_tokens,
            is_fallback: true,
        }
    }

    async fn has_capacity(
        &self,
        model: &str,
        estimated_tokens: i64,
        daily_budget: i64,
        daily_request_budget: i64,
        daily_threshold_percent: i64,
        reserve_requests: bool,
    ) -> anyhow::Result<bool> {
        let usage = self.database.daily_usage(model).await?;
        if (usage.total_tokens() + estimated_tokens) * 100 >= daily_budget * daily_threshold_percent
        {
            return Ok(false);
        }
        if (usage.requests + 1) * 100 >= daily_request_budget * daily_threshold_percent {
            return Ok(false);
        }

        let snapshots = self.snapshots.read().await;
        let Some(snapshot) = snapshots.get(model) else {
            return Ok(true);
        };

        if snapshot
            .cooldown_until
            .is_some_and(|until| until > tokio::time::Instant::now())
        {
            return Ok(false);
        }

        if let (Some(limit), Some(remaining)) = (snapshot.limit_tokens, snapshot.remaining_tokens) {
            let reserve = limit * i64::from(self.config.rate_reserve_percent) / 100;
            if estimated_tokens > remaining.saturating_sub(reserve) {
                return Ok(false);
            }
        }

        if let (Some(limit), Some(remaining)) =
            (snapshot.limit_requests, snapshot.remaining_requests)
        {
            let request_reserve = if reserve_requests { limit / 10 } else { 0 };
            if remaining <= request_reserve {
                return Ok(false);
            }
        }
        Ok(true)
    }
}

pub fn estimate_tokens(messages: &[ChatMessage]) -> i64 {
    messages
        .iter()
        .map(|message| {
            let chars = i64::try_from(message.content.chars().count()).unwrap_or(i64::MAX);
            let bytes = i64::try_from(message.content.len()).unwrap_or(i64::MAX);
            chars.max((bytes + 3) / 4) + 8
        })
        .sum()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::Role;

    #[test]
    fn token_estimate_is_conservative_for_multibyte_text() {
        let messages = vec![ChatMessage {
            role: Role::User,
            content: "日本語".repeat(10),
        }];
        assert!(estimate_tokens(&messages) >= 38);
    }
}
