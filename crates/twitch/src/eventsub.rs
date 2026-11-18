//! EventSub over WebSocket: what the frames mean. No I/O.
//!
//! A session starts with `session_welcome`; the subscriptions must be created within ten seconds of it, and
//! the connection is considered dead if nothing (not even a keepalive) arrives for the announced timeout.

use serde::Deserialize;

pub const WS_URL: &str = "wss://eventsub.wss.twitch.tv/ws";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChatMessage {
    pub message_id: String,
    pub broadcaster_user_id: String,
    pub chatter_user_id: String,
    pub chatter_login: String,
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Frame {
    Welcome {
        session_id: String,
        keepalive_timeout_secs: u64,
    },
    Keepalive,
    Chat(ChatMessage),
    /// A notification for a subscription this code does not read.
    OtherNotification {
        subscription_type: String,
    },
    /// Twitch asks the client to move to a new URL before the old connection closes.
    Reconnect {
        url: String,
    },
    Revocation {
        subscription_type: String,
        status: String,
    },
    Unknown {
        message_type: String,
    },
}

#[derive(Debug, PartialEq, Eq)]
pub struct FrameError(String);

impl std::fmt::Display for FrameError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "unreadable EventSub frame: {}", self.0)
    }
}

impl std::error::Error for FrameError {}

#[derive(Deserialize)]
struct Envelope {
    metadata: Metadata,
    #[serde(default)]
    payload: Payload,
}

#[derive(Deserialize)]
struct Metadata {
    message_type: String,
    #[serde(default)]
    subscription_type: Option<String>,
}

#[derive(Default, Deserialize)]
struct Payload {
    #[serde(default)]
    session: Option<Session>,
    #[serde(default)]
    subscription: Option<Subscription>,
    /// Kept raw: only the subscription types this code reads are parsed further.
    #[serde(default)]
    event: Option<serde_json::Value>,
}

#[derive(Deserialize)]
struct Session {
    id: String,
    #[serde(default)]
    keepalive_timeout_seconds: Option<u64>,
    #[serde(default)]
    reconnect_url: Option<String>,
}

#[derive(Deserialize)]
struct Subscription {
    #[serde(rename = "type", default)]
    kind: String,
    #[serde(default)]
    status: String,
}

#[derive(Deserialize)]
struct ChatEvent {
    message_id: String,
    broadcaster_user_id: String,
    chatter_user_id: String,
    chatter_user_login: String,
    message: ChatText,
}

#[derive(Deserialize)]
struct ChatText {
    text: String,
}

/// Twitch's default when a welcome does not say.
const DEFAULT_KEEPALIVE_SECS: u64 = 10;

