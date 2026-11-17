//! `pyxeetb probe`: what this machine is, how fast it really runs, and how far Twitch is from it.
//!
//! The network half times each stage of a fresh connection (DNS, TCP, TLS, first answer) against the Twitch
//! endpoints the bot will use. The CPU half runs a fixed workload for a while and watches whether the speed
//! holds: a 1/8-OCPU burstable server can look fast for a few seconds and then be slowed down.
//! Nothing here needs a Twitch account; the IRC check logs in as an anonymous read-only `justinfan` user.

use std::hint::black_box;
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream, ToSocketAddrs};
use std::sync::Arc;
use std::time::{Duration, Instant};

use pyxeetb_core::procfs;
use pyxeetb_core::report::{self, Report};
use pyxeetb_core::stats;

use crate::cli::ProbeOptions;

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

fn tls_config() -> Result<Arc<rustls::ClientConfig>, String> {
    build_tls_config(Vec::new())
}

/// The bundled Mozilla roots plus any extra trusted certificates (tests behind an inspecting proxy use this).
fn build_tls_config(
    extra_roots: Vec<rustls::pki_types::CertificateDer<'static>>,
) -> Result<Arc<rustls::ClientConfig>, String> {
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let mut roots = rustls::RootCertStore::empty();
    roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
    for cert in extra_roots {
        roots.add(cert).map_err(|e| format!("extra root: {e}"))?;
    }
    let config = rustls::ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .map_err(|e| format!("TLS setup failed: {e}"))?
        .with_root_certificates(roots)
        .with_no_client_auth();
    Ok(Arc::new(config))
}

/// The HTTP status code of a response's first line, if the bytes so far contain one.
fn parse_status_line(bytes: &[u8]) -> Option<u16> {
    let end = bytes.windows(2).position(|w| w == b"\r\n")?;
    let line = std::str::from_utf8(&bytes[..end]).ok()?;
    let mut parts = line.split_whitespace();
    if !parts.next()?.starts_with("HTTP/") {
        return None;
    }
    parts.next()?.parse().ok()
}

/// True once the IRC welcome (numeric 001) has arrived.
fn irc_welcomed(text: &str) -> bool {
    text.lines()
        .any(|line| line.split_whitespace().nth(1) == Some("001"))
}

fn one_sample(target: &Target, config: &Arc<rustls::ClientConfig>) -> Result<Sample, String> {
    let started = Instant::now();
    let addrs: Vec<SocketAddr> = (target.host, target.port)
        .to_socket_addrs()
        .map_err(|e| format!("dns: {e}"))?
        .collect();
    let addr = addrs
        .iter()
        .find(|a| a.is_ipv4())
        .or(addrs.first())
        .ok_or("dns: no address")?;
    let dns = ms(started.elapsed());

    let started = Instant::now();
    let mut tcp =
        TcpStream::connect_timeout(addr, IO_TIMEOUT).map_err(|e| format!("tcp connect: {e}"))?;
    tcp.set_nodelay(true).ok();
    tcp.set_read_timeout(Some(IO_TIMEOUT)).ok();
    tcp.set_write_timeout(Some(IO_TIMEOUT)).ok();
    let tcp_ms = ms(started.elapsed());

    let started = Instant::now();
    let name = rustls::pki_types::ServerName::try_from(target.host.to_string())
        .map_err(|e| format!("tls name: {e}"))?;
    let mut conn =
        rustls::ClientConnection::new(Arc::clone(config), name).map_err(|e| format!("tls: {e}"))?;
    while conn.is_handshaking() {
        conn.complete_io(&mut tcp)
            .map_err(|e| format!("tls handshake: {e}"))?;
    }
    let tls = ms(started.elapsed());
    let mut stream = rustls::StreamOwned::new(conn, tcp);

    let started = Instant::now();
    let request = match target.kind {
        Kind::Http { path } => format!(
            "GET {path} HTTP/1.1\r\nHost: {}\r\nUser-Agent: pyxeetb-probe/{}\r\nConnection: close\r\n\r\n",
            target.host,
            pyxeetb_core::VERSION
        ),
        Kind::IrcWelcome => {
            let nanos = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.subsec_nanos());
            format!("PASS SCHMOOPIIE\r\nNICK justinfan{}\r\n", 10_000 + nanos % 80_000)
        }
    };
    stream
        .write_all(request.as_bytes())
        .map_err(|e| format!("send: {e}"))?;
    stream.flush().map_err(|e| format!("send: {e}"))?;

    let mut received = Vec::new();
    let mut buf = [0u8; 2048];
    let deadline = Instant::now() + IO_TIMEOUT;
    loop {
        if Instant::now() > deadline {
            return Err("no answer within 5 s".to_string());
        }
        let n = match stream.read(&mut buf) {
            Ok(0) => return Err("connection closed before an answer".to_string()),
            Ok(n) => n,
            Err(e) => return Err(format!("read: {e}")),
        };
        received.extend_from_slice(&buf[..n]);
        let answered = match target.kind {
            Kind::Http { .. } => parse_status_line(&received).is_some(),
            Kind::IrcWelcome => irc_welcomed(&String::from_utf8_lossy(&received)),
        };
        if answered {
            break;
        }
    }
    Ok(Sample {
        dns,
        tcp: tcp_ms,
        tls,
        first_answer: ms(started.elapsed()),
    })
}

struct TargetResult {
    samples: Vec<Sample>,
    failures: Vec<String>,
}

