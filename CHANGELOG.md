# Changelog

## 0.1.0 (unreleased)

- JetKVM devices: password login (`auth_failed` for a wrong or missing
  password, the device's login lock-out reported), HTTPS with certificate
  pinning by fingerprint, WebRTC (str0m) with H.264 video decoded by Media
  Foundation, keyboard and absolute pointer over HID-RPC (JSON-RPC on older
  firmware), wheel over JSON-RPC, key keep-alives, keys released on
  disconnect, `closed` when another session takes the device over or it
  restarts or stops answering for 8 seconds, a status picture while the
  device has no HDMI signal.
- Media Foundation is loaded at run time: without it (Windows N without the
  Media Feature Pack) the plugin starts and says what is missing.
- `tools/fake-jetkvm`, a stand-in device on JetKVM's own WebRTC stack (with
  optional packet loss), and end-to-end tests against it on Linux and
  Windows in CI.
- Casement plugin protocol v1: length-prefixed JSON framing, `hello`,
  `connect`, `state`, `disconnect`, shared-memory surfaces with
  `frame`/`frame_ack` flow control, key, pointer and wheel input.
  Capabilities: `password` (no `cursor`: a JetKVM picture shows the remote
  pointer itself).
- `jetkvm://test`: a moving test pattern that logs input, for testing
  Casement's side end to end.
- Address parsing for `jetkvm://<host>[:<port>][?tls[&fingerprint=…]]`.
- stdout carries protocol messages only; anything else printed goes to
  stderr. No console window on Windows.
- `plugin.json` with the connection fields (device, port, HTTPS,
  certificate fingerprint) for Casement's connection dialog, an icon, the
  protocol document, `casement-sim` example, integration tests
  over stdio, GitHub Actions (Windows build and tests, Linux tests,
  dependency licence check, releases from tags).
