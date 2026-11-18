//! Logging in with the device code flow, and keeping the login fresh.

use std::io::Write;
use std::time::Duration;

use pyxeetb_twitch::Secret;
use pyxeetb_twitch::oauth::{self, DeviceCode, Poll, TokenGrant, Tokens, Validated};

use crate::endpoints::Endpoints;
use crate::error::{Context, Error, Result};
use crate::http::Http;
use crate::state::{TokenStore, unix_now};

/// Everything a login needs to talk to Twitch's OAuth server.
#[derive(Debug)]
pub struct Client<'a> {
    pub http: &'a Http,
    pub endpoints: &'a Endpoints,
    pub app_id: &'a str,
    /// Only a confidential app has one, and only a refresh needs it.
    pub app_secret: Option<&'a Secret>,
}

impl Client<'_> {
    /// Ask the user to approve this app in a browser, wait for them, and return the login.
    ///
    /// `sleep` is how the wait happens, so a test can skip it.
    ///
    /// # Errors
    /// When Twitch refuses, the user denies or does not answer in time, or the answers cannot be read.
    pub fn login(
        &self,
        scopes: &[&str],
        out: &mut dyn Write,
        sleep: &dyn Fn(Duration),
    ) -> Result<Tokens> {
        let code = self.request_device_code(scopes)?;
        writeln!(
            out,
            "\nTo log in, open this address on any device (your PC is fine):\n\n    {}\n",
            code.verification_uri
        )
        .context("printing the instructions")?;
        writeln!(
            out,
            "Sign in as the account the bot should use and check that the code reads {}.",
            code.user_code
        )
        .context("printing the instructions")?;
        writeln!(
            out,
            "Waiting for you to approve (up to {} minutes)...",
            code.expires_in / 60
        )
        .context("printing the instructions")?;
        out.flush().context("printing the instructions")?;

        let grant = self.wait_for_grant(&code, scopes, sleep)?;
        let identity = self.validate(&grant.access_token)?;
        Ok(Tokens::from_grant(&identity, grant, unix_now()))
    }

    fn request_device_code(&self, scopes: &[&str]) -> Result<DeviceCode> {
        let url = format!("{}/device", self.endpoints.id_base);
        let reply = self
            .http
            .post_form(&url, &oauth::device_form(self.app_id, scopes), &[])?;
        if reply.status != 200 {
            return Err(Error::new(format!(
                "Twitch refused the login request (HTTP {}): {}",
                reply.status,
                oauth::error_message(&reply.body)
            )));
        }
        serde_json::from_str(&reply.body).context("reading Twitch's login answer")
    }

    fn wait_for_grant(
        &self,
        code: &DeviceCode,
        scopes: &[&str],
        sleep: &dyn Fn(Duration),
    ) -> Result<TokenGrant> {
        let url = format!("{}/token", self.endpoints.id_base);
        let form = oauth::poll_form(self.app_id, scopes, &code.device_code, self.app_secret);
        let mut interval = code.interval;
        let mut waited = 0;
        while waited <= code.expires_in {
            sleep(Duration::from_secs(interval));
            waited += interval.max(1);
            let reply = self.http.post_form(&url, &form, &[])?;
            match oauth::classify_poll(reply.status, &reply.body) {
                Poll::Granted(grant) => return Ok(*grant),
                Poll::Pending => {}
                Poll::SlowDown => interval = oauth::next_interval(interval, true),
                Poll::Denied => return Err(Error::new("you chose not to authorise the app")),
                Poll::Expired => break,
                Poll::Failed(why) => return Err(Error::new(format!("the login failed: {why}"))),
            }
        }
        Err(Error::new(
            "the code expired before it was approved; run the command again",
        ))
    }

    /// Ask Twitch whose token this is and what it may do.
    ///
    /// # Errors
    /// When Twitch rejects the token or the answer cannot be read.
    pub fn validate(&self, access_token: &Secret) -> Result<Validated> {
        let url = format!("{}/validate", self.endpoints.id_base);
        let header = format!("OAuth {}", access_token.expose());
        let reply = self.http.get(&url, &[("Authorization", header.as_str())])?;
        if reply.status != 200 {
            return Err(Error::new(format!(
                "Twitch does not accept the token (HTTP {}): {}",
                reply.status,
                oauth::error_message(&reply.body)
            )));
        }
        serde_json::from_str(&reply.body).context("reading Twitch's answer about the token")
    }

    /// The tokens, renewed first if they are about to expire. A renewal is saved before it is returned,
    /// because Twitch invalidates the old refresh token the moment it issues the new one.
    ///
    /// # Errors
    /// When the tokens belong to another app, cannot be renewed, or the new ones cannot be saved.
    pub fn ensure_fresh(&self, store: &TokenStore, mut tokens: Tokens, now: u64) -> Result<Tokens> {
        if tokens.client_id != self.app_id {
            return Err(Error::new(format!(
                "the saved login belongs to another Twitch app ({}); run `pyxeetb auth` with this app's client id",
                tokens.client_id
            )));
        }
        if !tokens.needs_refresh(now) {
            return Ok(tokens);
        }
        let refresh = tokens
            .refresh_token
            .clone()
            .context("the login has expired and has no refresh token; run `pyxeetb auth` again")?;
        let url = format!("{}/token", self.endpoints.id_base);
        let reply = self.http.post_form(
            &url,
            &oauth::refresh_form(self.app_id, &refresh, self.app_secret),
            &[],
        )?;
        if reply.status != 200 {
            return Err(Error::new(format!(
                "Twitch rejected the saved login (HTTP {}: {}); run `pyxeetb auth` again",
                reply.status,
                oauth::error_message(&reply.body)
            )));
        }
        let grant: TokenGrant =
            serde_json::from_str(&reply.body).context("reading the renewed login")?;
        tokens.apply_refresh(grant, now);
        store.save(&tokens)?;
        Ok(tokens)
    }
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;
    use crate::testkit::{MockHttp, Request};

    const GRANT: &str = r#"{"access_token":"the-access","refresh_token":"the-refresh","expires_in":14400,"scope":["chat:read","user:read:chat","user:write:chat"],"token_type":"bearer"}"#;
    const WHO: &str = r#"{"client_id":"cid","login":"pylxyr","user_id":"42","scopes":["chat:read","user:read:chat","user:write:chat"],"expires_in":14400}"#;

    fn endpoints(server: &MockHttp) -> Endpoints {
        Endpoints {
            id_base: server.base(),
            ..Endpoints::default()
        }
    }

    fn temp_store(name: &str) -> TokenStore {
        TokenStore::new(std::env::temp_dir().join(format!(
            "pyxeetb-auth-{name}-{}-{}",
            std::process::id(),
            unix_now()
        )))
    }

    /// A mock OAuth server that says "pending" `pending` times, then grants.
    fn oauth_server(pending: usize, interval: u64) -> MockHttp {
        let polls = AtomicUsize::new(0);
        MockHttp::start(move |request: &Request| match request.path.as_str() {
            "/device" => (
                200,
                format!(
                    r#"{{"device_code":"dc","expires_in":1800,"interval":{interval},"user_code":"WXYZ","verification_uri":"https://www.twitch.tv/activate?device-code=WXYZ"}}"#
                ),
            ),
            "/token" if request.body.contains("device_code=dc") => {
                if polls.fetch_add(1, Ordering::SeqCst) < pending {
                    (
                        400,
                        r#"{"status":400,"message":"authorization_pending"}"#.to_owned(),
                    )
                } else {
                    (200, GRANT.to_owned())
                }
            }
            "/validate" if request.header("authorization") == Some("OAuth the-access") => {
                (200, WHO.to_owned())
            }
            _ => (404, r#"{"message":"unexpected"}"#.to_owned()),
        })
    }

    #[test]
    fn a_login_waits_through_pending_answers_and_returns_the_identity() {
        let server = oauth_server(2, 0);
        let (http, endpoints) = (Http::new(), endpoints(&server));
        let client = Client {
            http: &http,
            endpoints: &endpoints,
            app_id: "cid",
            app_secret: None,
        };
        let slept = RefCell::new(Vec::new());
        let mut out = Vec::new();
        let tokens = client
            .login(&["chat:read"], &mut out, &|d| slept.borrow_mut().push(d))
            .unwrap();
        assert_eq!(
            (
                tokens.login.as_str(),
                tokens.user_id.as_str(),
                tokens.access_token.expose()
            ),
            ("pylxyr", "42", "the-access")
        );
        assert_eq!(
            slept.borrow().len(),
            3,
            "two pending answers, then the grant"
        );
        let printed = String::from_utf8(out).unwrap();
        assert!(
            printed.contains("WXYZ") && printed.contains("activate?device-code=WXYZ"),
            "{printed}"
        );
        assert!(
            !printed.contains("the-access") && !printed.contains("the-refresh"),
            "tokens must never be printed"
        );
    }

    #[test]
    fn slow_down_lengthens_the_wait() {
        let polls = AtomicUsize::new(0);
        let server = MockHttp::start(move |request: &Request| {
            match request.path.as_str() {
            "/device" => (200, r#"{"device_code":"dc","expires_in":1800,"interval":5,"user_code":"U","verification_uri":"https://x"}"#.to_owned()),
            "/token" => match polls.fetch_add(1, Ordering::SeqCst) {
                0 => (400, r#"{"message":"slow_down"}"#.to_owned()),
                _ => (200, GRANT.to_owned()),
            },
            _ => (200, WHO.to_owned()),
        }
        });
        let (http, endpoints) = (Http::new(), endpoints(&server));
        let client = Client {
            http: &http,
            endpoints: &endpoints,
            app_id: "cid",
            app_secret: None,
        };
        let slept = RefCell::new(Vec::new());
        client
            .login(&[], &mut Vec::new(), &|d| {
                slept.borrow_mut().push(d.as_secs());
            })
            .unwrap();
        assert_eq!(*slept.borrow(), [5, 10]);
    }

    #[test]
    fn a_denied_or_expired_login_is_a_clear_error() {
        for (message, expected) in [
            ("access_denied", "chose not to authorise"),
            ("expired_token", "code expired"),
        ] {
            let server = MockHttp::start(move |request: &Request| {
                match request.path.as_str() {
                "/device" => (200, r#"{"device_code":"dc","expires_in":1800,"interval":0,"user_code":"U","verification_uri":"https://x"}"#.to_owned()),
                _ => (400, format!(r#"{{"message":"{message}"}}"#)),
            }
            });
            let (http, endpoints) = (Http::new(), endpoints(&server));
            let client = Client {
                http: &http,
                endpoints: &endpoints,
                app_id: "cid",
                app_secret: None,
            };
            let err = client.login(&[], &mut Vec::new(), &|_| {}).unwrap_err();
            assert!(err.to_string().contains(expected), "{err}");
        }
    }

    #[test]
    fn an_app_twitch_refuses_is_reported_with_twitchs_reason() {
        let server = MockHttp::start(|_: &Request| {
            (
                400,
                r#"{"status":400,"message":"invalid client"}"#.to_owned(),
            )
        });
        let (http, endpoints) = (Http::new(), endpoints(&server));
        let client = Client {
            http: &http,
            endpoints: &endpoints,
            app_id: "bad",
            app_secret: None,
        };
        let err = client.login(&[], &mut Vec::new(), &|_| {}).unwrap_err();
        assert_eq!(
            err.to_string(),
            "Twitch refused the login request (HTTP 400): invalid client"
        );
    }

    fn stale_tokens(expires_at: u64) -> Tokens {
        Tokens {
            client_id: "cid".into(),
            user_id: "42".into(),
            login: "pylxyr".into(),
            access_token: Secret::new("old-access"),
            refresh_token: Some(Secret::new("old-refresh")),
            scopes: vec!["chat:read".into()],
            expires_at,
        }
    }

    #[test]
    fn a_token_far_from_expiry_is_left_alone_without_any_request() {
        let server = MockHttp::start(|_: &Request| (500, String::new()));
        let (http, endpoints) = (Http::new(), endpoints(&server));
        let client = Client {
            http: &http,
            endpoints: &endpoints,
            app_id: "cid",
            app_secret: None,
        };
        let store = temp_store("fresh");
        let tokens = client
            .ensure_fresh(&store, stale_tokens(1_000_000), 1_000)
            .unwrap();
        assert_eq!(tokens.access_token.expose(), "old-access");
        assert_eq!(store.load().unwrap(), None, "nothing was written");
    }

    #[test]
    fn an_expiring_token_is_renewed_with_the_secret_and_saved_first() {
        let server = MockHttp::start(|request: &Request| {
            assert!(
                request.body.contains("grant_type=refresh_token")
                    && request.body.contains("refresh_token=old-refresh")
            );
            assert!(
                request.body.contains("client_secret=shh"),
                "a confidential app sends its secret: {}",
                request.body
            );
            (200, r#"{"access_token":"new-access","refresh_token":"new-refresh","expires_in":14400,"scope":["chat:read"]}"#.to_owned())
        });
        let (http, endpoints) = (Http::new(), endpoints(&server));
        let secret = Secret::new("shh");
        let client = Client {
            http: &http,
            endpoints: &endpoints,
            app_id: "cid",
            app_secret: Some(&secret),
        };
        let store = temp_store("renew");
        let tokens = client
            .ensure_fresh(&store, stale_tokens(1_100), 1_000)
            .unwrap();
        assert_eq!(
            (tokens.access_token.expose(), tokens.expires_at),
            ("new-access", 15_400)
        );
        assert_eq!(
            store
                .load()
                .unwrap()
                .unwrap()
                .refresh_token
                .unwrap()
                .expose(),
            "new-refresh"
        );
        std::fs::remove_dir_all(store.path().parent().unwrap()).unwrap();
    }

    #[test]
    fn a_rejected_refresh_tells_the_user_to_log_in_again_and_saves_nothing() {
        let server = MockHttp::start(|_: &Request| {
            (
                400,
                r#"{"status":400,"message":"Invalid refresh token"}"#.to_owned(),
            )
        });
        let (http, endpoints) = (Http::new(), endpoints(&server));
        let client = Client {
            http: &http,
            endpoints: &endpoints,
            app_id: "cid",
            app_secret: None,
        };
        let store = temp_store("rejected");
        let err = client
            .ensure_fresh(&store, stale_tokens(0), 1_000)
            .unwrap_err();
        assert!(
            err.to_string().contains("Invalid refresh token")
                && err.to_string().contains("pyxeetb auth"),
            "{err}"
        );
        assert_eq!(store.load().unwrap(), None);
    }

    #[test]
    fn tokens_of_another_app_and_tokens_without_a_refresh_token_are_refused() {
        let server = MockHttp::start(|_: &Request| (500, String::new()));
        let (http, endpoints) = (Http::new(), endpoints(&server));
        let client = Client {
            http: &http,
            endpoints: &endpoints,
            app_id: "cid",
            app_secret: None,
        };
        let store = temp_store("refused");
        let mut other = stale_tokens(0);
        other.client_id = "another".into();
        assert!(
            client
                .ensure_fresh(&store, other, 1_000)
                .unwrap_err()
                .to_string()
                .contains("another Twitch app")
        );
        let mut bare = stale_tokens(0);
        bare.refresh_token = None;
        assert!(
            client
                .ensure_fresh(&store, bare, 1_000)
                .unwrap_err()
                .to_string()
                .contains("no refresh token")
        );
    }
}
