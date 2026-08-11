use std::{env, str::FromStr};

use anyhow::{Context, bail};

#[derive(Clone, Debug)]
pub struct Config {
    pub database_url: String,
    pub telegram_bot_token: String,
    pub telegram_webhook_secret: String,
    pub telegram_bot_username: String,
    pub groq_api_key: String,
    pub primary_model: String,
    pub fallback_model: String,
    pub public_base_url: Option<String>,
    pub auto_register_webhook: bool,
    pub system_prompt: String,
    pub port: u16,
    pub context_max_messages: i64,
    pub context_max_chars: usize,
    pub context_retention_days: i64,
    pub primary_answer_max_tokens: u32,
    pub fallback_answer_max_tokens: u32,
    pub primary_daily_token_budget: i64,
    pub fallback_daily_token_budget: i64,
    pub primary_daily_request_budget: i64,
    pub fallback_daily_request_budget: i64,
    pub primary_daily_switch_percent: u8,
    pub rate_reserve_percent: u8,
    pub worker_poll_milliseconds: u64,
    pub job_lease_seconds: i64,
    pub max_job_attempts: i32,
}

impl Config {
    pub fn from_env() -> anyhow::Result<Self> {
        let telegram_webhook_secret = required("TELEGRAM_WEBHOOK_SECRET")?;
        if !(1..=256).contains(&telegram_webhook_secret.len())
            || !telegram_webhook_secret
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-'))
        {
            bail!(
                "TELEGRAM_WEBHOOK_SECRET must be 1-256 ASCII letters, numbers, underscores, or hyphens"
            );
        }

        let primary_daily_switch_percent = optional("PRIMARY_DAILY_SWITCH_PERCENT", 80_u8)?;
        let rate_reserve_percent = optional("RATE_RESERVE_PERCENT", 20_u8)?;
        if primary_daily_switch_percent > 100 || rate_reserve_percent > 100 {
            bail!("percentage configuration values must be between 0 and 100");
        }

        let config = Self {
            database_url: required("DATABASE_URL")?,
            telegram_bot_token: required("TELEGRAM_BOT_TOKEN")?,
            telegram_webhook_secret,
            telegram_bot_username: required("TELEGRAM_BOT_USERNAME")?
                .trim_start_matches('@')
                .to_owned(),
            groq_api_key: required("GROQ_API_KEY")?,
            primary_model: value_or("GROQ_PRIMARY_MODEL", "openai/gpt-oss-120b"),
            fallback_model: value_or("GROQ_FALLBACK_MODEL", "llama-3.1-8b-instant"),
            public_base_url: env::var("PUBLIC_BASE_URL").ok().filter(|s| !s.is_empty()),
            auto_register_webhook: optional("AUTO_REGISTER_WEBHOOK", false)?,
            system_prompt: value_or(
                "SYSTEM_PROMPT",
                "You are a helpful assistant in a Telegram group. Answer clearly, accurately, and concisely. Format responses using GitHub-flavored Markdown supported by Telegram rich messages. Write inline formulas as $LaTeX$ and display formulas as $$LaTeX$$. Keep the complete response under 7,800 characters and finish cleanly rather than ending mid-sentence or mid-table. Use headings sparingly. Do not claim access to current information or tools that you do not have.",
            ),
            port: optional("PORT", 8080_u16)?,
            context_max_messages: optional("CONTEXT_MAX_MESSAGES", 12_i64)?,
            context_max_chars: optional("CONTEXT_MAX_CHARS", 16_384_usize)?,
            context_retention_days: optional("CONTEXT_RETENTION_DAYS", 7_i64)?,
            primary_answer_max_tokens: optional("PRIMARY_ANSWER_MAX_TOKENS", 2_400_u32)?,
            fallback_answer_max_tokens: optional("FALLBACK_ANSWER_MAX_TOKENS", 2_400_u32)?,
            primary_daily_token_budget: optional("PRIMARY_DAILY_TOKEN_BUDGET", 200_000_i64)?,
            fallback_daily_token_budget: optional("FALLBACK_DAILY_TOKEN_BUDGET", 500_000_i64)?,
            primary_daily_request_budget: optional("PRIMARY_DAILY_REQUEST_BUDGET", 1_000_i64)?,
            fallback_daily_request_budget: optional("FALLBACK_DAILY_REQUEST_BUDGET", 14_400_i64)?,
            primary_daily_switch_percent,
            rate_reserve_percent,
            worker_poll_milliseconds: optional("WORKER_POLL_MILLISECONDS", 500_u64)?,
            job_lease_seconds: optional("JOB_LEASE_SECONDS", 120_i64)?,
            max_job_attempts: optional("MAX_JOB_ATTEMPTS", 8_i32)?,
        };
        config.validate()?;
        Ok(config)
    }

    fn validate(&self) -> anyhow::Result<()> {
        if self.telegram_bot_username.is_empty() {
            bail!("TELEGRAM_BOT_USERNAME cannot be empty");
        }
        if self.port == 0 {
            bail!("PORT must be greater than zero");
        }
        if self.context_max_messages <= 0
            || self.context_max_chars == 0
            || self.context_retention_days <= 0
        {
            bail!("context limits and retention must be greater than zero");
        }
        if self.primary_answer_max_tokens == 0 || self.fallback_answer_max_tokens == 0 {
            bail!("answer token limits must be greater than zero");
        }
        if self.primary_daily_token_budget <= 0
            || self.fallback_daily_token_budget <= 0
            || self.primary_daily_request_budget <= 0
            || self.fallback_daily_request_budget <= 0
        {
            bail!("daily model budgets must be greater than zero");
        }
        if self.worker_poll_milliseconds == 0
            || self.job_lease_seconds <= 0
            || self.max_job_attempts <= 0
        {
            bail!("worker timing and attempt limits must be greater than zero");
        }
        Ok(())
    }
}

fn required(name: &str) -> anyhow::Result<String> {
    env::var(name)
        .with_context(|| format!("{name} is required"))
        .and_then(|value| {
            if value.trim().is_empty() {
                bail!("{name} cannot be empty")
            }
            Ok(value)
        })
}

fn value_or(name: &str, default: &str) -> String {
    env::var(name).unwrap_or_else(|_| default.to_owned())
}

fn optional<T>(name: &str, default: T) -> anyhow::Result<T>
where
    T: FromStr,
    T::Err: std::fmt::Display,
{
    match env::var(name) {
        Ok(value) => value
            .parse::<T>()
            .map_err(|error| anyhow::anyhow!("{name} has an invalid value {value}: {error}")),
        Err(_) => Ok(default),
    }
}
