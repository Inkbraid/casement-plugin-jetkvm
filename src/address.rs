//! `jetkvm://` addresses.
//!
//! ```text
//! jetkvm://<host>[:<port>][?tls[&fingerprint=<sha256 of the certificate, hex>]]
//! jetkvm://test[?width=…&height=…&fps=…&password=…&fail=…&close_after=…&resize_every=…]
//! ```
//!
//! Without `tls` the device is reached over plain HTTP (port 80), as JetKVM
//! serves it by default. With `tls` it is HTTPS (port 443); the certificate
//! is checked against the system's roots, or, for JetKVM's self-signed
//! certificate, against `fingerprint`.

use percent_encoding::percent_decode_str;
use std::collections::HashMap;

pub const SCHEME: &str = "jetkvm";
pub const HTTP_PORT: u16 = 80;
pub const HTTPS_PORT: u16 = 443;

#[derive(Debug, Clone, PartialEq)]
pub enum Address {
    /// A JetKVM device.
    Device(DeviceAddress),
    /// The built-in test pattern.
    Test(TestOptions),
}

#[derive(Debug, Clone, PartialEq)]
pub struct DeviceAddress {
    /// Host name, IPv4 address or bracketed IPv6 address.
    pub host: String,
    pub port: u16,
    pub tls: bool,
    /// SHA-256 of the device's TLS certificate, to accept a self-signed one.
    pub fingerprint: Option<[u8; 32]>,
}

