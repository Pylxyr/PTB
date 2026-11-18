//! `pyxeetb latency`: how long a chat message really takes, measured end to end.
//!
//! The bot posts a few short messages through Helix and watches for each one to come back over two routes at
//! once, IRC and EventSub. Everything is timed on this machine's own clock from the moment the send request
//! starts, so no clocks need to agree. It answers which route delivers first, by how much, and what a reply
//! costs from this server. Messages are matched by the id Twitch gives them.

use std::collections::HashMap;
use std::io::{self, Read, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::thread;
use std::time::{Duration, Instant};

use pyxeetb_core::report::Report;
use pyxeetb_core::stats;
use pyxeetb_twitch::eventsub::{self, Frame};
use pyxeetb_twitch::helix;
use pyxeetb_twitch::irc;
use pyxeetb_twitch::oauth::Tokens;
use tungstenite::Message;

use crate::endpoints::Endpoints;
use crate::error::{Context, Error, Result};
use crate::http::Http;
use crate::net;

const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
/// How often a receiver looks up from its socket to check whether it should stop.
const POLL: Duration = Duration::from_millis(250);
const READY_TIMEOUT: Duration = Duration::from_secs(20);
const ARRIVAL_TIMEOUT: Duration = Duration::from_secs(8);

/// Everything one measurement run needs.
#[derive(Debug)]
pub struct Plan<'a> {
    pub endpoints: &'a Endpoints,
    pub http: &'a Http,
    pub tokens: &'a Tokens,
    pub channel: &'a str,
    pub broadcaster_id: &'a str,
    pub count: usize,
    pub gap: Duration,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Via {
    Irc,
    EventSub,
}

impl Via {
    const fn label(self) -> &'static str {
        match self {
            Self::Irc => "IRC",
            Self::EventSub => "EventSub",
        }
    }
}

enum Signal {
    Ready(Via),
    Failed(Via, String),
    Arrived {
        via: Via,
        message_id: String,
        at: Instant,
    },
}

fn is_timeout(error: &io::Error) -> bool {
    matches!(
        error.kind(),
        io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
    )
}

fn emit(tx: &Sender<Signal>, signal: Signal) {
    // The receiver is gone only when the run is over, and then nobody needs the signal.
    let _ = tx.send(signal);
}

fn bearer(tokens: &Tokens) -> String {
    format!("Bearer {}", tokens.access_token.expose())
}

// ---- IRC ----------------------------------------------------------------------------------------------------

fn irc_receiver(plan: &Plan<'_>, tx: &Sender<Signal>, stop: &AtomicBool) {
    if let Err(e) = irc_session(plan, tx, stop) {
        emit(tx, Signal::Failed(Via::Irc, e.to_string()));
    }
}

fn irc_session(plan: &Plan<'_>, tx: &Sender<Signal>, stop: &AtomicBool) -> Result<()> {
    let endpoints = plan.endpoints;
    let tls = endpoints.irc_tls.then(net::tls_config).transpose()?;
    let mut conn = net::connect(
        &endpoints.irc_host,
        endpoints.irc_port,
        tls.as_ref(),
        CONNECT_TIMEOUT,
    )?;
    let login = irc::login_lines(&plan.tokens.login, &plan.tokens.access_token, plan.channel);
    conn.write_all(login.as_bytes())
        .context("logging in to IRC")?;
    conn.flush().context("logging in to IRC")?;
    conn.set_read_timeout(Some(POLL))
        .context("setting the IRC read timeout")?;

    let mut lines = irc::LineBuffer::default();
    let mut buf = [0u8; 4096];
    let mut ready = false;
    while !stop.load(Ordering::Relaxed) {
        let n = match conn.read(&mut buf) {
            Ok(0) => return Err(Error::new("the IRC connection was closed")),
            Ok(n) => n,
            Err(e) if is_timeout(&e) => continue,
            Err(e) => return Err(e).context("reading from IRC"),
        };
        let at = Instant::now();
        for line in lines.push(&buf[..n]) {
            let Some(message) = irc::parse(&line) else {
                continue;
            };
            if let Some(why) = message.login_failure() {
                return Err(Error::new(format!("IRC login failed: {why}")));
            }
            if let Some(pong) = irc::pong_for(&message) {
                conn.write_all(pong.as_bytes())
                    .context("answering a PING")?;
            }
            if !ready && message.is_join_done() {
                ready = true;
                emit(tx, Signal::Ready(Via::Irc));
            }
            if let Some(chat) = message.as_privmsg() {
                emit(
                    tx,
                    Signal::Arrived {
                        via: Via::Irc,
                        message_id: chat.id,
                        at,
                    },
                );
            }
        }
    }
    Ok(())
}

