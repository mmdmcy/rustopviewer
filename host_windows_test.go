package main

// These run on Windows (or Wine): the screen, input and clipboard through
// the real DLLs, and serve's hand-over to the desktop's side with Task
// Scheduler replaced by starting that side in this process.

import (
	"bufio"
	"errors"
	"io"
	"net/http"
	"os"
	"path/filepath"
	"strings"
	"testing"
	"time"
	"unsafe"
)

var procGetCursorPos = user32.NewProc("GetCursorPos")

func cursor(t *testing.T) (int, int) {
	var p struct{ x, y int32 }
	if r, _, err := procGetCursorPos.Call(uintptr(unsafe.Pointer(&p))); r == 0 {
		t.Fatal(err)
	}
	return int(p.x), int(p.y)
}

func TestWindowsScreenAndInput(t *testing.T) {
	s, err := openWinScreen()
	if err != nil {
		t.Skip("no desktop here:", err)
	}
	defer s.Close()
	w, h := s.Size()
	pixels, err := s.Capture()
	if err != nil {
		t.Fatal(err)
	}
	if len(pixels) != w*h*4 {
		t.Fatalf("%d bytes for %dx%d", len(pixels), w, h)
	}
	for _, p := range [][2]int{{10, 20}, {w - 1, h - 1}, {w / 2, h / 3}} {
		if err := s.Move(p[0], p[1]); err != nil {
			t.Fatal(err)
		}
		time.Sleep(50 * time.Millisecond)
		if x, y := cursor(t); x != p[0] || y != p[1] {
			t.Errorf("moved to %v, the pointer is at %d,%d", p, x, y)
		}
	}
	for _, b := range []int{4, 5, 6, 7, 9} { // wheel notches; 9 is ignored
		if err := s.Button(b, true); err != nil {
			t.Errorf("button %d: %v", b, err)
		}
	}
}

func TestWindowsClipboard(t *testing.T) {
	var c winClipboard
	const text = "héllo\nwörld 😀 中文\n"
	if err := c.SetText(text); err != nil {
		t.Fatal(err)
	}
	got, err := c.Text()
	if err != nil || got != text {
		t.Fatalf("got %q %v", got, err)
	}
	big := strings.Repeat("a\n", maxClipboard/2) // 1 MB here, 1.5 MB with CRLF there
	if err := c.SetText(big); err != nil {
		t.Fatal(err)
	}
	if got, err := c.Text(); err != nil || got != big {
		t.Fatalf("1 MB: %d bytes, %v", len(got), err)
	}
	if err := c.SetText(big + "x"); !errors.Is(err, errClipboardTooBig) {
		t.Fatalf("over 1 MB: %v", err)
	}
}

// lineReader hands on stdout's lines.
type lineReader struct{ lines chan string }

func (l *lineReader) Write(p []byte) (int, error) {
	for _, line := range strings.Split(strings.TrimSpace(string(p)), "\n") {
		l.lines <- line
	}
	return len(p), nil
}

func TestServeHandsTheDesktopToItsTask(t *testing.T) {
	if !desktopVisible() {
		t.Skip("no desktop session here")
	}
	local := t.TempDir()
	t.Setenv("LOCALAPPDATA", local)
	dir := filepath.Join(local, "rustopviewer")
	o := serveOptions{address: "127.0.0.1:47831", untilStdin: true}
	defer func(old bool) { autoInstall = old }(autoInstall)
	autoInstall = false // Task Scheduler is not called from a test
	if err := relay(o, strings.NewReader(""), io.Discard); !errors.Is(err, errNotInstalled) {
		t.Fatalf("without install: %v", err)
	}
	if err := relay(serveOptions{address: "0.0.0.0:47831"}, strings.NewReader(""), io.Discard); err == nil {
		t.Fatal("a network address must be refused")
	}
	exe, err := windowlessSelf()
	if err != nil {
		t.Fatal(err)
	}
	os.MkdirAll(dir, 0o700)
	if err := writeHostCopy(filepath.Join(dir, hostProgram), exe[:1000]); err != nil { // an older copy
		t.Fatal(err)
	}
	hostDone := make(chan error, 1)
	defer func(old func(string) error, poll time.Duration) { startTask, sessionPoll = old, poll }(startTask, sessionPoll)
	sessionPoll = 100 * time.Millisecond
	startTask = func(name string) error {
		if !strings.HasPrefix(name, "RustOp Viewer (") {
			t.Errorf("task %q", name)
		}
		go func() { hostDone <- host() }()
		return nil
	}
	stdin, endSession := io.Pipe()
	out := &lineReader{lines: make(chan string, 4)}
	relayDone := make(chan error, 1)
	go func() { relayDone <- relay(o, stdin, out) }()
	var key string
	select {
	case line := <-out.lines:
		key = strings.TrimPrefix(line, "ready key=")
	case err := <-relayDone:
		t.Fatalf("serve ended: %v", err)
	case <-time.After(30 * time.Second):
		t.Fatal("no ready line")
	}
	if len(key) != 64 {
		t.Fatalf("ready line: %q", key)
	}
	if copy, _ := os.ReadFile(filepath.Join(dir, hostProgram)); string(copy) != string(exe) {
		t.Error("the task's copy was not brought up to date")
	}
	log, _ := os.ReadFile(filepath.Join(dir, hostLog))
	if strings.Contains(string(log), key) {
		t.Error("host.log holds the key")
	}
	// The desktop's side answers with the session's key only.
	client := &http.Client{CheckRedirect: func(*http.Request, []*http.Request) error { return http.ErrUseLastResponse }}
	resp, err := client.Get("http://127.0.0.1:47831/?k=" + key)
	if err != nil || resp.StatusCode != http.StatusSeeOther {
		t.Fatalf("key exchange: %v %v", resp, err)
	}
	req, _ := http.NewRequest("GET", "http://127.0.0.1:47831/info", nil)
	req.AddCookie(&http.Cookie{Name: cookieName, Value: key})
	resp, err = client.Do(req)
	if err != nil {
		t.Fatal(err)
	}
	info, _ := bufio.NewReader(resp.Body).ReadString('\n')
	resp.Body.Close()
	if resp.StatusCode != 200 || !strings.Contains(info, `"clipboard":true`) {
		t.Fatalf("/info: %d %s", resp.StatusCode, info)
	}
	// A second session waits for this one to end.
	if err := relay(o, strings.NewReader(""), io.Discard); err == nil || !strings.Contains(err.Error(), "another rustopviewer session") {
		t.Errorf("a second session: %v", err)
	}
	// The SSH session ends: serve returns, the file goes, the desktop's side stops.
	endSession.Close()
	if err := <-relayDone; err != nil {
		t.Fatal(err)
	}
	select {
	case err := <-hostDone:
		if err != nil {
			t.Fatal(err)
		}
	case <-time.After(5 * time.Second):
		t.Fatal("the desktop's side kept running after the session ended")
	}
	if _, err := os.Stat(filepath.Join(dir, sessionFile)); !errors.Is(err, os.ErrNotExist) {
		t.Errorf("session file after the session: %v", err)
	}
	if _, err := http.Get("http://127.0.0.1:47831/info"); err == nil {
		t.Error("still listening after the session")
	}
	// The task's side alone has nothing to show, and says so for serve.
	if err := host(); err == nil {
		t.Error("host without a session")
	}
	if log, _ := os.ReadFile(filepath.Join(dir, hostLog)); !strings.HasPrefix(string(log), "error: no session") {
		t.Errorf("host.log: %q", log)
	}
}
