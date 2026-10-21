//! JetKVM devices.
//!
//! The plugin connects as JetKVM's own web UI does: it logs in with the
//! password for a session cookie (`POST /auth/login-local`), opens the
//! signaling WebSocket, and sets up a WebRTC peer connection that receives
//! the device's video and opens two data channels, `rpc` (JSON-RPC) and
//! `hidrpc` (binary keyboard and mouse messages). WebRTC is str0m's, driven
//! here over plain UDP sockets: the device is on the local network, so host
//! candidates are all it takes.

pub mod font;
pub mod h264;
pub mod hid;
pub mod net;
pub mod signaling;
pub mod video;
pub mod yuv;

use crate::address::DeviceAddress;
use crate::host::Host;
use crate::protocol::{FromCasement, State, ToCasement};
use crate::session::Session;
use hid::{Hid, Mode, Out};
use net::Login;
use serde_json::{json, Value};
use signaling::{Listener, OpenError, Signal, Signaling};
use std::io::ErrorKind;
use std::net::{IpAddr, SocketAddr, UdpSocket};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender, SyncSender, TrySendError};
use std::sync::Arc;
use std::time::{Duration, Instant};
use str0m::change::{SdpAnswer, SdpPendingOffer};
use str0m::channel::ChannelId;
use str0m::media::{Direction, MediaKind};
use str0m::net::{Protocol, Receive};
use str0m::{Candidate, Event as RtcEvent, IceConnectionState, Input, Output, Rtc};
use video::Video;

/// How long the peer connection may take to come up.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(20);
/// How long to wait for the HID-RPC handshake before using JSON-RPC.
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(1);
/// How long the connection may be interrupted before the session ends.
const DISCONNECTED_GRACE: Duration = Duration::from_secs(5);
/// How long the device may be silent. It checks the connection at least
/// every two seconds, and gives up on a peer itself after five.
const SILENCE: Duration = Duration::from_secs(8);
/// Input kept while the way to send it isn't known yet.
const MAX_QUEUED_INPUT: usize = 256;
/// Casement ends the process two seconds after `disconnect`.
const DISCONNECT_WAIT: Duration = Duration::from_millis(1500);
/// Room for bursts of video packets (a key frame) between reads.
const RECV_BUFFER: usize = 2 << 20;
const VIDEO_STATE_ID: &str = "casement-video-state";

const TAKEN_OVER: &str =
    "Someone else connected to the JetKVM, which allows one session at a time.";

/// What the session thread waits on.
pub enum Event {
    Signal(Signal),
    SignalClosed,
    Packet {
        data: Vec<u8>,
        source: SocketAddr,
        destination: SocketAddr,
        at: Instant,
    },
    Input(FromCasement),
    Disconnect,
}

/// The main loop's handle on a device session.
pub struct DeviceSession {
    tx: Sender<Event>,
    done: Receiver<()>,
}

/// Starts connecting to `device`. Progress and failures are reported to
/// Casement from the session's thread.
pub fn start(host: Arc<Host>, device: DeviceAddress, password: &str) -> Box<dyn Session> {
    let (tx, rx) = mpsc::channel();
    let (done_tx, done) = mpsc::channel();
    let password = password.to_owned();
    let events = tx.clone();
    let h = host.clone();
    let spawned = std::thread::Builder::new()
        .name("session".into())
        .spawn(move || {
            run(h, &device, password, events, rx);
            let _ = done_tx.send(());
        });
    if let Err(e) = spawned {
        host.finish(State::Failed, &format!("Couldn't start the session: {e}"));
    }
    Box::new(DeviceSession { tx, done })
}

impl Session for DeviceSession {
    fn input(&mut self, msg: &FromCasement) {
        let _ = self.tx.send(Event::Input(msg.clone()));
    }

    fn disconnect(&mut self) {
        if self.tx.send(Event::Disconnect).is_ok() {
            let _ = self.done.recv_timeout(DISCONNECT_WAIT);
        }
    }
}