// ---- EventSub -----------------------------------------------------------------------------------------------

/// Host, port and whether to use TLS for a `ws://` or `wss://` address.
fn parse_ws_url(url: &str) -> Result<(String, u16, bool)> {
    let uri: tungstenite::http::Uri = url.parse().context("reading the EventSub address")?;
    let tls = match uri.scheme_str() {
        Some("wss") => true,
        Some("ws") => false,
        other => {
            return Err(Error::new(format!(
                "the EventSub address must start with ws:// or wss://, not {other:?}"
            )));
        }
    };
    let host = uri
        .host()
        .context("the EventSub address has no host")?
        .to_owned();
    Ok((
        host,
        uri.port_u16().unwrap_or(if tls { 443 } else { 80 }),
        tls,
    ))
}

fn eventsub_receiver(plan: &Plan<'_>, tx: &Sender<Signal>, stop: &AtomicBool) {
    if let Err(e) = eventsub_session(plan, tx, stop) {
        emit(tx, Signal::Failed(Via::EventSub, e.to_string()));
    }
}

fn subscribe(plan: &Plan<'_>, session_id: &str) -> Result<()> {
    let url = format!("{}/eventsub/subscriptions", plan.endpoints.helix_base);
    let body =
        helix::chat_message_subscription(plan.broadcaster_id, &plan.tokens.user_id, session_id);
    let auth = bearer(plan.tokens);
    let reply = plan.http.post_json(
        &url,
        &body,
        &[
            ("Authorization", auth.as_str()),
            ("Client-Id", plan.tokens.client_id.as_str()),
        ],
    )?;
    if reply.status == 200 || reply.status == 202 {
        Ok(())
    } else {
        Err(Error::new(format!(
            "subscribing to chat messages failed: {}",
            helix::error_text(reply.status, &reply.body)
        )))
    }
}

fn eventsub_session(plan: &Plan<'_>, tx: &Sender<Signal>, stop: &AtomicBool) -> Result<()> {
    let url = plan.endpoints.eventsub_url.as_str();
    let (host, port, tls) = parse_ws_url(url)?;
    let tls = tls.then(net::tls_config).transpose()?;
    let conn = net::connect(&host, port, tls.as_ref(), CONNECT_TIMEOUT)?;
    let (mut socket, _) = tungstenite::client(url, conn)
        .map_err(|e| Error::new(format!("the WebSocket handshake failed: {e}")))?;

    let mut subscribed = false;
    while !stop.load(Ordering::Relaxed) {
        let text = match socket.read() {
            Ok(Message::Text(text)) => text,
            Ok(Message::Close(frame)) => {
                return Err(Error::new(format!(
                    "EventSub closed the connection: {frame:?}"
                )));
            }
            Ok(_) => continue,
            Err(tungstenite::Error::Io(e)) if is_timeout(&e) => continue,
            Err(e) => return Err(e).context("reading from EventSub"),
        };
        let at = Instant::now();
        match eventsub::parse_frame(text.as_str()).context("reading an EventSub frame")? {
            Frame::Welcome { session_id, .. } if !subscribed => {
                subscribe(plan, &session_id)?;
                subscribed = true;
                emit(tx, Signal::Ready(Via::EventSub));
                socket
                    .get_ref()
                    .set_read_timeout(Some(POLL))
                    .context("setting the EventSub read timeout")?;
            }
            Frame::Chat(chat) => emit(
                tx,
                Signal::Arrived {
                    via: Via::EventSub,
                    message_id: chat.message_id,
                    at,
                },
            ),
            Frame::Reconnect { url } => {
                return Err(Error::new(format!("EventSub asked to reconnect to {url}")));
            }
            Frame::Revocation {
                subscription_type,
                status,
            } => {
                return Err(Error::new(format!(
                    "EventSub revoked {subscription_type}: {status}"
                )));
            }
            Frame::Welcome { .. }
            | Frame::Keepalive
            | Frame::OtherNotification { .. }
            | Frame::Unknown { .. } => {}
        }
    }
    Ok(())
}

