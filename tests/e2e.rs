//! The plugin against `tools/fake-jetkvm`, which runs JetKVM's own stack
//! (pion WebRTC, its login and signaling) with a looping H.264 file for a
//! picture. Set `FAKE_JETKVM` to the built program to run these; with
//! `CASEMENT_E2E=1` (as in CI) a missing program is an error instead of a
//! skip.

use casement_jetkvm::jetkvm::hid;
use casement_jetkvm::jetkvm::video::{STATUS_BACKGROUND, STATUS_TEXT};
use casement_jetkvm::protocol::{FromCasement, State, ToCasement};
use casement_jetkvm::sim::{Casement, HostSurface};
use serde_json::Value;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::time::{Duration, Instant};

/// Generous: the WebRTC set-up includes ICE, DTLS and SCTP.
const T: Duration = Duration::from_secs(15);

struct Device {
    child: Child,
    events: Receiver<Value>,
    /// `host:port`.
    hostport: String,
    /// Hex SHA-256 of the TLS certificate, with `-tls`.
    fingerprint: String,
}

impl Device {
    /// Starts the fake device with these flags, or `None` to skip the test.
    fn start(args: &[&str]) -> Option<Device> {
        let Some(exe) = std::env::var_os("FAKE_JETKVM").map(PathBuf::from) else {
            if std::env::var_os("CASEMENT_E2E").is_some() {
                panic!("FAKE_JETKVM isn't set");
            }
            eprintln!("FAKE_JETKVM isn't set; skipping");
            return None;
        };
        let video = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tools/fake-jetkvm/testdata/bars-640x360.h264");
        let mut child = Command::new(exe)
            .args(["-listen", "127.0.0.1:0"])
            .arg("-video")
            .arg(video)
            .args(args)
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("start fake-jetkvm");
        let stdout = child.stdout.take().unwrap();
        let (tx, events) = mpsc::channel();
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                if let Ok(v) = serde_json::from_str::<Value>(&line) {
                    if tx.send(v).is_err() {
                        return;
                    }
                }
            }
        });
        let first = events
            .recv_timeout(T)
            .expect("fake-jetkvm says where it listens");
        assert_eq!(first["event"], "listening");
        let url = first["url"].as_str().unwrap();
        Some(Device {
            child,
            events,
            hostport: url.split_once("://").unwrap().1.to_string(),
            fingerprint: first["fingerprint"].as_str().unwrap_or("").to_string(),
        })
    }

    fn address(&self, query: &str) -> String {
        format!("jetkvm://{}{query}", self.hostport)
    }

    /// Waits for an event the predicate accepts, passing over others.
    fn expect(&self, what: &str, mut pick: impl FnMut(&Value) -> bool) -> Value {
        let end = Instant::now() + T;
        loop {
            let left = end.saturating_duration_since(Instant::now());
            match self.events.recv_timeout(left) {
                Ok(v) if pick(&v) => return v,
                Ok(_) => {}
                Err(_) => panic!("the device never reported {what}"),
            }
        }
    }
}

