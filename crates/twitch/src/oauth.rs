//! Twitch OAuth with the device code flow: the messages, the stored tokens, and when a token needs renewing.
//!
//! The device flow suits a headless server: the bot prints a short code, you approve it in a browser on any
//! other machine, and the bot polls until it is granted. It works for public and confidential apps; only a
//! confidential app needs its client secret, and only to refresh.

use serde::{Deserialize, Deserializer, Serialize};

use crate::Secret;

pub const ID_BASE: &str = "https://id.twitch.tv/oauth2";

/// What the latency harness needs: read chat (EventSub), send chat (Helix) and read chat over IRC.
pub const LATENCY_SCOPES: &[&str] = &["user:read:chat", "user:write:chat", "chat:read"];

/// Renew a token this many seconds before it expires, so a request never races the expiry.
pub const REFRESH_MARGIN_SECS: u64 = 300;

const DEFAULT_POLL_INTERVAL_SECS: u64 = 5;

/// Twitch writes `scope` as a list; the standard allows a space-separated string. Accept both.
fn scopes<'de, D: Deserializer<'de>>(de: D) -> Result<Vec<String>, D::Error> {
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Either {
        List(Vec<String>),
        Text(String),
    }
    Ok(match Either::deserialize(de)? {
        Either::List(list) => list,
        Either::Text(text) => text.split_whitespace().map(str::to_owned).collect(),
    })
}

#[derive(Debug, Clone, Deserialize)]
pub struct DeviceCode {
    pub device_code: String,
    pub user_code: String,
    pub verification_uri: String,
    pub expires_in: u64,
    #[serde(default = "default_interval")]
    pub interval: u64,
}

const fn default_interval() -> u64 {
    DEFAULT_POLL_INTERVAL_SECS
}

/// A successful token response, from the device flow or from a refresh.
#[derive(Debug, Clone, Deserialize)]
pub struct TokenGrant {
    pub access_token: Secret,
    #[serde(default)]
    pub refresh_token: Option<Secret>,
    #[serde(default)]
    pub expires_in: Option<u64>,
    #[serde(default, deserialize_with = "scopes")]
    pub scope: Vec<String>,
}

/// The answer of `GET /oauth2/validate`: which user a token belongs to and what it may do.
#[derive(Debug, Clone, Deserialize)]
pub struct Validated {
    pub client_id: String,
    pub login: String,
    pub user_id: String,
    #[serde(default)]
    pub scopes: Vec<String>,
    #[serde(default)]
    pub expires_in: u64,
}

/// Everything kept on disk between runs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Tokens {
    pub client_id: String,
    pub user_id: String,
    pub login: String,
    pub access_token: Secret,
    pub refresh_token: Option<Secret>,
    pub scopes: Vec<String>,
    /// Unix seconds.
    pub expires_at: u64,
}

impl Tokens {
    /// Tokens for a freshly granted device flow, identified by what `validate` said about them.
    #[must_use]
    pub fn from_grant(identity: &Validated, grant: TokenGrant, now: u64) -> Self {
        let scopes = if grant.scope.is_empty() {
            identity.scopes.clone()
        } else {
            grant.scope
        };
        Self {
            client_id: identity.client_id.clone(),
            user_id: identity.user_id.clone(),
            login: identity.login.clone(),
            access_token: grant.access_token,
            refresh_token: grant.refresh_token,
            scopes,
            expires_at: now + grant.expires_in.unwrap_or(identity.expires_in),
        }
    }

    /// True when the access token is expired or about to be.
    #[must_use]
    pub fn needs_refresh(&self, now: u64) -> bool {
        self.expires_at.saturating_sub(now) <= REFRESH_MARGIN_SECS
    }

    /// Take a refresh response. Twitch issues a new refresh token and the old one stops working, so the
    /// caller must store the result before using it. A response without scopes keeps the known ones.
    pub fn apply_refresh(&mut self, grant: TokenGrant, now: u64) {
        self.access_token = grant.access_token;
        if grant.refresh_token.is_some() {
            self.refresh_token = grant.refresh_token;
        }
        if !grant.scope.is_empty() {
            self.scopes = grant.scope;
        }
        self.expires_at = now + grant.expires_in.unwrap_or(0);
    }

    /// The required scopes this token does not have; empty when it has them all.
    #[must_use]
    pub fn missing_scopes<'a>(&self, required: &[&'a str]) -> Vec<&'a str> {
        required
            .iter()
            .copied()
            .filter(|r| !self.scopes.iter().any(|s| s == r))
            .collect()
    }
}

/// What one poll of the token endpoint means.
#[derive(Debug)]
pub enum Poll {
    Granted(Box<TokenGrant>),
    /// The user has not approved yet: wait one interval and ask again.
    Pending,
    /// Asking too fast: wait longer from now on.
    SlowDown,
    Denied,
    Expired,
    Failed(String),
}