fn run(
    host: Arc<Host>,
    dev: &DeviceAddress,
    password: String,
    events: Sender<Event>,
    rx: Receiver<Event>,
) {
    log::info!("connecting to {}", dev.base_url());
    let signaling = open_signaling(&host, dev, &password);
    drop(password);
    match Connection::new(host.clone(), signaling, events) {
        Ok(c) => c.run(rx),
        Err(text) => host.finish(State::Failed, &text),
    }
}

/// Logs in and opens the signaling socket.
fn open_signaling(host: &Host, dev: &DeviceAddress, password: &str) -> Signaling {
    // Without a password, try the socket directly: it opens on a device
    // without one, and otherwise says that one is needed.
    let token = if password.is_empty() {
        None
    } else {
        match net::login(dev, password) {
            Ok(Login::Token(t)) => Some(t),
            Ok(Login::NoPassword) => {
                log::info!("the JetKVM has no password; ignoring the one given");
                None
            }
            Ok(Login::Refused) => host.finish(State::AuthFailed, "Wrong password."),
            Err(text) => host.finish(State::Failed, &text),
        }
    };
    match Signaling::open(dev, token.as_deref()) {
        Ok(s) => s,
        Err(OpenError::Unauthorized) if password.is_empty() => {
            host.finish(State::AuthFailed, "This JetKVM needs a password.")
        }
        Err(OpenError::Unauthorized) => {
            host.finish(State::AuthFailed, "The JetKVM didn't accept the login.")
        }
        Err(OpenError::Failed(text)) => host.finish(State::Failed, &text),
    }
}

/// A UDP socket on one local address, with a thread that passes what
/// arrives to the session.
struct Socket {
    local: SocketAddr,
    socket: UdpSocket,
}

impl Socket {
    fn open(ip: IpAddr, events: &Sender<Event>, stop: &Arc<AtomicBool>) -> Result<Self, String> {
        let failed = |e: std::io::Error| format!("Couldn't open a UDP socket on {ip}: {e}");
        let socket = UdpSocket::bind((ip, 0)).map_err(failed)?;
        if let Err(e) = socket2::SockRef::from(&socket).set_recv_buffer_size(RECV_BUFFER) {
            log::debug!("can't enlarge the UDP receive buffer: {e}");
        }
        let local = socket.local_addr().map_err(failed)?;
        let reader = socket.try_clone().map_err(failed)?;
        reader
            .set_read_timeout(Some(Duration::from_millis(200)))
            .map_err(failed)?;
        let (events, stop) = (events.clone(), stop.clone());
        std::thread::Builder::new()
            .name("udp".into())
            .spawn(move || {
                let mut buf = vec![0u8; 65536];
                while !stop.load(Ordering::Relaxed) {
                    match reader.recv_from(&mut buf) {
                        Ok((n, source)) => {
                            let packet = Event::Packet {
                                data: buf[..n].to_vec(),
                                source,
                                destination: local,
                                at: Instant::now(),
                            };
                            if events.send(packet).is_err() {
                                return;
                            }
                        }
                        // Windows reports an ICMP "port unreachable" for an
                        // earlier send as a failed receive.
                        Err(e)
                            if matches!(
                                e.kind(),
                                ErrorKind::WouldBlock
                                    | ErrorKind::TimedOut
                                    | ErrorKind::Interrupted
                                    | ErrorKind::ConnectionReset
                            ) => {}
                        Err(e) => {
                            log::warn!("UDP receive on {local} failed: {e}");
                            return;
                        }
                    }
                }
            })
            .map_err(|e| format!("Couldn't start the UDP thread: {e}"))?;
        log::debug!("UDP socket on {local}");
        Ok(Socket { local, socket })
    }
}

/// The local address that routes to `remote`.
fn route_to(remote: SocketAddr) -> Option<IpAddr> {
    let any: SocketAddr = if remote.is_ipv4() {
        "0.0.0.0:0".parse().ok()?
    } else {
        "[::]:0".parse().ok()?
    };
    let s = UdpSocket::bind(any).ok()?;
    s.connect(remote).ok()?;
    Some(s.local_addr().ok()?.ip())
}

