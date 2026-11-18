//! The HTTPS client: one place that sets the timeouts and returns the status and body, errors included.

use std::time::Duration;

use serde_json::Value;

use crate::error::{Context, Result};

const TIMEOUT: Duration = Duration::from_secs(20);

/// A finished request. Any HTTP status is a reply here: it is the caller who knows what a 400 means.
#[derive(Debug)]
pub struct Reply {
    pub status: u16,
    pub body: String,
}

#[derive(Debug, Clone)]
pub struct Http {
    agent: ureq::Agent,
}

impl Http {
    pub fn new() -> Self {
        let config = ureq::Agent::config_builder()
            .timeout_global(Some(TIMEOUT))
            .http_status_as_error(false)
            .user_agent(concat!("pyxeetb/", env!("CARGO_PKG_VERSION")))
            .build();
        Self {
            agent: config.into(),
        }
    }

    /// # Errors
    /// When the request cannot be made or the answer cannot be read.
    pub fn get(&self, url: &str, headers: &[(&str, &str)]) -> Result<Reply> {
        let mut request = self.agent.get(url);
        for (name, value) in headers {
            request = request.header(*name, *value);
        }
        finish(request.call().context("the request failed"))
    }

    /// # Errors
    /// When the request cannot be made or the answer cannot be read.
    pub fn post_form(
        &self,
        url: &str,
        form: &[(&str, String)],
        headers: &[(&str, &str)],
    ) -> Result<Reply> {
        let mut request = self.agent.post(url);
        for (name, value) in headers {
            request = request.header(*name, *value);
        }
        finish(
            request
                .send_form(form.iter().map(|(k, v)| (*k, v.as_str())))
                .context("the request failed"),
        )
    }

    /// # Errors
    /// When the request cannot be made or the answer cannot be read.
    pub fn post_json(&self, url: &str, body: &Value, headers: &[(&str, &str)]) -> Result<Reply> {
        let mut request = self.agent.post(url);
        for (name, value) in headers {
            request = request.header(*name, *value);
        }
        // Compact JSON: ureq's own `send_json` pretty-prints, which only adds bytes to every request.
        let text = serde_json::to_string(body).context("writing the request body")?;
        finish(
            request
                .content_type("application/json")
                .send(text.as_str())
                .context("the request failed"),
        )
    }
}

fn finish(response: Result<ureq::http::Response<ureq::Body>>) -> Result<Reply> {
    let mut response = response?;
    let status = response.status().as_u16();
    let body = response
        .body_mut()
        .read_to_string()
        .context("reading the answer")?;
    Ok(Reply { status, body })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testkit::{MockHttp, Request};

    #[test]
    fn a_get_returns_status_and_body_even_for_an_error_status() {
        let server = MockHttp::start(|request: &Request| match request.path.as_str() {
            "/ok" => (200, r#"{"fine":true}"#.to_owned()),
            _ => (404, r#"{"message":"nope"}"#.to_owned()),
        });
        let http = Http::new();
        let ok = http.get(&server.url("/ok"), &[]).unwrap();
        assert_eq!((ok.status, ok.body.as_str()), (200, r#"{"fine":true}"#));
        let missing = http.get(&server.url("/missing"), &[]).unwrap();
        assert_eq!(
            (missing.status, missing.body.as_str()),
            (404, r#"{"message":"nope"}"#)
        );
    }

    #[test]
    fn headers_and_form_and_json_bodies_arrive_intact() {
        let server = MockHttp::start(|request: &Request| {
            (
                200,
                format!(
                    "{}|{}|{}",
                    request.method,
                    request.header("x-test").unwrap_or("-"),
                    request.body
                ),
            )
        });
        let http = Http::new();
        let form = http
            .post_form(
                &server.url("/f"),
                &[("a", "1 2".to_owned()), ("b", "x&y".to_owned())],
                &[("X-Test", "yes")],
            )
            .unwrap();
        assert_eq!(form.body, "POST|yes|a=1+2&b=x%26y");
        let json = http
            .post_json(&server.url("/j"), &serde_json::json!({"k": "v"}), &[])
            .unwrap();
        assert_eq!(json.body, r#"POST|-|{"k":"v"}"#);
    }

    #[test]
    fn a_refused_connection_is_an_error() {
        let port = std::net::TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port();
        assert!(
            Http::new()
                .get(&format!("http://127.0.0.1:{port}/"), &[])
                .is_err()
        );
    }
}
