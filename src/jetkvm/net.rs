//! Reaching the device over HTTP(S): the connection, with TLS certificate
//! checks, and the password login (JetKVM's `POST /auth/login-local`).

use crate::address::DeviceAddress;
use sha2::{Digest, Sha256};
use std::io::{self, Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::time::Duration;

pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const IO_TIMEOUT: Duration = Duration::from_secs(10);
const MAX_RESPONSE: usize = 64 * 1024;

/// A connection that failed, with a sentence for the user.
pub type Failure = String;

/// A connection to the device, plain or TLS.
pub enum Conn {
    Plain(TcpStream),
    Tls(Box<native_tls::TlsStream<TcpStream>>),
}

impl Conn {
    pub fn tcp(&self) -> &TcpStream {
        match self {
            Conn::Plain(s) => s,
            Conn::Tls(s) => s.get_ref(),
        }
    }
}

impl Read for Conn {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        match self {
            Conn::Plain(s) => s.read(buf),
            Conn::Tls(s) => s.read(buf),
        }
    }
}

impl Write for Conn {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        match self {
            Conn::Plain(s) => s.write(buf),
            Conn::Tls(s) => s.write(buf),
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        match self {
            Conn::Plain(s) => s.flush(),
            Conn::Tls(s) => s.flush(),
        }
    }
}

/// The host name without IPv6 brackets.
pub fn bare_host(dev: &DeviceAddress) -> &str {
    dev.host.trim_start_matches('[').trim_end_matches(']')
}

/// Connects to the device, checking its certificate with `tls`: against
/// `fingerprint` when given, otherwise against the system's trusted roots.
pub fn connect(dev: &DeviceAddress) -> Result<Conn, Failure> {
    let tcp = tcp(dev)?;
    if !dev.tls {
        return Ok(Conn::Plain(tcp));
    }
    let host = bare_host(dev);
    let mut builder = native_tls::TlsConnector::builder();
    if dev.fingerprint.is_some() {
        // The fingerprint decides instead of the certificate chain.
        builder
            .danger_accept_invalid_certs(true)
            .danger_accept_invalid_hostnames(true);
    }
    let connector = builder
        .build()
        .map_err(|e| format!("Couldn't set up TLS: {e}"))?;
    match connector.connect(host, tcp) {
        Ok(s) => {
            if let Some(expected) = dev.fingerprint {
                let actual =
                    certificate_fingerprint(&s).ok_or("The JetKVM sent no TLS certificate.")?;
                if actual != expected {
                    return Err(format!(
                        "The JetKVM's TLS certificate doesn't match the fingerprint in the address; it is {}.",
                        hex(&actual)
                    ));
                }
            }
            Ok(Conn::Tls(Box::new(s)))
        }
        Err(native_tls::HandshakeError::Failure(e)) if dev.fingerprint.is_none() => {
            // Most likely the device's self-signed certificate: say what to
            // add to the address, after looking at the certificate.
            match untrusted_fingerprint(dev) {
                Some(fp) => Err(format!(
                    "The JetKVM's TLS certificate isn't trusted ({e}). If it is the device's own certificate, add &fingerprint={} to the address.",
                    hex(&fp)
                )),
                None => Err(format!("The TLS connection to the JetKVM failed: {e}")),
            }
        }
        Err(e) => Err(format!("The TLS connection to the JetKVM failed: {e}")),
    }
}

fn tcp(dev: &DeviceAddress) -> Result<TcpStream, Failure> {
    let unreachable =
        |e: &dyn std::fmt::Display| format!("Couldn't reach the JetKVM at {}: {e}", dev.base_url());
    let addrs: Vec<_> = (bare_host(dev), dev.port)
        .to_socket_addrs()
        .map_err(|e| unreachable(&e))?
        .collect();
    let mut last = None;
    for addr in addrs {
        match TcpStream::connect_timeout(&addr, CONNECT_TIMEOUT) {
            Ok(s) => {
                let _ = s.set_nodelay(true);
                s.set_read_timeout(Some(IO_TIMEOUT))
                    .and_then(|_| s.set_write_timeout(Some(IO_TIMEOUT)))
                    .map_err(|e| unreachable(&e))?;
                return Ok(s);
            }
            Err(e) => last = Some(e),
        }
    }
    Err(match last {
        Some(e) => unreachable(&e),
        None => unreachable(&"no address found"),
    })
}

fn certificate_fingerprint(s: &native_tls::TlsStream<TcpStream>) -> Option<[u8; 32]> {
    let der = s.peer_certificate().ok()??.to_der().ok()?;
    Some(Sha256::digest(der).into())
}

/// The fingerprint of a certificate that failed verification, to show it.
fn untrusted_fingerprint(dev: &DeviceAddress) -> Option<[u8; 32]> {
    let tcp = tcp(dev).ok()?;
    let connector = native_tls::TlsConnector::builder()
        .danger_accept_invalid_certs(true)
        .danger_accept_invalid_hostnames(true)
        .build()
        .ok()?;
    let s = connector.connect(bare_host(dev), tcp).ok()?;
    certificate_fingerprint(&s)
}

pub fn hex(bytes: &[u8]) -> String {
    bytes
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect::<Vec<_>>()
        .join(":")
}

/// How a login ended.
#[derive(Debug, PartialEq)]
pub enum Login {
    /// Logged in; the `authToken` cookie value.
    Token(String),
    /// The device has no password (JetKVM's "noPassword" mode).
    NoPassword,
    /// The password is wrong.
    Refused,
}