/// The human message of a Twitch error body, `{"status":400,"message":"..."}`, or the raw text.
#[must_use]
pub fn error_message(body: &str) -> String {
    serde_json::from_str::<serde_json::Value>(body)
        .ok()
        .and_then(|v| {
            v.get("message")
                .or_else(|| v.get("error_description"))
                .and_then(|m| m.as_str().map(str::to_owned))
        })
        .unwrap_or_else(|| body.trim().chars().take(200).collect())
}

#[must_use]
pub fn classify_poll(status: u16, body: &str) -> Poll {
    if status == 200 {
        return match serde_json::from_str::<TokenGrant>(body) {
            Ok(grant) => Poll::Granted(Box::new(grant)),
            Err(e) => Poll::Failed(format!("unreadable token response: {e}")),
        };
    }
    let message = error_message(body);
    let lower = message.to_ascii_lowercase();
    if lower.contains("authorization_pending") {
        Poll::Pending
    } else if lower.contains("slow_down") {
        Poll::SlowDown
    } else if lower.contains("access_denied") {
        Poll::Denied
    } else if lower.contains("expired") {
        Poll::Expired
    } else {
        Poll::Failed(format!("HTTP {status}: {message}"))
    }
}

/// The poll interval after an answer: a `slow_down` adds five seconds, as the device flow standard says.
#[must_use]
pub const fn next_interval(current: u64, slow_down: bool) -> u64 {
    if slow_down { current + 5 } else { current }
}

