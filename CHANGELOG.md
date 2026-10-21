# Changelog

## 0.1.0 (unreleased)

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
