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
| JetKVM devices: local password login, WebRTC H.264 video, keyboard and mouse | in progress |
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
licences allowed and CI checks them (`cargo deny check licenses`). Crates
offered under a choice of licences (for example "MIT or Apache-2.0") are
used under the compatible one.

Casement itself is a separate program that talks to this plugin only through
the documented protocol in [docs/plugin-protocol.md](docs/plugin-protocol.md);
the plugin's licence does not extend to it. That document is a copy from
[Inkbraid/casement-plugin-protocol](https://github.com/Inkbraid/casement-plugin-protocol),
licensed CC BY 4.0. The icon is the project's own artwork
(`tools/make-icon.py`).

Every release of this plugin is published together with its source (the
release tag and its source archive).
