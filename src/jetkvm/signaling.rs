//! WebRTC signaling with the device over its WebSocket
//! (`/webrtc/signaling/client`), as JetKVM's web UI does it: the device
//! greets with `device-metadata`, the client sends an `offer` (its SDP as
//! base64 JSON), the device answers with `answer` the same way, and trickles
//! its ICE candidates as `new-ice-candidate`. Ours are all in the offer, so
//! after it the socket is only read.

use super::net::{self, Conn, Failure};
use super::Event;
use crate::address::DeviceAddress;
use base64::Engine as _;
use serde_json::{json, Value};
use std::net::{Shutdown, SocketAddr, TcpStream};
use std::sync::mpsc::Sender;
use tungstenite::client::IntoClientRequest;
use tungstenite::{Message, WebSocket};

/// What the device says over the signaling socket.
#[derive(Debug, Clone, PartialEq)]
pub enum Signal {
    /// `device-metadata`: the device's software version.
    Hello(String),
    /// The answer's SDP.
    Answer(String),
    /// A remote ICE candidate (`candidate:…`).
    Candidate(String),
    /// `{"error": …}`.
    Error(String),
    /// Another session took the device over (`other-session-connected`, or
    /// the socket closed with code 4001).
    Superseded,
}

/// Why opening the socket failed.
pub enum OpenError {
    /// The device wants a login (HTTP 401).
    Unauthorized,
    Failed(Failure),
}

/// An open signaling socket, before the offer.
pub struct Signaling {
    ws: WebSocket<Conn>,
    /// The addresses of the TCP connection: ours (on the interface that
    /// reaches the device) and the device's.
    pub local: SocketAddr,
    pub peer: SocketAddr,
}

/// The socket after the offer: a thread reads it and reports to the session
/// as `Event::Signal` and finally `Event::SignalClosed`. Dropping this
/// closes the socket.
pub struct Listener {
    tcp: Option<TcpStream>,
}

impl Drop for Listener {
    fn drop(&mut self) {
        if let Some(t) = &self.tcp {
            let _ = t.shutdown(Shutdown::Both);
        }
    }
}

impl Signaling {
    pub fn open(dev: &DeviceAddress, token: Option<&str>) -> Result<Signaling, OpenError> {
        let conn = net::connect(dev).map_err(OpenError::Failed)?;
        let url = format!(
            "{}://{}/webrtc/signaling/client",
            if dev.tls { "wss" } else { "ws" },
            net::host_header(dev)
        );
        let mut request = url
            .into_client_request()
            .map_err(|e| OpenError::Failed(format!("Couldn't build the signaling request: {e}")))?;
        if let Some(t) = token {
            let cookie = format!("authToken={t}")
                .parse()
                .map_err(|_| OpenError::Failed("The JetKVM's session token isn't valid.".into()))?;
            request.headers_mut().insert("Cookie", cookie);
        }
        let ws = match tungstenite::client::client(request, conn) {
            Ok((ws, _)) => ws,
            Err(tungstenite::HandshakeError::Failure(tungstenite::Error::Http(r)))
                if r.status() == 401 =>
            {
                return Err(OpenError::Unauthorized)
            }
            Err(e) => {
                return Err(OpenError::Failed(format!(
                    "The JetKVM's signaling connection failed: {e}"
                )))
            }
        };
        let tcp = ws.get_ref().tcp();
        let (local, peer) = tcp
            .local_addr()
            .and_then(|l| Ok((l, tcp.peer_addr()?)))
            .map_err(|e| OpenError::Failed(format!("The signaling connection broke: {e}")))?;
        Ok(Signaling { ws, local, peer })
    }

