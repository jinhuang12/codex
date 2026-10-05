//! Forks containing provider state stay with the source account. Only known
//! portable message content may cross an account boundary; unknown items do not.

use crate::session::ForkPersistence;
use codex_history::InitialHistory;
use codex_history::RolloutItem;
use codex_protocol::models::ContentItem;
use codex_protocol::models::ImageReference;
use codex_protocol::models::ResponseItem;

pub(super) fn requires_account_affinity(
    history: &InitialHistory,
    persistence: &ForkPersistence,
) -> bool {
    if !matches!(history, InitialHistory::Forked(_)) {
        return false;
    }
    if matches!(persistence, ForkPersistence::Referenced { .. }) {
        return true;
    }
    history.scan_rollout_items(|item| match item {
        RolloutItem::ResponseItem(envelope) => !portable_response(&envelope.item),
        // Compaction can contain encrypted replacement history. Do not discard it.
        RolloutItem::SessionMeta(_)
        | RolloutItem::TurnContext(_)
        | RolloutItem::TokenUsageRecord(_)
        | RolloutItem::EventMsg(_)
        | RolloutItem::SecurityRiskScore(_)
        | RolloutItem::RealtimeItem(_) => false,
        _ => true,
    })
}

fn portable_response(item: &ResponseItem) -> bool {
    match item {
        ResponseItem::Message {
            content,
            internal_chat_message_metadata_passthrough: None,
            ..
        } => content.iter().all(|content| {
            matches!(
                content,
                ContentItem::InputText { .. }
                    | ContentItem::OutputText { .. }
                    | ContentItem::InputImage {
                        image: ImageReference::Inline { .. },
                        ..
                    }
            )
        }),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn response(value: serde_json::Value) -> ResponseItem {
        serde_json::from_value(value).expect("valid response fixture")
    }

    #[test]
    fn plain_messages_are_portable_but_provider_references_are_not() {
        assert!(portable_response(&response(
            json!({"type":"message", "role":"user", "content":[{"type":"input_text","text":"Review the code"}]})
        )));
        assert!(portable_response(&response(
            json!({"type":"message", "role":"user", "content":[{"type":"input_image","image_url":"data:image/png;base64,eA=="}]})
        )));
        assert!(!portable_response(&response(
            json!({"type":"message", "role":"user", "content":[{"type":"input_image","file_id":"file-account-a"}]})
        )));
    }

    #[test]
    fn encrypted_reasoning_and_compaction_require_affinity() {
        for value in [
            json!({"type":"reasoning","summary":[],"encrypted_content":"account-a-state"}),
            json!({"type":"compaction","encrypted_content":"account-a-state"}),
        ] {
            assert!(!portable_response(&response(value)));
        }
    }

    #[test]
    fn referenced_fork_is_not_assumed_portable_from_empty_local_items() {
        let referenced = ForkPersistence::Referenced {
            history_base: None,
            inherited_item_count: 1,
        };
        assert!(requires_account_affinity(
            &InitialHistory::Forked(vec![]),
            &referenced
        ));
        assert!(!requires_account_affinity(
            &InitialHistory::New,
            &referenced
        ));
        assert!(!requires_account_affinity(
            &InitialHistory::Forked(vec![]),
            &ForkPersistence::Copied
        ));
    }
}
