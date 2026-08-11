use std::time::Duration;

use anyhow::Context;
use chrono::Utc;
use sqlx::{PgPool, postgres::PgPoolOptions};

use crate::domain::{ConversationMessage, Job, WorkItem};

#[derive(Clone)]
pub struct Database {
    pool: PgPool,
}

#[derive(Clone, Copy, Debug, Default, sqlx::FromRow)]
pub struct DailyUsage {
    pub requests: i64,
    pub prompt_tokens: i64,
    pub completion_tokens: i64,
}

pub struct GeneratedAnswer<'a> {
    pub job: &'a Job,
    pub question: &'a str,
    pub answer: &'a str,
    pub model: &'a str,
    pub prompt_tokens: i64,
    pub completion_tokens: i64,
    pub max_messages: i64,
    pub max_chars: i64,
    pub retention_days: i64,
}

impl DailyUsage {
    pub fn total_tokens(self) -> i64 {
        self.prompt_tokens + self.completion_tokens
    }
}

impl Database {
    pub async fn connect(url: &str) -> anyhow::Result<Self> {
        let pool = PgPoolOptions::new()
            .max_connections(5)
            .min_connections(0)
            .acquire_timeout(Duration::from_secs(10))
            .connect(url)
            .await
            .context("failed to connect to PostgreSQL")?;
        Ok(Self { pool })
    }

    pub async fn migrate(&self) -> anyhow::Result<()> {
        sqlx::migrate!().run(&self.pool).await?;
        Ok(())
    }

    pub async fn ready(&self) -> bool {
        sqlx::query_scalar::<_, i32>("SELECT 1")
            .fetch_one(&self.pool)
            .await
            .is_ok()
    }

    /// Returns false when Telegram has already delivered this update.
    pub async fn enqueue(&self, item: &WorkItem) -> anyhow::Result<bool> {
        let mut tx = self.pool.begin().await?;
        let inserted = sqlx::query_scalar::<_, i64>(
            "INSERT INTO processed_updates (update_id) VALUES ($1) \
             ON CONFLICT DO NOTHING RETURNING update_id",
        )
        .bind(item.update_id)
        .fetch_optional(&mut *tx)
        .await?;

        if inserted.is_none() {
            tx.rollback().await?;
            return Ok(false);
        }

        sqlx::query(
            "INSERT INTO jobs \
             (update_id, chat_id, chat_kind, actor_user_id, message_id, thread_id, kind, input) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8)",
        )
        .bind(item.update_id)
        .bind(item.chat_id)
        .bind(&item.chat_kind)
        .bind(item.actor_user_id)
        .bind(item.message_id)
        .bind(item.thread_id)
        .bind(item.kind.as_str())
        .bind(&item.input)
        .execute(&mut *tx)
        .await?;

        tx.commit().await?;
        Ok(true)
    }

    pub async fn claim_job(
        &self,
        lease_seconds: i64,
        max_attempts: i32,
    ) -> anyhow::Result<Option<Job>> {
        let job = sqlx::query_as::<_, Job>(
            "WITH candidate AS (\
                SELECT j.update_id \
                FROM jobs j \
                WHERE (\
                    (j.status = 'pending' AND j.available_at <= NOW()) \
                    OR (j.status = 'processing' AND j.lease_until <= NOW())\
                ) \
                AND j.attempts < $2 \
                AND NOT EXISTS (\
                    SELECT 1 FROM jobs active \
                    WHERE active.chat_id = j.chat_id \
                      AND active.status = 'processing' \
                      AND active.lease_until > NOW()\
                ) \
                ORDER BY j.created_at, j.update_id \
                FOR UPDATE SKIP LOCKED \
                LIMIT 1\
             ) \
             UPDATE jobs j \
             SET status = 'processing', \
                 attempts = j.attempts + 1, \
                 lease_until = NOW() + ($1::BIGINT * INTERVAL '1 second'), \
                 updated_at = NOW() \
             FROM candidate c \
             WHERE j.update_id = c.update_id \
             RETURNING j.update_id, j.chat_id, j.chat_kind, j.actor_user_id, \
                       j.message_id, j.thread_id, j.kind, j.input, j.answer, \
                       j.sent_chunks, j.wait_notified, j.attempts",
        )
        .bind(lease_seconds)
        .bind(max_attempts)
        .fetch_optional(&self.pool)
        .await?;
        Ok(job)
    }

    pub async fn load_context(
        &self,
        chat_id: i64,
        max_messages: i64,
        retention_days: i64,
    ) -> anyhow::Result<Vec<ConversationMessage>> {
        let mut messages = sqlx::query_as::<_, ConversationMessage>(
            "SELECT role, content \
             FROM messages \
             WHERE chat_id = $1 \
               AND created_at >= NOW() - ($3::BIGINT * INTERVAL '1 day') \
             ORDER BY created_at DESC, id DESC \
             LIMIT $2",
        )
        .bind(chat_id)
        .bind(max_messages)
        .bind(retention_days)
        .fetch_all(&self.pool)
        .await?;
        messages.reverse();
        Ok(messages)
    }

