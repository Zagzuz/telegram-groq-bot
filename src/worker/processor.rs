use std::{sync::Arc, time::Duration};

use anyhow::Context;

use crate::{
    config::Config,
    domain::{Job, WorkKind},
    groq::{
        Completion, GroqClient, GroqError, ModelChoice, ModelRouter, RouteDecision, estimate_tokens,
    },
    persistence::{Database, GeneratedAnswer},
    telegram::{TelegramClient, split_message},
};

use super::context::build_context;

#[derive(Clone)]
pub struct Processor {
    config: Arc<Config>,
    database: Database,
    telegram: TelegramClient,
    groq: GroqClient,
    router: ModelRouter,
}

impl Processor {
    pub fn new(
        config: Arc<Config>,
        database: Database,
        telegram: TelegramClient,
        groq: GroqClient,
        router: ModelRouter,
    ) -> Self {
        Self {
            config,
            database,
            telegram,
            groq,
            router,
        }
    }

    pub fn poll_interval(&self) -> Duration {
        Duration::from_millis(self.config.worker_poll_milliseconds)
    }

    pub async fn cleanup(&self) -> anyhow::Result<()> {
        self.database.cleanup().await
    }

    pub async fn process_next(&self) -> anyhow::Result<bool> {
        let Some(job) = self
            .database
            .claim_job(self.config.job_lease_seconds, self.config.max_job_attempts)
            .await?
        else {
            return Ok(false);
        };

        tracing::info!(
            update_id = job.update_id,
            kind = job.kind,
            attempt = job.attempts,
            "job claimed"
        );

        if let Err(error) = self.process(&job).await {
            tracing::warn!(
                update_id = job.update_id,
                chat_id = job.chat_id,
                attempt = job.attempts,
                %error,
                "job processing failed"
            );
            if job.attempts >= self.config.max_job_attempts {
                tracing::error!(
                    update_id = job.update_id,
                    "discarding job after maximum attempts"
                );
                self.database.complete_job(job.update_id).await?;
            } else {
                let delay = Duration::from_secs(
                    2_u64
                        .saturating_pow(job.attempts.clamp(1, 8) as u32)
                        .min(300),
                );
                self.database
                    .retry_job(job.update_id, delay, &error.to_string())
                    .await?;
            }
        }
        Ok(true)
    }

    async fn process(&self, job: &Job) -> anyhow::Result<()> {
        if job.answer.is_none() {
            match job.work_kind()? {
                WorkKind::Ask => {
                    if !self.answer_question(job).await? {
                        return Ok(());
                    }
                }
                WorkKind::AutoModel => self.handle_auto_model(job).await?,
                WorkKind::Reset => self.handle_reset(job).await?,
                WorkKind::Model => self.handle_model(job).await?,
                WorkKind::Help => self.save_local(job, help_text()).await?,
                WorkKind::Privacy => self.save_local(job, &self.privacy_text()).await?,
            }
        }

        let answer = if let Some(answer) = &job.answer {
            answer.clone()
        } else {
            self.current_answer(job.update_id).await?
        };
        self.deliver(job, &answer).await
    }

