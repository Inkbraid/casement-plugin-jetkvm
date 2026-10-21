# fake-jetkvm

A stand-in JetKVM for the plugin's end-to-end tests (`tests/e2e.rs`). Login,
signaling and the WebRTC session follow JetKVM's own `web.go`, `cloud.go`,
`webrtc.go` and `hidrpc.go` (github.com/jetkvm/kvm), on the same libraries
the device runs (pion/webrtc, coder/websocket). The picture is an H.264 file
sent in a loop; keyboard and mouse input is printed as JSON lines instead of
reaching a USB gadget.

```
go build -o ../../target/fake-jetkvm .
../../target/fake-jetkvm -listen 127.0.0.1:8080 -video testdata/bars-640x360.h264 [-password secret] [-tls] [-signal=false]
```

| Flag | Effect |
|---|---|
| `-password` | the device's password; without it the device is in JetKVM's "no password" mode |
| `-max-failures` | wrong passwords before logins are refused (HTTP 429) |
| `-tls` | HTTPS with a self-signed certificate; its SHA-256 is in the first line |
| `-signal=false` | no HDMI signal: no video, and the video state says `no_signal` |
| `-loss 0.05` | lose this share of the video packets (lost ones can still be resent on request) |
| `-hidrpc=false` | ignore HID-RPC, as firmware before it did, so input comes as JSON-RPC |
| `-fps` | frame rate of the file |

Every line on stdout is a JSON object with an `event` member: `listening`
(first, with `url` and `fingerprint`), `login`, `session`, `ice`, `channel`,
`rpc` (JSON-RPC calls) and `hid` (HID-RPC messages, decoded).

## Test pictures

SMPTE colour bars, H.264 Constrained Baseline with two slices a picture (as
x264 makes them with `-tune zerolatency`), Annex B:

```
ffmpeg -f lavfi -i smptebars=size=640x360:rate=30 -t 2 -c:v libx264 -profile:v baseline \
  -preset veryfast -tune zerolatency -pix_fmt yuv420p -x264-params keyint=15:repeat-headers=1 \
  -bsf:v h264_mp4toannexb -f h264 testdata/bars-640x360.h264
ffmpeg -f lavfi -i smptebars=size=160x120:rate=30 -frames:v 5 -c:v libx264 -profile:v baseline \
  -preset veryfast -tune zerolatency -pix_fmt yuv420p -x264-params keyint=5:repeat-headers=1 \
  -bsf:v h264_mp4toannexb -f h264 ../../tests/data/bars-160x120.h264
```

The first, with a key frame every half second like a JetKVM's stream, is
what the device sends; the second is for the decoder's unit test.
