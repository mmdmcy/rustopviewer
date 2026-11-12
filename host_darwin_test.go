//go:build darwin

package main

import (
	"net"
	"os"
	"path/filepath"
	"strings"
	"testing"
)

func TestMacPasswordFile(t *testing.T) {
	t.Setenv("XDG_CONFIG_HOME", t.TempDir())
	if _, err := readPassword(); err == nil || !strings.Contains(err.Error(), "rustopviewer install") {
		t.Fatalf("no file: %v", err)
	}
	made, err := writePassword()
	if err != nil {
		t.Fatal(err)
	}
	if len(made) != 8 || strings.Trim(made, "ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz23456789") != "" {
		t.Fatalf("password %d characters, or outside the alphabet", len(made))
	}
	if again, _ := newPassword(); again == made {
		t.Fatal("two passwords alike")
	}
	file := passwordFile()
	for path, want := range map[string]os.FileMode{file: 0o600, filepath.Dir(file): 0o700} {
		if info, err := os.Stat(path); err != nil || info.Mode().Perm() != want {
			t.Errorf("%s: %v %v, want %o", path, info.Mode().Perm(), err, want)
		}
	}
	if got, err := readPassword(); err != nil || got != made {
		t.Fatalf("read back: %v", err)
	}
	os.WriteFile(file, []byte("short\n"), 0o600) // by hand, with a newline
	if got, err := readPassword(); err != nil || got != "short" {
		t.Errorf("a hand-written password: %q %v", got, err)
	}
	os.WriteFile(file, []byte("longer than eight"), 0o600)
	if _, err := readPassword(); err == nil {
		t.Error("a password VNC would cut at 8 was taken")
	}
	os.Chmod(file, 0o644)
	if _, err := readPassword(); err == nil || !strings.Contains(err.Error(), "read by others") {
		t.Errorf("a file others can read was taken: %v", err)
	}
}

func TestMacHintWhenScreenSharingIsOff(t *testing.T) {
	l, err := net.Listen("tcp", "127.0.0.1:0")
	if err != nil {
		t.Fatal(err)
	}
	addr := l.Addr().String()
	l.Close() // nothing listens there now
	_, err = DialRFB(addr, "x")
	if err == nil || !strings.Contains(macHint(err).Error(), "Screen Sharing is off") {
		t.Errorf("a refused connection says: %v", macHint(err))
	}
}
