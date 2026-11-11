package main

import (
	"bufio"
	"bytes"
	"crypto/rand"
	"errors"
	"fmt"
	"io"
	"log"
	"net"
	"net/http"
	"net/http/httptest"
	"os"
	"os/exec"
	"path/filepath"
	"strings"
	"sync"
	"syscall"
	"testing"
	"time"
)

// fakeX is just enough of an X server for the clipboard: atoms, the
// client's window, properties, the CLIPBOARD owner and the events between
// them. It also plays another program (window otherWin) that copies or
// pastes.
type fakeX struct {
	t          *testing.T
	conn       net.Conn
	maxRequest int

	mu        sync.Mutex
	seq       uint16
	atoms     map[string]uint32
	props     map[[2]uint32]fakeProp
	owner     uint32
	ours      uint32 // the client's window
	other     []byte // the other program's text when it owns the clipboard
	otherForm string // "UTF8_STRING", "STRING" or "" (holds no text)
	piece     int    // >0: the other program sends in INCR pieces this size
	silent    bool   // the other program never answers
	answerAll bool   // like xclip: it answers every target with what it holds
	incr      []byte // what an INCR transfer still has to send; nil when none runs
	incrDone  bool
	notified  chan []byte // SelectionNotify events the client sent to the other program
}

type fakeProp struct {
	typ    uint32
	format byte
	data   []byte
}

const otherWin = 0x999

func newFakeClipboard(t *testing.T) (*X11Clipboard, *fakeX) {
	client, server := net.Pipe()
	f := &fakeX{t: t, conn: server, maxRequest: 16384, atoms: map[string]uint32{"ATOM": 4, "STRING": 31},
		props: map[[2]uint32]fakeProp{}, notified: make(chan []byte, 4)}
	go f.serve()
	x := &X11{conn: client, r: bufio.NewReader(client), Root: 0x100, idBase: 0x400000, idMask: 0x1fffff, maxRequest: f.maxRequest}
	c, err := newX11Clipboard(x)
	if err != nil {
		t.Fatal(err)
	}
	c.timeout = 2 * time.Second
	t.Cleanup(func() { c.Close(); server.Close() })
	return c, f
}

func (f *fakeX) atom(name string) uint32 {
	if a, ok := f.atoms[name]; ok {
		return a
	}
	a := uint32(100 + len(f.atoms))
	f.atoms[name] = a
	return a
}

func (f *fakeX) name(atom uint32) string {
	for n, a := range f.atoms {
		if a == atom {
			return n
		}
	}
	return fmt.Sprint(atom)
}

func (f *fakeX) write(b []byte) {
	if _, err := f.conn.Write(b); err != nil {
		f.t.Logf("fake X: %v", err)
	}
}

func (f *fakeX) event(code byte, words ...uint32) {
	b := make([]byte, 4, 32)
	b[0] = code
	le.PutUint16(b[2:], f.seq)
	for _, w := range words {
		b = le.AppendUint32(b, w)
	}
	f.write(append(b, make([]byte, 32-len(b))...))
}

func (f *fakeX) reply(detail byte, extra []byte, words ...uint32) {
	extra = append(extra, make([]byte, pad4(len(extra)))...)
	b := make([]byte, 8, 32+len(extra))
	b[0], b[1] = 1, detail
	le.PutUint16(b[2:], f.seq)
	le.PutUint32(b[4:], uint32(len(extra)/4))
	for _, w := range words {
		b = le.AppendUint32(b, w)
	}
	b = append(b, make([]byte, 32-len(b))...)
	f.write(append(b, extra...))
}

func (f *fakeX) setProp(window, property uint32, p fakeProp) {
	f.props[[2]uint32{window, property}] = p
	if window == f.ours {
		f.event(evPropertyNotify, window, property, 0, 0)
	}
}