    async fn answer_question(&self, job: &Job) -> anyhow::Result<bool> {
        let Some(question) = job
            .input
            .as_deref()
            .filter(|value| !value.trim().is_empty())
        else {
            self.save_local(job, "Usage: /ask <question>").await?;
            return Ok(true);
        };

        let _ = self.telegram.send_typing(job.chat_id, job.thread_id).await;
        let history = self
            .database
            .load_context(
                job.chat_id,
                self.config.context_max_messages,
                self.config.context_retention_days,
            )
            .await?;
        let messages = build_context(
            &self.config.system_prompt,
            &history,
            question,
            self.config.context_max_chars,
        );
        let estimated_prompt_tokens = estimate_tokens(&messages);
        let auto_switch = self.database.auto_model_switch(job.chat_id).await?;
        let choice = match self
            .router
            .choose(auto_switch, estimated_prompt_tokens)
            .await?
        {
            RouteDecision::Use(choice) => choice,
            RouteDecision::Wait(delay) => {
                self.defer_with_notice(job, delay, "configured model capacity unavailable")
                    .await?;
                return Ok(false);
            }
        };

        tracing::info!(
            update_id = job.update_id,
            model = choice.model,
            fallback = choice.is_fallback,
            "model selected"
        );

        let (completion, used_choice) = match self.request(&choice, &messages).await {
            Ok(completion) => (completion, choice),
            Err(GroqError::RateLimited(headers)) => {
                let retry_after = headers.retry_after.unwrap_or(Duration::from_secs(10));
                self.router.observe(&choice.model, &headers, true).await;
                if auto_switch && !choice.is_fallback {
                    match self.router.choose_fallback(estimated_prompt_tokens).await? {
                        RouteDecision::Use(fallback) => {
                            match self.request(&fallback, &messages).await {
                                Ok(completion) => (completion, fallback),
                                Err(GroqError::RateLimited(headers)) => {
                                    let delay =
                                        headers.retry_after.unwrap_or(Duration::from_secs(10));
                                    self.router.observe(&fallback.model, &headers, true).await;
                                    self.defer_with_notice(job, delay, "fallback model rate limit")
                                        .await?;
                                    return Ok(false);
                                }
                                Err(error) => return Err(error.into()),
                            }
                        }
                        RouteDecision::Wait(delay) => {
                            self.defer_with_notice(
                                job,
                                delay,
                                "fallback model capacity unavailable",
                            )
                            .await?;
                            return Ok(false);
                        }
                    }
                } else {
                    self.defer_with_notice(job, retry_after, "primary model rate limit")
                        .await?;
                    return Ok(false);
                }
            }
            Err(error) => return Err(error.into()),
        };

        let switched_from = self
            .database
            .save_generated_answer(GeneratedAnswer {
                job,
                question,
                answer: &completion.content,
                model: &used_choice.model,
                prompt_tokens: completion.prompt_tokens,
                completion_tokens: completion.completion_tokens,
                max_messages: self.config.context_max_messages,
                max_chars: i64::try_from(self.config.context_max_chars).unwrap_or(i64::MAX),
                retention_days: self.config.context_retention_days,
            })
            .await?;
        if let Some(previous_model) = switched_from {
            tracing::info!(
                update_id = job.update_id,
                previous_model,
                model = used_choice.model,
                "chat model switched"
            );
        }
        Ok(true)
    }

    async fn request(
        &self,
        choice: &ModelChoice,
        messages: &[crate::groq::ChatMessage],
    ) -> Result<Completion, GroqError> {
        let started = std::time::Instant::now();
        let result = self
            .groq
            .complete(&choice.model, messages, choice.max_completion_tokens)
            .await;
        if let Ok(completion) = &result {
            self.router
                .observe(&choice.model, &completion.rate_headers, false)
                .await;
        }
        let elapsed_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
        match &result {
            Ok(_) => tracing::info!(model = choice.model, elapsed_ms, "Groq request completed"),
            Err(error) => tracing::warn!(
                model = choice.model,
                elapsed_ms,
                %error,
                "Groq request failed"
            ),
        }
        result
    }

    async fn defer_with_notice(
        &self,
        job: &Job,
        delay: Duration,
        reason: &'static str,
    ) -> anyhow::Result<()> {
        if !job.wait_notified {
            let notice = format!(
                "⏳ Groq is temporarily at capacity. Your question is queued. The next attempt will be in about {}.",
                describe_delay(delay)
            );
            self.telegram
                .send_message(job.chat_id, job.thread_id, Some(job.message_id), &notice)
                .await?;
            self.database.mark_wait_notified(job.update_id).await?;
        }

        tracing::warn!(
            update_id = job.update_id,
            delay_seconds = delay.as_secs(),
            reason,
            "job deferred"
        );
        self.database.defer_job(job.update_id, delay).await
    }

    async fn handle_auto_model(&self, job: &Job) -> anyhow::Result<()> {
        let current = self.database.auto_model_switch(job.chat_id).await?;
        let requested = job.input.as_deref().map(|input| input.to_ascii_lowercase());
        let next = match requested.as_deref() {
            None | Some("") | Some("status") => None,
            Some("on" | "enable" | "enabled" | "true") => Some(true),
            Some("off" | "disable" | "disabled" | "false") => Some(false),
            Some("toggle") => Some(!current),
            Some(_) => {
                return self
                    .save_local(job, "Usage: /automodel [on|off|toggle]")
                    .await;
            }
        };

        if next.is_some() && !self.can_administer(job).await? {
            return self
                .save_local(
                    job,
                    "Only a chat administrator can change automatic model switching.",
                )
                .await;
        }

        let enabled = if let Some(next) = next {
            self.database
                .set_auto_model_switch(job.chat_id, next)
                .await?;
            next
        } else {
            current
        };
        let state = if enabled { "ON" } else { "OFF" };
        self.save_local(
            job,
            &format!(
                "Automatic model switching is {state}.\nPrimary: {}\nFallback: {}",
                self.config.primary_model, self.config.fallback_model
            ),
        )
        .await
    }

