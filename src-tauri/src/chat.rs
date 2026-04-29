//! # Live Chat Feed
//!
//! This module manages the live chat overlay content. It spawns a background Tokio task
//! that maintains a rolling buffer of chat messages and writes them to the overlay file.
//!
//! ## Current Status: Mock Implementation
//!
//! The current implementation generates synthetic chat messages for testing the overlay pipeline.
//! To integrate real YouTube Live chat:
//!
//! 1. Replace the synthetic message generator with YouTube Data API v3 `liveChatMessages.list` polling
//! 2. The rolling buffer and file-write logic remain unchanged
//!
//! ## File Protocol
//!
//! The chat worker writes to `/tmp/stagebadger_chat.txt`. FFmpeg reads this file
//! every frame via `drawtext reload=1`.

use std::fs::OpenOptions;
use std::io::Write;
use tokio::time::{sleep, Duration};

/// Maximum number of chat messages visible in the overlay at any time.
pub const MAX_VISIBLE_MESSAGES: usize = 10;

/// Format a synthetic chat message.
///
/// Generates a mock username and message for testing.
pub fn format_mock_message(index: usize) -> String {
    let usernames = ["RustFan", "StreamerPro", "CodeNinja", "M4Gang", "BadgerLover", "DevOps42", "AIEnthusiast"];
    let messages = [
        "🔥 This is amazing!",
        "Hello from the chat!",
        "StageBadger is the future",
        "Rust + FFmpeg = 🚀",
        "How is the latency?",
        "Love the overlay feature",
        "AI captions are wild",
        "First time here, hi all!",
        "Can you show the code?",
        "This app is so fast",
    ];

    let username = usernames[index % usernames.len()];
    let message = messages[index % messages.len()];
    format!("{}: {}", username, message)
}

/// Format a chat history buffer for overlay display.
///
/// Joins messages with newlines, suitable for FFmpeg's `drawtext` filter.
pub fn format_chat_overlay(messages: &[String]) -> String {
    messages.join("\n")
}

/// Spawn the chat feed background worker.
///
/// Creates a Tokio task that:
/// 1. Generates a new mock chat message every 3 seconds
/// 2. Maintains a rolling buffer of the last [`MAX_VISIBLE_MESSAGES`] messages
/// 3. Writes the formatted buffer to `/tmp/stagebadger_chat.txt`
pub fn spawn_chat_worker() {
    tokio::spawn(async move {
        let mut i = 0usize;
        let mut chat_history: Vec<String> = vec!["[StageBadger] Live Chat Connected".to_string()];

        loop {
            sleep(Duration::from_secs(3)).await;
            i += 1;
            chat_history.push(format_mock_message(i));

            if chat_history.len() > MAX_VISIBLE_MESSAGES {
                chat_history.remove(0);
            }

            let display = format_chat_overlay(&chat_history);

            if let Ok(mut file) = OpenOptions::new()
                .write(true)
                .create(true)
                .truncate(true)
                .open("/tmp/stagebadger_chat.txt")
            {
                let _ = writeln!(file, "{}", display);
            }
        }
    });
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_format_mock_message_not_empty() {
        for i in 0..20 {
            let msg = format_mock_message(i);
            assert!(!msg.is_empty());
            assert!(msg.contains(':'), "Message should contain ':' separator");
        }
    }

    #[test]
    fn test_format_mock_message_deterministic() {
        let msg_a = format_mock_message(5);
        let msg_b = format_mock_message(5);
        assert_eq!(msg_a, msg_b, "Same index should produce same message");
    }

    #[test]
    fn test_format_mock_message_varies() {
        let msg_0 = format_mock_message(0);
        let msg_1 = format_mock_message(1);
        assert_ne!(msg_0, msg_1, "Different indices should produce different messages");
    }

    #[test]
    fn test_format_chat_overlay_empty() {
        let messages: Vec<String> = vec![];
        let result = format_chat_overlay(&messages);
        assert_eq!(result, "");
    }

    #[test]
    fn test_format_chat_overlay_single() {
        let messages = vec!["Hello!".to_string()];
        let result = format_chat_overlay(&messages);
        assert_eq!(result, "Hello!");
    }

    #[test]
    fn test_format_chat_overlay_multiple() {
        let messages = vec!["Line 1".to_string(), "Line 2".to_string(), "Line 3".to_string()];
        let result = format_chat_overlay(&messages);
        assert_eq!(result, "Line 1\nLine 2\nLine 3");
    }

    #[test]
    fn test_max_visible_messages_reasonable() {
        assert!(MAX_VISIBLE_MESSAGES > 0);
        assert!(MAX_VISIBLE_MESSAGES <= 50, "Too many visible messages would clutter the overlay");
    }

    #[test]
    fn test_chat_buffer_respects_max_size() {
        let mut buffer: Vec<String> = Vec::new();
        for i in 0..25 {
            buffer.push(format_mock_message(i));
            if buffer.len() > MAX_VISIBLE_MESSAGES {
                buffer.remove(0);
            }
        }
        assert_eq!(buffer.len(), MAX_VISIBLE_MESSAGES);
    }
}
