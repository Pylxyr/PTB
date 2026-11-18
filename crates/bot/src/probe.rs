//! `pyxeetb probe`: what this machine is, how fast it really runs, and how far Twitch is from it.
//!
//! The network half times each stage of a fresh connection (DNS, TCP, TLS, first answer) against the Twitch
//! endpoints the bot will use. The CPU half runs a fixed workload and watches whether the speed holds, and a
//! separate idle test measures how late the machine wakes a sleeping thread, which is what the bot feels
//! between chat messages. Nothing here needs a Twitch account: the IRC check logs in as an anonymous,
//! read-only `justinfan` user.

use std::fs;
use std::hint::black_box;
use std::io::{Read, Write};
use std::net::TcpStream;
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use pyxeetb_core::procfs;
use pyxeetb_core::report::{self, Report};
use pyxeetb_core::stats::{self, count_f64};
use rustls::{ClientConfig, ClientConnection, StreamOwned};

use crate::cli::ProbeOptions;
use crate::error::{Context, Result};
use crate::net;

const IO_TIMEOUT: Duration = Duration::from_secs(5);
const SAMPLE_GAP: Duration = Duration::from_millis(250);

#[derive(Clone, Copy)]
enum Kind {
    /// A plain HTTPS GET; any answer proves the round trip.
    Http { path: &'static str },
    /// An anonymous IRC login over TLS; the welcome line proves the chat server answered.
    IrcWelcome,
}

struct Target {
    label: &'static str,
    host: &'static str,
    port: u16,
    kind: Kind,
}

const TARGETS: [Target; 3] = [
    Target {
        label: "helix api",
        host: "api.twitch.tv",
        port: 443,
        kind: Kind::Http {
            path: "/helix/users",
        },
    },
    Target {
        label: "eventsub websocket",
        host: "eventsub.wss.twitch.tv",
        port: 443,
        kind: Kind::Http { path: "/ws" },
    },
    Target {
        label: "irc chat (tls)",
        host: "irc.chat.twitch.tv",
        port: 6697,
        kind: Kind::IrcWelcome,
    },
];

/// Milliseconds spent in each stage of one connection.
#[derive(Debug, Clone, Copy)]
struct Sample {
    dns: f64,
    tcp: f64,
    tls: f64,
    first_answer: f64,
}

/// A labelled way to read one stage out of a sample.
type Stage = (&'static str, fn(&Sample) -> f64);

fn ms(d: Duration) -> f64 {
    d.as_secs_f64() * 1000.0
}

/// The HTTP status code of a response's first line, if the bytes so far contain one.
fn parse_status_line(bytes: &[u8]) -> Option<u16> {
    let end = bytes.windows(2).position(|w| w == b"\r\n")?;
    let line = std::str::from_utf8(&bytes[..end]).ok()?;
    let mut parts = line.split_whitespace();
    parts.next().filter(|p| p.starts_with("HTTP/"))?;
    parts.next()?.parse().ok()
}

/// True once the IRC welcome (numeric 001) has arrived.
fn irc_welcomed(text: &str) -> bool {
    text.lines()
        .any(|line| line.split_whitespace().nth(1) == Some("001"))
}

fn request_for(target: &Target) -> String {
    match target.kind {
        Kind::Http { path } => format!(
            "GET {path} HTTP/1.1\r\nHost: {}\r\nUser-Agent: pyxeetb-probe/{}\r\nConnection: close\r\n\r\n",
            target.host,
            pyxeetb_core::VERSION
        ),
        Kind::IrcWelcome => {
            let nanos = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.subsec_nanos());
            format!(
                "PASS SCHMOOPIIE\r\nNICK justinfan{}\r\n",
                10_000 + nanos % 80_000
            )
        }
    }
}

fn has_answered(kind: Kind, received: &[u8]) -> bool {
    match kind {
        Kind::Http { .. } => parse_status_line(received).is_some(),
        Kind::IrcWelcome => irc_welcomed(&String::from_utf8_lossy(received)),
    }
}

/// Send the request and wait for the first usable answer.
fn first_answer(
    target: &Target,
    stream: &mut StreamOwned<ClientConnection, TcpStream>,
) -> Result<f64> {
    let started = Instant::now();
    stream
        .write_all(request_for(target).as_bytes())
        .context("sending")?;
    stream.flush().context("sending")?;
    let mut received = Vec::new();
    let mut buf = [0u8; 2048];
    while started.elapsed() <= IO_TIMEOUT {
        let n = stream.read(&mut buf).context("reading")?;
        if n == 0 {
            return Err(crate::error::Error::new(
                "connection closed before an answer",
            ));
        }
        received.extend_from_slice(&buf[..n]);
        if has_answered(target.kind, &received) {
            return Ok(ms(started.elapsed()));
        }
    }
    Err(crate::error::Error::new("no answer within 5 s"))
}

fn one_sample(target: &Target, config: &Arc<ClientConfig>) -> Result<Sample> {
    let started = Instant::now();
    let addr = net::resolve(target.host, target.port)?;
    let dns = ms(started.elapsed());

    let started = Instant::now();
    let tcp = TcpStream::connect_timeout(&addr, IO_TIMEOUT).context("tcp connect")?;
    tcp.set_nodelay(true).context("tcp options")?;
    tcp.set_read_timeout(Some(IO_TIMEOUT))
        .context("tcp options")?;
    tcp.set_write_timeout(Some(IO_TIMEOUT))
        .context("tcp options")?;
    let tcp_ms = ms(started.elapsed());

    let started = Instant::now();
    let name =
        rustls::pki_types::ServerName::try_from(target.host.to_owned()).context("tls name")?;
    let conn = ClientConnection::new(Arc::clone(config), name).context("tls")?;
    let mut stream = StreamOwned::new(conn, tcp);
    while stream.conn.is_handshaking() {
        stream
            .conn
            .complete_io(&mut stream.sock)
            .context("tls handshake")?;
    }
    let tls = ms(started.elapsed());

    Ok(Sample {
        dns,
        tcp: tcp_ms,
        tls,
        first_answer: first_answer(target, &mut stream)?,
    })
}

#[derive(Default)]
struct TargetResult {
    samples: Vec<Sample>,
    failures: Vec<String>,
}

fn measure_target(target: &Target, config: &Arc<ClientConfig>, count: usize) -> TargetResult {
    let mut result = TargetResult::default();
    for i in 0..count {
        match one_sample(target, config) {
            Ok(sample) => result.samples.push(sample),
            Err(e) => result.failures.push(e.to_string()),
        }
        if i + 1 < count {
            thread::sleep(SAMPLE_GAP);
        }
    }
    result
}

/// Distinct failure messages with how often each happened, most frequent first.
fn tally(failures: &[String]) -> Vec<(String, usize)> {
    let mut counts: Vec<(String, usize)> = Vec::new();
    for failure in failures {
        match counts.iter_mut().find(|(m, _)| m == failure) {
            Some((_, n)) => *n += 1,
            None => counts.push((failure.clone(), 1)),
        }
    }
    counts.sort_by_key(|(_, n)| std::cmp::Reverse(*n));
    counts
}

fn add_network_section(report: &mut Report, target: &Target, result: &TargetResult) {
    let section = report.section(format!(
        "Twitch: {} ({}:{})",
        target.label, target.host, target.port
    ));
    section.kv(
        "connections ok",
        format!(
            "{} of {}",
            result.samples.len(),
            result.samples.len() + result.failures.len()
        ),
    );
    let answer_label = match target.kind {
        Kind::Http { .. } => "first http answer",
        Kind::IrcWelcome => "irc welcome (001)",
    };
    let stages: [Stage; 4] = [
        ("dns lookup", |s| s.dns),
        ("tcp connect", |s| s.tcp),
        ("tls handshake", |s| s.tls),
        (answer_label, |s| s.first_answer),
    ];
    for (label, pick) in stages {
        let values: Vec<f64> = result.samples.iter().map(pick).collect();
        if let Some(summary) = stats::summarize(&values) {
            section.latency(label, &summary);
        }
    }
    for (message, n) in tally(&result.failures) {
        section.line(format!("failure x{n}: {message}"));
    }
}

fn read_text(path: &str) -> Option<String> {
    fs::read_to_string(path).ok()
}

fn add_host_section(report: &mut Report) {
    let section = report.section("Host");
    section.kv("pyxeetb", pyxeetb_core::VERSION);
    section.kv(
        "os",
        read_text("/etc/os-release")
            .and_then(|t| procfs::parse_os_pretty_name(&t))
            .unwrap_or_else(|| "unknown".into()),
    );
    section.kv("architecture", std::env::consts::ARCH);
    section.kv(
        "cpu",
        read_text("/proc/cpuinfo")
            .and_then(|t| procfs::parse_cpu_model(&t))
            .unwrap_or_else(|| "unknown".into()),
    );
    section.kv(
        "logical cpus",
        thread::available_parallelism().map_or_else(|_| "unknown".to_owned(), |n| n.to_string()),
    );
    match read_text("/proc/meminfo").and_then(|t| procfs::parse_meminfo(&t)) {
        Some(mem) => {
            section.kv("memory total", report::mib(mem.total_kib));
            if let Some(available) = mem.available_kib {
                section.kv("memory available", report::mib(available));
            }
        }
        None => {
            section.kv("memory", "unknown (no /proc/meminfo)");
        }
    }
}

// ---- CPU throttling test ------------------------------------------------------------------------------------

const TARGET_CHUNK: Duration = Duration::from_millis(20);
const WINDOW: Duration = Duration::from_secs(2);

/// A fixed amount of integer work. `black_box` keeps the optimiser from deleting it.
fn work(iterations: u64) -> u64 {
    let mut x: u64 = 0x9E37_79B9_7F4A_7C15;
    for _ in 0..iterations {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
    }
    black_box(x)
}

/// An iteration count that takes about 20 ms on an unthrottled core.
///
/// Time stolen by the hypervisor can only make a trial slower, never faster, so the fastest of several trials is
/// the machine's real speed; calibrating on a single trial under-estimates it and makes runs incomparable.
fn calibrate() -> u64 {
    const TRIAL: u64 = 1_000_000;
    let fastest = (0..8)
        .map(|_| {
            let started = Instant::now();
            work(TRIAL);
            started.elapsed()
        })
        .min()
        .unwrap_or(TARGET_CHUNK)
        .max(Duration::from_micros(1));
    let scaled = u128::from(TRIAL) * TARGET_CHUNK.as_nanos() / fastest.as_nanos();
    u64::try_from(scaled)
        .unwrap_or(u64::MAX)
        .clamp(10_000, 2_000_000_000)
}

/// Work chunks completed per second in each consecutive window, and every chunk's duration in ms.
struct Burst {
    windows: Vec<f64>,
    chunk_ms: Vec<f64>,
}

fn run_burst(seconds: u64) -> Burst {
    let iterations = calibrate();
    let total = Duration::from_secs(seconds);
    let begin = Instant::now();
    let mut burst = Burst {
        windows: Vec::new(),
        chunk_ms: Vec::new(),
    };
    let mut window_start = begin;
    let mut in_window = 0usize;
    while begin.elapsed() < total {
        let started = Instant::now();
        work(iterations);
        burst.chunk_ms.push(ms(started.elapsed()));
        in_window += 1;
        let span = window_start.elapsed();
        if span >= WINDOW {
            burst
                .windows
                .push(count_f64(in_window) / span.as_secs_f64());
            window_start = Instant::now();
            in_window = 0;
        }
    }
    burst
}

/// How much slower the slowest window ran than the fastest, as a percentage of the fastest (0 = steady).
fn slowdown_percent(windows: &[f64]) -> Option<f64> {
    let best = windows.iter().copied().fold(f64::NAN, f64::max);
    let worst = windows.iter().copied().fold(f64::NAN, f64::min);
    (best.is_finite() && best > 0.0).then(|| (best - worst) * 100.0 / best)
}

fn add_cpu_section(report: &mut Report, seconds: u64) {
    if seconds == 0 {
        report
            .section("CPU throttling test")
            .line("skipped (--burst-seconds 0)");
        return;
    }
    let before = read_text("/proc/stat").and_then(|t| procfs::parse_cpu_times(&t));
    let burst = run_burst(seconds);
    let after = read_text("/proc/stat").and_then(|t| procfs::parse_cpu_times(&t));

    let section = report.section(format!(
        "CPU throttling test ({seconds} s of one busy thread)"
    ));
    if let Some(summary) = stats::summarize(&burst.chunk_ms) {
        section.latency("work chunk (~20 ms)", &summary);
    }
    if let Some(first) = burst.windows.first().copied().filter(|f| *f > 0.0) {
        let mut from = 0.0;
        for rate in &burst.windows {
            let to = from + WINDOW.as_secs_f64();
            section.line(format!(
                "{from:>4.0}-{to:<4.0} s  {rate:>7.1} chunks/s  {:>5.1}% of the first window",
                rate * 100.0 / first
            ));
            from = to;
        }
    }
    if let Some(slowdown) = slowdown_percent(&burst.windows) {
        section.kv("slowest vs fastest", format!("{slowdown:.1}% slower"));
    }
    let steal = before
        .zip(after)
        .map(|(b, a)| procfs::steal_percent(&b, &a));
    section.kv(
        "hypervisor steal",
        match steal {
            Some(Some(percent)) => format!("{percent:.2}% of all CPU time during the test"),
            Some(None) => "no reading".to_owned(),
            None => "unavailable (no /proc/stat)".to_owned(),
        },
    );
}

// ---- idle wake-up test --------------------------------------------------------------------------------------

const JITTER_SLEEP: Duration = Duration::from_millis(5);
const JITTER_LATE_MS: f64 = 10.0;

/// How late each 5 ms sleep returned, in ms. On an idle machine this is what a socket event waits for a turn.
fn run_jitter(seconds: u64) -> Vec<f64> {
    let end = Instant::now() + Duration::from_secs(seconds);
    let mut lateness = Vec::new();
    while Instant::now() < end {
        let started = Instant::now();
        thread::sleep(JITTER_SLEEP);
        lateness.push(ms(started.elapsed().saturating_sub(JITTER_SLEEP)));
    }
    lateness
}

fn add_jitter_section(report: &mut Report, seconds: u64) {
    if seconds == 0 {
        report
            .section("Idle wake-up test")
            .line("skipped (--jitter-seconds 0)");
        return;
    }
    let lateness = run_jitter(seconds);
    let section = report.section(format!("Idle wake-up test ({seconds} s of 5 ms sleeps)"));
    if let Some(summary) = stats::summarize(&lateness) {
        section.latency("wake-up lateness", &summary);
        let late = lateness.iter().filter(|l| **l > JITTER_LATE_MS).count();
        section.kv("later than 10 ms", format!("{late} of {}", lateness.len()));
    }
}

fn add_process_section(report: &mut Report) {
    let (peak, current) =
        read_text("/proc/self/status").map_or((None, None), |t| procfs::parse_self_memory(&t));
    let section = report.section("This probe's own memory");
    if let Some((peak, current)) = peak.zip(current) {
        section.kv("peak resident", report::mib(peak));
        section.kv("resident at end", report::mib(current));
    } else {
        section.line("unavailable (no /proc/self/status)");
    }
}

/// Run the whole probe and return the report text.
pub fn run(options: &ProbeOptions) -> String {
    let mut report = Report::new(format!("PyxeeTB probe {}", pyxeetb_core::VERSION));
    add_host_section(&mut report);

    match net::tls_config() {
        Ok(config) => {
            for target in &TARGETS {
                eprintln!(
                    "measuring {} ({} connections)...",
                    target.label, options.samples
                );
                let result = measure_target(target, &config, options.samples);
                add_network_section(&mut report, target, &result);
            }
        }
        Err(e) => {
            report.section("Twitch").line(e.to_string());
        }
    }

    eprintln!(
        "running the idle wake-up test ({} s)...",
        options.jitter_seconds
    );
    add_jitter_section(&mut report, options.jitter_seconds);
    eprintln!("running the CPU test ({} s)...", options.burst_seconds);
    add_cpu_section(&mut report, options.burst_seconds);
    add_process_section(&mut report);
    report.render()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_lines_are_parsed_once_complete() {
        assert_eq!(
            parse_status_line(b"HTTP/1.1 401 Unauthorized\r\nx: y\r\n"),
            Some(401)
        );
        assert_eq!(parse_status_line(b"HTTP/2 200\r\n"), Some(200));
        assert_eq!(parse_status_line(b"HTTP/1.1 40"), None, "no line end yet");
        assert_eq!(parse_status_line(b"garbage\r\n"), None);
        assert_eq!(parse_status_line(b"HTTP/1.1 abc\r\n"), None);
    }

    #[test]
    fn irc_welcome_is_numeric_001() {
        assert!(irc_welcomed(
            ":tmi.twitch.tv 001 justinfan1 :Welcome, GLHF!\r\n"
        ));
        assert!(!irc_welcomed(
            ":tmi.twitch.tv 002 justinfan1 :Your host\r\n"
        ));
        assert!(!irc_welcomed("PING :tmi.twitch.tv\r\n"));
        assert!(!irc_welcomed(""));
    }

    #[test]
    fn failures_are_counted_and_ordered() {
        let failures = ["a", "b", "a", "a", "b", "c"].map(String::from);
        assert_eq!(
            tally(&failures),
            vec![
                ("a".to_owned(), 3),
                ("b".to_owned(), 2),
                ("c".to_owned(), 1)
            ]
        );
        assert_eq!(tally(&[]), Vec::new());
    }

    #[test]
    fn slowdown_compares_slowest_to_fastest_window() {
        assert_eq!(slowdown_percent(&[50.0, 50.0, 50.0]), Some(0.0));
        let s = slowdown_percent(&[50.0, 40.0, 12.5]).unwrap();
        assert!((s - 75.0).abs() < 1e-9, "{s}");
        assert_eq!(slowdown_percent(&[]), None);
        assert_eq!(slowdown_percent(&[0.0]), None);
    }

    #[test]
    fn work_is_deterministic_and_calibration_is_sane() {
        assert_eq!(work(1000), work(1000));
        assert_ne!(work(1000), work(1001));
        assert!((10_000..=2_000_000_000).contains(&calibrate()));
    }

    #[test]
    fn a_zero_second_test_is_skipped_in_the_report() {
        let mut report = Report::new("t");
        add_cpu_section(&mut report, 0);
        add_jitter_section(&mut report, 0);
        let text = report.render();
        assert!(
            text.contains("skipped (--burst-seconds 0)")
                && text.contains("skipped (--jitter-seconds 0)"),
            "{text}"
        );
    }

    #[test]
    fn a_short_burst_reports_chunk_times() {
        let burst = run_burst(1);
        assert_ne!(burst.chunk_ms, Vec::<f64>::new());
        assert!(burst.chunk_ms.iter().all(|c| *c > 0.0));
    }

    #[test]
    fn the_jitter_test_measures_lateness_that_is_never_negative() {
        let lateness = run_jitter(1);
        assert!(
            lateness.len() > 20,
            "about 200 sleeps fit in a second, got {}",
            lateness.len()
        );
        assert!(lateness.iter().all(|l| *l >= 0.0));
    }

    /// Needs the internet, so it only runs on request: `cargo test -p pyxeetb -- --ignored`.
    /// Proves the DNS -> TCP -> TLS -> HTTP path end to end against a host any machine can reach.
    #[test]
    #[ignore = "needs internet access"]
    fn live_https_round_trip_against_a_public_host() {
        use rustls::pki_types::{CertificateDer, pem::PemObject};
        let target = Target {
            label: "test",
            host: "api.github.com",
            port: 443,
            kind: Kind::Http { path: "/" },
        };
        // Behind a TLS-inspecting proxy the bundled roots are not enough; trust the machine's own store too.
        let system: Vec<CertificateDer<'static>> =
            CertificateDer::pem_file_iter("/etc/ssl/certs/ca-certificates.crt").map_or_else(
                |_| Vec::new(),
                |certs| certs.filter_map(std::result::Result::ok).collect(),
            );
        let config = net::tls_config_with(system).unwrap();
        let sample = one_sample(&target, &config).expect("a round trip to api.github.com");
        assert!(
            sample.dns >= 0.0 && sample.tcp > 0.0 && sample.tls > 0.0 && sample.first_answer > 0.0,
            "{sample:?}"
        );
    }
}