/// The session with the device, from the offer to the end.
struct Connection {
    host: Arc<Host>,
    rtc: Rtc,
    /// When `rtc` next wants `Input::Timeout`.
    rtc_timeout: Instant,
    sockets: Vec<Socket>,
    events: Sender<Event>,
    stop: Arc<AtomicBool>,
    signaling: Option<Listener>,
    /// Our offer, until the answer comes.
    pending: Option<SdpPendingOffer>,
    /// Candidates that came before the answer.
    early_candidates: Vec<String>,
    rpc: ChannelId,
    hidrpc: ChannelId,
    hid: Hid,
    handshake_deadline: Option<Instant>,
    queued: Vec<FromCasement>,
    video: SyncSender<Video>,
    /// A picture was dropped because decoding fell behind.
    video_gap: bool,
    status: Option<(&'static str, &'static str)>,
    connected: bool,
    connect_deadline: Instant,
    disconnected_since: Option<Instant>,
    /// When the last packet came.
    last_packet: Instant,
}

impl Connection {
    fn new(host: Arc<Host>, signaling: Signaling, events: Sender<Event>) -> Result<Self, String> {
        let now = Instant::now();
        let stop = Arc::new(AtomicBool::new(false));
        // The address we reach the device from is the one it can reach us on.
        let first = Socket::open(signaling.local.ip(), &events, &stop)?;
        let mut rtc = Rtc::builder()
            .clear_codecs()
            // JetKVM sends H.265 only when offered it, and H.264 otherwise.
            .enable_h264(true)
            .build(now);
        let candidate = Candidate::host(first.local, "udp")
            .map_err(|e| format!("Couldn't use {}: {e}", first.local))?;
        rtc.add_local_candidate(candidate);
        let mut api = rtc.sdp_api();
        api.add_media(MediaKind::Video, Direction::RecvOnly, None, None, None);
        let rpc = api.add_channel("rpc".into());
        let hidrpc = api.add_channel("hidrpc".into());
        let (offer, pending) = api
            .apply()
            .ok_or("Couldn't make the WebRTC offer.".to_string())?;
        let listener = signaling.offer(&offer.to_sdp_string(), events.clone())?;
        log::debug!("offer sent");
        Ok(Connection {
            video: video::start(host.clone()),
            host,
            rtc,
            rtc_timeout: now,
            sockets: vec![first],
            events,
            stop,
            signaling: Some(listener),
            pending: Some(pending),
            early_candidates: Vec::new(),
            rpc,
            hidrpc,
            hid: Hid::default(),
            handshake_deadline: None,
            queued: Vec::new(),
            video_gap: false,
            status: None,
            connected: false,
            connect_deadline: now + CONNECT_TIMEOUT,
            disconnected_since: None,
            last_packet: now,
        })
    }

    fn run(mut self, rx: Receiver<Event>) {
        self.drain();
        loop {
            let mut deadline = self.rtc_timeout;
            let others = [
                self.hid.next_deadline(),
                self.handshake_deadline,
                (!self.connected).then_some(self.connect_deadline),
                self.disconnected_since.map(|t| t + DISCONNECTED_GRACE),
                self.connected.then_some(self.last_packet + SILENCE),
            ];
            for d in others.into_iter().flatten() {
                deadline = deadline.min(d);
            }
            match rx.recv_timeout(deadline.saturating_duration_since(Instant::now())) {
                Ok(Event::Packet {
                    data,
                    source,
                    destination,
                    at,
                }) => self.receive(&data, source, destination, at),
                Ok(Event::Signal(s)) => self.signal(s),
                Ok(Event::SignalClosed) => {
                    if !self.connected {
                        self.host
                            .finish(State::Failed, "The JetKVM closed the signaling connection.");
                    }
                    log::debug!("signaling closed");
                    self.signaling = None;
                }
                Ok(Event::Input(m)) => self.input(m),
                Ok(Event::Disconnect) | Err(RecvTimeoutError::Disconnected) => {
                    self.close();
                    return;
                }
                Err(RecvTimeoutError::Timeout) => {}
            }
            self.timers(Instant::now());
        }
    }