// ---- sending and collecting ---------------------------------------------------------------------------------

/// One test message as seen from this machine. Every value is in milliseconds.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct Row {
    /// How long the Helix send request took, start to answer.
    pub http: f64,
    /// From the start of the send request until the message came back over each route.
    pub irc: Option<f64>,
    pub eventsub: Option<f64>,
}

#[derive(Debug, Default)]
pub struct Outcome {
    pub rows: Vec<Row>,
    pub problems: Vec<String>,
}

#[derive(Default)]
struct Seen {
    irc: Option<Instant>,
    eventsub: Option<Instant>,
}

/// What the receivers have reported so far.
struct Collector {
    rx: Receiver<Signal>,
    seen: HashMap<String, Seen>,
    up: Vec<Via>,
    problems: Vec<String>,
}

impl Collector {
    fn handle(&mut self, signal: Signal) {
        match signal {
            Signal::Ready(via) => self.up.push(via),
            Signal::Failed(via, why) => {
                self.up.retain(|v| *v != via);
                self.problems
                    .push(format!("{} stopped: {why}", via.label()));
            }
            Signal::Arrived {
                via,
                message_id,
                at,
            } => {
                let slot = self.seen.entry(message_id).or_default();
                match via {
                    Via::Irc => slot.irc.get_or_insert(at),
                    Via::EventSub => slot.eventsub.get_or_insert(at),
                };
            }
        }
    }

    /// Process whatever is waiting, without blocking.
    fn drain(&mut self) {
        while let Ok(signal) = self.rx.try_recv() {
            self.handle(signal);
        }
    }

    /// Wait for one more signal until `deadline`; false when the time ran out.
    fn wait_one(&mut self, deadline: Instant) -> bool {
        match self
            .rx
            .recv_timeout(deadline.saturating_duration_since(Instant::now()))
        {
            Ok(signal) => {
                self.handle(signal);
                true
            }
            Err(RecvTimeoutError::Timeout | RecvTimeoutError::Disconnected) => false,
        }
    }

    /// Wait until both routes are up or reported failing, or the time is out.
    fn await_ready(&mut self) {
        let deadline = Instant::now() + READY_TIMEOUT;
        let settled = |c: &Self| {
            [Via::Irc, Via::EventSub]
                .iter()
                .all(|v| c.up.contains(v) || c.failed(*v))
        };
        while !settled(self) && self.wait_one(deadline) {}
        for via in [Via::Irc, Via::EventSub] {
            if !self.up.contains(&via) && !self.failed(via) {
                self.problems.push(format!(
                    "{} did not become ready within {} s",
                    via.label(),
                    READY_TIMEOUT.as_secs()
                ));
            }
        }
    }

    fn failed(&self, via: Via) -> bool {
        let prefix = format!("{} stopped", via.label());
        self.problems.iter().any(|p| p.starts_with(&prefix))
    }

    /// Wait until the message has come back over every route that is up, or the time is out.
    fn await_arrivals(&mut self, id: &str) {
        let deadline = Instant::now() + ARRIVAL_TIMEOUT;
        loop {
            self.drain();
            let complete = self.seen.get(id).is_some_and(|s| {
                (s.irc.is_some() || !self.up.contains(&Via::Irc))
                    && (s.eventsub.is_some() || !self.up.contains(&Via::EventSub))
            });
            if complete || !self.wait_one(deadline) {
                return;
            }
        }
    }
}