    pub async fn save_generated_answer(
        &self,
        generated: GeneratedAnswer<'_>,
    ) -> anyhow::Result<Option<String>> {
        let GeneratedAnswer {
            job,
            question,
            answer,
            model,
            prompt_tokens,
            completion_tokens,
            max_messages,
            max_chars,
            retention_days,
        } = generated;
        let mut tx = self.pool.begin().await?;
        let previous_model = sqlx::query_scalar::<_, Option<String>>(
            "SELECT last_model FROM chat_settings WHERE chat_id = $1 FOR UPDATE",
        )
        .bind(job.chat_id)
        .fetch_optional(&mut *tx)
        .await?
        .flatten();
        let switched_from = previous_model.filter(|previous| previous != model);
        let delivery_answer = answer_with_model_switch(answer, switched_from.as_deref(), model);

        sqlx::query(
            "INSERT INTO chat_settings (chat_id, last_model) VALUES ($1, $2) \
             ON CONFLICT (chat_id) DO UPDATE \
             SET last_model = EXCLUDED.last_model, updated_at = NOW()",
        )
        .bind(job.chat_id)
        .bind(model)
        .execute(&mut *tx)
        .await?;

        sqlx::query(
            "INSERT INTO messages (chat_id, role, content) \
             VALUES ($1, 'user', $2), ($1, 'assistant', $3)",
        )
        .bind(job.chat_id)
        .bind(question)
        .bind(answer)
        .execute(&mut *tx)
        .await?;

        sqlx::query(
            "INSERT INTO model_usage \
             (model, usage_date, requests, prompt_tokens, completion_tokens) \
             VALUES ($1, $2, 1, $3, $4) \
             ON CONFLICT (model, usage_date) DO UPDATE \
             SET requests = model_usage.requests + 1, \
                 prompt_tokens = model_usage.prompt_tokens + EXCLUDED.prompt_tokens, \
                 completion_tokens = model_usage.completion_tokens + EXCLUDED.completion_tokens",
        )
        .bind(model)
        .bind(Utc::now().date_naive())
        .bind(prompt_tokens)
        .bind(completion_tokens)
        .execute(&mut *tx)
        .await?;

        sqlx::query(
            "UPDATE jobs SET answer = $2, model_used = $3, updated_at = NOW() \
             WHERE update_id = $1",
        )
        .bind(job.update_id)
        .bind(&delivery_answer)
        .bind(model)
        .execute(&mut *tx)
        .await?;

        sqlx::query(
            "WITH ranked AS (\
                 SELECT id, \
                        ROW_NUMBER() OVER (ORDER BY created_at DESC, id DESC) AS row_num, \
                        SUM(CHAR_LENGTH(content)) OVER (ORDER BY created_at DESC, id DESC) AS total_chars \
                 FROM messages \
                 WHERE chat_id = $1 \
                   AND created_at >= NOW() - ($4::BIGINT * INTERVAL '1 day')\
             ) \
             DELETE FROM messages m \
             USING ranked r \
             WHERE m.id = r.id \
               AND (r.row_num > $2 OR r.total_chars > $3)",
        )
        .bind(job.chat_id)
        .bind(max_messages)
        .bind(max_chars)
        .bind(retention_days)
        .execute(&mut *tx)
        .await?;

        sqlx::query(
            "DELETE FROM messages \
             WHERE chat_id = $1 \
               AND created_at < NOW() - ($2::BIGINT * INTERVAL '1 day')",
        )
        .bind(job.chat_id)
        .bind(retention_days)
        .execute(&mut *tx)
        .await?;

        tx.commit().await?;
        Ok(switched_from)
    }