func (f *fakeX) deleteProp(window, property uint32) {
	key := [2]uint32{window, property}
	if _, ok := f.props[key]; !ok {
		return
	}
	delete(f.props, key)
	if window != f.ours {
		return
	}
	f.event(evPropertyNotify, window, property, 0, 1)
	if f.incr != nil { // the requestor took a piece: the next one, an empty one last
		n := min(len(f.incr), f.piece)
		f.setProp(window, property, fakeProp{f.atom("UTF8_STRING"), 8, f.incr[:n]})
		if f.incr = f.incr[n:]; n == 0 {
			f.incr, f.incrDone = nil, true
		}
	}
}

// serve answers the requests the clipboard sends.
func (f *fakeX) serve() {
	for {
		head := make([]byte, 4)
		if _, err := io.ReadFull(f.conn, head); err != nil {
			return
		}
		req := make([]byte, int(le.Uint16(head[2:]))*4)
		copy(req, head)
		if _, err := io.ReadFull(f.conn, req[4:]); err != nil {
			return
		}
		f.mu.Lock()
		f.seq++
		f.handle(req)
		f.mu.Unlock()
	}
}

func (f *fakeX) handle(req []byte) {
	u32 := func(at int) uint32 { return le.Uint32(req[at:]) }
	if len(req) > f.maxRequest {
		f.t.Errorf("request %d is %d bytes, over the server's %d", req[0], len(req), f.maxRequest)
	}
	switch req[0] {
	case 16: // InternAtom
		f.reply(0, nil, f.atom(string(req[8:8+le.Uint16(req[4:])])))
	case 1: // CreateWindow
		if u32(28) != 0x800 || u32(32) != propertyChangeMask {
			f.t.Errorf("the window does not hear about its properties")
		}
		f.ours = u32(4)
	case 3: // GetWindowAttributes
		f.reply(0, make([]byte, 12))
	case 18: // ChangeProperty
		window, property := u32(4), u32(8)
		p := fakeProp{u32(12), req[16], req[24 : 24+int(u32(20))*int(req[16]/8)]}
		if old, ok := f.props[[2]uint32{window, property}]; ok && req[1] == 2 {
			p.data = append(old.data, p.data...)
		}
		f.setProp(window, property, p)
	case 19: // DeleteProperty
		f.deleteProp(u32(4), u32(8))
	case 20: // GetProperty
		window, property := u32(4), u32(8)
		p, ok := f.props[[2]uint32{window, property}]
		if !ok {
			f.reply(0, nil, 0, 0, 0)
			return
		}
		end := min(len(p.data), int(u32(20))*4)
		after := len(p.data) - end
		f.reply(p.format, p.data[:end], p.typ, uint32(after), uint32(end/int(p.format/8)))
		if req[1] == 1 && after == 0 {
			f.deleteProp(window, property)
		}
	case 22: // SetSelectionOwner
		f.own(u32(4))
	case 23: // GetSelectionOwner
		f.reply(0, nil, f.owner)
	case 24: // ConvertSelection
		f.convert(u32(4), u32(12), u32(16))
	case 25: // SendEvent
		if req[12]&0x7f == evSelectionNotify && u32(4) == otherWin {
			f.notified <- req[12:44]
		}
	default:
		f.t.Errorf("unexpected request %d", req[0])
	}
}

func (f *fakeX) own(owner uint32) {
	if f.owner == f.ours && owner != f.ours {
		f.event(evSelectionClear, 0, f.ours, f.atom("CLIPBOARD"))
	}
	f.owner = owner
}

// convert plays the clipboard's owner when the client asks for its text.
func (f *fakeX) convert(requestor, target, property uint32) {
	if f.owner == otherWin && f.silent {
		return
	}
	answered := uint32(atomNone)
	if f.owner == otherWin && f.otherForm != "" && (target == f.atom(f.otherForm) || f.answerAll) {
		answered = property
		if f.piece > 0 {
			f.setProp(requestor, property, fakeProp{f.atom("INCR"), 32, le.AppendUint32(nil, uint32(len(f.other)))})
			f.incr, f.incrDone = f.other, false
		} else {
			f.setProp(requestor, property, fakeProp{f.atom(f.otherForm), 8, f.other})
		}
	}
	f.event(evSelectionNotify|0x80, 0, requestor, f.atom("CLIPBOARD"), target, answered)
}

