package main

import (
	"bufio"
	"bytes"
	"encoding/hex"
	"errors"
	"io"
	"net"
	"strings"
	"testing"
	"time"
)

// Computed with libvncclient's rfbClientEncryptBytes for the challenge
// 00 01 .. 0f, so the DES key's bit order is checked against another
// implementation.
func TestVNCResponseMatchesLibvncclient(t *testing.T) {
	challenge := make([]byte, 16)
	for i := range challenge {
		challenge[i] = byte(i)
	}
	for password, want := range map[string]string{
		"Secr3t!x":       "16860a9a74bc7b897c6a7f0336cb51c2",
		"ab":             "fe01155de95da3e28adf6cc730f06f08",
		"Secr3t!xLONGER": "16860a9a74bc7b897c6a7f0336cb51c2", // only 8 count
	} {
		if got := hex.EncodeToString(vncResponse(password, challenge)); got != want {
			t.Errorf("%q: got %s, want %s", password, got, want)
		}
	}
}

// fakeVNC is a VNC server on loopback that speaks as macOS's Screen
// Sharing does: RFB 003.889, its own security types and VNC
// authentication.
type fakeVNC struct {
	t        *testing.T
	l        net.Listener
	version  string
	types    []byte
	password string
	w, h     int
	conns    chan *fakeConn
}

type fakeConn struct {
	net.Conn
	r *bufio.Reader
}

func newFakeVNC(t *testing.T, setup ...func(*fakeVNC)) *fakeVNC {
	l, err := net.Listen("tcp", "127.0.0.1:0")
	if err != nil {
		t.Fatal(err)
	}
	f := &fakeVNC{t: t, l: l, version: "RFB 003.889\n", types: []byte{30, 33, 36, 35, 2}, password: "pa55word", w: 4, h: 2, conns: make(chan *fakeConn, 4)}
	for _, s := range setup {
		s(f)
	}
	t.Cleanup(func() { l.Close() })
	go func() {
		for {
			c, err := l.Accept()
			if err != nil {
				return
			}
			go f.handshake(&fakeConn{Conn: c, r: bufio.NewReader(c)})
		}
	}()
	return f
}

func (f *fakeVNC) addr() string { return f.l.Addr().String() }

// handshake answers up to ServerInit and hands the connection on.
func (f *fakeVNC) handshake(c *fakeConn) {
	c.SetDeadline(time.Now().Add(5 * time.Second))
	io.WriteString(c, f.version)
	version := make([]byte, 12)
	if _, err := io.ReadFull(c.r, version); err != nil {
		c.Close()
		return
	}
	if string(version) != "RFB 003.008\n" {
		f.t.Errorf("client answered version %q", version)
	}
	c.Write(append([]byte{byte(len(f.types))}, f.types...))
	if len(f.types) == 0 {
		c.Write([]byte{0, 0, 0, 4})
		io.WriteString(c, "busy")
		c.Close()
		return
	}
	choice, err := c.r.ReadByte()
	if err != nil {
		c.Close() // the client left: it saw nothing it could use
		return
	}
	if choice != 2 {
		f.t.Errorf("client chose security type %d", choice)
		c.Close()
		return
	}
	challenge := []byte("0123456789abcdef")
	c.Write(challenge)
	answer := make([]byte, 16)
	io.ReadFull(c.r, answer)
	if !bytes.Equal(answer, vncResponse(f.password, challenge)) {
		c.Write([]byte{0, 0, 0, 1, 0, 0, 0, 22})
		io.WriteString(c, "Authentication failed.")
		c.Close()
		return
	}
	c.Write([]byte{0, 0, 0, 0})
	if shared, _ := c.r.ReadByte(); shared != 1 {
		f.t.Errorf("client asked for an exclusive session")
	}
	init := make([]byte, 24)
	rfbOrder.PutUint16(init, uint16(f.w))
	rfbOrder.PutUint16(init[2:], uint16(f.h))
	rfbOrder.PutUint32(init[20:], 7)
	c.Write(append(init, "a-mac-1"...))
	c.SetDeadline(time.Time{})
	f.conns <- c
}

