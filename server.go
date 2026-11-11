package main

// The viewer's web side. Every request must name a loopback host (no DNS
// rebinding), every page but the key exchange needs the session cookie,
// and input must be a same-origin JSON POST, so a web page elsewhere on
// the machine cannot watch or drive the screen.

import (
	"bytes"
	"crypto/subtle"
	_ "embed"
	"encoding/json"
	"errors"
	"hash/crc32"
	"image"
	"image/jpeg"
	"io"
	"net"
	"net/http"
	"net/url"
	"strings"
	"sync"
	"time"
)

//go:embed viewer.html
var viewerHTML []byte

//go:embed viewer.js
var viewerJS []byte

//go:embed viewer.css
var viewerCSS []byte

// Screen is what the server shows and drives: an X11 display, or another
// platform later.
type Screen interface {
	Size() (int, int)
	Capture() ([]byte, error)
	Move(x, y int) error
	Button(button int, down bool) error
	Key(sym uint32, down, shiftHeld bool) error
}

// Clipboard is the host's clipboard, text only.
type Clipboard interface {
	SetText(text string) error
	Text() (string, error)
}

func (x *X11) Size() (int, int) { return x.Width, x.Height }

const (
	cookieName = "rov"
	// minFrameGap caps the frame rate; capture and JPEG encoding of a
	// large screen take most of it anyway.
	minFrameGap = 125 * time.Millisecond
	jpegQuality = 70
	maxEvents   = 400
)

type Server struct {
	key    string
	screen Screen
	clip   Clipboard // nil when the host has none

	mu      sync.Mutex
	cond    *sync.Cond
	frame   []byte
	version uint64
	viewers int
	lastErr string
}

func NewServer(key string, screen Screen, clip Clipboard) *Server {
	s := &Server{key: key, screen: screen, clip: clip}
	s.cond = sync.NewCond(&s.mu)
	go s.captureLoop()
	return s
}

// captureLoop grabs the screen while someone watches, and publishes a new
// JPEG only when the picture changed.
func (s *Server) captureLoop() {
	var lastSum uint32
	for {
		s.mu.Lock()
		for s.viewers == 0 {
			s.cond.Wait()
		}
		s.mu.Unlock()
		started := time.Now()
		pixels, err := s.screen.Capture()
		if err == nil {
			if sum := crc32.ChecksumIEEE(pixels); sum != lastSum || s.current() == nil {
				lastSum = sum
				w, h := s.screen.Size()
				var encoded []byte
				if encoded, err = encodeJPEG(pixels, w, h); err == nil {
					s.publish(encoded, "")
				}
			}
		}
		if err != nil {
			s.publish(nil, err.Error())
			time.Sleep(time.Second)
		}
		if gap := minFrameGap - time.Since(started); gap > 0 {
			time.Sleep(gap)
		}
	}
}

func (s *Server) current() []byte {
	s.mu.Lock()
	defer s.mu.Unlock()
	return s.frame
}

func (s *Server) publish(frame []byte, errText string) {
	s.mu.Lock()
	if frame != nil {
		s.frame = frame
		s.version++
	}
	s.lastErr = errText
	s.mu.Unlock()
	s.cond.Broadcast()
}

// encodeJPEG turns BGRX pixels into a JPEG.
func encodeJPEG(bgrx []byte, w, h int) ([]byte, error) {
	img := image.NewRGBA(image.Rect(0, 0, w, h))
	pix := img.Pix
	for i := 0; i+3 < len(bgrx) && i+3 < len(pix); i += 4 {
		pix[i], pix[i+1], pix[i+2], pix[i+3] = bgrx[i+2], bgrx[i+1], bgrx[i], 0xff
	}
	var out bytes.Buffer
	err := jpeg.Encode(&out, img, &jpeg.Options{Quality: jpegQuality})
	return out.Bytes(), err
}

func isLoopbackHost(hostport string) bool {
	host := hostport
	if h, _, err := net.SplitHostPort(hostport); err == nil {
		host = h
	}
	host = strings.Trim(strings.ToLower(host), "[]")
	if host == "localhost" {
		return true
	}
	ip := net.ParseIP(host)
	return ip != nil && ip.IsLoopback()
}

func (s *Server) authed(r *http.Request) bool {
	c, err := r.Cookie(cookieName)
	return err == nil && subtle.ConstantTimeCompare([]byte(c.Value), []byte(s.key)) == 1
}