// otherCopies makes the other program the clipboard's owner.
func (f *fakeX) otherCopies(text []byte, form string, piece int) {
	f.mu.Lock()
	defer f.mu.Unlock()
	f.other, f.otherForm, f.piece = text, form, piece
	f.own(otherWin)
}

// otherPastes asks the clipboard's owner (the client) for target, as a
// program pasting would, and returns the type and data it got.
func (f *fakeX) otherPastes(target string) (string, []byte) {
	f.mu.Lock()
	property := f.atom("PASTE_HERE")
	f.event(evSelectionRequest, 0, f.ours, otherWin, f.atom("CLIPBOARD"), f.atom(target), property)
	f.mu.Unlock()
	select {
	case notify := <-f.notified:
		f.mu.Lock()
		defer f.mu.Unlock()
		if le.Uint32(notify[20:]) == atomNone {
			return "refused", nil
		}
		p := f.props[[2]uint32{otherWin, property}]
		delete(f.props, [2]uint32{otherWin, property})
		return f.name(p.typ), p.data
	case <-time.After(2 * time.Second):
		f.t.Fatalf("no answer for %s", target)
		return "", nil
	}
}

func TestClipboardOffersTheViewersText(t *testing.T) {
	c, f := newFakeClipboard(t)
	if err := c.SetText("héllo €\r\nnext"); err != nil {
		t.Fatal(err)
	}
	if f.owner != c.window {
		t.Fatalf("CLIPBOARD owner %#x, want %#x", f.owner, c.window)
	}
	typ, data := f.otherPastes("TARGETS")
	var names []string
	for i := 0; i+4 <= len(data); i += 4 {
		names = append(names, f.name(le.Uint32(data[i:])))
	}
	if typ != "ATOM" || strings.Join(names, " ") != "TARGETS UTF8_STRING TEXT STRING" {
		t.Errorf("TARGETS: %s %v", typ, names)
	}
	for target, want := range map[string]string{
		"UTF8_STRING": "UTF8_STRING héllo €\nnext",
		"TEXT":        "UTF8_STRING héllo €\nnext",
		"STRING":      "STRING h\xe9llo ?\nnext",
		"image/png":   "refused ",
	} {
		if typ, data := f.otherPastes(target); typ+" "+string(data) != want {
			t.Errorf("%s: got %q, want %q", target, typ+" "+string(data), want)
		}
	}
	// A megabyte goes out in requests the server accepts (fakeX checks).
	big := strings.Repeat("0123456789abcdef", maxClipboard/16)
	if err := c.SetText(big); err != nil {
		t.Fatal(err)
	}
	if _, data := f.otherPastes("UTF8_STRING"); string(data) != big {
		t.Errorf("1 MB paste came out as %d bytes", len(data))
	}
	if err := c.SetText(big + "!"); !errors.Is(err, errClipboardTooBig) {
		t.Errorf("over 1 MB: %v", err)
	}
	// Another program copies: we stop offering ours.
	f.otherCopies([]byte("theirs"), "UTF8_STRING", 0)
	if text, err := c.Text(); err != nil || text != "theirs" {
		t.Errorf("after another program copied: %q %v", text, err)
	}
	if c.owned || c.text != nil {
		t.Error("the text is kept after losing the clipboard")
	}
}

