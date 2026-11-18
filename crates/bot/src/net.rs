//! Connections: plain TCP or TLS behind one type, so the same code talks to Twitch and to a local test server.

use std::io::{self, Read, Write};
use std::net::{SocketAddr, TcpStream, ToSocketAddrs};
use std::sync::Arc;
use std::time::Duration;

use rustls::pki_types::ServerName;
use rustls::{ClientConfig, ClientConnection, RootCertStore, StreamOwned};

use crate::error::{Context, Result};

/// The TLS settings every connection uses: the bundled Mozilla roots, TLS 1.2 and 1.3, the `ring` provider.
///
/// # Errors
/// When the TLS library rejects its own defaults, which does not happen in practice.
pub fn tls_config() -> Result<Arc<ClientConfig>> {
    tls_config_with(Vec::new())
}

/// [`tls_config`] plus extra trusted certificates (tests behind a TLS-inspecting proxy use this).
///
/// # Errors
/// When an extra certificate cannot be parsed, or the TLS library rejects its defaults.
pub fn tls_config_with(
    extra_roots: Vec<rustls::pki_types::CertificateDer<'static>>,
) -> Result<Arc<ClientConfig>> {
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let mut roots = RootCertStore::empty();
    roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
    for cert in extra_roots {
        roots.add(cert).context("adding a trusted certificate")?;
    }
    let config = ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .context("setting up TLS")?
        .with_root_certificates(roots)
        .with_no_client_auth();
    Ok(Arc::new(config))
}

#[derive(Debug)]
pub enum Conn {
    Plain(TcpStream),
    Tls(Box<StreamOwned<ClientConnection, TcpStream>>),
}

impl Conn {
    /// How long a read waits before giving up with `WouldBlock` or `TimedOut`; `None` waits forever.
    ///
    /// # Errors
    /// When the operating system refuses the setting.
    pub fn set_read_timeout(&self, timeout: Option<Duration>) -> io::Result<()> {
        match self {
            Self::Plain(tcp) => tcp.set_read_timeout(timeout),
            Self::Tls(tls) => tls.sock.set_read_timeout(timeout),
        }
    }
}

impl Read for Conn {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        match self {
            Self::Plain(tcp) => tcp.read(buf),
            Self::Tls(tls) => tls.read(buf),
        }
    }
}

impl Write for Conn {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        match self {
            Self::Plain(tcp) => tcp.write(buf),
            Self::Tls(tls) => tls.write(buf),
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        match self {
            Self::Plain(tcp) => tcp.flush(),
            Self::Tls(tls) => tls.flush(),
        }
    }
}

/// Prefer an IPv4 address when a name has both, so repeated runs compare like with like.
///
/// # Errors
/// When the name does not resolve.
pub fn resolve(host: &str, port: u16) -> Result<SocketAddr> {
    let addrs: Vec<SocketAddr> = (host, port)
        .to_socket_addrs()
        .context("looking up the address")?
        .collect();
    addrs
        .iter()
        .find(|a| a.is_ipv4())
        .or(addrs.first())
        .copied()
        .context("the name has no address")
}

/// Connect to `host:port`, over TLS when `tls` is given.
///
/// # Errors
/// When the name does not resolve, the connection or the TLS handshake fails, or it takes longer than `timeout`.
pub fn connect(
    host: &str,
    port: u16,
    tls: Option<&Arc<ClientConfig>>,
    timeout: Duration,
) -> Result<Conn> {
    let addr = resolve(host, port)?;
    let tcp = TcpStream::connect_timeout(&addr, timeout).context("connecting")?;
    tcp.set_nodelay(true).context("setting TCP_NODELAY")?;
    tcp.set_write_timeout(Some(timeout))
        .context("setting the write timeout")?;
    tcp.set_read_timeout(Some(timeout))
        .context("setting the read timeout")?;
    let Some(config) = tls else {
        return Ok(Conn::Plain(tcp));
    };
    let name = ServerName::try_from(host.to_owned()).context("reading the server name")?;
    let conn = ClientConnection::new(Arc::clone(config), name).context("starting TLS")?;
    let mut stream = StreamOwned::new(conn, tcp);
    while stream.conn.is_handshaking() {
        stream
            .conn
            .complete_io(&mut stream.sock)
            .context("the TLS handshake")?;
    }
    Ok(Conn::Tls(Box::new(stream)))
}

#[cfg(test)]
mod tests {
    use std::net::TcpListener;

    use super::*;

    #[test]
    fn the_default_tls_config_builds() {
        assert!(tls_config().is_ok());
    }

    #[test]
    fn a_plain_connection_reads_and_writes() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = std::thread::spawn(move || {
            let (mut socket, _) = listener.accept().unwrap();
            let mut buf = [0u8; 4];
            socket.read_exact(&mut buf).unwrap();
            socket.write_all(&buf).unwrap();
        });
        let mut conn = connect("127.0.0.1", port, None, Duration::from_secs(5)).unwrap();
        conn.write_all(b"ping").unwrap();
        let mut echoed = [0u8; 4];
        conn.read_exact(&mut echoed).unwrap();
        assert_eq!(&echoed, b"ping");
        server.join().unwrap();
    }

    #[test]
    fn a_read_timeout_surfaces_as_would_block_or_timed_out() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let mut conn = connect("127.0.0.1", port, None, Duration::from_secs(5)).unwrap();
        conn.set_read_timeout(Some(Duration::from_millis(50)))
            .unwrap();
        let err = conn.read(&mut [0u8; 1]).unwrap_err();
        assert!(
            matches!(
                err.kind(),
                io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
            ),
            "{err}"
        );
    }

    #[test]
    fn a_closed_port_is_an_error_with_context() {
        let port = TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port();
        let err = connect("127.0.0.1", port, None, Duration::from_millis(500)).unwrap_err();
        assert!(err.to_string().starts_with("connecting:"), "{err}");
    }
}
