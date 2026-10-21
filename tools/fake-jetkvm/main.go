// fake-jetkvm stands in for a JetKVM device in the plugin's end-to-end tests.
//
// Login, WebRTC signaling and the session set-up follow JetKVM's own web.go,
// cloud.go, webrtc.go and hidrpc.go (github.com/jetkvm/kvm, GPL-2.0), using
// the same libraries (pion/webrtc, coder/websocket), so the plugin talks to
// the same stack a real device runs. The picture is an H.264 file sent in a
// loop, and HID input is printed as JSON lines on stdout instead of reaching
// a USB gadget.
//
//	fake-jetkvm -listen 127.0.0.1:8080 -video bars.h264 [-password secret] [-tls] [-signal=false]
//
// Every line on stdout is a JSON object with an "event" member; the first
// is {"event":"listening","url":…,"fingerprint":…}.
package main

import (
	"bytes"
	"context"
	"crypto/ecdsa"
	"crypto/elliptic"
	"crypto/rand"
	"crypto/sha256"
	"crypto/tls"
	"crypto/x509"
	"crypto/x509/pkix"
	"encoding/base64"
	"encoding/binary"
	"encoding/hex"
	"encoding/json"
	"errors"
	"flag"
	"fmt"
	"io"
	"log"
	"math/big"
	"net"
	"net/http"
	"os"
	"strings"
	"sync"
	"time"

	"github.com/coder/websocket"
	"github.com/coder/websocket/wsjson"
	"github.com/google/uuid"
	"github.com/pion/interceptor"
	"github.com/pion/webrtc/v4"
	"github.com/pion/webrtc/v4/pkg/media"
	"github.com/pion/webrtc/v4/pkg/media/h264reader"
)

var (
	listen   = flag.String("listen", "127.0.0.1:0", "address to listen on")
	password = flag.String("password", "", "device password; empty is JetKVM's noPassword mode")
	useTLS   = flag.Bool("tls", false, "serve HTTPS with a self-signed certificate")
	video    = flag.String("video", "", "Annex-B H.264 file to send in a loop")
	fps      = flag.Int("fps", 30, "frames per second of the video file")
	maxFails = flag.Int("max-failures", 5, "wrong passwords before logins are refused with 429")
	signal   = flag.Bool("signal", true, "whether the HDMI input has a signal; without one no video is sent")
)

var (
	out      = json.NewEncoder(os.Stdout)
	outMu    sync.Mutex
	stateMu  sync.Mutex
	token    string
	failures int
	current  *session
)

func emit(fields map[string]any) {
	outMu.Lock()
	defer outMu.Unlock()
	_ = out.Encode(fields)
}

func main() {
	flag.Parse()
	log.SetOutput(os.Stderr)
	if *video == "" {
		log.Fatal("-video is required")
	}
	frames, err := readAccessUnits(*video)
	if err != nil {
		log.Fatalf("reading %s: %v", *video, err)
	}

	mux := http.NewServeMux()
	mux.HandleFunc("POST /auth/login-local", handleLogin)
	mux.HandleFunc("GET /webrtc/signaling/client", func(w http.ResponseWriter, r *http.Request) {
		handleSignaling(w, r, frames)
	})

	ln, err := net.Listen("tcp", *listen)
	if err != nil {
		log.Fatal(err)
	}
	srv := &http.Server{Handler: mux}
	fingerprint := ""
	scheme := "http"
	if *useTLS {
		cert, fp, err := selfSigned()
		if err != nil {
			log.Fatal(err)
		}
		fingerprint = fp
		scheme = "https"
		ln = tls.NewListener(ln, &tls.Config{Certificates: []tls.Certificate{cert}})
	}
	emit(map[string]any{
		"event":       "listening",
		"url":         fmt.Sprintf("%s://%s", scheme, ln.Addr()),
		"fingerprint": fingerprint,
	})
	log.Fatal(srv.Serve(ln))
}