func TestClipboardReadsTheHostsText(t *testing.T) {
	c, f := newFakeClipboard(t)
	if text, err := c.Text(); err != nil || text != "" {
		t.Fatalf("no owner: %q %v", text, err)
	}
	f.otherCopies([]byte("from the host ✓"), "UTF8_STRING", 0)
	if text, err := c.Text(); err != nil || text != "from the host ✓" {
		t.Errorf("plain: %q %v", text, err)
	}
	f.otherCopies([]byte("caf\xe9"), "STRING", 0) // an old program: Latin-1 only
	if text, err := c.Text(); err != nil || text != "café" {
		t.Errorf("STRING: %q %v", text, err)
	}
	f.otherCopies([]byte("\x89PNG"), "", 0) // holds an image, no text
	if text, err := c.Text(); err != nil || text != "" {
		t.Errorf("no text: %q %v", text, err)
	}
	f.answerAll = true // the image, whatever form is asked for
	f.otherCopies([]byte("\x89PNG"), "image/png", 0)
	if text, err := c.Text(); err != nil || text != "" {
		t.Errorf("an image handed out as text: %q %v", text, err)
	}
	f.answerAll = false
	big := bytes.Repeat([]byte("ab"), maxClipboard/2)
	f.otherCopies(big, "UTF8_STRING", 100000) // INCR
	if text, err := c.Text(); err != nil || text != string(big) {
		t.Errorf("INCR: %d bytes, %v", len(text), err)
	}
	f.otherCopies(append(big, '!'), "UTF8_STRING", 100000)
	if _, err := c.Text(); !errors.Is(err, errClipboardTooBig) {
		t.Errorf("INCR over 1 MB: %v", err)
	}
	f.mu.Lock()
	if !f.incrDone {
		t.Error("the refused INCR transfer was left hanging")
	}
	f.mu.Unlock()
	f.otherCopies(append(big, strings.Repeat("!", 100)...), "UTF8_STRING", 0)
	if _, err := c.Text(); !errors.Is(err, errClipboardTooBig) {
		t.Errorf("over 1 MB: %v", err)
	}
	for i := 0; ; i++ { // the DeleteProperty has no reply: give the server a moment
		f.mu.Lock()
		left := len(f.props)
		f.mu.Unlock()
		if left == 0 {
			break
		}
		if i == 100 {
			t.Fatalf("a refused transfer stays on the server: %d properties", left)
		}
		time.Sleep(10 * time.Millisecond)
	}
	f.mu.Lock()
	f.silent = true
	f.mu.Unlock()
	c.timeout = 100 * time.Millisecond
	if _, err := c.Text(); !errors.Is(err, errClipboardTimeout) {
		t.Errorf("silent owner: %v", err)
	}
	f.mu.Lock()
	f.silent = false
	f.mu.Unlock()
	f.otherCopies([]byte("after"), "UTF8_STRING", 0)
	if text, err := c.Text(); err != nil || text != "after" {
		t.Errorf("after a timeout: %q %v", text, err)
	}
}

type memClip struct{ text string }

func (m *memClip) SetText(text string) error {
	if len(text) > maxClipboard {
		return errClipboardTooBig
	}
	m.text = text
	return nil
}
func (m *memClip) Text() (string, error) { return m.text, nil }