    /// Takes everything `rtc` has to say: sends packets, handles events.
    /// Must follow every change to `rtc`.
    fn drain(&mut self) {
        loop {
            let mut events = Vec::new();
            loop {
                match self.rtc.poll_output() {
                    Ok(Output::Timeout(t)) => {
                        self.rtc_timeout = t;
                        break;
                    }
                    Ok(Output::Transmit(t)) => {
                        match self.sockets.iter().find(|s| s.local == t.source) {
                            Some(s) => {
                                if let Err(e) = s.socket.send_to(&t.contents, t.destination) {
                                    log::trace!("UDP send to {} failed: {e}", t.destination);
                                }
                            }
                            None => log::debug!("no socket for {}", t.source),
                        }
                    }
                    Ok(Output::Event(e)) => events.push(e),
                    Err(e) => self.end(&format!("The WebRTC connection failed: {e}")),
                }
            }
            if events.is_empty() {
                break;
            }
            for e in events {
                self.event(e);
            }
        }
        if !self.rtc.is_alive() {
            self.end("The JetKVM ended the session.");
        }
    }

    /// Ends the session: `failed` while connecting, `closed` once connected.
    fn end(&self, text: &str) -> ! {
        self.stop.store(true, Ordering::Relaxed);
        if self.connected {
            self.host.finish(State::Closed, text)
        } else {
            self.host.finish(State::Failed, text)
        }
    }

    fn receive(&mut self, data: &[u8], source: SocketAddr, destination: SocketAddr, at: Instant) {
        self.last_packet = at;
        match Receive::new(Protocol::Udp, source, destination, data) {
            Ok(r) => {
                if let Err(e) = self.rtc.handle_input(Input::Receive(at, r)) {
                    log::debug!("packet from {source}: {e}");
                }
                self.drain();
            }
            Err(e) => log::trace!("not WebRTC, from {source}: {e}"),
        }
    }

    fn timers(&mut self, now: Instant) {
        if now >= self.rtc_timeout {
            if let Err(e) = self.rtc.handle_input(Input::Timeout(now)) {
                log::debug!("timeout: {e}");
            }
            self.drain();
        }
        if !self.connected && now >= self.connect_deadline {
            self.end("The JetKVM didn't answer over WebRTC. Is UDP between this computer and the JetKVM blocked?");
        }
        let interrupted = self
            .disconnected_since
            .is_some_and(|t| now >= t + DISCONNECTED_GRACE);
        if interrupted || (self.connected && now >= self.last_packet + SILENCE) {
            self.end("The connection to the JetKVM was lost.");
        }
        if self.handshake_deadline.is_some_and(|t| now >= t) && self.hid.mode == Mode::Pending {
            log::info!("no HID-RPC; sending input as JSON-RPC");
            self.hid.mode = Mode::JsonRpc;
            self.handshake_deadline = None;
            self.replay();
        }
        let outs = self.hid.tick(now);
        self.send(outs);
    }

    fn signal(&mut self, s: Signal) {
        match s {
            Signal::Hello(version) => log::info!("JetKVM software {version}"),
            Signal::Answer(sdp) => {
                let Some(pending) = self.pending.take() else {
                    log::warn!("a second answer; ignored");
                    return;
                };
                let answer = SdpAnswer::from_sdp_string(&sdp)
                    .map_err(|e| e.to_string())
                    .and_then(|a| {
                        self.rtc
                            .sdp_api()
                            .accept_answer(pending, a)
                            .map_err(|e| e.to_string())
                    });
                if let Err(e) = answer {
                    self.end(&format!("The JetKVM's WebRTC answer didn't work: {e}"));
                }
                log::debug!("answer accepted");
                for c in std::mem::take(&mut self.early_candidates) {
                    self.add_candidate(&c);
                }
                self.drain();
            }
            Signal::Candidate(c) => {
                if self.pending.is_some() {
                    self.early_candidates.push(c);
                } else {
                    self.add_candidate(&c);
                    self.drain();
                }
            }
            Signal::Error(e) if !self.connected => {
                let e = if e.is_empty() || e == "{}" {
                    "no reason given".to_string()
                } else {
                    e
                };
                self.end(&format!("The JetKVM refused the connection ({e})."))
            }
            Signal::Error(e) => log::warn!("signaling error: {e}"),
            Signal::Superseded => self.end(TAKEN_OVER),
        }
    }