// next is the next signed-in connection, after it set its pixel format and
// encodings.
func (f *fakeVNC) next() *fakeConn {
	select {
	case c := <-f.conns:
		head := make([]byte, 20)
		io.ReadFull(c.r, head)
		if head[0] != 0 || head[4] != 32 || head[7] != 1 || head[14] != 16 || head[15] != 8 || head[16] != 0 {
			f.t.Errorf("pixel format %v is not 32-bit BGRX", head)
		}
		io.ReadFull(c.r, head[:4])
		n := int(rfbOrder.Uint16(head[2:]))
		io.ReadFull(c.r, make([]byte, 4*n))
		return c
	case <-time.After(5 * time.Second):
		f.t.Fatal("no connection signed in")
	}
	return nil
}

// read reads one client message of n bytes.
func (c *fakeConn) read(t *testing.T, n int) []byte {
	t.Helper()
	b := make([]byte, n)
	c.SetReadDeadline(time.Now().Add(5 * time.Second))
	if _, err := io.ReadFull(c.r, b); err != nil {
		t.Fatalf("reading %d bytes from the client: %v", n, err)
	}
	return b
}

func rect(x, y, w, h int, enc int32) []byte {
	b := make([]byte, 12)
	rfbOrder.PutUint16(b, uint16(x))
	rfbOrder.PutUint16(b[2:], uint16(y))
	rfbOrder.PutUint16(b[4:], uint16(w))
	rfbOrder.PutUint16(b[6:], uint16(h))
	rfbOrder.PutUint32(b[8:], uint32(enc))
	return b
}

func update(rects ...[]byte) []byte {
	b := []byte{0, 0, 0, byte(len(rects))}
	for _, r := range rects {
		b = append(b, r...)
	}
	return b
}

func TestRFBScreenMouseAndKeys(t *testing.T) {
	f := newFakeVNC(t)
	r, err := DialRFB(f.addr(), "pa55word")
	if err != nil {
		t.Fatal(err)
	}
	defer r.Close()
	c := f.next()
	if w, h := r.Size(); w != 4 || h != 2 {
		t.Fatalf("size %dx%d", w, h)
	}

	// The first capture asks for the whole screen.
	got := make(chan []byte, 1)
	go func() { p, _ := r.Capture(); got <- append([]byte(nil), p...) }()
	if req := c.read(t, 10); !bytes.Equal(req, []byte{3, 0, 0, 0, 0, 0, 0, 4, 0, 2}) {
		t.Fatalf("first update request %v", req)
	}
	pixels := make([]byte, 4*2*4)
	for i := range pixels {
		pixels[i] = byte(i)
	}
	c.Write(append(update(rect(0, 0, 4, 2, encRaw)), pixels...))
	if p := <-got; !bytes.Equal(p, pixels) {
		t.Fatalf("capture %v, want %v", p, pixels)
	}

	// Later ones ask for changes: a copied rectangle and a raw pixel.
	go func() { p, _ := r.Capture(); got <- append([]byte(nil), p...) }()
	if req := c.read(t, 10); req[1] != 1 {
		t.Fatalf("second update request is not incremental: %v", req)
	}
	copyFrom := rect(1, 0, 2, 2, encCopyRect)
	copyFrom = append(copyFrom, 0, 0, 0, 0) // from (0,0)
	c.Write(append(update(copyFrom, rect(3, 1, 1, 1, encRaw)), 9, 9, 9, 9))
	want := append([]byte(nil), pixels...)
	copy(want[4:12], pixels[0:8])    // row 0: pixels 0,1 to 1,2
	copy(want[20:28], pixels[16:24]) // row 1
	copy(want[28:32], []byte{9, 9, 9, 9})
	if p := <-got; !bytes.Equal(p, want) {
		t.Fatalf("after CopyRect and Raw: %v, want %v", p, want)
	}

	// Mouse: the pointer stays on the screen, buttons are a mask.
	r.Move(10, -3)
	r.Button(1, true)
	r.Button(4, true)
	r.Button(4, false)
	r.Button(1, false)
	r.Button(9, true) // no such button: nothing sent
	for _, want := range [][]byte{{5, 0, 0, 3, 0, 0}, {5, 1, 0, 3, 0, 0}, {5, 9, 0, 3, 0, 0}, {5, 1, 0, 3, 0, 0}, {5, 0, 0, 3, 0, 0}} {
		if ev := c.read(t, 6); !bytes.Equal(ev, want) {
			t.Fatalf("pointer event %v, want %v", ev, want)
		}
	}
	// Keys go as X keysyms.
	r.Key(0xffeb, true, false)
	r.Key('v', false, false)
	if ev := c.read(t, 16); !bytes.Equal(ev, []byte{4, 1, 0, 0, 0, 0, 0xff, 0xeb, 4, 0, 0, 0, 0, 0, 0, 'v'}) {
		t.Fatalf("key events %v", ev)
	}

	// A new screen size is taken.
	go func() { p, _ := r.Capture(); got <- p }()
	c.read(t, 10)
	c.Write(update(rect(0, 0, 8, 3, encDesktopSize)))
	if p := <-got; len(p) != 8*3*4 {
		t.Fatalf("after DesktopSize the screen has %d bytes", len(p))
	}
	if w, h := r.Size(); w != 8 || h != 3 {
		t.Fatalf("size %dx%d after DesktopSize", w, h)
	}
}