func TestClipboardOverHTTPStaysSameOriginAndUnlogged(t *testing.T) {
	var logged bytes.Buffer
	log.SetOutput(&logged)
	t.Cleanup(func() { log.SetOutput(os.Stderr) })
	const secret = "correct horse battery staple"
	clip := &memClip{}
	s := NewServer("secret", &fakeScreen{}, clip)
	headers := func(extra ...string) map[string]string {
		h := map[string]string{"Cookie": cookieName + "=secret", "Content-Type": "application/json"}
		for i := 0; i+1 < len(extra); i += 2 {
			h[extra[i]] = extra[i+1]
		}
		return h
	}
	set := `{"text":"` + secret + `"}`
	cases := []struct {
		name   string
		r      *http.Request
		want   int
		answer string
	}{
		{"set", request("POST", "/clipboard/set", "127.0.0.1:9", headers(), set), http.StatusNoContent, ""},
		{"get", request("POST", "/clipboard/get", "127.0.0.1:9", headers(), `{}`), http.StatusOK, `{"text":"` + secret + `"}`},
		{"get by GET", request("GET", "/clipboard/get", "127.0.0.1:9", headers(), ""), http.StatusNotFound, ""},
		{"text in the URL", request("POST", "/clipboard/set?text=x", "127.0.0.1:9", headers(), `{}`), http.StatusBadRequest, ""},
		{"not json", request("POST", "/clipboard/set", "127.0.0.1:9", headers("Content-Type", "text/plain"), set), http.StatusUnsupportedMediaType, ""},
		{"no cookie", request("POST", "/clipboard/get", "127.0.0.1:9", map[string]string{"Content-Type": "application/json"}, `{}`), http.StatusUnauthorized, ""},
		{"wrong key", request("POST", "/clipboard/get", "127.0.0.1:9", headers("Cookie", cookieName+"=guess"), `{}`), http.StatusUnauthorized, ""},
		{"foreign origin", request("POST", "/clipboard/get", "127.0.0.1:9", headers("Origin", "https://evil.example"), `{}`), http.StatusForbidden, ""},
		{"same-site page", request("POST", "/clipboard/get", "127.0.0.1:9", headers("Sec-Fetch-Site", "same-site"), `{}`), http.StatusForbidden, ""},
		{"rebinding host", request("POST", "/clipboard/get", "evil.example:9", headers(), `{}`), http.StatusForbidden, ""},
		{"broken json", request("POST", "/clipboard/set", "127.0.0.1:9", headers(), `{"text":"`+secret), http.StatusBadRequest, ""},
		{"over 1 MB", request("POST", "/clipboard/set", "127.0.0.1:9", headers(), `{"text":"`+strings.Repeat("a", maxClipboard+1)+`"}`), http.StatusRequestEntityTooLarge, ""},
		{"huge body", request("POST", "/clipboard/set", "127.0.0.1:9", headers(), `{"text":"`+strings.Repeat(`\u0001`, maxClipboard+20)+`"}`), http.StatusRequestEntityTooLarge, ""},
	}
	for _, c := range cases {
		w := httptest.NewRecorder()
		s.ServeHTTP(w, c.r)
		body := strings.TrimSpace(w.Body.String())
		if w.Code != c.want || c.answer != "" && body != c.answer {
			t.Errorf("%s: got %d %.80q, want %d", c.name, w.Code, body, c.want)
		}
		if w.Code >= 400 && strings.Contains(body, secret) {
			t.Errorf("%s: the error repeats the clipboard text", c.name)
		}
	}
	if clip.text != secret {
		t.Errorf("clipboard holds %q", clip.text)
	}
	if strings.Contains(logged.String(), secret) {
		t.Error("the clipboard text was logged")
	}
	w := httptest.NewRecorder()
	NewServer("secret", &fakeScreen{}, nil).ServeHTTP(w, request("POST", "/clipboard/get", "127.0.0.1:9", headers(), `{}`))
	if w.Code != http.StatusNotImplemented {
		t.Errorf("no clipboard: %d", w.Code)
	}
	w = httptest.NewRecorder()
	s.ServeHTTP(w, request("GET", "/info", "127.0.0.1:9", headers(), ""))
	if !strings.Contains(w.Body.String(), `"clipboard":true`) {
		t.Errorf("/info does not say the clipboard works: %s", w.Body)
	}
}

