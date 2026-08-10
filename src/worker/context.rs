use crate::{
    domain::{ConversationMessage, Role},
    groq::ChatMessage,
};

pub fn build_context(
    system_prompt: &str,
    history: &[ConversationMessage],
    question: &str,
    max_chars: usize,
) -> Vec<ChatMessage> {
    let fixed_chars = system_prompt.chars().count() + question.chars().count();
    let mut remaining = max_chars.saturating_sub(fixed_chars);
    let mut selected = Vec::new();

    for message in history.iter().rev() {
        let length = message.content.chars().count();
        if length > remaining {
            break;
        }
        let role = match message.role.as_str() {
            "user" => Role::User,
            "assistant" => Role::Assistant,
            _ => continue,
        };
        selected.push(ChatMessage {
            role,
            content: message.content.clone(),
        });
        remaining -= length;
    }
    selected.reverse();

    let mut result = Vec::with_capacity(selected.len() + 2);
    result.push(ChatMessage {
        role: Role::System,
        content: system_prompt.to_owned(),
    });
    result.extend(selected);
    result.push(ChatMessage {
        role: Role::User,
        content: question.to_owned(),
    });
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_only_the_newest_history_that_fits() {
        let history = vec![
            ConversationMessage {
                role: "user".into(),
                content: "old-old".into(),
            },
            ConversationMessage {
                role: "assistant".into(),
                content: "recent".into(),
            },
        ];
        let result = build_context("sys", &history, "question", 18);
        assert_eq!(result.len(), 3);
        assert_eq!(result[1].content, "recent");
    }
}
