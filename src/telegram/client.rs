use anyhow::{Context, bail};
use reqwest::Url;
use serde::{Deserialize, Serialize, de::DeserializeOwned};

use super::formatting::markdown_to_telegram_html;

#[derive(Clone)]
pub struct TelegramClient {
    http: reqwest::Client,
    base_url: Url,
}

impl TelegramClient {
    pub fn new(token: &str) -> anyhow::Result<Self> {
        let base_url = Url::parse(&format!("https://api.telegram.org/bot{token}/"))?;
        Ok(Self {
            http: reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(30))
                .build()?,
            base_url,
        })
    }

    pub async fn set_webhook(
        &self,
        public_base_url: &str,
        secret_token: &str,
    ) -> anyhow::Result<()> {
        let webhook_url = format!("{}/telegram/webhook", public_base_url.trim_end_matches('/'));
        let request = SetWebhookRequest {
            url: &webhook_url,
            secret_token,
            allowed_updates: ["message"],
            drop_pending_updates: false,
        };
        let _: bool = self.post("setWebhook", &request).await?;
        Ok(())
    }

    pub async fn delete_webhook(&self) -> anyhow::Result<()> {
        let _: bool = self
            .post(
                "deleteWebhook",
                &serde_json::json!({ "drop_pending_updates": false }),
            )
            .await?;
        Ok(())
    }

    pub async fn send_message(
        &self,
        chat_id: i64,
        thread_id: Option<i64>,
        reply_to_message_id: Option<i64>,
        text: &str,
    ) -> anyhow::Result<()> {
        let formatted = markdown_to_telegram_html(text);
        let request = SendMessageRequest {
            chat_id,
            message_thread_id: thread_id,
            text: &formatted,
            parse_mode: "HTML",
            reply_parameters: reply_to_message_id.map(|message_id| ReplyParameters { message_id }),
        };
        let _: serde_json::Value = self.post("sendMessage", &request).await?;
        Ok(())
    }

    pub async fn send_typing(&self, chat_id: i64, thread_id: Option<i64>) -> anyhow::Result<()> {
        let _: bool = self
            .post(
                "sendChatAction",
                &serde_json::json!({
                    "chat_id": chat_id,
                    "message_thread_id": thread_id,
                    "action": "typing"
                }),
            )
            .await?;
        Ok(())
    }

    pub async fn is_chat_admin(&self, chat_id: i64, user_id: i64) -> anyhow::Result<bool> {
        let member: ChatMember = self
            .post(
                "getChatMember",
                &serde_json::json!({ "chat_id": chat_id, "user_id": user_id }),
            )
            .await?;
        Ok(matches!(
            member.status.as_str(),
            "creator" | "administrator"
        ))
    }

    async fn post<B, T>(&self, method: &str, body: &B) -> anyhow::Result<T>
    where
        B: Serialize + ?Sized,
        T: DeserializeOwned,
    {
        let url = self.base_url.join(method)?;
        let response = self
            .http
            .post(url)
            .json(body)
            .send()
            .await
            .with_context(|| format!("Telegram {method} request failed"))?;
        let status = response.status();
        let envelope: ApiResponse<T> = response
            .json()
            .await
            .with_context(|| format!("Telegram {method} returned an invalid response"))?;
        if !status.is_success() || !envelope.ok {
            bail!(
                "Telegram {method} failed with {status}: {}",
                envelope
                    .description
                    .unwrap_or_else(|| "unknown error".into())
            );
        }
        envelope
            .result
            .with_context(|| format!("Telegram {method} response did not contain a result"))
    }
}

#[derive(Debug, Serialize)]
struct SetWebhookRequest<'a> {
    url: &'a str,
    secret_token: &'a str,
    allowed_updates: [&'a str; 1],
    drop_pending_updates: bool,
}

#[derive(Debug, Serialize)]
struct SendMessageRequest<'a> {
    chat_id: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    message_thread_id: Option<i64>,
    text: &'a str,
    parse_mode: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    reply_parameters: Option<ReplyParameters>,
}

#[derive(Debug, Serialize)]
struct ReplyParameters {
    message_id: i64,
}

#[derive(Debug, Deserialize)]
struct ChatMember {
    status: String,
}

#[derive(Debug, Deserialize)]
struct ApiResponse<T> {
    ok: bool,
    result: Option<T>,
    description: Option<String>,
}

pub fn split_message(text: &str, max_chars: usize, max_chunks: usize) -> Vec<String> {
    if max_chars == 0 || max_chunks == 0 {
        return Vec::new();
    }
    if text.chars().count() <= max_chars {
        return vec![text.to_owned()];
    }

    let mut chunks = Vec::new();
    let mut rest = text.trim();
    while rest.chars().count() > max_chars {
        let hard_end = rest
            .char_indices()
            .nth(max_chars)
            .map(|(index, _)| index)
            .unwrap_or(rest.len());
        let candidate = &rest[..hard_end];
        let soft_end = candidate
            .rfind('\n')
            .or_else(|| candidate.rfind(' '))
            .filter(|index| *index >= hard_end / 2)
            .unwrap_or(hard_end);
        chunks.push(rest[..soft_end].trim().to_owned());
        rest = rest[soft_end..].trim();
    }
    if !rest.is_empty() {
        chunks.push(rest.to_owned());
    }
    if chunks.len() > max_chunks {
        chunks.truncate(max_chunks);
        if let Some(last) = chunks.last_mut() {
            if last.chars().count() >= max_chars {
                last.pop();
            }
            last.push('…');
        }
    }
    chunks
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_without_breaking_utf8() {
        let chunks = split_message(&"日".repeat(9), 4, 3);
        assert_eq!(chunks, vec!["日日日日", "日日日日", "日"]);
    }

    #[test]
    fn prefers_a_word_boundary() {
        let chunks = split_message("alpha beta gamma", 11, 2);
        assert_eq!(chunks, vec!["alpha beta", "gamma"]);
    }

    #[test]
    fn truncates_at_the_chunk_limit() {
        let chunks = split_message(&"a".repeat(10), 4, 2);
        assert_eq!(chunks, vec!["aaaa", "aaa…"]);
        assert!(chunks.iter().all(|chunk| chunk.chars().count() <= 4));
    }

    #[test]
    fn serializes_html_delivery() {
        let request = SendMessageRequest {
            chat_id: 42,
            message_thread_id: None,
            text: "<b>bold</b>",
            parse_mode: "HTML",
            reply_parameters: Some(ReplyParameters { message_id: 7 }),
        };
        let value = serde_json::to_value(request).unwrap();
        assert_eq!(value["text"], "<b>bold</b>");
        assert_eq!(value["parse_mode"], "HTML");
        assert_eq!(value["reply_parameters"]["message_id"], 7);
    }
}