    async fn handle_reset(&self, job: &Job) -> anyhow::Result<()> {
        if !self.can_administer(job).await? {
            return self
                .save_local(
                    job,
                    "Only a chat administrator can reset this chat's context.",
                )
                .await;
        }
        self.database.clear_context(job.chat_id).await?;
        self.save_local(
            job,
            "This chat's saved conversation context has been cleared.",
        )
        .await
    }

    async fn handle_model(&self, job: &Job) -> anyhow::Result<()> {
        let auto = self.database.auto_model_switch(job.chat_id).await?;
        self.save_local(
            job,
            &format!(
                "Primary model: {}\nFallback model: {}\nAutomatic switching: {}",
                self.config.primary_model,
                self.config.fallback_model,
                if auto { "ON" } else { "OFF" }
            ),
        )
        .await
    }

    async fn can_administer(&self, job: &Job) -> anyhow::Result<bool> {
        if job.chat_kind == "private" {
            return Ok(true);
        }
        let Some(user_id) = job.actor_user_id else {
            return Ok(false);
        };
        self.telegram.is_chat_admin(job.chat_id, user_id).await
    }

    async fn save_local(&self, job: &Job, answer: &str) -> anyhow::Result<()> {
        self.database.save_local_answer(job.update_id, answer).await
    }

    fn privacy_text(&self) -> String {
        format!(
            "The bot stores only this chat's most recent {} user/assistant messages, capped at {} characters and {} days, plus its automatic-switch setting and last model ID. It does not retain usernames or raw Telegram updates. Completed jobs are deleted, and update IDs expire after 48 hours. Use /reset to clear this chat's context immediately.",
            self.config.context_max_messages,
            self.config.context_max_chars,
            self.config.context_retention_days
        )
    }

    async fn current_answer(&self, update_id: i64) -> anyhow::Result<String> {
        self.database
            .job_answer(update_id)
            .await?
            .context("saved answer could not be reloaded")
    }

    async fn deliver(&self, job: &Job, answer: &str) -> anyhow::Result<()> {
        let chunks = split_message(answer, 4_096, 2);
        tracing::info!(
            update_id = job.update_id,
            answer_chars = answer.chars().count(),
            chunks_total = chunks.len(),
            "Telegram reply prepared"
        );
        let start = usize::try_from(job.sent_chunks)
            .unwrap_or(0)
            .min(chunks.len());
        for (index, chunk) in chunks.iter().enumerate().skip(start) {
            let started = std::time::Instant::now();
            self.telegram
                .send_message(
                    job.chat_id,
                    job.thread_id,
                    (index == 0).then_some(job.message_id),
                    chunk,
                )
                .await?;
            tracing::info!(
                update_id = job.update_id,
                chunk = index + 1,
                elapsed_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
                "Telegram reply chunk delivered"
            );
            self.database
                .mark_chunk_sent(job.update_id, i32::try_from(index + 1).unwrap_or(i32::MAX))
                .await?;
        }
        self.database.complete_job(job.update_id).await?;
        tracing::info!(update_id = job.update_id, "job completed");
        Ok(())
    }
}

fn describe_delay(delay: Duration) -> String {
    let seconds = delay.as_secs().max(1);
    if seconds < 60 {
        format!("{seconds} seconds")
    } else if seconds < 3_600 {
        let minutes = seconds.div_ceil(60);
        format!("{minutes} minute{}", if minutes == 1 { "" } else { "s" })
    } else {
        let hours = seconds.div_ceil(3_600);
        format!("{hours} hour{}", if hours == 1 { "" } else { "s" })
    }
}

fn help_text() -> &'static str {
    "Commands:\n/ask <question> — ask using this chat's context\n/automodel [on|off|toggle] — view or change adaptive model switching\n/model — show model settings\n/reset — clear this chat's context\n/privacy — show stored-data policy\n\nYou can also mention the bot or reply to one of its messages."
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn describes_retry_delays_for_users() {
        assert_eq!(describe_delay(Duration::from_secs(10)), "10 seconds");
        assert_eq!(describe_delay(Duration::from_secs(60)), "1 minute");
        assert_eq!(describe_delay(Duration::from_secs(3_601)), "2 hours");
    }
}