// handleLogin follows JetKVM's handleLogin: 400 in noPassword mode, 429
// after too many wrong passwords, 401 for a wrong one, otherwise a fresh
// authToken cookie (which also logs out any earlier login).
func handleLogin(w http.ResponseWriter, r *http.Request) {
	if *password == "" {
		writeJSON(w, http.StatusBadRequest, map[string]any{"error": "Login is disabled in noPassword mode"})
		return
	}
	stateMu.Lock()
	defer stateMu.Unlock()
	if failures >= *maxFails {
		w.Header().Set("Retry-After", "60")
		writeJSON(w, http.StatusTooManyRequests, map[string]any{
			"error":       "Too many failed attempts. Please try again later.",
			"retry_after": 60,
		})
		return
	}
	var req struct {
		Password string `json:"password"`
	}
	if err := json.NewDecoder(r.Body).Decode(&req); err != nil {
		writeJSON(w, http.StatusBadRequest, map[string]any{"error": "Invalid request body"})
		return
	}
	if req.Password != *password {
		failures++
		emit(map[string]any{"event": "login", "ok": false})
		writeJSON(w, http.StatusUnauthorized, map[string]any{"error": "Invalid password"})
		return
	}
	failures = 0
	token = uuid.New().String()
	http.SetCookie(w, &http.Cookie{Name: "authToken", Value: token, Path: "/", HttpOnly: true, MaxAge: 7 * 24 * 3600})
	emit(map[string]any{"event": "login", "ok": true})
	writeJSON(w, http.StatusOK, map[string]any{"message": "Login successful"})
}

func authorized(r *http.Request) bool {
	if *password == "" {
		return true
	}
	c, err := r.Cookie("authToken")
	stateMu.Lock()
	defer stateMu.Unlock()
	return err == nil && c.Value != "" && c.Value == token
}

func writeJSON(w http.ResponseWriter, status int, v any) {
	w.Header().Set("Content-Type", "application/json")
	w.WriteHeader(status)
	_ = json.NewEncoder(w).Encode(v)
}

// handleSignaling follows JetKVM's handleLocalWebRTCSignal and
// handleWebRTCSignalWsMessages.
func handleSignaling(w http.ResponseWriter, r *http.Request, frames [][]byte) {
	if !authorized(r) {
		writeJSON(w, http.StatusUnauthorized, map[string]any{"error": "Unauthorized"})
		return
	}
	ws, err := websocket.Accept(w, r, &websocket.AcceptOptions{InsecureSkipVerify: true})
	if err != nil {
		return
	}
	defer ws.Close(websocket.StatusNormalClosure, "")
	ctx := context.Background()
	_ = wsjson.Write(ctx, ws, map[string]any{"type": "device-metadata", "data": map[string]any{"deviceVersion": "0.0.0-fake"}})

	var s *session
	for {
		typ, msg, err := ws.Read(ctx)
		if err != nil {
			return
		}
		if typ == websocket.MessageText && bytes.Equal(msg, []byte("ping")) {
			_ = ws.Write(ctx, websocket.MessageText, []byte("pong"))
			continue
		}
		var m struct {
			Type string          `json:"type"`
			Data json.RawMessage `json:"data"`
		}
		if json.Unmarshal(msg, &m) != nil {
			continue
		}
		switch m.Type {
		case "offer":
			var req struct {
				Sd string `json:"sd"`
			}
			if json.Unmarshal(m.Data, &req) != nil {
				continue
			}
			s, err = newSession(ws, req.Sd, frames)
			if err != nil {
				_ = wsjson.Write(ctx, ws, map[string]any{"error": err.Error()})
				continue
			}
		case "new-ice-candidate":
			var c webrtc.ICECandidateInit
			if json.Unmarshal(m.Data, &c) != nil || c.Candidate == "" || s == nil {
				continue
			}
			if err := s.pc.AddICECandidate(c); err != nil {
				log.Printf("adding a remote candidate: %v", err)
			}
		}
	}
}

type session struct {
	pc  *webrtc.PeerConnection
	rpc *webrtc.DataChannel
	mu  sync.Mutex
}

// videoState is what JetKVM reports as its video input state.
func videoState() map[string]any {
	if !*signal {
		return map[string]any{"ready": false, "error": "no_signal", "width": 0, "height": 0, "fps": 0}
	}
	return map[string]any{"ready": true, "width": 640, "height": 360, "fps": *fps}
}