fn send_message(plan: &Plan<'_>, text: &str) -> Result<(String, Duration)> {
    let url = format!("{}/chat/messages", plan.endpoints.helix_base);
    let body = helix::send_chat_body(plan.broadcaster_id, &plan.tokens.user_id, text);
    let auth = bearer(plan.tokens);
    let started = Instant::now();
    let reply = plan.http.post_json(
        &url,
        &body,
        &[
            ("Authorization", auth.as_str()),
            ("Client-Id", plan.tokens.client_id.as_str()),
        ],
    )?;
    let took = started.elapsed();
    if reply.status != 200 {
        return Err(Error::new(format!(
            "sending failed: {}",
            helix::error_text(reply.status, &reply.body)
        )));
    }
    let sent = helix::parse_send_response(&reply.body).context("sending failed")?;
    Ok((sent.message_id, took))
}

fn row_for(started: Instant, took: Duration, seen: Option<&Seen>) -> Row {
    let since = |at: Option<Instant>| {
        at.map(|t| t.saturating_duration_since(started).as_secs_f64() * 1000.0)
    };
    Row {
        http: took.as_secs_f64() * 1000.0,
        irc: seen.and_then(|s| since(s.irc)),
        eventsub: seen.and_then(|s| since(s.eventsub)),
    }
}

fn nonce() -> u32 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.subsec_micros())
}

fn measure(plan: &Plan<'_>, collector: &mut Collector) -> Vec<Row> {
    collector.await_ready();
    let mut rows = Vec::new();
    if collector.up.is_empty() {
        collector
            .problems
            .push("neither route is up, so there is nothing to measure".to_owned());
        return rows;
    }
    for n in 1..=plan.count {
        // A unique suffix, because Twitch silently drops a message identical to the last one.
        let text = format!(
            "pyxeetb latency probe {n}/{} #{:06}",
            plan.count,
            nonce() % 1_000_000
        );
        let started = Instant::now();
        match send_message(plan, &text) {
            Ok((id, took)) => {
                collector.await_arrivals(&id);
                rows.push(row_for(started, took, collector.seen.get(&id)));
            }
            Err(e) => {
                collector.problems.push(format!("message {n}: {e}"));
                break;
            }
        }
        if n < plan.count {
            thread::sleep(plan.gap);
        }
    }
    rows
}

/// Send the test messages and collect how they came back.
pub fn run(plan: &Plan<'_>) -> Outcome {
    let (tx, rx) = mpsc::channel();
    let stop = AtomicBool::new(false);
    let mut collector = Collector {
        rx,
        seen: HashMap::new(),
        up: Vec::new(),
        problems: Vec::new(),
    };
    let rows = thread::scope(|scope| {
        scope.spawn(|| irc_receiver(plan, &tx, &stop));
        scope.spawn(|| eventsub_receiver(plan, &tx, &stop));
        let rows = measure(plan, &mut collector);
        stop.store(true, Ordering::Relaxed);
        rows
    });
    collector.drain();
    Outcome {
        rows,
        problems: collector.problems,
    }
}

// ---- the report ---------------------------------------------------------------------------------------------

fn column(rows: &[Row], pick: impl Fn(&Row) -> Option<f64>) -> Vec<f64> {
    rows.iter().filter_map(pick).collect()
}

/// Which route delivered first, over the messages that came back on both.
fn first_counts(rows: &[Row]) -> (usize, usize) {
    rows.iter()
        .filter_map(|r| r.irc.zip(r.eventsub))
        .fold(
            (0, 0),
            |(irc, es), (a, b)| if a <= b { (irc + 1, es) } else { (irc, es + 1) },
        )
}