type Form = Vec<(&'static str, String)>;

#[must_use]
pub fn device_form(client_id: &str, scopes: &[&str]) -> Form {
    vec![
        ("client_id", client_id.to_owned()),
        ("scopes", scopes.join(" ")),
    ]
}

#[must_use]
pub fn poll_form(
    client_id: &str,
    scopes: &[&str],
    device_code: &str,
    client_secret: Option<&Secret>,
) -> Form {
    let mut form = vec![
        ("client_id", client_id.to_owned()),
        ("scopes", scopes.join(" ")),
        ("device_code", device_code.to_owned()),
        (
            "grant_type",
            "urn:ietf:params:oauth:grant-type:device_code".to_owned(),
        ),
    ];
    if let Some(secret) = client_secret {
        form.push(("client_secret", secret.expose().to_owned()));
    }
    form
}

#[must_use]
pub fn refresh_form(
    client_id: &str,
    refresh_token: &Secret,
    client_secret: Option<&Secret>,
) -> Form {
    let mut form = vec![
        ("client_id", client_id.to_owned()),
        ("grant_type", "refresh_token".to_owned()),
        ("refresh_token", refresh_token.expose().to_owned()),
    ];
    if let Some(secret) = client_secret {
        form.push(("client_secret", secret.expose().to_owned()));
    }
    form
}

#[cfg(test)]
mod tests {
    use super::*;

    fn identity() -> Validated {
        Validated {
            client_id: "cid".into(),
            login: "pylxyr".into(),
            user_id: "42".into(),
            scopes: vec!["chat:read".into()],
            expires_in: 14_000,
        }
    }

    fn grant(json: &str) -> TokenGrant {
        serde_json::from_str(json).unwrap()
    }

    #[test]
    fn a_device_code_response_is_read_with_a_default_interval() {
        let code: DeviceCode = serde_json::from_str(
            r#"{"device_code":"dc","expires_in":1800,"user_code":"ABCDEFGH","verification_uri":"https://www.twitch.tv/activate?device-code=ABCDEFGH"}"#,
        )
        .unwrap();
        assert_eq!(
            (code.user_code.as_str(), code.expires_in, code.interval),
            ("ABCDEFGH", 1800, 5)
        );
    }

    #[test]
    fn scope_is_accepted_as_a_list_or_as_text() {
        assert_eq!(
            grant(r#"{"access_token":"a","scope":["x","y"]}"#).scope,
            ["x", "y"]
        );
        assert_eq!(
            grant(r#"{"access_token":"a","scope":"x  y"}"#).scope,
            ["x", "y"]
        );
        assert_eq!(grant(r#"{"access_token":"a"}"#).scope, Vec::<String>::new());
    }

    #[test]
    fn a_grant_becomes_tokens_that_expire_at_the_right_second() {
        let t = Tokens::from_grant(
            &identity(),
            grant(
                r#"{"access_token":"a","refresh_token":"r","expires_in":3600,"scope":["chat:read","user:read:chat"]}"#,
            ),
            1_000,
        );
        assert_eq!(
            (t.expires_at, t.login.as_str(), t.user_id.as_str()),
            (4_600, "pylxyr", "42")
        );
        assert_eq!(t.scopes, ["chat:read", "user:read:chat"]);
        assert_eq!(t.refresh_token.as_ref().map(Secret::expose), Some("r"));
    }

    #[test]
    fn a_grant_without_scopes_or_expiry_falls_back_to_what_validate_said() {
        let t = Tokens::from_grant(&identity(), grant(r#"{"access_token":"a"}"#), 1_000);
        assert_eq!(
            (t.expires_at, t.scopes),
            (15_000, vec!["chat:read".to_string()])
        );
    }

    #[test]
    fn refresh_is_due_inside_the_margin_and_not_before() {
        let mut t = Tokens::from_grant(
            &identity(),
            grant(r#"{"access_token":"a","expires_in":1000}"#),
            0,
        );
        assert!(!t.needs_refresh(699));
        assert!(t.needs_refresh(700));
        assert!(t.needs_refresh(5_000), "long expired");
        t.expires_at = 0;
        assert!(t.needs_refresh(0));
    }

    #[test]
    fn a_refresh_replaces_the_tokens_and_keeps_what_the_response_omits() {
        let mut t = Tokens::from_grant(
            &identity(),
            grant(r#"{"access_token":"old","refresh_token":"r1","expires_in":10,"scope":["a"]}"#),
            0,
        );
        t.apply_refresh(
            grant(r#"{"access_token":"new","refresh_token":"r2","expires_in":7200}"#),
            100,
        );
        assert_eq!(t.access_token.expose(), "new");
        assert_eq!(t.refresh_token.as_ref().map(Secret::expose), Some("r2"));
        assert_eq!(
            (t.expires_at, t.scopes.as_slice()),
            (7_300, ["a".to_string()].as_slice())
        );
        t.apply_refresh(grant(r#"{"access_token":"newer","expires_in":60}"#), 200);
        assert_eq!(
            t.refresh_token.as_ref().map(Secret::expose),
            Some("r2"),
            "no new refresh token: keep the last"
        );
    }

    #[test]
    fn missing_scopes_are_listed() {
        let t = Tokens::from_grant(
            &identity(),
            grant(r#"{"access_token":"a","scope":["chat:read"]}"#),
            0,
        );
        assert_eq!(
            t.missing_scopes(LATENCY_SCOPES),
            ["user:read:chat", "user:write:chat"]
        );
        assert_eq!(t.missing_scopes(&["chat:read"]), Vec::<&str>::new());
    }

    #[test]
    fn tokens_survive_a_round_trip_through_json_and_never_print() {
        let t = Tokens::from_grant(
            &identity(),
            grant(r#"{"access_token":"sekrit","refresh_token":"sekrit2","expires_in":5}"#),
            0,
        );
        let json = serde_json::to_string(&t).unwrap();
        assert_eq!(serde_json::from_str::<Tokens>(&json).unwrap(), t);
        assert!(!format!("{t:?}").contains("sekrit"));
    }

    #[test]
    fn polling_answers_are_told_apart() {
        let ok = r#"{"access_token":"a","refresh_token":"r","expires_in":1,"scope":["x"],"token_type":"bearer"}"#;
        assert!(matches!(classify_poll(200, ok), Poll::Granted(_)));
        assert!(matches!(
            classify_poll(400, r#"{"status":400,"message":"authorization_pending"}"#),
            Poll::Pending
        ));
        assert!(matches!(
            classify_poll(400, r#"{"status":400,"message":"slow_down"}"#),
            Poll::SlowDown
        ));
        assert!(matches!(
            classify_poll(400, r#"{"status":400,"message":"access_denied"}"#),
            Poll::Denied
        ));
        assert!(matches!(
            classify_poll(400, r#"{"status":400,"message":"expired_token"}"#),
            Poll::Expired
        ));
        assert!(matches!(classify_poll(200, "not json"), Poll::Failed(_)));
        match classify_poll(401, r#"{"status":401,"message":"invalid client"}"#) {
            Poll::Failed(m) => assert_eq!(m, "HTTP 401: invalid client"),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn error_text_falls_back_to_the_raw_body() {
        assert_eq!(error_message(r#"{"message":"nope"}"#), "nope");
        assert_eq!(error_message("  plain text  "), "plain text");
        assert_eq!(error_message(&"x".repeat(500)).len(), 200);
    }

    #[test]
    fn the_poll_interval_only_grows_on_slow_down() {
        assert_eq!(next_interval(5, false), 5);
        assert_eq!(next_interval(5, true), 10);
    }

    #[test]
    fn forms_carry_the_secret_only_when_there_is_one() {
        let secret = Secret::new("shh");
        let with = poll_form("cid", LATENCY_SCOPES, "dc", Some(&secret));
        assert!(with.contains(&("client_secret", "shh".to_owned())));
        assert!(with.contains(&(
            "scopes",
            "user:read:chat user:write:chat chat:read".to_owned()
        )));
        assert!(
            !poll_form("cid", &[], "dc", None)
                .iter()
                .any(|(k, _)| *k == "client_secret")
        );
        let refresh = refresh_form("cid", &Secret::new("r"), None);
        assert!(refresh.contains(&("grant_type", "refresh_token".to_owned())));
        assert_eq!(
            device_form("cid", &["a", "b"]),
            [
                ("client_id", "cid".to_owned()),
                ("scopes", "a b".to_owned())
            ]
        );
    }
}