fn measure_target(
    target: &Target,
    config: &Arc<rustls::ClientConfig>,
    count: usize,
) -> TargetResult {
    let mut result = TargetResult {
        samples: Vec::new(),
        failures: Vec::new(),
    };
    for i in 0..count {
        match one_sample(target, config) {
            Ok(sample) => result.samples.push(sample),
            Err(message) => result.failures.push(message),
        }
        if i + 1 < count {
            std::thread::sleep(SAMPLE_GAP);
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
    counts.sort_by(|a, b| b.1.cmp(&a.1));
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
    let stages: [Stage; 4] = [
        ("dns lookup", |s| s.dns),
        ("tcp connect", |s| s.tcp),
        ("tls handshake", |s| s.tls),
        (
            match target.kind {
                Kind::Http { .. } => "first http answer",
                Kind::IrcWelcome => "irc welcome (001)",
            },
            |s| s.first_answer,
        ),
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
    std::fs::read_to_string(path).ok()
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
        std::thread::available_parallelism()
            .map_or_else(|_| "unknown".to_string(), |n| n.to_string()),
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

/// Pick an iteration count that takes about 20 ms on an unthrottled core.
fn calibrate() -> u64 {
    let mut iterations = 1_000_000u64;
    for _ in 0..4 {
        let started = Instant::now();
        work(iterations);
        let elapsed = started.elapsed().as_secs_f64().max(1e-6);
        iterations =
            ((iterations as f64) * 0.020 / elapsed).clamp(10_000.0, 2_000_000_000.0) as u64;
    }
    iterations
}

const WINDOW_SECONDS: f64 = 2.0;

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
    let mut in_window = 0u64;
    while begin.elapsed() < total {
        let started = Instant::now();
        work(iterations);
        burst.chunk_ms.push(ms(started.elapsed()));
        in_window += 1;
        let span = window_start.elapsed().as_secs_f64();
        if span >= WINDOW_SECONDS {
            burst.windows.push(in_window as f64 / span);
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
        for (i, rate) in burst.windows.iter().enumerate() {
            let from = i as f64 * WINDOW_SECONDS;
            section.line(format!(
                "{:>4.0}-{:<4.0} s  {:>7.1} chunks/s  {:>5.1}% of the first window",
                from,
                from + WINDOW_SECONDS,
                rate,
                rate * 100.0 / first
            ));
        }
    }
    if let Some(slowdown) = slowdown_percent(&burst.windows) {
        section.kv("slowest vs fastest", format!("{slowdown:.1}% slower"));
    }
    match (before, after) {
        (Some(b), Some(a)) => match procfs::steal_percent(&b, &a) {
            Some(steal) => {
                section.kv(
                    "hypervisor steal",
                    format!("{steal:.2}% of all CPU time during the test"),
                );
            }
            None => {
                section.kv("hypervisor steal", "no reading");
            }
        },
        _ => {
            section.kv("hypervisor steal", "unavailable (no /proc/stat)");
        }
    }
}

fn add_process_section(report: &mut Report) {
    let (peak, current) =
        read_text("/proc/self/status").map_or((None, None), |t| procfs::parse_self_memory(&t));
    let section = report.section("This probe's own memory");
    match (peak, current) {
        (Some(peak), Some(current)) => {
            section.kv("peak resident", report::mib(peak));
            section.kv("resident at end", report::mib(current));
        }
        _ => {
            section.line("unavailable (no /proc/self/status)");
        }
    }
}

/// Run the whole probe and return the report text.
pub fn run(options: &ProbeOptions) -> String {
    let mut report = Report::new(format!("PyxeeTB probe {}", pyxeetb_core::VERSION));
    add_host_section(&mut report);

    match tls_config() {
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
        Err(message) => {
            report.section("Twitch").line(message);
        }
    }

    eprintln!("running the CPU test ({} s)...", options.burst_seconds);
    add_cpu_section(&mut report, options.burst_seconds);
    add_process_section(&mut report);
    report.render()
}

#[cfg(test)]
mod tests {
    use super::*;
    use rustls::pki_types::pem::PemObject;

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
                ("a".to_string(), 3),
                ("b".to_string(), 2),
                ("c".to_string(), 1)
            ]
        );
        assert!(tally(&[]).is_empty());
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
        let iterations = calibrate();
        assert!((10_000..=2_000_000_000).contains(&iterations));
    }

    #[test]
    fn a_zero_second_burst_is_skipped_in_the_report() {
        let mut report = Report::new("t");
        add_cpu_section(&mut report, 0);
        assert!(report.render().contains("skipped"));
    }

    #[test]
    fn a_short_burst_reports_windows_and_chunk_times() {
        let burst = run_burst(1);
        assert!(!burst.chunk_ms.is_empty());
        assert!(burst.chunk_ms.iter().all(|c| *c > 0.0));
    }

    #[test]
    fn tls_config_builds_with_the_bundled_roots() {
        assert!(tls_config().is_ok());
    }

    /// Needs the internet, so it only runs on request: `cargo test -p pyxeetb -- --ignored`.
    /// Proves the DNS -> TCP -> TLS -> HTTP path end to end against a host any machine can reach.
    #[test]
    #[ignore = "needs internet access"]
    fn live_https_round_trip_against_a_public_host() {
        let target = Target {
            label: "test",
            host: "api.github.com",
            port: 443,
            kind: Kind::Http { path: "/" },
        };
        // Behind a TLS-inspecting proxy the bundled roots are not enough; trust the machine's own store too.
        let system: Vec<rustls::pki_types::CertificateDer<'static>> =
            rustls::pki_types::CertificateDer::pem_file_iter("/etc/ssl/certs/ca-certificates.crt")
                .map(|certs| certs.filter_map(Result::ok).collect())
                .unwrap_or_default();
        let config = build_tls_config(system).unwrap();
        let sample = one_sample(&target, &config).expect("a round trip to api.github.com");
        assert!(
            sample.dns >= 0.0 && sample.tcp > 0.0 && sample.tls > 0.0 && sample.first_answer > 0.0,
            "{sample:?}"
        );
    }
}