// sameOrigin refuses requests a browser marks as coming from another site.
func sameOrigin(r *http.Request) bool {
	if site := r.Header.Get("Sec-Fetch-Site"); site == "cross-site" || site == "same-site" {
		return false
	}
	if origin := r.Header.Get("Origin"); origin != "" {
		u, err := url.Parse(origin)
		if err != nil || !isLoopbackHost(u.Host) {
			return false
		}
	}
	return true
}

func (s *Server) ServeHTTP(w http.ResponseWriter, r *http.Request) {
	h := w.Header()
	h.Set("X-Frame-Options", "DENY")
	h.Set("Content-Security-Policy", "default-src 'self'; img-src 'self'; frame-ancestors 'none'; base-uri 'none'; form-action 'none'")
	h.Set("Referrer-Policy", "no-referrer")
	h.Set("Cache-Control", "no-store")
	if !isLoopbackHost(r.Host) || !sameOrigin(r) {
		http.Error(w, "refused", http.StatusForbidden)
		return
	}
	if r.URL.Path == "/" && r.URL.Query().Has("k") {
		if subtle.ConstantTimeCompare([]byte(r.URL.Query().Get("k")), []byte(s.key)) != 1 {
			http.Error(w, "wrong key", http.StatusForbidden)
			return
		}
		http.SetCookie(w, &http.Cookie{Name: cookieName, Value: s.key, Path: "/", HttpOnly: true, SameSite: http.SameSiteStrictMode})
		http.Redirect(w, r, "/", http.StatusSeeOther)
		return
	}
	if !s.authed(r) {
		http.Error(w, "open this through lmice desktop DEVICE", http.StatusUnauthorized)
		return
	}
	switch {
	case r.URL.Path == "/" && r.Method == http.MethodGet:
		h.Set("Content-Type", "text/html; charset=utf-8")
		w.Write(viewerHTML)
	case r.URL.Path == "/viewer.js" && r.Method == http.MethodGet:
		h.Set("Content-Type", "text/javascript; charset=utf-8")
		w.Write(viewerJS)
	case r.URL.Path == "/viewer.css" && r.Method == http.MethodGet:
		h.Set("Content-Type", "text/css; charset=utf-8")
		w.Write(viewerCSS)
	case r.URL.Path == "/info" && r.Method == http.MethodGet:
		width, height := s.screen.Size()
		s.mu.Lock()
		errText := s.lastErr
		s.mu.Unlock()
		h.Set("Content-Type", "application/json")
		json.NewEncoder(w).Encode(map[string]any{"width": width, "height": height, "error": errText, "clipboard": s.clip != nil})
	case r.URL.Path == "/stream" && r.Method == http.MethodGet:
		s.stream(w, r)
	case r.URL.Path == "/input" && r.Method == http.MethodPost:
		s.input(w, r)
	case r.URL.Path == "/clipboard/set" && r.Method == http.MethodPost:
		s.clipboardSet(w, r)
	case r.URL.Path == "/clipboard/get" && r.Method == http.MethodPost:
		s.clipboardGet(w, r)
	default:
		http.NotFound(w, r)
	}
}

// stream sends frames as multipart JPEG, which an <img> shows directly.
func (s *Server) stream(w http.ResponseWriter, r *http.Request) {
	flusher, ok := w.(http.Flusher)
	if !ok {
		http.Error(w, "streaming unsupported", http.StatusInternalServerError)
		return
	}
	w.Header().Set("Content-Type", "multipart/x-mixed-replace; boundary=rovframe")
	s.mu.Lock()
	s.viewers++
	s.mu.Unlock()
	s.cond.Broadcast()
	defer func() {
		s.mu.Lock()
		s.viewers--
		s.mu.Unlock()
	}()
	done := r.Context().Done()
	go func() { <-done; s.cond.Broadcast() }()
	var seen uint64
	for {
		s.mu.Lock()
		for s.version == seen && r.Context().Err() == nil {
			s.cond.Wait()
		}
		frame, version := s.frame, s.version
		s.mu.Unlock()
		if r.Context().Err() != nil {
			return
		}
		seen = version
		if frame == nil {
			continue
		}
		if _, err := io.WriteString(w, "--rovframe\r\nContent-Type: image/jpeg\r\n\r\n"); err != nil {
			return
		}
		if _, err := w.Write(frame); err != nil {
			return
		}
		io.WriteString(w, "\r\n")
		flusher.Flush()
	}
}

