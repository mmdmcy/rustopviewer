package main

import (
	"encoding/binary"
	"net/http"
	"net/http/httptest"
	"strings"
	"sync"
	"testing"
)

type fakeScreen struct {
	mu     sync.Mutex
	events []string
}

func (f *fakeScreen) Size() (int, int)         { return 4, 2 }
func (f *fakeScreen) Capture() ([]byte, error) { return make([]byte, 4*2*4), nil }
func (f *fakeScreen) record(s string) error {
	f.mu.Lock()
	defer f.mu.Unlock()
	f.events = append(f.events, s)
	return nil
}
func (f *fakeScreen) Move(x, y int) error { return f.record("move") }
func (f *fakeScreen) Button(b int, down bool) error {
	if down {
		return f.record("down")
	}
	return f.record("up")
}
func (f *fakeScreen) Key(sym uint32, down, shift bool) error { return f.record("key") }

func request(method, target, host string, headers map[string]string, body string) *http.Request {
	r := httptest.NewRequest(method, target, strings.NewReader(body))
	r.Host = host
	for k, v := range headers {
		r.Header.Set(k, v)
	}
	return r
}

func TestOnlyTheKeyHolderOnLoopbackGetsIn(t *testing.T) {
	screen := &fakeScreen{}
	s := NewServer("secret", screen, nil)
	cookie := map[string]string{"Cookie": cookieName + "=secret"}
	json := map[string]string{"Cookie": cookieName + "=secret", "Content-Type": "application/json"}

	cases := []struct {
		name string
		r    *http.Request
		want int
	}{
		{"key exchange", request("GET", "/?k=secret", "127.0.0.1:9", nil, ""), http.StatusSeeOther},
		{"wrong key", request("GET", "/?k=guess", "127.0.0.1:9", nil, ""), http.StatusForbidden},
		{"no cookie", request("GET", "/info", "127.0.0.1:9", nil, ""), http.StatusUnauthorized},
		{"with cookie", request("GET", "/info", "localhost:9", cookie, ""), http.StatusOK},
		{"rebinding host", request("GET", "/info", "evil.example:9", cookie, ""), http.StatusForbidden},
		{"input", request("POST", "/input", "127.0.0.1:9", json, `[{"t":"move","x":1,"y":1},{"t":"down","b":1}]`), http.StatusNoContent},
		{"input not json", request("POST", "/input", "127.0.0.1:9", map[string]string{"Cookie": cookieName + "=secret", "Content-Type": "text/plain"}, `[]`), http.StatusUnsupportedMediaType},
		{"cross-site", request("POST", "/input", "127.0.0.1:9", map[string]string{"Cookie": cookieName + "=secret", "Content-Type": "application/json", "Origin": "https://evil.example"}, `[]`), http.StatusForbidden},
	}
	for _, c := range cases {
		w := httptest.NewRecorder()
		s.ServeHTTP(w, c.r)
		if w.Code != c.want {
			t.Errorf("%s: got %d, want %d", c.name, w.Code, c.want)
		}
	}
	if got := strings.Join(screen.events, ","); got != "move,down" {
		t.Errorf("input reached the screen as %q", got)
	}
	w := httptest.NewRecorder()
	s.ServeHTTP(w, request("GET", "/?k=secret", "127.0.0.1:9", nil, ""))
	set := w.Header().Get("Set-Cookie")
	if !strings.Contains(set, "HttpOnly") || !strings.Contains(set, "SameSite=Strict") {
		t.Errorf("session cookie is not locked down: %s", set)
	}
}

func TestListenRefusesTheNetwork(t *testing.T) {
	for _, address := range []string{"0.0.0.0:47800", "192.168.1.5:47800", ":47800"} {
		if _, _, err := listen("", address); err == nil {
			t.Errorf("%s should be refused", address)
		}
	}
}

func xauthRecord(number, name string, data []byte) []byte {
	be := binary.BigEndian
	field := func(b []byte) []byte {
		out := make([]byte, 2, 2+len(b))
		be.PutUint16(out, uint16(len(b)))
		return append(out, b...)
	}
	rec := []byte{0x01, 0x00}
	rec = append(rec, field([]byte("host"))...)
	rec = append(rec, field([]byte(number))...)
	rec = append(rec, field([]byte(name))...)
	return append(rec, field(data)...)
}

func TestXauthorityPicksTheDisplaysCookie(t *testing.T) {
	file := append(xauthRecord("1", "MIT-MAGIC-COOKIE-1", []byte("one")), xauthRecord("0", "MIT-MAGIC-COOKIE-1", []byte("zero"))...)
	if got, err := xauthCookie(file, 0); err != nil || string(got) != "zero" {
		t.Fatalf("display 0: %q %v", got, err)
	}
	if got, err := xauthCookie(file, 7); err != nil || string(got) != "one" {
		t.Fatalf("fallback: %q %v", got, err)
	}
	if _, err := xauthCookie(file[:5], 0); err == nil {
		t.Fatal("a cut-short file must fail")
	}
}

func TestDisplaysAndSetupRequest(t *testing.T) {
	if n, err := parseDisplay(":1.0"); err != nil || n != 1 {
		t.Fatalf(":1.0 -> %d %v", n, err)
	}
	if _, err := parseDisplay("remote:0"); err == nil {
		t.Fatal("remote displays are refused")
	}
	b := setupRequest([]byte("0123456789abcdef"))
	if b[0] != 'l' || le.Uint16(b[2:]) != 11 || len(b)%4 != 0 {
		t.Fatalf("setup request: %v", b[:12])
	}
}

func TestSetupReplyGivesTheScreen(t *testing.T) {
	d := make([]byte, 32)
	le.PutUint16(d[16:], 4) // vendor length
	d[20], d[21] = 1, 1     // one screen, one format
	d = append(d, []byte("Test")...)
	d = append(d, make([]byte, 8)...) // the format
	screen := make([]byte, 40)
	le.PutUint32(screen[0:], 0x2a)
	le.PutUint16(screen[20:], 1920)
	le.PutUint16(screen[22:], 1080)
	screen[38] = 24
	d = append(d, screen...)
	var x X11
	if err := x.parseSetup(d); err != nil {
		t.Fatal(err)
	}
	if x.Root != 0x2a || x.Width != 1920 || x.Height != 1080 || x.Depth != 24 {
		t.Fatalf("parsed root=%#x %dx%d depth %d", x.Root, x.Width, x.Height, x.Depth)
	}
}

func TestKeyMapAndKeysyms(t *testing.T) {
	// Two keycodes (8, 9), two columns: 8 = a/A, 9 = 1/!.
	data := make([]byte, 16)
	le.PutUint32(data[0:], 'a')
	le.PutUint32(data[4:], 'A')
	le.PutUint32(data[8:], '1')
	le.PutUint32(data[12:], '!')
	keys := keyMap(data, 2, 8, 2)
	if keys['a'] != (keyRef{8, false}) || keys['A'] != (keyRef{8, true}) || keys['!'] != (keyRef{9, true}) {
		t.Fatalf("map: %v", keys)
	}
	for key, want := range map[string]uint32{"a": 'a', "Enter": 0xff0d, "é": 0xe9, "€": 0x010020ac, " ": 0x20} {
		if got, ok := keysym(key); !ok || got != want {
			t.Errorf("%q -> %#x %v", key, got, ok)
		}
	}
	if _, ok := keysym("Unidentified"); ok {
		t.Error("unknown names are ignored")
	}
}