    /// Adds a device candidate, with a socket on the local address that
    /// routes to it if there is none yet.
    fn add_candidate(&mut self, text: &str) {
        let c = match Candidate::from_sdp_string(text) {
            Ok(c) => c,
            Err(e) => {
                log::debug!("ignoring candidate {text}: {e}");
                return;
            }
        };
        if c.proto() != Protocol::Udp {
            log::debug!("ignoring candidate {text}: not UDP");
            return;
        }
        log::debug!("device candidate {}", c.addr());
        if let Some(ip) = route_to(c.addr()) {
            if !self.sockets.iter().any(|s| s.local.ip() == ip) {
                match Socket::open(ip, &self.events, &self.stop) {
                    Ok(s) => {
                        if let Ok(local) = Candidate::host(s.local, "udp") {
                            self.rtc.add_local_candidate(local);
                        }
                        self.sockets.push(s);
                    }
                    Err(e) => log::debug!("{e}"),
                }
            }
        }
        self.rtc.add_remote_candidate(c);
    }

    fn event(&mut self, e: RtcEvent) {
        match e {
            RtcEvent::Connected => {
                if !self.connected {
                    self.connected = true;
                    log::info!("connected");
                    self.host.send(&ToCasement::state(State::Connected, ""));
                }
            }
            RtcEvent::IceConnectionStateChange(s) => {
                log::debug!("ICE {s:?}");
                if s == IceConnectionState::Disconnected {
                    if self.connected && self.disconnected_since.is_none() {
                        self.disconnected_since = Some(Instant::now());
                    }
                } else if s.is_connected() {
                    self.disconnected_since = None;
                }
            }
            RtcEvent::MediaData(m) => {
                let frame = Video::Frame {
                    data: m.data,
                    contiguous: m.contiguous && !self.video_gap,
                };
                match self.video.try_send(frame) {
                    Ok(()) => self.video_gap = false,
                    Err(TrySendError::Full(_)) => {
                        if !self.video_gap {
                            log::warn!("decoding is behind; skipping to the next key frame");
                        }
                        self.video_gap = true;
                    }
                    Err(TrySendError::Disconnected(_)) => {}
                }
            }
            RtcEvent::ChannelOpen(id, label) => {
                log::debug!("channel {label} open");
                if self.hid.mode == Mode::Pending && self.handshake_deadline.is_none() {
                    self.handshake_deadline = Some(Instant::now() + HANDSHAKE_TIMEOUT);
                }
                if id == self.hidrpc {
                    self.write(id, true, &hid::handshake());
                } else if id == self.rpc {
                    let request = json!({
                        "jsonrpc": "2.0",
                        "method": "getVideoState",
                        "params": {},
                        "id": VIDEO_STATE_ID,
                    });
                    self.write(id, false, request.to_string().as_bytes());
                }
            }
            RtcEvent::ChannelData(d) => {
                if d.id == self.hidrpc {
                    self.hid_message(&d.data);
                } else if d.id == self.rpc {
                    self.rpc_message(&d.data);
                }
            }
            RtcEvent::ChannelClose(id) if id == self.rpc || id == self.hidrpc => {
                self.end("The JetKVM ended the session.")
            }
            _ => {}
        }
    }

    fn hid_message(&mut self, data: &[u8]) {
        if data.first() == Some(&hid::HANDSHAKE) && self.hid.mode == Mode::Pending {
            log::info!("HID-RPC version {}", data.get(1).copied().unwrap_or(0));
            self.hid.mode = Mode::HidRpc;
            self.handshake_deadline = None;
            self.replay();
        }
    }