type inputEvent struct {
	T     string `json:"t"`
	X     int    `json:"x"`
	Y     int    `json:"y"`
	B     int    `json:"b"`
	DY    int    `json:"dy"`
	K     string `json:"k"`
	Down  bool   `json:"d"`
	Shift bool   `json:"s"`
}

// jsonOnly refuses anything but a JSON body: a page elsewhere cannot send
// one without the browser asking this server first, which it refuses.
func jsonOnly(w http.ResponseWriter, r *http.Request) bool {
	if strings.HasPrefix(r.Header.Get("Content-Type"), "application/json") {
		return true
	}
	http.Error(w, "JSON only", http.StatusUnsupportedMediaType)
	return false
}

func (s *Server) input(w http.ResponseWriter, r *http.Request) {
	if !jsonOnly(w, r) {
		return
	}
	var events []inputEvent
	if err := json.NewDecoder(io.LimitReader(r.Body, 64<<10)).Decode(&events); err != nil || len(events) > maxEvents {
		http.Error(w, "bad input", http.StatusBadRequest)
		return
	}
	for _, e := range events {
		if err := s.apply(e); err != nil {
			http.Error(w, err.Error(), http.StatusInternalServerError)
			return
		}
	}
	w.WriteHeader(http.StatusNoContent)
}

// clipboardSet makes the viewer's text the host's clipboard, so the next
// paste on the host pastes it; clipboardGet hands the host's clipboard
// text to the viewer. Both are same-origin JSON POSTs like /input, so the
// text is never in a URL, and nothing here logs it.
func (s *Server) clipboardSet(w http.ResponseWriter, r *http.Request) {
	if !s.clipboardReady(w, r) {
		return
	}
	var body struct {
		Text *string `json:"text"`
	}
	// JSON can spell one byte of text in six ("\u0001").
	err := json.NewDecoder(http.MaxBytesReader(w, r.Body, 6*maxClipboard+64)).Decode(&body)
	var tooLarge *http.MaxBytesError
	switch {
	case errors.As(err, &tooLarge) || err == nil && body.Text != nil && len(*body.Text) > maxClipboard:
		http.Error(w, errClipboardTooBig.Error(), http.StatusRequestEntityTooLarge)
		return
	case err != nil || body.Text == nil:
		http.Error(w, `send {"text": "..."}`, http.StatusBadRequest)
		return
	}
	if err := s.clip.SetText(*body.Text); err != nil {
		clipboardError(w, err)
		return
	}
	w.WriteHeader(http.StatusNoContent)
}

func (s *Server) clipboardGet(w http.ResponseWriter, r *http.Request) {
	if !s.clipboardReady(w, r) {
		return
	}
	text, err := s.clip.Text()
	if err != nil {
		clipboardError(w, err)
		return
	}
	w.Header().Set("Content-Type", "application/json")
	json.NewEncoder(w).Encode(map[string]string{"text": text})
}

func (s *Server) clipboardReady(w http.ResponseWriter, r *http.Request) bool {
	if s.clip == nil {
		http.Error(w, "this host has no clipboard RustOp Viewer can use", http.StatusNotImplemented)
		return false
	}
	return jsonOnly(w, r)
}

// clipboardError answers with the error's own words, which never hold the
// clipboard's text.
func clipboardError(w http.ResponseWriter, err error) {
	status := http.StatusBadGateway
	if errors.Is(err, errClipboardTooBig) {
		status = http.StatusRequestEntityTooLarge
	}
	http.Error(w, err.Error(), status)
}

func (s *Server) apply(e inputEvent) error {
	switch e.T {
	case "move":
		return s.screen.Move(e.X, e.Y)
	case "down", "up":
		return s.screen.Button(e.B, e.T == "down")
	case "wheel":
		button := 4
		if e.DY > 0 {
			button = 5
		}
		for i := 0; i < min(abs(e.DY), 10); i++ {
			if err := s.screen.Button(button, true); err != nil {
				return err
			}
			if err := s.screen.Button(button, false); err != nil {
				return err
			}
		}
	case "key":
		if sym, ok := keysym(e.K); ok {
			return s.screen.Key(sym, e.Down, e.Shift)
		}
	}
	return nil
}

func abs(v int) int {
	if v < 0 {
		return -v
	}
	return v
}