/// Read one text frame of the EventSub WebSocket.
///
/// # Errors
/// When the text is not an EventSub frame or lacks what its type requires.
pub fn parse_frame(text: &str) -> Result<Frame, FrameError> {
    let Envelope { metadata, payload } =
        serde_json::from_str(text).map_err(|e| FrameError(e.to_string()))?;
    let need = |what: &str| FrameError(format!("{} without {what}", metadata.message_type));
    Ok(match metadata.message_type.as_str() {
        "session_welcome" => {
            let session = payload.session.ok_or_else(|| need("a session"))?;
            Frame::Welcome {
                session_id: session.id,
                keepalive_timeout_secs: session
                    .keepalive_timeout_seconds
                    .unwrap_or(DEFAULT_KEEPALIVE_SECS),
            }
        }
        "session_keepalive" => Frame::Keepalive,
        "session_reconnect" => {
            let url = payload
                .session
                .and_then(|s| s.reconnect_url)
                .ok_or_else(|| need("a reconnect_url"))?;
            Frame::Reconnect { url }
        }
        "notification" => {
            let kind = metadata
                .subscription_type
                .clone()
                .or_else(|| payload.subscription.map(|s| s.kind))
                .unwrap_or_default();
            if kind == "channel.chat.message" {
                let raw = payload.event.ok_or_else(|| need("an event"))?;
                let e: ChatEvent = serde_json::from_value(raw)
                    .map_err(|e| FrameError(format!("chat event: {e}")))?;
                Frame::Chat(ChatMessage {
                    message_id: e.message_id,
                    broadcaster_user_id: e.broadcaster_user_id,
                    chatter_user_id: e.chatter_user_id,
                    chatter_login: e.chatter_user_login,
                    text: e.message.text,
                })
            } else {
                Frame::OtherNotification {
                    subscription_type: kind,
                }
            }
        }
        "revocation" => {
            let s = payload.subscription.ok_or_else(|| need("a subscription"))?;
            Frame::Revocation {
                subscription_type: s.kind,
                status: s.status,
            }
        }
        other => Frame::Unknown {
            message_type: other.to_owned(),
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const WELCOME: &str = r#"{"metadata":{"message_id":"m1","message_type":"session_welcome","message_timestamp":"2026-11-17T00:00:00Z"},
        "payload":{"session":{"id":"AQoQ","status":"connected","keepalive_timeout_seconds":10,"reconnect_url":null,"connected_at":"2026-11-17T00:00:00Z"}}}"#;

    #[test]
    fn a_welcome_gives_the_session_and_the_timeout() {
        assert_eq!(
            parse_frame(WELCOME),
            Ok(Frame::Welcome {
                session_id: "AQoQ".into(),
                keepalive_timeout_secs: 10
            })
        );
    }

    #[test]
    fn a_welcome_without_a_timeout_uses_the_default() {
        let text =
            r#"{"metadata":{"message_type":"session_welcome"},"payload":{"session":{"id":"x"}}}"#;
        assert_eq!(
            parse_frame(text),
            Ok(Frame::Welcome {
                session_id: "x".into(),
                keepalive_timeout_secs: 10
            })
        );
    }

    #[test]
    fn keepalives_carry_nothing() {
        assert_eq!(
            parse_frame(r#"{"metadata":{"message_type":"session_keepalive"},"payload":{}}"#),
            Ok(Frame::Keepalive)
        );
    }

    #[test]
    fn a_chat_notification_is_read_in_full() {
        let text = r#"{"metadata":{"message_type":"notification","subscription_type":"channel.chat.message"},
            "payload":{"subscription":{"type":"channel.chat.message","status":"enabled"},
            "event":{"broadcaster_user_id":"1","broadcaster_user_login":"b","chatter_user_id":"2","chatter_user_login":"c",
                     "message_id":"id-9","message":{"text":"hello there","fragments":[]}}}}"#;
        assert_eq!(
            parse_frame(text),
            Ok(Frame::Chat(ChatMessage {
                message_id: "id-9".into(),
                broadcaster_user_id: "1".into(),
                chatter_user_id: "2".into(),
                chatter_login: "c".into(),
                text: "hello there".into(),
            }))
        );
    }

    #[test]
    fn other_notifications_are_named_not_parsed() {
        let text = r#"{"metadata":{"message_type":"notification","subscription_type":"channel.follow"},"payload":{"event":{"anything":1}}}"#;
        assert_eq!(
            parse_frame(text),
            Ok(Frame::OtherNotification {
                subscription_type: "channel.follow".into()
            })
        );
    }

    #[test]
    fn reconnect_and_revocation_are_read() {
        let r = r#"{"metadata":{"message_type":"session_reconnect"},"payload":{"session":{"id":"s","reconnect_url":"wss://x/ws?y=1"}}}"#;
        assert_eq!(
            parse_frame(r),
            Ok(Frame::Reconnect {
                url: "wss://x/ws?y=1".into()
            })
        );
        let v = r#"{"metadata":{"message_type":"revocation"},"payload":{"subscription":{"type":"channel.chat.message","status":"authorization_revoked"}}}"#;
        assert_eq!(
            parse_frame(v),
            Ok(Frame::Revocation {
                subscription_type: "channel.chat.message".into(),
                status: "authorization_revoked".into()
            })
        );
    }

    #[test]
    fn unknown_types_and_broken_frames_do_not_panic() {
        assert_eq!(
            parse_frame(r#"{"metadata":{"message_type":"future_thing"},"payload":{}}"#),
            Ok(Frame::Unknown {
                message_type: "future_thing".into()
            })
        );
        assert!(parse_frame("not json").is_err());
        assert!(
            parse_frame(r#"{"metadata":{"message_type":"session_welcome"},"payload":{}}"#).is_err()
        );
        assert!(parse_frame(r#"{"metadata":{"message_type":"session_reconnect"},"payload":{"session":{"id":"s"}}}"#).is_err());
    }
}
