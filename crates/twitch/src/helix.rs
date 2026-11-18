//! Helix (the REST API): the request bodies and answers the bot needs, with no I/O.

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

pub const HELIX_BASE: &str = "https://api.twitch.tv/helix";

/// Why Twitch refused to post a message that it otherwise accepted.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct DropReason {
    pub code: String,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct SentMessage {
    pub message_id: String,
    pub is_sent: bool,
    #[serde(default)]
    pub drop_reason: Option<DropReason>,
}

#[derive(Serialize)]
struct SendChat<'a> {
    broadcaster_id: &'a str,
    sender_id: &'a str,
    message: &'a str,
}

/// The JSON body of `POST /chat/messages`.
#[must_use]
pub fn send_chat_body(broadcaster_id: &str, sender_id: &str, message: &str) -> Value {
    json!(SendChat {
        broadcaster_id,
        sender_id,
        message
    })
}

#[derive(Debug, PartialEq, Eq)]
pub enum SendError {
    Unreadable(String),
    NoResult,
    Dropped(DropReason),
}

impl std::fmt::Display for SendError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unreadable(e) => write!(f, "unreadable answer: {e}"),
            Self::NoResult => f.write_str("the answer had no message in it"),
            Self::Dropped(d) => write!(
                f,
                "Twitch did not post the message: {} ({})",
                d.message, d.code
            ),
        }
    }
}

impl std::error::Error for SendError {}

/// The result of `POST /chat/messages`. A message Twitch accepted but did not post is an error here.
///
/// # Errors
/// When the answer cannot be read, holds no message, or says the message was dropped.
pub fn parse_send_response(body: &str) -> Result<SentMessage, SendError> {
    #[derive(Deserialize)]
    struct Answer {
        data: Vec<SentMessage>,
    }
    let answer: Answer =
        serde_json::from_str(body).map_err(|e| SendError::Unreadable(e.to_string()))?;
    let sent = answer.data.into_iter().next().ok_or(SendError::NoResult)?;
    match (sent.is_sent, sent.drop_reason.clone()) {
        (false, Some(reason)) => Err(SendError::Dropped(reason)),
        (false, None) => Err(SendError::Dropped(DropReason {
            code: "unknown".into(),
            message: "no reason given".into(),
        })),
        (true, _) => Ok(sent),
    }
}

/// The body that subscribes a WebSocket session to chat messages of one channel as one user.
#[must_use]
pub fn chat_message_subscription(broadcaster_id: &str, user_id: &str, session_id: &str) -> Value {
    json!({
        "type": "channel.chat.message",
        "version": "1",
        "condition": { "broadcaster_user_id": broadcaster_id, "user_id": user_id },
        "transport": { "method": "websocket", "session_id": session_id },
    })
}

/// The id of the first user in a `GET /users` answer.
#[must_use]
pub fn parse_user_id(body: &str) -> Option<String> {
    let value: Value = serde_json::from_str(body).ok()?;
    value
        .get("data")?
        .get(0)?
        .get("id")?
        .as_str()
        .map(str::to_owned)
}

/// A Helix error body is `{"error":"Unauthorized","status":401,"message":"..."}`.
#[must_use]
pub fn error_text(status: u16, body: &str) -> String {
    let detail = serde_json::from_str::<Value>(body)
        .ok()
        .and_then(|v| v.get("message").and_then(Value::as_str).map(str::to_owned))
        .unwrap_or_else(|| body.trim().chars().take(200).collect());
    format!("HTTP {status}: {detail}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_send_body_has_exactly_the_three_fields() {
        let body = send_chat_body("1", "2", "hello");
        assert_eq!(
            body,
            json!({"broadcaster_id":"1","sender_id":"2","message":"hello"})
        );
    }

    #[test]
    fn a_sent_message_gives_its_id() {
        let sent =
            parse_send_response(r#"{"data":[{"message_id":"abc-123","is_sent":true}]}"#).unwrap();
        assert_eq!(sent.message_id, "abc-123");
    }

    #[test]
    fn a_dropped_message_is_an_error_with_the_reason() {
        let err = parse_send_response(
            r#"{"data":[{"message_id":"","is_sent":false,"drop_reason":{"code":"msg_duplicate","message":"duplicate"}}]}"#,
        )
        .unwrap_err();
        assert_eq!(
            err.to_string(),
            "Twitch did not post the message: duplicate (msg_duplicate)"
        );
        assert!(parse_send_response(r#"{"data":[{"message_id":"","is_sent":false}]}"#).is_err());
    }

    #[test]
    fn empty_and_broken_answers_are_errors() {
        assert_eq!(
            parse_send_response(r#"{"data":[]}"#),
            Err(SendError::NoResult)
        );
        assert!(matches!(
            parse_send_response("nope"),
            Err(SendError::Unreadable(_))
        ));
    }

    #[test]
    fn the_subscription_names_the_session_and_both_users() {
        let body = chat_message_subscription("10", "20", "sess");
        assert_eq!(body["type"], "channel.chat.message");
        assert_eq!(body["condition"]["broadcaster_user_id"], "10");
        assert_eq!(body["condition"]["user_id"], "20");
        assert_eq!(
            body["transport"],
            json!({"method":"websocket","session_id":"sess"})
        );
    }

    #[test]
    fn a_user_lookup_gives_the_first_id() {
        assert_eq!(
            parse_user_id(r#"{"data":[{"id":"77","login":"x"},{"id":"78"}]}"#).as_deref(),
            Some("77")
        );
        assert_eq!(parse_user_id(r#"{"data":[]}"#), None);
        assert_eq!(parse_user_id("nope"), None);
        assert_eq!(
            parse_user_id(r#"{"data":[{"id":5}]}"#),
            None,
            "ids are strings"
        );
    }

    #[test]
    fn error_bodies_are_summarised() {
        assert_eq!(
            error_text(
                401,
                r#"{"error":"Unauthorized","status":401,"message":"Invalid OAuth token"}"#
            ),
            "HTTP 401: Invalid OAuth token"
        );
        assert_eq!(error_text(502, " bad gateway "), "HTTP 502: bad gateway");
    }
}