    /// Sends our offer, then reads the socket on a thread.
    pub fn offer(mut self, sdp: &str, events: Sender<Event>) -> Result<Listener, Failure> {
        let sd = base64::engine::general_purpose::STANDARD
            .encode(json!({ "type": "offer", "sdp": sdp }).to_string());
        let offer = json!({ "type": "offer", "data": { "sd": sd } }).to_string();
        self.ws
            .send(Message::text(offer))
            .map_err(|e| format!("The JetKVM's signaling connection failed: {e}"))?;
        // Blocking reads from here: the device pings the socket regularly,
        // and the session closes it through `Listener`.
        let tcp = self.ws.get_ref().tcp();
        let _ = tcp.set_read_timeout(None);
        let listener = Listener {
            tcp: tcp.try_clone().ok(),
        };
        let ws = self.ws;
        std::thread::Builder::new()
            .name("signaling".into())
            .spawn(move || run(ws, events))
            .map_err(|e| format!("Couldn't start signaling: {e}"))?;
        Ok(listener)
    }
}

fn run(mut ws: WebSocket<Conn>, events: Sender<Event>) {
    loop {
        // Reading also answers the device's pings.
        match ws.read() {
            Ok(Message::Text(text)) => {
                if let Some(signal) = parse(text.as_str()) {
                    if events.send(Event::Signal(signal)).is_err() {
                        return;
                    }
                }
            }
            Ok(Message::Close(frame)) => {
                if frame.is_some_and(|f| u16::from(f.code) == 4001) {
                    let _ = events.send(Event::Signal(Signal::Superseded));
                }
                let _ = ws.flush();
                let _ = events.send(Event::SignalClosed);
                return;
            }
            Ok(_) => {}
            Err(e) => {
                log::debug!("signaling closed: {e}");
                let _ = events.send(Event::SignalClosed);
                return;
            }
        }
    }
}

/// Parses one signaling message; unknown ones are ignored.
pub fn parse(text: &str) -> Option<Signal> {
    let v: Value = serde_json::from_str(text).ok()?;
    if let Some(e) = v.get("error") {
        return Some(Signal::Error(match e {
            Value::String(s) => s.clone(),
            other => other.to_string(),
        }));
    }
    match v.get("type")?.as_str()? {
        "device-metadata" => Some(Signal::Hello(
            v.pointer("/data/deviceVersion")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_owned(),
        )),
        "answer" => {
            let raw = base64::engine::general_purpose::STANDARD
                .decode(v.get("data")?.as_str()?)
                .ok()?;
            let desc: Value = serde_json::from_slice(&raw).ok()?;
            Some(Signal::Answer(desc.get("sdp")?.as_str()?.to_owned()))
        }
        "other-session-connected" => Some(Signal::Superseded),
        "new-ice-candidate" => {
            let c = v.pointer("/data/candidate")?.as_str()?;
            (!c.is_empty()).then(|| Signal::Candidate(c.to_owned()))
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_device_messages() {
        assert_eq!(
            parse(r#"{"type":"device-metadata","data":{"deviceVersion":"0.4.8"}}"#),
            Some(Signal::Hello("0.4.8".into()))
        );
        let sd = base64::engine::general_purpose::STANDARD
            .encode(r#"{"type":"answer","sdp":"v=0\r\n"}"#);
        assert_eq!(
            parse(&format!(r#"{{"type":"answer","data":"{sd}"}}"#)),
            Some(Signal::Answer("v=0\r\n".into()))
        );
        assert_eq!(
            parse(
                r#"{"type":"new-ice-candidate","data":{"candidate":"candidate:1 1 udp 2130706431 192.168.1.5 50000 typ host","sdpMid":"0","sdpMLineIndex":0}}"#
            ),
            Some(Signal::Candidate(
                "candidate:1 1 udp 2130706431 192.168.1.5 50000 typ host".into()
            ))
        );
        assert_eq!(
            parse(r#"{"type":"new-ice-candidate","data":{"candidate":""}}"#),
            None
        );
        assert_eq!(
            parse(r#"{"error":"bad offer"}"#),
            Some(Signal::Error("bad offer".into()))
        );
        assert_eq!(
            parse(r#"{"type":"other-session-connected"}"#),
            Some(Signal::Superseded)
        );
        assert_eq!(parse("pong"), None);
    }
}