/// The measurement as a report to paste back.
pub fn build_report(plan: &Plan<'_>, outcome: &Outcome) -> Report {
    let mut report = Report::new(format!("PyxeeTB latency {}", pyxeetb_core::VERSION));
    let rows = &outcome.rows;
    report
        .section("Setup")
        .kv(
            "account",
            format!("{} (id {})", plan.tokens.login, plan.tokens.user_id),
        )
        .kv("channel", plan.channel)
        .kv(
            "messages sent",
            format!("{} of {} planned", rows.len(), plan.count),
        );

    let mut add = |heading: &str, label: &str, values: &[f64]| {
        if let Some(summary) = stats::summarize(values) {
            report.section(heading).latency(label, &summary);
        }
    };
    add(
        "Helix send (the HTTP request alone)",
        "send round trip",
        &column(rows, |r| Some(r.http)),
    );
    add(
        "Message back on this machine, from the start of the send",
        "via IRC",
        &column(rows, |r| r.irc),
    );
    add(
        "Message back on this machine, from the start of the send",
        "via EventSub",
        &column(rows, |r| r.eventsub),
    );
    let deltas = column(rows, |r| r.irc.zip(r.eventsub).map(|(irc, es)| es - irc));
    if let Some(summary) = stats::summarize(&deltas) {
        let (irc_first, es_first) = first_counts(rows);
        let section = report.section("IRC against EventSub (positive: EventSub arrived later)");
        section.latency("eventsub minus irc", &summary);
        section.kv(
            "first to arrive",
            format!("IRC {irc_first}, EventSub {es_first}"),
        );
    }

    let notes = report.section("Notes");
    for via in [Via::Irc, Via::EventSub] {
        let got = column(rows, |r| if via == Via::Irc { r.irc } else { r.eventsub }).len();
        if !rows.is_empty() && got < rows.len() {
            notes.line(format!(
                "{} delivered {got} of {} messages back to this account.",
                via.label(),
                rows.len()
            ));
        }
    }
    for problem in &outcome.problems {
        notes.line(format!("problem: {problem}"));
    }
    notes.line("Times include both network legs (to Twitch and back) and Twitch's own processing.");
    report
}

#[cfg(test)]
mod tests {
    use std::io::{BufRead, BufReader};
    use std::net::TcpListener;
    use std::sync::Mutex;
    use std::sync::atomic::AtomicUsize;

    use pyxeetb_twitch::Secret;

    use super::*;
    use crate::testkit::{MockHttp, Request};

    fn tokens() -> Tokens {
        Tokens {
            client_id: "cid".into(),
            user_id: "42".into(),
            login: "pylxyr".into(),
            access_token: Secret::new("tok"),
            refresh_token: None,
            scopes: vec![],
            expires_at: u64::MAX,
        }
    }

    /// A local stand-in for Twitch: Helix over HTTP, IRC over a socket, EventSub over a WebSocket. A message
    /// sent through the fake Helix comes back over IRC after `irc_delay` and over EventSub after `eventsub_delay`.
    struct FakeTwitch {
        http: MockHttp,
        endpoints: Endpoints,
    }