// TestClipboardWithXvfb runs the clipboard against a real X server and
// xclip, on a display of its own; it is skipped when either is missing.
func TestClipboardWithXvfb(t *testing.T) {
	xclip := lookPaths(t, "Xvfb", "xclip")
	display := startXvfb(t)
	var logged bytes.Buffer
	log.SetOutput(&logged)
	t.Cleanup(func() { log.SetOutput(os.Stderr) })
	c, err := DialClipboard(display)
	if err != nil {
		t.Fatal(err)
	}
	defer c.Close()
	paste := func(target string) string {
		out, err := exec.Command(xclip, "-selection", "clipboard", "-o", "-t", target).Output()
		if err != nil {
			t.Fatalf("xclip -o -t %s: %v", target, err)
		}
		return string(out)
	}
	if err := c.SetText("héllo €\nzwei"); err != nil {
		t.Fatal(err)
	}
	if got := paste("UTF8_STRING"); got != "héllo €\nzwei" {
		t.Errorf("UTF8_STRING: %q", got)
	}
	if got := paste("STRING"); got != "h\xe9llo ?\nzwei" {
		t.Errorf("STRING: %q", got)
	}
	if got := paste("TARGETS"); !strings.Contains(got, "UTF8_STRING") || !strings.Contains(got, "TEXT") {
		t.Errorf("TARGETS: %q", got)
	}
	big := strings.Repeat("0123456789abcdef", maxClipboard/16)
	if err := c.SetText(big); err != nil {
		t.Fatal(err)
	}
	if got := paste("UTF8_STRING"); got != big {
		t.Errorf("1 MB paste: %d bytes", len(got))
	}
	copyIn := func(target, text string) {
		cmd := exec.Command(xclip, "-selection", "clipboard", "-i", "-t", target)
		cmd.Stdin = strings.NewReader(text)
		if err := cmd.Run(); err != nil { // xclip stays in the background, serving
			t.Fatalf("xclip -i: %v", err)
		}
	}
	waitText := func(want string) {
		deadline := time.Now().Add(3 * time.Second)
		for {
			got, err := c.Text()
			if err == nil && got == want {
				return
			}
			if time.Now().After(deadline) {
				t.Fatalf("host clipboard: %d bytes %v, want %d bytes", len(got), err, len(want))
			}
			time.Sleep(20 * time.Millisecond)
		}
	}
	copyIn("UTF8_STRING", "from xclip ✓")
	waitText("from xclip ✓")
	copyIn("UTF8_STRING", strings.ToUpper(big)) // over xclip's piece size: INCR
	waitText(strings.ToUpper(big))
	copyIn("UTF8_STRING", big+"!")
	deadline := time.Now().Add(3 * time.Second)
	for _, err = c.Text(); !errors.Is(err, errClipboardTooBig) && time.Now().Before(deadline); _, err = c.Text() {
		time.Sleep(20 * time.Millisecond)
	}
	if !errors.Is(err, errClipboardTooBig) {
		t.Errorf("over 1 MB: %v", err)
	}
	copyIn("image/png", "\x89PNG")
	waitText("")
	if strings.Contains(logged.String(), "from xclip") || strings.Contains(logged.String(), "0123456789abcdef") {
		t.Error("clipboard text was logged")
	}
}

func lookPaths(t *testing.T, names ...string) string {
	var last string
	for _, name := range names {
		path, err := exec.LookPath(name)
		if err != nil {
			t.Skipf("%s is not installed", name)
		}
		last = path
	}
	return last
}

// startXvfb starts an X server on a free display with a cookie of its own,
// points XAUTHORITY at it, and stops it when the test ends.
func startXvfb(t *testing.T) string {
	n := 90
	for ; n < 200; n++ {
		_, err1 := os.Stat(fmt.Sprintf("/tmp/.X11-unix/X%d", n))
		_, err2 := os.Stat(fmt.Sprintf("/tmp/.X%d-lock", n))
		if os.IsNotExist(err1) && os.IsNotExist(err2) {
			break
		}
	}
	cookie := make([]byte, 16)
	rand.Read(cookie)
	auth := filepath.Join(t.TempDir(), "Xauthority")
	record := xauthRecord(fmt.Sprint(n), "MIT-MAGIC-COOKIE-1", cookie)
	record[0], record[1] = 0xff, 0xff // FamilyWild: any host
	if err := os.WriteFile(auth, record, 0o600); err != nil {
		t.Fatal(err)
	}
	t.Setenv("XAUTHORITY", auth)
	t.Setenv("DISPLAY", fmt.Sprintf(":%d", n))
	xvfb := exec.Command("Xvfb", fmt.Sprintf(":%d", n), "-nolisten", "tcp", "-auth", auth, "-screen", "0", "640x480x24")
	if err := xvfb.Start(); err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { xvfb.Process.Signal(syscall.SIGTERM); xvfb.Wait() }) // so it removes its lock and socket
	for i := 0; i < 100; i++ {
		if _, err := os.Stat(fmt.Sprintf("/tmp/.X11-unix/X%d", n)); err == nil {
			return fmt.Sprintf(":%d", n)
		}
		time.Sleep(50 * time.Millisecond)
	}
	t.Fatal("Xvfb did not start")
	return ""
}