    pub async fn save_local_answer(&self, update_id: i64, answer: &str) -> anyhow::Result<()> {
        sqlx::query("UPDATE jobs SET answer = $2, updated_at = NOW() WHERE update_id = $1")
            .bind(update_id)
            .bind(answer)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    pub async fn job_answer(&self, update_id: i64) -> anyhow::Result<Option<String>> {
        Ok(
            sqlx::query_scalar::<_, Option<String>>("SELECT answer FROM jobs WHERE update_id = $1")
                .bind(update_id)
                .fetch_optional(&self.pool)
                .await?
                .flatten(),
        )
    }

    pub async fn mark_chunk_sent(&self, update_id: i64, sent_chunks: i32) -> anyhow::Result<()> {
        sqlx::query("UPDATE jobs SET sent_chunks = $2, updated_at = NOW() WHERE update_id = $1")
            .bind(update_id)
            .bind(sent_chunks)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    pub async fn mark_wait_notified(&self, update_id: i64) -> anyhow::Result<()> {
        sqlx::query(
            "UPDATE jobs SET wait_notified = TRUE, updated_at = NOW() WHERE update_id = $1",
        )
        .bind(update_id)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn complete_job(&self, update_id: i64) -> anyhow::Result<()> {
        sqlx::query("DELETE FROM jobs WHERE update_id = $1")
            .bind(update_id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    pub async fn retry_job(
        &self,
        update_id: i64,
        delay: Duration,
        error: &str,
    ) -> anyhow::Result<()> {
        let delay_seconds = i64::try_from(delay.as_secs()).unwrap_or(i64::MAX).max(1);
        let error = truncate(error, 500);
        sqlx::query(
            "UPDATE jobs \
             SET status = 'pending', lease_until = NULL, \
                 available_at = NOW() + ($2::BIGINT * INTERVAL '1 second'), \
                 last_error = $3, updated_at = NOW() \
             WHERE update_id = $1",
        )
        .bind(update_id)
        .bind(delay_seconds)
        .bind(error)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn defer_job(&self, update_id: i64, delay: Duration) -> anyhow::Result<()> {
        let delay_seconds = i64::try_from(delay.as_secs()).unwrap_or(i64::MAX).max(1);
        sqlx::query(
            "UPDATE jobs \
             SET status = 'pending', attempts = GREATEST(attempts - 1, 0), \
                 lease_until = NULL, \
                 available_at = NOW() + ($2::BIGINT * INTERVAL '1 second'), \
                 updated_at = NOW() \
             WHERE update_id = $1",
        )
        .bind(update_id)
        .bind(delay_seconds)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn auto_model_switch(&self, chat_id: i64) -> anyhow::Result<bool> {
        Ok(sqlx::query_scalar::<_, bool>(
            "SELECT auto_model_switch FROM chat_settings WHERE chat_id = $1",
        )
        .bind(chat_id)
        .fetch_optional(&self.pool)
        .await?
        .unwrap_or(true))
    }

    pub async fn set_auto_model_switch(&self, chat_id: i64, enabled: bool) -> anyhow::Result<()> {
        sqlx::query(
            "INSERT INTO chat_settings (chat_id, auto_model_switch) VALUES ($1, $2) \
             ON CONFLICT (chat_id) DO UPDATE \
             SET auto_model_switch = EXCLUDED.auto_model_switch, updated_at = NOW()",
        )
        .bind(chat_id)
        .bind(enabled)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn clear_context(&self, chat_id: i64) -> anyhow::Result<u64> {
        let result = sqlx::query("DELETE FROM messages WHERE chat_id = $1")
            .bind(chat_id)
            .execute(&self.pool)
            .await?;
        Ok(result.rows_affected())
    }

    pub async fn daily_usage(&self, model: &str) -> anyhow::Result<DailyUsage> {
        Ok(sqlx::query_as::<_, DailyUsage>(
            "SELECT requests, prompt_tokens, completion_tokens \
             FROM model_usage WHERE model = $1 AND usage_date = $2",
        )
        .bind(model)
        .bind(Utc::now().date_naive())
        .fetch_optional(&self.pool)
        .await?
        .unwrap_or_default())
    }

    pub async fn cleanup(&self) -> anyhow::Result<()> {
        let mut tx = self.pool.begin().await?;
        sqlx::query(
            "DELETE FROM processed_updates p \
             WHERE p.processed_at < NOW() - INTERVAL '48 hours' \
               AND NOT EXISTS (SELECT 1 FROM jobs j WHERE j.update_id = p.update_id)",
        )
        .execute(&mut *tx)
        .await?;
        sqlx::query("DELETE FROM model_usage WHERE usage_date < CURRENT_DATE - 2")
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        Ok(())
    }
}

fn truncate(value: &str, max_chars: usize) -> String {
    value.chars().take(max_chars).collect()
}

fn answer_with_model_switch(answer: &str, previous_model: Option<&str>, model: &str) -> String {
    let Some(previous_model) = previous_model else {
        return answer.to_owned();
    };
    let previous_model = previous_model.replace('`', "ˋ");
    let model = model.replace('`', "ˋ");
    format!("🔄 **Model switched:** `{previous_model}` → `{model}`\n\n{answer}")
}

#[cfg(test)]
mod tests {
    use super::answer_with_model_switch;

    #[test]
    fn leaves_first_answer_without_a_switch_notice() {
        assert_eq!(
            answer_with_model_switch("Answer", None, "primary"),
            "Answer"
        );
    }

    #[test]
    fn prepends_switch_notice_to_answer() {
        assert_eq!(
            answer_with_model_switch("Answer", Some("primary"), "fallback"),
            "🔄 **Model switched:** `primary` → `fallback`\n\nAnswer"
        );
    }
}