func TestRFBConnectsAgainAfterTheServerHangsUp(t *testing.T) {
	f := newFakeVNC(t)
	r, err := DialRFB(f.addr(), "pa55word")
	if err != nil {
		t.Fatal(err)
	}
	defer r.Close()
	f.next().Close()
	r.mu.Lock()
	first := r.c
	r.mu.Unlock()
	select {
	case <-first.done:
	case <-time.After(5 * time.Second):
		t.Fatal("a closed connection went unnoticed")
	}
	go r.Move(1, 1)
	c := f.next()
	if ev := c.read(t, 6); !bytes.Equal(ev, []byte{5, 0, 0, 1, 0, 1}) {
		t.Fatalf("after reconnecting the pointer event is %v", ev)
	}
}

func TestRFBRefuses(t *testing.T) {
	cases := []struct {
		name  string
		setup func(*fakeVNC)
		pass  string
		want  error
		text  string
	}{
		{"wrong password", func(*fakeVNC) {}, "guess", errRFBRefused, "Authentication failed."},
		{"no VNC password set", func(f *fakeVNC) { f.types = []byte{30, 33, 36, 35} }, "pa55word", errRFBNoPassword, "[30 33 36 35]"},
		{"turned away", func(f *fakeVNC) { f.types = nil }, "pa55word", nil, "busy"},
		{"old protocol", func(f *fakeVNC) { f.version = "RFB 003.003\n" }, "pa55word", nil, "needs 3.8"},
		{"not VNC", func(f *fakeVNC) { f.version = "SSH-2.0-Open" }, "pa55word", nil, "does not speak RFB"},
	}
	for _, c := range cases {
		f := newFakeVNC(t, c.setup)
		_, err := DialRFB(f.addr(), c.pass)
		if err == nil || c.want != nil && !errors.Is(err, c.want) || !strings.Contains(err.Error(), c.text) {
			t.Errorf("%s: got %v", c.name, err)
		}
	}
	if _, err := DialRFB("192.0.2.1:5900", "x"); err == nil || !strings.Contains(err.Error(), "loopback") {
		t.Errorf("a VNC server off this machine was not refused: %v", err)
	}
}

func TestRFBDropsAServerThatSendsNonsense(t *testing.T) {
	for name, msg := range map[string][]byte{
		"rectangle off the screen": update(rect(3, 0, 2, 1, encRaw)),
		"copy from off the screen": append(update(rect(0, 0, 1, 1, encCopyRect)), 0, 9, 0, 0),
		"encoding not asked for":   update(rect(0, 0, 1, 1, 16)),
		"unknown message":          {200},
		"huge screen":              update(rect(0, 0, 60000, 60000, encDesktopSize)),
	} {
		f := newFakeVNC(t)
		r, err := DialRFB(f.addr(), "pa55word")
		if err != nil {
			t.Fatal(err)
		}
		c := f.next()
		done := make(chan error, 1)
		go func() { _, err := r.Capture(); done <- err }()
		c.read(t, 10)
		c.Write(msg)
		if err := <-done; err == nil {
			t.Errorf("%s: taken", name)
		}
		r.Close()
	}
}
