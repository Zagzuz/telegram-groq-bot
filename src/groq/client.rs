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
    pub token_reset_after: Option<Duration>,
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
                reasoning_effort: model.starts_with("openai/gpt-oss-").then_some("low"),
            })
            .send()
            .await?;

        let status = response.status();
        let rate_headers = RateHeaders::from_headers(response.headers());
        if !status.is_success() {
            let message = response
                .text()
                .await
                .unwrap_or_else(|_| "unreadable response".into());
            if is_rate_limit_response(status, &message) {
                return Err(GroqError::RateLimited(rate_headers));
            }
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
            retry_after: duration_header(headers, "retry-after")
                .or_else(|| duration_header(headers, "x-ratelimit-reset-tokens")),
            token_reset_after: duration_header(headers, "x-ratelimit-reset-tokens"),
        }
    }
}

fn is_rate_limit_response(status: StatusCode, message: &str) -> bool {
    status == StatusCode::TOO_MANY_REQUESTS
        || (status == StatusCode::PAYLOAD_TOO_LARGE && message.contains("rate_limit_exceeded"))
}

fn integer_header(headers: &HeaderMap, name: &str) -> Option<i64> {
    headers.get(name)?.to_str().ok()?.parse().ok()
}

fn duration_header(headers: &HeaderMap, name: &str) -> Option<Duration> {
    parse_duration(headers.get(name)?.to_str().ok()?)
}

fn parse_duration(value: &str) -> Option<Duration> {
    if let Ok(seconds) = value.parse::<f64>() {
        return duration_from_seconds(seconds);
    }

    let mut total_seconds = 0.0_f64;
    let mut rest = value.trim();
    while !rest.is_empty() {
        let number_end = rest
            .find(|character: char| !character.is_ascii_digit() && character != '.')
            .unwrap_or(rest.len());
        if number_end == 0 {
            return None;
        }
        let amount = rest[..number_end].parse::<f64>().ok()?;
        rest = &rest[number_end..];

        let unit_end = rest
            .find(|character: char| !character.is_ascii_alphabetic())
            .unwrap_or(rest.len());
        if unit_end == 0 {
            return None;
        }
        let multiplier = match &rest[..unit_end] {
            "ms" => 0.001,
            "s" => 1.0,
            "m" => 60.0,
            "h" => 3_600.0,
            _ => return None,
        };
        total_seconds += amount * multiplier;
        rest = &rest[unit_end..];
    }
    duration_from_seconds(total_seconds)
}

fn duration_from_seconds(seconds: f64) -> Option<Duration> {
    if !seconds.is_finite() || seconds < 0.0 {
        return None;
    }
    Some(Duration::from_secs_f64(seconds.max(0.001)))
}

#[derive(Debug, Serialize)]
struct CompletionRequest<'a> {
    model: &'a str,
    messages: &'a [ChatMessage],
    max_completion_tokens: u32,
    temperature: f32,
    #[serde(skip_serializing_if = "Option::is_none")]
    reasoning_effort: Option<&'static str>,
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

#[cfg(test)]
mod tests {
    use super::*;
    use reqwest::header::{HeaderMap, HeaderValue};

    #[test]
    fn uses_token_reset_when_retry_after_is_absent() {
        let mut headers = HeaderMap::new();
        headers.insert(
            "x-ratelimit-reset-tokens",
            HeaderValue::from_static("2m59.56s"),
        );

        let parsed = RateHeaders::from_headers(&headers);

        assert_eq!(parsed.retry_after, Some(Duration::from_millis(179_560)));
        assert_eq!(
            parsed.token_reset_after,
            Some(Duration::from_millis(179_560))
        );
    }

    #[test]
    fn recognizes_payload_too_large_rate_limit_errors() {
        assert!(is_rate_limit_response(
            StatusCode::PAYLOAD_TOO_LARGE,
            r#"{"error":{"code":"rate_limit_exceeded"}}"#,
        ));
        assert!(!is_rate_limit_response(
            StatusCode::PAYLOAD_TOO_LARGE,
            r#"{"error":{"code":"request_too_large"}}"#,
        ));
    }

    #[test]
    fn enables_low_reasoning_only_for_gpt_oss() {
        let gpt_oss = CompletionRequest {
            model: "openai/gpt-oss-120b",
            messages: &[],
            max_completion_tokens: 1_200,
            temperature: 0.6,
            reasoning_effort: Some("low"),
        };
        let llama = CompletionRequest {
            model: "llama-3.1-8b-instant",
            messages: &[],
            max_completion_tokens: 1_200,
            temperature: 0.6,
            reasoning_effort: None,
        };

        assert_eq!(
            serde_json::to_value(gpt_oss).unwrap()["reasoning_effort"],
            "low"
        );
        assert!(serde_json::to_value(llama).unwrap()["reasoning_effort"].is_null());
    }
}