/// Logs in with `password` (`POST /auth/login-local`).
pub fn login(dev: &DeviceAddress, password: &str) -> Result<Login, Failure> {
    let body = serde_json::json!({ "password": password }).to_string();
    let request = format!(
        "POST /auth/login-local HTTP/1.1\r\nHost: {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        host_header(dev),
        body.len(),
        body
    );
    let mut conn = connect(dev)?;
    conn.write_all(request.as_bytes())
        .and_then(|_| conn.flush())
        .map_err(|e| format!("The JetKVM dropped the connection: {e}"))?;
    let response = read_response(&mut conn)?;
    let parsed = parse_response(&response)?;
    let error = serde_json::from_slice::<serde_json::Value>(&parsed.body)
        .ok()
        .and_then(|v| v.get("error").and_then(|e| e.as_str()).map(str::to_owned))
        .unwrap_or_default();
    match parsed.status {
        200 => parsed
            .cookies
            .iter()
            .find_map(|c| c.strip_prefix("authToken="))
            .map(|v| Login::Token(v.split(';').next().unwrap_or("").to_owned()))
            .filter(|l| !matches!(l, Login::Token(t) if t.is_empty()))
            .ok_or_else(|| {
                "The JetKVM accepted the password but sent no session token.".to_string()
            }),
        400 if error.contains("noPassword") => Ok(Login::NoPassword),
        401 => Ok(Login::Refused),
        429 => Err(format!(
            "The JetKVM refuses logins for a while after too many wrong passwords{}.",
            parsed
                .retry_after
                .map(|s| format!("; try again in {s} seconds"))
                .unwrap_or_default()
        )),
        status => Err(format!(
            "The JetKVM refused the login (HTTP {status}{}).",
            if error.is_empty() {
                String::new()
            } else {
                format!(": {error}")
            }
        )),
    }
}

pub fn host_header(dev: &DeviceAddress) -> String {
    let default = if dev.tls { 443 } else { 80 };
    if dev.port == default {
        dev.host.clone()
    } else {
        format!("{}:{}", dev.host, dev.port)
    }
}

fn read_response(conn: &mut Conn) -> Result<Vec<u8>, Failure> {
    let mut buf = Vec::new();
    let mut chunk = [0u8; 4096];
    loop {
        match conn.read(&mut chunk) {
            Ok(0) => break,
            Ok(n) => {
                buf.extend_from_slice(&chunk[..n]);
                if buf.len() > MAX_RESPONSE {
                    return Err("The JetKVM's answer is too long.".into());
                }
            }
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            // A TLS peer may close without close_notify.
            Err(_) if !buf.is_empty() => break,
            Err(e) => return Err(format!("The JetKVM didn't answer the login: {e}")),
        }
    }
    Ok(buf)
}

struct Response {
    status: u16,
    cookies: Vec<String>,
    retry_after: Option<u64>,
    body: Vec<u8>,
}

fn parse_response(raw: &[u8]) -> Result<Response, Failure> {
    let bad = || "The JetKVM's answer isn't HTTP.".to_string();
    let end = raw
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .ok_or_else(bad)?;
    let head = std::str::from_utf8(&raw[..end]).map_err(|_| bad())?;
    let mut lines = head.split("\r\n");
    let status = lines
        .next()
        .and_then(|l| l.split(' ').nth(1))
        .and_then(|s| s.parse().ok())
        .ok_or_else(bad)?;
    let mut cookies = Vec::new();
    let mut retry_after = None;
    let mut chunked = false;
    for line in lines {
        let Some((k, v)) = line.split_once(':') else {
            continue;
        };
        let v = v.trim();
        match k.trim().to_ascii_lowercase().as_str() {
            "set-cookie" => cookies.push(v.to_owned()),
            "retry-after" => retry_after = v.parse().ok(),
            "transfer-encoding" => chunked = v.eq_ignore_ascii_case("chunked"),
            _ => {}
        }
    }
    let mut body = raw[end + 4..].to_vec();
    if chunked {
        body = dechunk(&body);
    }
    Ok(Response {
        status,
        cookies,
        retry_after,
        body,
    })
}

fn dechunk(mut data: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    while let Some(pos) = data.windows(2).position(|w| w == b"\r\n") {
        let size = std::str::from_utf8(&data[..pos])
            .ok()
            .and_then(|s| usize::from_str_radix(s.split(';').next().unwrap_or("").trim(), 16).ok());
        let Some(size) = size else { break };
        data = &data[pos + 2..];
        if size == 0 || data.len() < size {
            break;
        }
        out.extend_from_slice(&data[..size]);
        data = data.get(size + 2..).unwrap_or(&[]);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_login_answers() {
        let ok = b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nSet-Cookie: authToken=abc-123; Path=/; Max-Age=604800; HttpOnly\r\n\r\n{\"message\":\"Login successful\"}";
        let r = parse_response(ok).unwrap();
        assert_eq!(r.status, 200);
        assert_eq!(
            r.cookies[0],
            "authToken=abc-123; Path=/; Max-Age=604800; HttpOnly"
        );

        let limited = b"HTTP/1.1 429 Too Many Requests\r\nRetry-After: 60\r\nTransfer-Encoding: chunked\r\n\r\n5\r\n{\"a\":\r\n2\r\n1}\r\n0\r\n\r\n";
        let r = parse_response(limited).unwrap();
        assert_eq!((r.status, r.retry_after), (429, Some(60)));
        assert_eq!(r.body, b"{\"a\":1}");

        assert!(parse_response(b"garbage").is_err());
    }

    #[test]
    fn hex_with_colons() {
        assert_eq!(hex(&[0x0a, 0xff, 0x00]), "0a:ff:00");
    }
}
