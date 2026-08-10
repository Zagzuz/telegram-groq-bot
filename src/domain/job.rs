use sqlx::FromRow;

#[derive(Clone, Debug)]
pub struct WorkItem {
    pub update_id: i64,
    pub chat_id: i64,
    pub chat_kind: String,
    pub actor_user_id: Option<i64>,
    pub message_id: i64,
    pub thread_id: Option<i64>,
    pub kind: WorkKind,
    pub input: Option<String>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WorkKind {
    Ask,
    AutoModel,
    Reset,
    Model,
    Help,
    Privacy,
}

impl WorkKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Ask => "ask",
            Self::AutoModel => "automodel",
            Self::Reset => "reset",
            Self::Model => "model",
            Self::Help => "help",
            Self::Privacy => "privacy",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        Some(match value {
            "ask" => Self::Ask,
            "automodel" => Self::AutoModel,
            "reset" => Self::Reset,
            "model" => Self::Model,
            "help" => Self::Help,
            "privacy" => Self::Privacy,
            _ => return None,
        })
    }
}

#[derive(Clone, Debug, FromRow)]
pub struct Job {
    pub update_id: i64,
    pub chat_id: i64,
    pub chat_kind: String,
    pub actor_user_id: Option<i64>,
    pub message_id: i64,
    pub thread_id: Option<i64>,
    pub kind: String,
    pub input: Option<String>,
    pub answer: Option<String>,
    pub sent_chunks: i32,
    pub attempts: i32,
}

impl Job {
    pub fn work_kind(&self) -> anyhow::Result<WorkKind> {
        WorkKind::parse(&self.kind)
            .ok_or_else(|| anyhow::anyhow!("unknown work kind: {}", self.kind))
    }
}