    fn rpc_message(&mut self, data: &[u8]) {
        let Ok(v) = serde_json::from_slice::<Value>(data) else {
            return;
        };
        match v.get("method").and_then(Value::as_str) {
            Some("otherSessionConnected") => self.end(TAKEN_OVER),
            Some("willReboot") => self.end("The JetKVM is restarting."),
            Some("videoInputState") => self.video_state(v.get("params")),
            Some(_) => {}
            None if v.get("id").and_then(Value::as_str) == Some(VIDEO_STATE_ID) => {
                self.video_state(v.get("result"))
            }
            None => {
                if let Some(e) = v.get("error") {
                    log::debug!("JSON-RPC error: {e}");
                }
            }
        }
    }

    fn video_state(&mut self, state: Option<&Value>) {
        let Some(state) = state else { return };
        let status = video::status_for(state);
        if status.is_none() {
            log::info!(
                "video {}x{} at {} fps",
                state.get("width").unwrap_or(&Value::Null),
                state.get("height").unwrap_or(&Value::Null),
                state.get("fps").unwrap_or(&Value::Null)
            );
        }
        if status != self.status {
            self.status = status;
            if let Some((title, detail)) = status {
                let _ = self.video.send(Video::Status(title, detail));
            }
        }
    }

    fn input(&mut self, m: FromCasement) {
        if self.hid.mode == Mode::Pending {
            if self.queued.len() < MAX_QUEUED_INPUT {
                self.queued.push(m);
            }
            return;
        }
        let outs = match m {
            FromCasement::Key { scancode, down } => self.hid.key(scancode, down, Instant::now()),
            FromCasement::Pointer { x, y, buttons } => {
                let size = self.host.display().size();
                if size.0 == 0 || size.1 == 0 {
                    return;
                }
                self.hid.pointer(x, y, buttons, size)
            }
            FromCasement::Wheel { delta } => self.hid.wheel(delta),
            other => {
                log::debug!("ignoring {other:?}");
                return;
            }
        };
        self.send(outs);
    }

    /// Sends input that came before the device's HID mode was known.
    fn replay(&mut self) {
        for m in std::mem::take(&mut self.queued) {
            self.input(m);
        }
    }

    fn send(&mut self, outs: Vec<Out>) {
        if outs.is_empty() {
            return;
        }
        for out in outs {
            match out {
                Out::Hid(bytes) => self.write(self.hidrpc, true, &bytes),
                Out::Rpc(v) => self.write(self.rpc, false, v.to_string().as_bytes()),
            }
        }
        self.drain();
    }

    fn write(&mut self, id: ChannelId, binary: bool, data: &[u8]) {
        match self.rtc.channel(id).map(|mut c| c.write(binary, data)) {
            Some(Ok(true)) => {}
            Some(Ok(false)) => log::warn!("data channel full; input dropped"),
            Some(Err(e)) => log::warn!("data channel write failed: {e}"),
            None => log::debug!("data channel not open; input dropped"),
        }
    }

    /// Releases held keys and buttons and closes the connection.
    fn close(&mut self) {
        log::info!("disconnecting");
        let outs = self.hid.release_all();
        self.send(outs);
        if let Err(e) = self.rtc.close() {
            log::debug!("close: {e}");
        }
        self.drain_closing();
        self.stop.store(true, Ordering::Relaxed);
        let _ = self.video.send(Video::Stop);
        self.signaling = None;
    }

    /// Sends what closing produces (RTCP BYE, SCTP shutdown, DTLS
    /// close_notify), without waiting for answers.
    fn drain_closing(&mut self) {
        while let Ok(o) = self.rtc.poll_output() {
            match o {
                Output::Transmit(t) => {
                    if let Some(s) = self.sockets.iter().find(|s| s.local == t.source) {
                        let _ = s.socket.send_to(&t.contents, t.destination);
                    }
                }
                Output::Event(_) => {}
                Output::Timeout(_) => break,
            }
        }
    }
}