func newSession(ws *websocket.Conn, sd string, frames [][]byte) (*session, error) {
	// As JetKVM's newSession, plus loopback candidates for tests on one host.
	se := webrtc.SettingEngine{}
	se.SetIncludeLoopbackCandidate(true)
	me := &webrtc.MediaEngine{}
	if err := me.RegisterDefaultCodecs(); err != nil {
		return nil, err
	}
	ir := &interceptor.Registry{}
	if err := webrtc.RegisterDefaultInterceptors(me, ir); err != nil {
		return nil, err
	}
	api := webrtc.NewAPI(webrtc.WithSettingEngine(se), webrtc.WithMediaEngine(me), webrtc.WithInterceptorRegistry(ir))
	pc, err := api.NewPeerConnection(webrtc.Configuration{})
	if err != nil {
		return nil, err
	}
	s := &session{pc: pc}
	pc.OnICECandidate(func(c *webrtc.ICECandidate) {
		if c != nil {
			_ = wsjson.Write(context.Background(), ws, map[string]any{"type": "new-ice-candidate", "data": c.ToJSON()})
		}
	})
	connected := make(chan struct{})
	var once sync.Once
	pc.OnICEConnectionStateChange(func(st webrtc.ICEConnectionState) {
		emit(map[string]any{"event": "ice", "state": st.String()})
		if st == webrtc.ICEConnectionStateConnected {
			once.Do(func() { close(connected) })
		}
	})
	pc.OnDataChannel(func(d *webrtc.DataChannel) {
		emit(map[string]any{"event": "channel", "label": d.Label()})
		switch d.Label() {
		case "rpc":
			s.mu.Lock()
			s.rpc = d
			s.mu.Unlock()
			d.OnMessage(func(m webrtc.DataChannelMessage) { onRPC(d, m.Data) })
			// JetKVM reports its video state as the channel opens.
			d.OnOpen(func() { s.event("videoInputState", videoState()) })
		case "hidrpc", "hidrpc-unreliable-ordered", "hidrpc-unreliable-nonordered":
			d.OnMessage(func(m webrtc.DataChannelMessage) { onHID(d, m.Data) })
		}
	})

	b, err := base64.StdEncoding.DecodeString(sd)
	if err != nil {
		return nil, err
	}
	offer := webrtc.SessionDescription{}
	if err := json.Unmarshal(b, &offer); err != nil {
		return nil, err
	}
	if !strings.Contains(strings.ToUpper(offer.SDP), "H264") {
		return nil, errors.New("the offer has no H264")
	}
	track, err := webrtc.NewTrackLocalStaticSample(webrtc.RTPCodecCapability{MimeType: webrtc.MimeTypeH264}, "video", "kvm")
	if err != nil {
		return nil, err
	}
	sender, err := pc.AddTrack(track)
	if err != nil {
		return nil, err
	}
	go drainRTCP(sender)
	if err := pc.SetRemoteDescription(offer); err != nil {
		return nil, err
	}
	answer, err := pc.CreateAnswer(nil)
	if err != nil {
		return nil, err
	}
	if err := pc.SetLocalDescription(answer); err != nil {
		return nil, err
	}
	local, err := json.Marshal(pc.LocalDescription())
	if err != nil {
		return nil, err
	}

	stateMu.Lock()
	previous := current
	current = s
	stateMu.Unlock()
	if previous != nil {
		previous.event("otherSessionConnected", nil)
		go func() {
			time.Sleep(time.Second)
			_ = previous.pc.Close()
		}()
	}
	emit(map[string]any{"event": "session"})
	_ = wsjson.Write(context.Background(), ws, map[string]any{"type": "answer", "data": base64.StdEncoding.EncodeToString(local)})

	go func() {
		<-connected
		if *signal {
			sendVideo(pc, track, frames)
		}
	}()
	return s, nil
}

// event sends a JSON-RPC notification, as JetKVM's writeJSONRPCEvent.
func (s *session) event(method string, params any) {
	s.mu.Lock()
	d := s.rpc
	s.mu.Unlock()
	if d == nil {
		return
	}
	b, _ := json.Marshal(map[string]any{"jsonrpc": "2.0", "method": method, "params": params})
	_ = d.SendText(string(b))
}

func drainRTCP(s *webrtc.RTPSender) {
	buf := make([]byte, 1500)
	for {
		if _, _, err := s.Read(buf); err != nil {
			return
		}
	}
}

func sendVideo(pc *webrtc.PeerConnection, track *webrtc.TrackLocalStaticSample, frames [][]byte) {
	tick := time.NewTicker(time.Second / time.Duration(*fps))
	defer tick.Stop()
	for i := 0; ; i++ {
		<-tick.C
		if pc.ConnectionState() == webrtc.PeerConnectionStateClosed {
			return
		}
		if err := track.WriteSample(media.Sample{Data: frames[i%len(frames)], Duration: time.Second / time.Duration(*fps)}); err != nil {
			return
		}
	}
}