impl DeviceAddress {
    /// The device's base URL, e.g. `http://192.168.1.20` or `https://kvm:8443`.
    pub fn base_url(&self) -> String {
        let (scheme, default) = if self.tls {
            ("https", HTTPS_PORT)
        } else {
            ("http", HTTP_PORT)
        };
        if self.port == default {
            format!("{scheme}://{}", self.host)
        } else {
            format!("{scheme}://{}:{}", self.host, self.port)
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct TestOptions {
    pub width: u32,
    pub height: u32,
    pub fps: u32,
    /// Report `auth_failed` unless `connect` carries this password.
    pub password: Option<String>,
    /// Report `failed` with this text instead of connecting.
    pub fail: Option<String>,
    /// Report `closed` after this many seconds.
    pub close_after: Option<f64>,
    /// Switch between two display sizes every this many seconds.
    pub resize_every: Option<f64>,
}

impl Default for TestOptions {
    fn default() -> Self {
        TestOptions {
            width: 1280,
            height: 720,
            fps: 30,
            password: None,
            fail: None,
            close_after: None,
            resize_every: None,
        }
    }
}

/// Parses an address. Errors are sentences for the user.
pub fn parse(address: &str) -> Result<Address, String> {
    let rest = address
        .split_once("://")
        .filter(|(scheme, _)| scheme.eq_ignore_ascii_case(SCHEME))
        .map(|(_, rest)| rest)
        .ok_or_else(|| format!("“{address}” isn't a jetkvm:// address."))?;
    let (target, query) = rest.split_once('?').unwrap_or((rest, ""));
    let target = target.trim_end_matches('/');
    let params = parse_query(query)?;

    if target.eq_ignore_ascii_case("test") {
        return parse_test(&params).map(Address::Test);
    }

    let tls = match params.get("tls").map(String::as_str) {
        None => false,
        Some("" | "1" | "true" | "yes") => true,
        Some("0" | "false" | "no") => false,
        Some(v) => {
            return Err(format!(
                "tls={v} isn't understood; write tls or leave it out."
            ))
        }
    };
    let (host, port) = split_host_port(target, if tls { HTTPS_PORT } else { HTTP_PORT })?;
    let fingerprint = match params.get("fingerprint") {
        None => None,
        Some(f) => Some(parse_fingerprint(f)?),
    };
    if fingerprint.is_some() && !tls {
        return Err("fingerprint= only applies with tls.".into());
    }
    Ok(Address::Device(DeviceAddress {
        host,
        port,
        tls,
        fingerprint,
    }))
}

fn parse_query(query: &str) -> Result<HashMap<String, String>, String> {
    let mut params = HashMap::new();
    for pair in query.split('&').filter(|p| !p.is_empty()) {
        let (k, v) = pair.split_once('=').unwrap_or((pair, ""));
        let decode = |s: &str| {
            percent_decode_str(s)
                .decode_utf8()
                .map(|s| s.into_owned())
                .map_err(|_| format!("“{pair}” in the address isn't valid text."))
        };
        params.insert(decode(k)?.to_ascii_lowercase(), decode(v)?);
    }
    Ok(params)
}

/// Splits `host`, `host:port`, `[v6]` or `[v6]:port`.
fn split_host_port(target: &str, default_port: u16) -> Result<(String, u16), String> {
    let target = percent_decode_str(target)
        .decode_utf8()
        .map_err(|_| "The address isn't valid text.".to_string())?;
    let target = target.trim();
    let bad = || format!("“{target}” isn't a host name or host:port.");
    if target.is_empty() {
        return Err("The address has no device.".into());
    }
    if target.contains(['/', '?', '#', ' ', '@']) {
        return Err(bad());
    }
    let port = |p: &str| p.parse::<u16>().ok().filter(|p| *p != 0).ok_or_else(bad);
    if let Some(rest) = target.strip_prefix('[') {
        let (v6, after) = rest.split_once(']').ok_or_else(bad)?;
        v6.parse::<std::net::Ipv6Addr>().map_err(|_| bad())?;
        let p = match after {
            "" => default_port,
            _ => port(after.strip_prefix(':').ok_or_else(bad)?)?,
        };
        return Ok((format!("[{v6}]"), p));
    }
    match target.matches(':').count() {
        0 => Ok((target.to_string(), default_port)),
        1 => {
            let (h, p) = target.split_once(':').ok_or_else(bad)?;
            if h.is_empty() {
                return Err(bad());
            }
            Ok((h.to_string(), port(p)?))
        }
        // A bare IPv6 address.
        _ => {
            target.parse::<std::net::Ipv6Addr>().map_err(|_| bad())?;
            Ok((format!("[{target}]"), default_port))
        }
    }
}

fn parse_fingerprint(f: &str) -> Result<[u8; 32], String> {
    let hex: String = f.chars().filter(|c| *c != ':').collect();
    let bad = || {
        "fingerprint= should be the certificate's SHA-256, 64 hex digits (colons allowed)."
            .to_string()
    };
    if hex.len() != 64 || !hex.is_ascii() {
        return Err(bad());
    }
    let mut out = [0u8; 32];
    for (i, b) in out.iter_mut().enumerate() {
        *b = u8::from_str_radix(&hex[i * 2..i * 2 + 2], 16).map_err(|_| bad())?;
    }
    Ok(out)
}

fn parse_test(params: &HashMap<String, String>) -> Result<TestOptions, String> {
    let mut o = TestOptions::default();
    let num = |k: &str| -> Result<Option<f64>, String> {
        params
            .get(k)
            .map(|v| {
                v.parse::<f64>()
                    .ok()
                    .filter(|n| n.is_finite() && *n > 0.0)
                    .ok_or(format!("{k}={v} isn't a positive number."))
            })
            .transpose()
    };
    if let Some(w) = num("width")? {
        o.width = (w as u32).clamp(16, 8192);
    }
    if let Some(h) = num("height")? {
        o.height = (h as u32).clamp(16, 8192);
    }
    if let Some(f) = num("fps")? {
        o.fps = (f as u32).clamp(1, 240);
    }
    o.close_after = num("close_after")?;
    o.resize_every = num("resize_every")?;
    o.password = params.get("password").cloned();
    o.fail = params.get("fail").cloned();
    Ok(o)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn device(a: &str) -> DeviceAddress {
        match parse(a).unwrap() {
            Address::Device(d) => d,
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn plain_http() {
        let d = device("jetkvm://192.168.1.20");
        assert_eq!(
            (d.host.as_str(), d.port, d.tls),
            ("192.168.1.20", 80, false)
        );
        assert_eq!(d.base_url(), "http://192.168.1.20");
        let d = device("JetKVM://kvm.local:8080/");
        assert_eq!(d.base_url(), "http://kvm.local:8080");
    }

    #[test]
    fn tls_and_fingerprint() {
        let fp = "AB:".repeat(31) + "AB";
        let d = device(&format!("jetkvm://kvm?tls&fingerprint={fp}"));
        assert_eq!(
            (d.port, d.tls, d.fingerprint),
            (443, true, Some([0xAB; 32]))
        );
        assert_eq!(d.base_url(), "https://kvm");
        let d = device("jetkvm://[fd00::5]:8443?tls=1");
        assert_eq!(d.base_url(), "https://[fd00::5]:8443");
        assert_eq!(device("jetkvm://fd00::5").base_url(), "http://[fd00::5]");
    }

    #[test]
    fn rejects_bad_addresses() {
        assert!(parse("vnc://host").is_err());
        assert!(parse("jetkvm://").is_err());
        assert!(parse("jetkvm://a b").is_err());
        assert!(parse("jetkvm://host:0").is_err());
        assert!(parse("jetkvm://host:99999").is_err());
        assert!(parse("jetkvm://host?tls=maybe").is_err());
        assert!(parse("jetkvm://host?tls&fingerprint=00").is_err());
        assert!(parse(&format!("jetkvm://host?fingerprint={}", "0".repeat(64))).is_err());
        assert!(parse("jetkvm://user@host").is_err());
        assert!(parse("jetkvm://[nope]").is_err());
    }

    #[test]
    fn test_address() {
        assert_eq!(
            parse("jetkvm://test").unwrap(),
            Address::Test(TestOptions::default())
        );
        let Address::Test(o) =
            parse("jetkvm://test?width=640&height=480&password=pw&close_after=2").unwrap()
        else {
            panic!()
        };
        assert_eq!((o.width, o.height), (640, 480));
        assert_eq!(o.password.as_deref(), Some("pw"));
        assert_eq!(o.close_after, Some(2.0));
        assert!(parse("jetkvm://test?fps=x").is_err());
    }
}
