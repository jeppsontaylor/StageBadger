//! # YouTube Live Chat Formatting
//!
//! The runtime chat source is YouTube Live Chat. The stream worker favors
//! `liveChatMessages.streamList` when OAuth/API support is available and can
//! fall back to polling `liveChatMessages.list` using YouTube's advertised
//! `pollingIntervalMillis`.

use crate::types::ChatMessage;
use std::fs::OpenOptions;
use std::io::Write;

pub const MAX_VISIBLE_MESSAGES: usize = 10;

pub fn initialize_chat_overlay() {
    let _ = write_chat_overlay(&[]);
}

pub fn stream_list_url(live_chat_id: &str) -> String {
    format!(
        "https://www.googleapis.com/youtube/v3/liveChat/messages:stream?part=snippet,authorDetails&liveChatId={}",
        live_chat_id
    )
}

pub fn poll_list_url(live_chat_id: &str, page_token: Option<&str>) -> String {
    let mut url = format!(
        "https://www.googleapis.com/youtube/v3/liveChat/messages?part=snippet,authorDetails&liveChatId={}",
        live_chat_id
    );
    if let Some(token) = page_token {
        url.push_str("&pageToken=");
        url.push_str(token);
    }
    url
}

pub fn format_chat_message(message: &ChatMessage) -> String {
    let role = message
        .role
        .as_ref()
        .map(|role| format!(" [{}]", role))
        .unwrap_or_default();
    let super_chat = message
        .amount_display
        .as_ref()
        .filter(|_| message.is_super_chat)
        .map(|amount| format!(" {}", amount))
        .unwrap_or_default();
    format!("{}{}{}: {}", message.author, role, super_chat, message.message)
}

pub fn format_chat_overlay(messages: &[ChatMessage]) -> String {
    messages
        .iter()
        .rev()
        .take(MAX_VISIBLE_MESSAGES)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .map(|message| format_chat_message(message))
        .collect::<Vec<_>>()
        .join("\n")
}

pub fn write_chat_overlay(messages: &[ChatMessage]) -> Result<(), String> {
    let display = if messages.is_empty() {
        "YouTube chat disconnected".to_string()
    } else {
        format_chat_overlay(messages)
    };

    let mut file = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .open("/tmp/stagebadger_chat.txt")
        .map_err(|e| e.to_string())?;
    writeln!(file, "{}", display).map_err(|e| e.to_string())
}

pub fn parse_polling_interval_millis(value: Option<u64>) -> std::time::Duration {
    std::time::Duration::from_millis(value.unwrap_or(2_000).clamp(500, 30_000))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_message(id: &str, text: &str) -> ChatMessage {
        ChatMessage {
            id: id.to_string(),
            author: "Taylor".to_string(),
            message: text.to_string(),
            role: Some("member".to_string()),
            published_at: None,
            amount_display: None,
            is_super_chat: false,
        }
    }

    #[test]
    fn test_format_chat_message_with_role() {
        let msg = sample_message("1", "Hello chat");
        assert_eq!(format_chat_message(&msg), "Taylor [member]: Hello chat");
    }

    #[test]
    fn test_format_chat_message_with_super_chat() {
        let mut msg = sample_message("1", "Great stream");
        msg.is_super_chat = true;
        msg.amount_display = Some("$20.00".to_string());
        assert_eq!(format_chat_message(&msg), "Taylor [member] $20.00: Great stream");
    }

    #[test]
    fn test_format_chat_overlay_limits_visible_messages() {
        let messages: Vec<ChatMessage> = (0..25)
            .map(|idx| sample_message(&idx.to_string(), &format!("message {}", idx)))
            .collect();
        let overlay = format_chat_overlay(&messages);
        assert_eq!(overlay.lines().count(), MAX_VISIBLE_MESSAGES);
        assert!(overlay.contains("message 24"));
        assert!(!overlay.contains("message 0"));
    }

    #[test]
    fn test_stream_list_url_uses_live_chat_id() {
        let url = stream_list_url("abc123");
        assert!(url.contains("liveChatId=abc123"));
        assert!(url.contains("messages:stream"));
    }

    #[test]
    fn test_poll_list_url_includes_page_token() {
        let url = poll_list_url("abc123", Some("next"));
        assert!(url.contains("pageToken=next"));
    }

    #[test]
    fn test_polling_interval_bounds() {
        assert_eq!(parse_polling_interval_millis(Some(100)).as_millis(), 500);
        assert_eq!(parse_polling_interval_millis(Some(60_000)).as_millis(), 30_000);
        assert_eq!(parse_polling_interval_millis(None).as_millis(), 2_000);
    }
}