// readAccessUnits splits an Annex-B file into access units (pictures),
// joined with start codes: a new one starts at a parameter set, SEI or
// delimiter after a slice, or at a picture's first slice (first_mb_in_slice
// 0, so the slice header's first bit is set).
func readAccessUnits(path string) ([][]byte, error) {
	f, err := os.Open(path)
	if err != nil {
		return nil, err
	}
	defer f.Close()
	r, err := h264reader.NewReader(f)
	if err != nil {
		return nil, err
	}
	var frames [][]byte
	var cur []byte
	hasSlice := false
	for {
		nal, err := r.NextNAL()
		if errors.Is(err, io.EOF) {
			break
		}
		if err != nil {
			return nil, err
		}
		t := nal.UnitType
		slice := t == h264reader.NalUnitTypeCodedSliceNonIdr || t == h264reader.NalUnitTypeCodedSliceIdr
		firstSlice := slice && len(nal.Data) > 1 && nal.Data[1]&0x80 != 0
		if hasSlice && (firstSlice || (t >= h264reader.NalUnitTypeSEI && t <= h264reader.NalUnitTypeAUD)) {
			frames = append(frames, cur)
			cur, hasSlice = nil, false
		}
		cur = append(cur, 0, 0, 0, 1)
		cur = append(cur, nal.Data...)
		hasSlice = hasSlice || slice
	}
	if hasSlice {
		frames = append(frames, cur)
	}
	if len(frames) == 0 {
		return nil, errors.New("no frames")
	}
	return frames, nil
}

// onRPC answers the JSON-RPC calls the plugin makes and reports the rest.
func onRPC(d *webrtc.DataChannel, data []byte) {
	var req struct {
		ID     any             `json:"id"`
		Method string          `json:"method"`
		Params json.RawMessage `json:"params"`
	}
	if json.Unmarshal(data, &req) != nil {
		return
	}
	var params any
	_ = json.Unmarshal(req.Params, &params)
	emit(map[string]any{"event": "rpc", "method": req.Method, "params": params})
	var result any
	switch req.Method {
	case "getVideoState":
		result = videoState()
	}
	if req.ID != nil {
		b, _ := json.Marshal(map[string]any{"jsonrpc": "2.0", "id": req.ID, "result": result})
		_ = d.SendText(string(b))
	}
}

// onHID decodes HID-RPC messages (internal/hidrpc in JetKVM) and answers the
// handshake.
func onHID(d *webrtc.DataChannel, data []byte) {
	if len(data) < 1 {
		return
	}
	switch data[0] {
	case 0x01: // handshake
		emit(map[string]any{"event": "hid", "type": "handshake", "version": int(data[1])})
		_ = d.Send([]byte{0x01, 0x01})
	case 0x05: // keypress report
		if len(data) >= 3 {
			emit(map[string]any{"event": "hid", "type": "keypress", "key": int(data[1]), "press": data[2] == 1})
		}
	case 0x02: // keyboard report
		emit(map[string]any{"event": "hid", "type": "keyboard", "modifier": int(data[1]), "keys": data[2:]})
	case 0x03: // pointer report
		if len(data) == 10 {
			emit(map[string]any{
				"event": "hid", "type": "pointer",
				"x":       int(int32(binary.BigEndian.Uint32(data[1:5]))),
				"y":       int(int32(binary.BigEndian.Uint32(data[5:9]))),
				"buttons": int(data[9]),
			})
		}
	case 0x09: // keypress keep-alive
		emit(map[string]any{"event": "hid", "type": "keepalive"})
	default:
		emit(map[string]any{"event": "hid", "type": "unknown", "data": hex.EncodeToString(data)})
	}
}

// selfSigned makes a certificate like a JetKVM's self-signed one and returns
// it with the SHA-256 of its DER encoding.
func selfSigned() (tls.Certificate, string, error) {
	key, err := ecdsa.GenerateKey(elliptic.P256(), rand.Reader)
	if err != nil {
		return tls.Certificate{}, "", err
	}
	tmpl := &x509.Certificate{
		SerialNumber: big.NewInt(time.Now().UnixNano()),
		Subject:      pkix.Name{CommonName: "jetkvm.local"},
		NotBefore:    time.Now().Add(-time.Hour),
		NotAfter:     time.Now().Add(24 * time.Hour),
		DNSNames:     []string{"jetkvm.local"},
		KeyUsage:     x509.KeyUsageDigitalSignature,
		ExtKeyUsage:  []x509.ExtKeyUsage{x509.ExtKeyUsageServerAuth},
	}
	der, err := x509.CreateCertificate(rand.Reader, tmpl, tmpl, &key.PublicKey, key)
	if err != nil {
		return tls.Certificate{}, "", err
	}
	sum := sha256.Sum256(der)
	return tls.Certificate{Certificate: [][]byte{der}, PrivateKey: key}, hex.EncodeToString(sum[:]), nil
}