    fn fake_twitch(
        irc_delay: Duration,
        eventsub_delay: Duration,
        irc_login_works: bool,
    ) -> FakeTwitch {
        let (irc_tx, irc_rx) = mpsc::channel::<String>();
        let (ws_tx, ws_rx) = mpsc::channel::<String>();

        let irc_listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let irc_port = irc_listener.local_addr().unwrap().port();
        thread::spawn(move || {
            let (stream, _) = irc_listener.accept().unwrap();
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut writer = stream;
            let mut line = String::new();
            while reader.read_line(&mut line).unwrap_or(0) > 0 {
                if line.starts_with("JOIN") {
                    break;
                }
                line.clear();
            }
            if !irc_login_works {
                let _ =
                    writer.write_all(b":tmi.twitch.tv NOTICE * :Login authentication failed\r\n");
                return;
            }
            let _ = writer.write_all(b":bot.tmi.twitch.tv 366 bot #pylxyr :End of /NAMES list\r\n");
            while let Ok(id) = irc_rx.recv() {
                let line = format!(
                    "@id={id} :pylxyr!pylxyr@pylxyr.tmi.twitch.tv PRIVMSG #pylxyr :hello\r\n"
                );
                if writer.write_all(line.as_bytes()).is_err() {
                    return;
                }
            }
        });

        let ws_listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let ws_port = ws_listener.local_addr().unwrap().port();
        thread::spawn(move || {
            let (stream, _) = ws_listener.accept().unwrap();
            let mut socket = tungstenite::accept(stream).unwrap();
            let welcome = r#"{"metadata":{"message_type":"session_welcome"},"payload":{"session":{"id":"sess-1","keepalive_timeout_seconds":10}}}"#;
            socket.send(Message::text(welcome)).unwrap();
            while let Ok(id) = ws_rx.recv() {
                let frame = format!(
                    r#"{{"metadata":{{"message_type":"notification","subscription_type":"channel.chat.message"}},"payload":{{"event":{{"broadcaster_user_id":"42","chatter_user_id":"42","chatter_user_login":"pylxyr","message_id":"{id}","message":{{"text":"hello"}}}}}}}}"#
                );
                if socket.send(Message::text(frame)).is_err() {
                    return;
                }
            }
        });

        let counter = AtomicUsize::new(0);
        let channels = Mutex::new((irc_tx, ws_tx));
        let http = MockHttp::start(move |request: &Request| match request.path.as_str() {
            "/eventsub/subscriptions" => {
                assert!(
                    request.body.contains("sess-1")
                        && request.body.contains("channel.chat.message"),
                    "{}",
                    request.body
                );
                assert_eq!(request.header("authorization"), Some("Bearer tok"));
                assert_eq!(request.header("client-id"), Some("cid"));
                (202, r#"{"data":[{"id":"sub"}]}"#.to_owned())
            }
            "/chat/messages" => {
                assert!(
                    request.body.contains("\"sender_id\":\"42\"")
                        && request.body.contains("pyxeetb latency probe"),
                    "{}",
                    request.body
                );
                let id = format!("msg-{}", counter.fetch_add(1, Ordering::SeqCst));
                let (irc_tx, ws_tx) = channels.lock().unwrap().clone();
                let (a, b) = (id.clone(), id.clone());
                thread::spawn(move || {
                    thread::sleep(irc_delay);
                    let _ = irc_tx.send(a);
                });
                thread::spawn(move || {
                    thread::sleep(eventsub_delay);
                    let _ = ws_tx.send(b);
                });
                (
                    200,
                    format!(r#"{{"data":[{{"message_id":"{id}","is_sent":true}}]}}"#),
                )
            }
            _ => (404, "{}".to_owned()),
        });
        let endpoints = Endpoints {
            helix_base: http.base(),
            eventsub_url: format!("ws://127.0.0.1:{ws_port}/ws"),
            irc_host: "127.0.0.1".to_owned(),
            irc_port,
            irc_tls: false,
            ..Endpoints::default()
        };
        FakeTwitch { http, endpoints }
    }

    fn plan<'a>(
        fake: &'a FakeTwitch,
        http: &'a Http,
        tokens: &'a Tokens,
        count: usize,
    ) -> Plan<'a> {
        Plan {
            endpoints: &fake.endpoints,
            http,
            tokens,
            channel: "pylxyr",
            broadcaster_id: "42",
            count,
            gap: Duration::from_millis(5),
        }
    }

    #[test]
    fn every_message_is_matched_on_both_routes_and_the_slower_route_shows_its_delay() {
        let fake = fake_twitch(Duration::from_millis(10), Duration::from_millis(80), true);
        let (http, tokens) = (Http::new(), tokens());
        let outcome = run(&plan(&fake, &http, &tokens, 3));
        assert!(outcome.problems.is_empty(), "{:?}", outcome.problems);
        assert_eq!(outcome.rows.len(), 3);
        for row in &outcome.rows {
            let (irc, es) = (
                row.irc.expect("IRC delivered"),
                row.eventsub.expect("EventSub delivered"),
            );
            assert!(
                es - irc > 40.0,
                "EventSub was set 70 ms later than IRC but the gap is {:.1} ms",
                es - irc
            );
            assert!(irc >= 10.0 && row.http >= 0.0);
        }
        let report = build_report(&plan(&fake, &http, &tokens, 3), &outcome).render();
        assert!(
            report.contains("eventsub minus irc") && report.contains("IRC 3, EventSub 0"),
            "{report}"
        );
        assert!(
            report.contains("messages sent         3 of 3 planned"),
            "{report}"
        );
        drop(fake.http);
    }

