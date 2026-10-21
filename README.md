# JetKVM support for Casement

`casement-jetkvm.exe` is a Casement protocol plugin: it lets Casement open
`jetkvm://` addresses in a tab, showing a [JetKVM](https://jetkvm.com) device
on your network — the computer it is plugged into, from its HDMI picture,
with keyboard and mouse through its USB connection.

This is an independent project. It is not made or endorsed by JetKVM;
"JetKVM" names the device and software it works with.

Casement keeps the connection list and saved passwords; the plugin only
connects, shows the picture and passes keyboard and mouse input.

## Status

| | |
|---|---|
| Plugin protocol v1 (framing, start, states, surfaces, frame flow control, input) | done |
| `jetkvm://test` test pattern for testing Casement's side | done |
| JetKVM devices: local password login, TLS with certificate pinning, WebRTC H.264 video, keyboard and mouse | done; tested against a stand-in device (below), not yet against a real JetKVM |
| Audio, paste as typing, virtual media, JetKVM Cloud | later, if at all |

See [CHANGELOG.md](CHANGELOG.md).

## Installing

Download the zip for the latest release from this repository's
[Releases](https://github.com/Inkbraid/casement-plugin-jetkvm/releases) and
unpack it so that you get

```
%LOCALAPPDATA%\Casement\plugins\
    jetkvm\
        plugin.json
        icon.png
        casement-jetkvm.exe
```

then restart Casement. Each release also carries the matching source code
(the tag and GitHub's source archives).

## Addresses

```
jetkvm://<host>[:<port>][?tls[&fingerprint=<sha256>]]
```

- `host`: the device's name or IP address (IPv6 in brackets: `[fd00::5]`).
- `port`: 80 unless given (443 with `tls`).
- `tls`: reach the device over HTTPS, for devices with TLS turned on. The
  certificate is checked against Windows' trusted roots, or, for JetKVM's
  self-signed certificate, against `fingerprint`: the certificate's SHA-256,
  as 64 hex digits (colons allowed).

Examples:

```
jetkvm://192.168.1.50
jetkvm://kvm.lan?tls&fingerprint=3A:5F:…:C2
```

The password comes from Casement (saved, or typed when the plugin reports
that one is needed). The plugin doesn't store it. Devices without a password
(JetKVM's "no password" mode) connect without one.

## How it works with a device

The plugin connects the way JetKVM's own web UI does:

1. It logs in with the password (`POST /auth/login-local`) for a session
   cookie. A wrong password is reported as `auth_failed`, so Casement asks
   for another; after too many the device refuses logins for a while, and
   the plugin says so.
2. It opens the device's signaling WebSocket and offers a WebRTC connection:
   H.264 video from the device, and two data channels, `rpc` (JSON-RPC) and
   `hidrpc` (JetKVM's binary keyboard and mouse messages).
3. Video is decoded by Windows' own H.264 decoder (Media Foundation) and
   handed to Casement; only the parts that changed are copied.
4. Keys go to the device as USB HID key presses, the pointer as absolute
   positions, the wheel as wheel reports. A held key is kept alive the way
   the web UI does it, so the device lets go of it if the connection drops.
   Disconnecting releases everything still held.

Things to know:

- **One session at a time.** A JetKVM serves one session: connecting from
  Casement ends a session open in a browser (or another Casement tab), and
  opening the web UI ends Casement's, which then shows as closed.
- **Network.** Besides HTTP(S) to the device, the video and input travel
  over UDP directly between this computer and the JetKVM (WebRTC, no
  relay), so a firewall must let UDP through on the local network. JetKVM
  Cloud connections aren't supported.
- **No signal.** When the device has no HDMI signal (computer off or
  asleep), the tab shows a message saying so until the picture returns.
- **Windows N.** Media Foundation is part of Windows except in N editions,
  which need the Media Feature Pack (Settings → Apps → Optional features);
  without it the plugin says so instead of showing a picture.

### Test pattern

`jetkvm://test` connects to nothing: it draws a moving test pattern and logs
all input to stderr (Casement's log), so Casement's side of the protocol can be
checked without a device. Options:

| Option | Effect |
|---|---|
| `width=1280&height=720` | picture size |
| `fps=30` | frame rate |
| `password=…` | report `auth_failed` unless Casement sends this password |
| `fail=…` | report `failed` with this text |
| `close_after=5` | report `closed` after 5 seconds |
| `resize_every=3` | switch between two picture sizes every 3 seconds |

The pointer is drawn into the picture as a cross, the way a JetKVM's picture
shows the remote computer's own pointer.

## Building

```
cargo build --release          # target\release\casement-jetkvm.exe
cargo test
```

The Windows build links the C runtime statically (`.cargo/config.toml`) and
is built for the GUI subsystem, so starting it never opens a console window;
its redirected standard handles work as usual.

### Testing without a JetKVM

`tools/fake-jetkvm` is a stand-in device for the end-to-end tests: JetKVM's
login, signaling and WebRTC session set-up on the same libraries the device
runs (pion WebRTC, coder/websocket), with a looping H.264 file as the
picture. It prints the input it receives as JSON lines.

```
cd tools/fake-jetkvm && go build -o ../../target/fake-jetkvm . && cd ../..
FAKE_JETKVM=target/fake-jetkvm cargo test --test e2e
```

Without `FAKE_JETKVM` those tests are skipped (CI sets `CASEMENT_E2E=1`,
which makes a missing device an error). `-password`, `-tls` and
`-signal=false` select the device's modes.

### Trying it without Casement

`examples/casement-sim.rs` plays Casement's side: it answers surface requests,
acknowledges frames, prints the messages and saves the last picture as a PPM.

```
cargo build && cargo run --example casement-sim -- "jetkvm://test" --seconds 3 --out .
```

Off Windows there are no file-mapping handles to pass, so the simulator and
the tests put each surface in a plain file `surface-<handle>` in the folder
named by `CASEMENT_SURFACE_DIR`, and the plugin maps that instead.

### Logging

The plugin logs to stderr, which Casement copies into its log.
`CASEMENT_JETKVM_LOG` takes an [env_logger](https://docs.rs/env_logger)
filter, e.g. `debug`. Nothing but protocol messages is ever written to stdout:
at start the plugin keeps the stdout handle for itself and points the
process's standard output at stderr.

## Licensing

This program is free software under the **GNU General Public License,
version 2 only** ([LICENSE](LICENSE)), the licence of JetKVM's own software
([jetkvm/kvm](https://github.com/jetkvm/kvm)), so that code from it can be
reused here.

Every dependency must be usable under GPL-2.0-only; `deny.toml` lists the
licences allowed and CI checks them (`cargo deny check licenses`) for the
Windows build, which is the one distributed. Crates offered under a choice
of licences (for example "MIT or Apache-2.0") are used under the compatible
one. In particular:

- WebRTC is [str0m](https://github.com/algesten/str0m) (MIT), its DTLS
  from dimpl, with the cryptography itself done by Windows (CNG).
- H.264 is decoded by Windows (Media Foundation), so the plugin contains no
  H.264 decoder of its own.
- The status pictures use the public-domain
  [font8x8](https://github.com/dhepper/font8x8) by Daniel Hepper.

The Linux builds exist only for tests and use OpenSSL and OpenH264 instead;
they are not distributed. `tools/fake-jetkvm` is part of this program's
source under the same licence.

Casement itself is a separate program that talks to this plugin only through
the documented protocol in [docs/plugin-protocol.md](docs/plugin-protocol.md);
the plugin's licence does not extend to it. That document is a copy from
[Inkbraid/casement-plugin-protocol](https://github.com/Inkbraid/casement-plugin-protocol),
licensed CC BY 4.0. The icon is the project's own artwork
(`tools/make-icon.py`).

Every release of this plugin is published together with its source (the
release tag and its source archive).