impl Drop for Device {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn plugin() -> Casement {
    let mut c = Casement::spawn(Path::new(env!("CARGO_BIN_EXE_casement-jetkvm")), true).unwrap();
    c.send(&FromCasement::Hello {
        protocol: 1,
        app: "Casement test".into(),
    })
    .unwrap();
    assert_eq!(c.recv(T), Some(ToCasement::hello()));
    c
}

fn connect(address: &str, password: &str) -> Casement {
    let mut c = plugin();
    c.send(&FromCasement::Connect {
        address: address.into(),
        username: String::new(),
        password: password.into(),
        title: "Test".into(),
    })
    .unwrap();
    c
}

fn state(c: &Casement) -> (State, String) {
    c.expect(T, |m| match m {
        ToCasement::State { state, text } => Some((*state, text.clone())),
        _ => None,
    })
    .expect("a state message")
}

fn surface_request(c: &Casement) -> (u32, u32) {
    c.expect(T, |m| match m {
        ToCasement::SurfaceRequest { width, height } => Some((*width, *height)),
        _ => None,
    })
    .expect("a surface_request")
}

/// Answers the surface request and returns the surface after its first frame.
fn first_frame(c: &mut Casement, (w, h): (u32, u32)) -> HostSurface {
    let (s, msg) = c.create_surface(1, w, h, 0).unwrap();
    c.send(&msg).unwrap();
    let rects = c
        .expect(T, |m| match m {
            ToCasement::Frame { surface: 1, rects } => Some(rects.clone()),
            _ => None,
        })
        .expect("a frame");
    assert!(rects.is_empty(), "the first frame covers everything");
    s
}

fn pixel(s: &HostSurface, width: u32, x: u32, y: u32) -> [u8; 4] {
    let i = ((y * width + x) * 4) as usize;
    s.pixels()[i..i + 4].try_into().unwrap()
}

/// The fake device's picture: SMPTE bars, 75 % white, yellow, cyan, green,
/// magenta, red and blue across the top two thirds of 640x360.
fn assert_bars(s: &HostSurface) {
    let bars: [[u8; 3]; 7] = [
        [191, 191, 191],
        [0, 191, 191],
        [191, 191, 0],
        [0, 191, 0],
        [191, 0, 191],
        [0, 0, 191],
        [191, 0, 0],
    ];
    for (i, want) in bars.iter().enumerate() {
        let x = (i as u32 * 640 / 7) + 640 / 14;
        let got = pixel(s, 640, x, 100);
        for ch in 0..3 {
            assert!(
                (got[ch] as i32 - want[ch] as i32).abs() <= 24,
                "bar {i} at x={x}: got {got:?}, want {want:?} (BGR)"
            );
        }
    }
}

fn disconnect(mut c: Casement) {
    c.send(&FromCasement::Disconnect).unwrap();
    c.close_stdin();
    let status = c
        .wait(Duration::from_secs(3))
        .expect("exits after disconnect");
    assert_eq!(status.code(), Some(0));
}

fn send(c: &mut Casement, msg: FromCasement) {
    c.send(&msg).unwrap();
}

fn is_hid(v: &Value, kind: &str) -> bool {
    v["event"] == "hid" && v["type"] == kind
}

#[test]
fn video_and_input() {
    let Some(dev) = Device::start(&["-password", "secret"]) else {
        return;
    };
    let mut c = connect(&dev.address(""), "secret");
    assert_eq!(state(&c), (State::Connected, String::new()));
    dev.expect("a login", |v| v["event"] == "login" && v["ok"] == true);

    assert_eq!(surface_request(&c), (640, 360));
    let s = first_frame(&mut c, (640, 360));
    assert_bars(&s);
    dev.expect("the HID-RPC handshake", |v| is_hid(v, "handshake"));

    // The pointer, scaled to 0..32767, with a right click (HID button 2).
    send(
        &mut c,
        FromCasement::Pointer {
            x: 320,
            y: 180,
            buttons: 0,
        },
    );
    send(
        &mut c,
        FromCasement::Pointer {
            x: 320,
            y: 180,
            buttons: 4,
        },
    );
    send(
        &mut c,
        FromCasement::Pointer {
            x: 639,
            y: 359,
            buttons: 0,
        },
    );
    let (x, y) = (hid::scale(320, 640), hid::scale(180, 360));
    let p = dev.expect("a pointer report", |v| is_hid(v, "pointer"));
    assert_eq!(
        (p["x"].as_i64(), p["y"].as_i64()),
        (Some(x as i64), Some(y as i64))
    );
    let p = dev.expect("a right click", |v| {
        is_hid(v, "pointer") && v["buttons"] == 2
    });
    assert_eq!(p["x"].as_i64(), Some(x as i64));
    let p = dev.expect("the far corner", |v| {
        is_hid(v, "pointer") && v["buttons"] == 0 && v["x"] != x
    });
    assert_eq!(
        (p["x"].as_i64(), p["y"].as_i64()),
        (Some(32767), Some(32767))
    );

    // A held key is kept alive; a modifier isn't.
    send(
        &mut c,
        FromCasement::Key {
            scancode: 0x1E,
            down: true,
        },
    );
    std::thread::sleep(Duration::from_millis(300));
    send(
        &mut c,
        FromCasement::Key {
            scancode: 0x1E,
            down: false,
        },
    );
    dev.expect("A down", |v| {
        is_hid(v, "keypress") && v["key"] == 0x04 && v["press"] == true
    });
    let mut keepalives = 0;
    dev.expect("A up", |v| {
        if is_hid(v, "keepalive") {
            keepalives += 1;
        }
        is_hid(v, "keypress") && v["key"] == 0x04 && v["press"] == false
    });
    assert!(keepalives >= 3, "only {keepalives} keep-alives in 300 ms");
    send(
        &mut c,
        FromCasement::Key {
            scancode: 0x11D,
            down: true,
        },
    );
    std::thread::sleep(Duration::from_millis(200));
    send(
        &mut c,
        FromCasement::Key {
            scancode: 0x11D,
            down: false,
        },
    );
    dev.expect("Right Ctrl down", |v| {
        is_hid(v, "keypress") && v["key"] == 0xE4 && v["press"] == true
    });
    let mut keepalives = 0;
    dev.expect("Right Ctrl up", |v| {
        if is_hid(v, "keepalive") {
            keepalives += 1;
        }
        is_hid(v, "keypress") && v["key"] == 0xE4 && v["press"] == false
    });
    assert_eq!(keepalives, 0, "modifiers need no keep-alive");

    // Pause, sent by Casement as 0x21D then 0x45, is the HID Pause key.
    for (scancode, down) in [(0x21D, true), (0x45, true), (0x21D, false), (0x45, false)] {
        send(&mut c, FromCasement::Key { scancode, down });
    }
    dev.expect("Pause down", |v| {
        is_hid(v, "keypress") && v["key"] == 0x48 && v["press"] == true
    });
    dev.expect("Pause up", |v| {
        is_hid(v, "keypress") && v["key"] == 0x48 && v["press"] == false
    });

    // The wheel goes over JSON-RPC.
    send(&mut c, FromCasement::Wheel { delta: -2 });
    let w = dev.expect("a wheel report", |v| {
        v["event"] == "rpc" && v["method"] == "wheelReport"
    });
    assert_eq!(w["params"]["wheelY"], -2);
    assert_eq!(w["params"]["wheelX"], 0);

    // Disconnecting releases what is still held, then closes the connection.
    send(
        &mut c,
        FromCasement::Key {
            scancode: 0x30,
            down: true,
        },
    );
    dev.expect("B down", |v| {
        is_hid(v, "keypress") && v["key"] == 0x05 && v["press"] == true
    });
    disconnect(c);
    dev.expect("B released", |v| {
        is_hid(v, "keypress") && v["key"] == 0x05 && v["press"] == false
    });
    dev.expect("the connection closed", |v| {
        v["event"] == "ice" && v["state"] == "closed"
    });
}

#[test]
fn passwords() {
    let Some(dev) = Device::start(&["-password", "secret", "-max-failures", "2"]) else {
        return;
    };
    let c = connect(&dev.address(""), "wrong");
    assert_eq!(state(&c), (State::AuthFailed, "Wrong password.".into()));

    let mut c = connect(&dev.address(""), "");
    assert_eq!(
        state(&c),
        (State::AuthFailed, "This JetKVM needs a password.".into())
    );
    assert_eq!(c.wait(T).and_then(|s| s.code()), Some(0));

    // The second wrong password is the device's limit; it then refuses logins.
    let c = connect(&dev.address(""), "wrong again");
    assert_eq!(state(&c).0, State::AuthFailed);
    let mut c = connect(&dev.address(""), "secret");
    let (s, text) = state(&c);
    assert_eq!(s, State::Failed);
    assert!(text.contains("try again in 60 seconds"), "{text}");
    assert_eq!(c.wait(T).and_then(|s| s.code()), Some(1));
}

#[test]
fn device_without_password() {
    let Some(dev) = Device::start(&[]) else {
        return;
    };
    // A password given anyway is ignored.
    let mut c = connect(&dev.address(""), "anything");
    assert_eq!(state(&c).0, State::Connected);
    assert_eq!(surface_request(&c), (640, 360));
    first_frame(&mut c, (640, 360));
    disconnect(c);
    dev.expect("the connection closed", |v| {
        v["event"] == "ice" && v["state"] == "closed"
    });
}

#[test]
fn tls_with_fingerprint() {
    let Some(dev) = Device::start(&["-tls"]) else {
        return;
    };
    let fp = &dev.fingerprint;
    assert_eq!(fp.len(), 64);
    let colons = fp
        .as_bytes()
        .chunks(2)
        .map(|p| std::str::from_utf8(p).unwrap())
        .collect::<Vec<_>>()
        .join(":");

    // Self-signed, so without the fingerprint it fails and says what to add.
    let c = connect(&dev.address("?tls"), "");
    let (s, text) = state(&c);
    assert_eq!(s, State::Failed);
    assert!(text.contains(&format!("&fingerprint={colons}")), "{text}");

    let c = connect(
        &dev.address(&format!("?tls&fingerprint={}", "0".repeat(64))),
        "",
    );
    let (s, text) = state(&c);
    assert_eq!(s, State::Failed);
    assert!(text.contains("doesn't match"), "{text}");

    let mut c = connect(&dev.address(&format!("?tls&fingerprint={fp}")), "");
    assert_eq!(state(&c).0, State::Connected);
    let size = surface_request(&c);
    first_frame(&mut c, size);
    disconnect(c);
}

#[test]
fn another_session_takes_over() {
    let Some(dev) = Device::start(&[]) else {
        return;
    };
    let mut first = connect(&dev.address(""), "");
    assert_eq!(state(&first).0, State::Connected);
    // Its rpc channel is open once it has asked for the video state.
    dev.expect("the first session's rpc channel", |v| {
        v["event"] == "rpc" && v["method"] == "getVideoState"
    });
    let second = connect(&dev.address(""), "");
    assert_eq!(state(&second).0, State::Connected);
    let (s, text) = state(&first);
    assert_eq!(s, State::Closed);
    assert!(text.contains("Someone else connected"), "{text}");
    assert_eq!(first.wait(T).and_then(|s| s.code()), Some(0));
    disconnect(second);
}

#[test]
fn no_signal_shows_a_status_picture() {
    let Some(dev) = Device::start(&["-signal=false"]) else {
        return;
    };
    let mut c = connect(&dev.address(""), "");
    assert_eq!(state(&c).0, State::Connected);
    assert_eq!(surface_request(&c), (1280, 720));
    let s = first_frame(&mut c, (1280, 720));
    assert_eq!(pixel(&s, 1280, 0, 0), STATUS_BACKGROUND);
    let text = s
        .pixels()
        .chunks_exact(4)
        .filter(|p| *p == STATUS_TEXT)
        .count();
    assert!(text > 100, "no title drawn ({text} pixels)");
    disconnect(c);
}

#[test]
fn lost_video_packets_are_resent() {
    let Some(dev) = Device::start(&["-loss", "0.1"]) else {
        return;
    };
    let mut c = connect(&dev.address(""), "");
    assert_eq!(state(&c).0, State::Connected);
    assert_eq!(surface_request(&c), (640, 360));
    let s = first_frame(&mut c, (640, 360));
    assert_bars(&s);
    disconnect(c);
}

#[test]
fn device_that_disappears_closes_the_session() {
    let Some(mut dev) = Device::start(&[]) else {
        return;
    };
    let mut c = connect(&dev.address(""), "");
    assert_eq!(state(&c).0, State::Connected);
    assert_eq!(surface_request(&c), (640, 360));
    first_frame(&mut c, (640, 360));
    // Pulled from the network, or switched off: nothing more arrives.
    dev.child.kill().unwrap();
    let gone = Instant::now();
    let (s, text) = state(&c);
    assert_eq!(s, State::Closed);
    assert_eq!(text, "The connection to the JetKVM was lost.");
    assert!(
        gone.elapsed() < Duration::from_secs(12),
        "{:?}",
        gone.elapsed()
    );
    assert_eq!(c.wait(T).and_then(|s| s.code()), Some(0));
}

#[test]
fn unreachable_device_fails() {
    // A port nobody listens on.
    let port = {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        l.local_addr().unwrap().port()
    };
    let mut c = connect(&format!("jetkvm://127.0.0.1:{port}"), "secret");
    let (s, text) = state(&c);
    assert_eq!(s, State::Failed);
    assert!(text.contains("Couldn't reach the JetKVM"), "{text}");
    assert_eq!(c.wait(T).and_then(|s| s.code()), Some(1));
}
