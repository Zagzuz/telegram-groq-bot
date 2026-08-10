use crate::domain::{WorkItem, WorkKind};

use super::types::{Message, Update};

pub fn extract_work_item(update: &Update, bot_username: &str) -> Option<WorkItem> {
    let message = update.message.as_ref()?;
    let text = message.text.as_deref()?.trim();
    if text.is_empty() {
        return None;
    }

    let (kind, input) = if text.starts_with('/') {
        parse_command(text, bot_username)?
    } else if is_reply_to_bot(message, bot_username) || contains_mention(text, bot_username) {
        let question = strip_mention(text, bot_username);
        if question.is_empty() {
            (WorkKind::Help, None)
        } else {
            (WorkKind::Ask, Some(question))
        }
    } else {
        return None;
    };

    Some(WorkItem {
        update_id: update.update_id,
        chat_id: message.chat.id,
        chat_kind: message.chat.kind.clone(),
        actor_user_id: message.from.as_ref().map(|user| user.id),
        message_id: message.message_id,
        thread_id: message.message_thread_id,
        kind,
        input,
    })
}

fn parse_command(text: &str, bot_username: &str) -> Option<(WorkKind, Option<String>)> {
    let mut parts = text.splitn(2, char::is_whitespace);
    let command_token = parts.next()?.trim_start_matches('/');
    let input = parts
        .next()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned);

    let mut command_parts = command_token.splitn(2, '@');
    let command = command_parts.next()?.to_ascii_lowercase();
    if let Some(target) = command_parts.next()
        && !target.eq_ignore_ascii_case(bot_username)
    {
        return None;
    }

    let kind = match command.as_str() {
        "start" | "help" => WorkKind::Help,
        "ask" => WorkKind::Ask,
        "automodel" => WorkKind::AutoModel,
        "reset" => WorkKind::Reset,
        "model" => WorkKind::Model,
        "privacy" => WorkKind::Privacy,
        _ => return None,
    };
    Some((kind, input))
}

fn is_reply_to_bot(message: &Message, bot_username: &str) -> bool {
    message
        .reply_to_message
        .as_ref()
        .and_then(|reply| reply.from.as_ref())
        .is_some_and(|user| {
            user.is_bot
                && user
                    .username
                    .as_deref()
                    .is_some_and(|name| name.eq_ignore_ascii_case(bot_username))
        })
}

fn contains_mention(text: &str, bot_username: &str) -> bool {
    let expected = format!("@{bot_username}");
    text.split_whitespace().any(|token| {
        token
            .trim_matches(|c: char| !c.is_ascii_alphanumeric() && c != '@' && c != '_')
            .eq_ignore_ascii_case(&expected)
    })
}

fn strip_mention(text: &str, bot_username: &str) -> String {
    let expected = format!("@{bot_username}");
    text.split_whitespace()
        .filter(|token| {
            !token
                .trim_matches(|c: char| !c.is_ascii_alphanumeric() && c != '@' && c != '_')
                .eq_ignore_ascii_case(&expected)
        })
        .collect::<Vec<_>>()
        .join(" ")
        .trim()
        .to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::telegram::types::{Chat, Message, Update, User};

    fn update(text: &str) -> Update {
        Update {
            update_id: 10,
            message: Some(Message {
                message_id: 20,
                message_thread_id: None,
                from: Some(User {
                    id: 30,
                    is_bot: false,
                    username: Some("person".into()),
                }),
                chat: Chat {
                    id: -40,
                    kind: "supergroup".into(),
                },
                text: Some(text.into()),
                reply_to_message: None,
            }),
        }
    }

    #[test]
    fn parses_auto_model_command_for_this_bot() {
        let item = extract_work_item(&update("/automodel@MyBot off"), "MyBot").unwrap();
        assert_eq!(item.kind, WorkKind::AutoModel);
        assert_eq!(item.input.as_deref(), Some("off"));
    }

    #[test]
    fn ignores_commands_for_another_bot() {
        assert!(extract_work_item(&update("/ask@OtherBot hello"), "MyBot").is_none());
    }

    #[test]
    fn turns_a_mention_into_a_question() {
        let item = extract_work_item(&update("@MyBot, what is Rust?"), "MyBot").unwrap();
        assert_eq!(item.kind, WorkKind::Ask);
        assert_eq!(item.input.as_deref(), Some("what is Rust?"));
    }
}
