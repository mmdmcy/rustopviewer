//go:build !windows

// The fake pasteboard commands are shell scripts.

package main

import (
	"errors"
	"os"
	"path/filepath"
	"strings"
	"testing"
)

// A pasteboard whose commands keep the text in a file, as pbcopy and
// pbpaste keep it on macOS's pasteboard.
func filePasteboard(t *testing.T) (Pasteboard, string) {
	file := filepath.Join(t.TempDir(), "board")
	return Pasteboard{
		Paste: []string{"/bin/sh", "-c", `cat "$BOARD" 2>/dev/null; true`},
		Copy:  []string{"/bin/sh", "-c", `cat > "$BOARD"`},
		Env:   []string{"BOARD=" + file},
	}, file
}

func TestPasteboardBothWays(t *testing.T) {
	p, file := filePasteboard(t)
	if text, err := p.Text(); err != nil || text != "" {
		t.Fatalf("empty board: %q, %v", text, err)
	}
	if err := p.SetText("héllo ✓\r\nline two"); err != nil {
		t.Fatal(err)
	}
	if data, _ := os.ReadFile(file); string(data) != "héllo ✓\nline two" {
		t.Fatalf("the board holds %q (lines must end in \\n on the host)", data)
	}
	if text, err := p.Text(); err != nil || text != "héllo ✓\nline two" {
		t.Fatalf("read back %q, %v", text, err)
	}
}

func TestPasteboardRefuses(t *testing.T) {
	p, file := filePasteboard(t)
	big := strings.Repeat("x", maxClipboard+1)
	if err := p.SetText(big); !errors.Is(err, errClipboardTooBig) {
		t.Errorf("more than 1 MB was taken: %v", err)
	}
	os.WriteFile(file, []byte(big), 0o600)
	if _, err := p.Text(); !errors.Is(err, errClipboardTooBig) {
		t.Errorf("more than 1 MB came back: %v", err)
	}
	os.WriteFile(file, []byte{'o', 'k', 0xff}, 0o600)
	if text, err := p.Text(); err != nil || text != "ok�" {
		t.Errorf("text that is not UTF-8: %q, %v", text, err)
	}
	broken := Pasteboard{Paste: []string{"/bin/sh", "-c", "echo 'no pasteboard here' >&2; exit 1"}}
	if _, err := broken.Text(); err == nil || !strings.Contains(err.Error(), "no pasteboard here") {
		t.Errorf("a failing command's own words are lost: %v", err)
	}
}
