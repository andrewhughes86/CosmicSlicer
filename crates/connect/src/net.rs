//! Sockets with timeouts, plain or TLS. Every read and write has a deadline, so a printer
//! that stops answering is an error, never a hang.

use std::io::{self, Read, Write};
use std::net::{SocketAddr, TcpStream, ToSocketAddrs};
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use rustls::{ClientConfig, ClientConnection, DigitallySignedStruct, SignatureScheme, StreamOwned};

/// How long a connection may take to open.
pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);

/// How certificates are checked.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Trust {
    /// Against the public web's roots.
    Verified,
    /// Not at all: printers on the local network have self-signed certificates.
    Any,
}

/// A connection, plain or TLS.
pub enum Stream {
    Plain(TcpStream),
    Tls(Box<StreamOwned<ClientConnection, TcpStream>>),
}

impl Stream {
    pub fn tcp(&self) -> &TcpStream {
        match self {
            Stream::Plain(s) => s,
            Stream::Tls(s) => s.get_ref(),
        }
    }

    pub fn set_timeout(&self, t: Duration) -> io::Result<()> {
        self.tcp().set_read_timeout(Some(t))?;
        self.tcp().set_write_timeout(Some(t))
    }

    /// Wraps an open TCP connection in TLS to `host`.
    pub fn tls(tcp: TcpStream, host: &str, trust: Trust) -> io::Result<Stream> {
        let name = ServerName::try_from(host.to_owned()).map_err(|e| io::Error::other(format!("{host}: {e}")))?;
        let conn = ClientConnection::new(config(trust)?, name).map_err(io::Error::other)?;
        let mut s = StreamOwned::new(conn, tcp);
        // Handshake now, so certificate problems show as connection errors.
        while s.conn.is_handshaking() {
            s.conn.complete_io(&mut s.sock)?;
        }
        Ok(Stream::Tls(Box::new(s)))
    }
}

impl Read for Stream {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        match self {
            Stream::Plain(s) => s.read(buf),
            Stream::Tls(s) => match s.read(buf) {
                // Servers that close without a TLS goodbye (common on printers): the end.
                Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => Ok(0),
                r => r,
            },
        }
    }
}

impl Write for Stream {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        match self {
            Stream::Plain(s) => s.write(buf),
            Stream::Tls(s) => s.write(buf),
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        match self {
            Stream::Plain(s) => s.flush(),
            Stream::Tls(s) => s.flush(),
        }
    }
}

/// The addresses `host`:`port` names.
pub fn resolve(host: &str, port: u16) -> io::Result<Vec<SocketAddr>> {
    let addrs: Vec<SocketAddr> = (host, port).to_socket_addrs()?.collect();
    if addrs.is_empty() {
        return Err(io::Error::other(format!("{host} has no address")));
    }
    Ok(addrs)
}

/// Opens a connection to `host`:`port`, TLS if `tls` is given, with `timeout` on every read
/// and write.
pub fn connect(host: &str, port: u16, tls: Option<Trust>, timeout: Duration) -> io::Result<Stream> {
    let mut last = io::Error::other(format!("could not reach {host}"));
    for addr in resolve(host, port)? {
        match TcpStream::connect_timeout(&addr, CONNECT_TIMEOUT) {
            Ok(tcp) => {
                tcp.set_read_timeout(Some(timeout))?;
                tcp.set_write_timeout(Some(timeout))?;
                let _ = tcp.set_nodelay(true);
                return match tls {
                    Some(trust) => Stream::tls(tcp, host, trust),
                    None => Ok(Stream::Plain(tcp)),
                };
            }
            Err(e) => last = e,
        }
    }
    Err(last)
}

/// One TLS configuration per trust, shared so sessions resume (FTPS data connections need it).
pub fn config(trust: Trust) -> io::Result<Arc<ClientConfig>> {
    static VERIFIED: OnceLock<Option<Arc<ClientConfig>>> = OnceLock::new();
    static ANY: OnceLock<Option<Arc<ClientConfig>>> = OnceLock::new();
    let provider = || Arc::new(rustls::crypto::ring::default_provider());
    let made = match trust {
        Trust::Verified => VERIFIED.get_or_init(|| {
            let roots = rustls::RootCertStore { roots: webpki_roots::TLS_SERVER_ROOTS.to_vec() };
            ClientConfig::builder_with_provider(provider())
                .with_safe_default_protocol_versions()
                .ok()
                .map(|b| Arc::new(b.with_root_certificates(roots).with_no_client_auth()))
        }),
        Trust::Any => ANY.get_or_init(|| {
            let provider = provider();
            let schemes = provider.signature_verification_algorithms.supported_schemes();
            ClientConfig::builder_with_provider(provider).with_safe_default_protocol_versions().ok().map(|b| {
                Arc::new(
                    b.dangerous().with_custom_certificate_verifier(Arc::new(AcceptAny(schemes))).with_no_client_auth(),
                )
            })
        }),
    };
    made.clone().ok_or_else(|| io::Error::other("TLS is not available"))
}

/// Accepts any certificate (for printers on the local network, which sign their own).
#[derive(Debug)]
struct AcceptAny(Vec<SignatureScheme>);

impl ServerCertVerifier for AcceptAny {
    fn verify_server_cert(
        &self,
        _end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        Ok(ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        _message: &[u8],
        _cert: &CertificateDer<'_>,
        _dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        Ok(HandshakeSignatureValid::assertion())
    }

    fn verify_tls13_signature(
        &self,
        _message: &[u8],
        _cert: &CertificateDer<'_>,
        _dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        Ok(HandshakeSignatureValid::assertion())
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.0.clone()
    }
}

/// Reads until `buf` is full, or fails.
pub fn read_exact(r: &mut impl Read, buf: &mut [u8]) -> io::Result<()> {
    r.read_exact(buf)
}

/// Writes all of `bytes` and flushes.
pub fn send(w: &mut impl Write, bytes: &[u8]) -> io::Result<()> {
    w.write_all(bytes)?;
    w.flush()
}