    #[test]
    fn a_route_that_cannot_log_in_is_reported_and_the_other_is_still_measured() {
        let fake = fake_twitch(Duration::from_millis(5), Duration::from_millis(20), false);
        let (http, tokens) = (Http::new(), tokens());
        let outcome = run(&plan(&fake, &http, &tokens, 2));
        assert!(
            outcome
                .problems
                .iter()
                .any(|p| p.contains("IRC stopped") && p.contains("authentication failed")),
            "{:?}",
            outcome.problems
        );
        assert_eq!(outcome.rows.len(), 2);
        assert!(
            outcome
                .rows
                .iter()
                .all(|r| r.irc.is_none() && r.eventsub.is_some())
        );
        let report = build_report(&plan(&fake, &http, &tokens, 2), &outcome).render();
        assert!(
            report.contains("IRC delivered 0 of 2 messages")
                && report.contains("problem: IRC stopped"),
            "{report}"
        );
        assert!(
            !report.contains("eventsub minus irc"),
            "no comparison without both routes: {report}"
        );
    }

    #[test]
    fn a_refused_send_is_a_problem_and_stops_the_run() {
        let fake = fake_twitch(Duration::from_millis(5), Duration::from_millis(5), true);
        let rejecting = MockHttp::start(|request: &Request| match request.path.as_str() {
            "/eventsub/subscriptions" => (202, "{}".to_owned()),
            _ => (
                401,
                r#"{"error":"Unauthorized","status":401,"message":"Invalid OAuth token"}"#
                    .to_owned(),
            ),
        });
        let endpoints = Endpoints {
            helix_base: rejecting.base(),
            ..fake.endpoints
        };
        let (http, tokens) = (Http::new(), tokens());
        let plan = Plan {
            endpoints: &endpoints,
            http: &http,
            tokens: &tokens,
            channel: "pylxyr",
            broadcaster_id: "42",
            count: 5,
            gap: Duration::from_millis(1),
        };
        let outcome = run(&plan);
        assert_eq!(outcome.rows, Vec::new());
        assert!(
            outcome
                .problems
                .iter()
                .any(|p| p.contains("message 1") && p.contains("Invalid OAuth token")),
            "{:?}",
            outcome.problems
        );
    }

    #[test]
    fn websocket_addresses_are_split_into_host_port_and_tls() {
        assert_eq!(
            parse_ws_url("wss://eventsub.wss.twitch.tv/ws").unwrap(),
            ("eventsub.wss.twitch.tv".to_owned(), 443, true)
        );
        assert_eq!(
            parse_ws_url("ws://127.0.0.1:8080/ws").unwrap(),
            ("127.0.0.1".to_owned(), 8080, false)
        );
        assert!(parse_ws_url("https://x/ws").is_err());
        assert!(parse_ws_url("not a url").is_err());
    }

    #[test]
    fn the_first_to_arrive_is_counted_per_message() {
        let rows = [
            Row {
                http: 1.0,
                irc: Some(100.0),
                eventsub: Some(120.0),
            },
            Row {
                http: 1.0,
                irc: Some(150.0),
                eventsub: Some(110.0),
            },
            Row {
                http: 1.0,
                irc: Some(100.0),
                eventsub: None,
            },
        ];
        assert_eq!(first_counts(&rows), (1, 1));
    }

    #[test]
    fn rows_measure_from_the_start_of_the_send() {
        let started = Instant::now();
        let later = started + Duration::from_millis(120);
        let seen = Seen {
            irc: Some(later),
            eventsub: None,
        };
        let row = row_for(started, Duration::from_millis(100), Some(&seen));
        assert!((row.http - 100.0).abs() < 1e-6);
        assert!((row.irc.unwrap() - 120.0).abs() < 1e-6);
        assert_eq!(row.eventsub, None);
    }
}
