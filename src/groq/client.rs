use std::time::Duration;

use reqwest::{StatusCode, header::HeaderMap};
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::domain::Role;

const GROQ_CHAT_URL: &str = "https://api.groq.com/openai/v1/chat/completions";

#[derive(Clone)]
pub struct GroqClient {
    http: reqwest::Client,
    api_key: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct ChatMessage {
    pub role: Role,
    pub content: String,
}

#[derive(Clone, Debug)]
pub struct Completion {
    pub content: String,
    pub prompt_tokens: i64,
    pub completion_tokens: i64,
    pub rate_headers: RateHeaders,
}

#[derive(Clone, Debug, Default)]
pub struct RateHeaders {
    pub limit_requests: Option<i64>,
    pub remaining_requests: Option<i64>,
    pub limit_tokens: Option<i64>,
    pub remaining_tokens: Option<i64>,
    pub retry_after: Option<Duration>,
}

#[derive(Debug, Error)]
pub enum GroqError {
    #[error("Groq rate limit reached")]
    RateLimited(RateHeaders),
    #[error("Groq request failed: {0}")]
    Transport(#[from] reqwest::Error),
    #[error("Groq API returned {status}: {message}")]
    Api { status: StatusCode, message: String },
    #[error("Groq returned an empty completion")]
    EmptyCompletion,
}

impl GroqClient {
    pub fn new(api_key: String) -> Result<Self, reqwest::Error> {
        Ok(Self {
            http: reqwest::Client::builder()
                .timeout(Duration::from_secs(90))
                .build()?,
            api_key,
        })
    }

    pub async fn complete(
        &self,
        model: &str,
        messages: &[ChatMessage],
        max_completion_tokens: u32,
    ) -> Result<Completion, GroqError> {
        let response = self
            .http
            .post(GROQ_CHAT_URL)
            .bearer_auth(&self.api_key)
            .json(&CompletionRequest {
                model,
                messages,
                max_completion_tokens,
                temperature: 0.6,
            })
            .send()
            .await?;

        let status = response.status();
        let rate_headers = RateHeaders::from_headers(response.headers());
        if status == StatusCode::TOO_MANY_REQUESTS {
            return Err(GroqError::RateLimited(rate_headers));
        }
        if !status.is_success() {
            let message = response
                .text()
                .await
                .unwrap_or_else(|_| "unreadable response".into());
            return Err(GroqError::Api {
                status,
                message: message.chars().take(500).collect(),
            });
        }

        let body: CompletionResponse = response.json().await?;
        let content = body
            .choices
            .into_iter()
            .next()
            .map(|choice| choice.message.content)
            .filter(|content| !content.trim().is_empty())
            .ok_or(GroqError::EmptyCompletion)?;

        Ok(Completion {
            content,
            prompt_tokens: body.usage.prompt_tokens,
            completion_tokens: body.usage.completion_tokens,
            rate_headers,
        })
    }
}

impl RateHeaders {
    fn from_headers(headers: &HeaderMap) -> Self {
        Self {
            limit_requests: integer_header(headers, "x-ratelimit-limit-requests"),
            remaining_requests: integer_header(headers, "x-ratelimit-remaining-requests"),
            limit_tokens: integer_header(headers, "x-ratelimit-limit-tokens"),
            remaining_tokens: integer_header(headers, "x-ratelimit-remaining-tokens"),
            retry_after: integer_header(headers, "retry-after")
                .and_then(|seconds| u64::try_from(seconds).ok())
                .map(Duration::from_secs),
        }
    }
}

fn integer_header(headers: &HeaderMap, name: &str) -> Option<i64> {
    headers.get(name)?.to_str().ok()?.parse().ok()
}

#[derive(Debug, Serialize)]
struct CompletionRequest<'a> {
    model: &'a str,
    messages: &'a [ChatMessage],
    max_completion_tokens: u32,
    temperature: f32,
}

#[derive(Debug, Deserialize)]
struct CompletionResponse {
    choices: Vec<Choice>,
    #[serde(default)]
    usage: Usage,
}

#[derive(Debug, Deserialize)]
struct Choice {
    message: ResponseMessage,
}

#[derive(Debug, Deserialize)]
struct ResponseMessage {
    content: String,
}

#[derive(Debug, Default, Deserialize)]
struct Usage {
    #[serde(default)]
    prompt_tokens: i64,
    #[serde(default)]
    completion_tokens: i64,
}
