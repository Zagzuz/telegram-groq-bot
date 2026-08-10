use serde::Serialize;
use sqlx::FromRow;

#[derive(Clone, Debug, FromRow)]
pub struct ConversationMessage {
    pub role: String,
    pub content: String,
}

#[derive(Clone, Copy, Debug, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    System,
    User,
    Assistant,
}
