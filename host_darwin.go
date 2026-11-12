//go:build darwin

package main

// A Mac host. macOS's own Screen Sharing shows the screen and takes mouse
// and keyboard (rfb.go, on 127.0.0.1:5900), and pbpaste and pbcopy carry
// the clipboard (pasteboard.go). Screen Sharing already holds macOS's
// screen and input permissions and starts only when a connection comes,
// so RustOp Viewer asks macOS for no permission and keeps nothing running.
// It signs in with a VNC password only RustOp Viewer knows, in a file only
// this account can read; `rustopviewer install` makes it and walks through
// giving it to Screen Sharing once.

import (
	"crypto/rand"
	"errors"
	"fmt"
	"math/big"
	"os"
	"os/exec"
	"path/filepath"
	"strings"
	"syscall"
	"time"
)

// On a Mac the viewer listens on a private socket by default, and runs
// where it is started: the SSH session reaches Screen Sharing on loopback.
const (
	defaultListen = ""
	socketsWork   = true
)

const (
	screenSharing   = "127.0.0.1:5900"
	sharingSettings = "x-apple.systempreferences:com.apple.Sharing-Settings.extension"
	installWait     = 10 * time.Minute
)

// macSteps is how the password gets into Screen Sharing, in System
// Settings.
const macSteps = `In System Settings → General → Sharing (open now):
  1. Turn on Screen Sharing.
  2. Click ⓘ beside it, then Computer Settings…
  3. Tick "VNC viewers may control screen with password", click in the
     password field and press ⌘V (the password is on the clipboard), then OK
     and Done.`

func passwordFile() string {
	dir := os.Getenv("XDG_CONFIG_HOME")
	if dir == "" {
		home, _ := os.UserHomeDir()
		dir = filepath.Join(home, ".config")
	}
	return filepath.Join(dir, "rustopviewer", "vnc-password")
}

func openHost(string) (Screen, Clipboard, func(), error) {
	password, err := readPassword()
	if err != nil {
		return nil, nil, nil, err
	}
	screen, err := DialRFB(screenSharing, password)
	if err != nil {
		return nil, nil, nil, macHint(err)
	}
	return screen, macPasteboard, func() { screen.Close() }, nil
}

func hostName(string) string { return "macOS Screen Sharing" }

func serveElsewhere(serveOptions) (bool, error) { return false, nil }

// macHint says what to do about a failed Screen Sharing connection.
func macHint(err error) error {
	switch {
	case errors.Is(err, syscall.ECONNREFUSED):
		return errors.New("macOS Screen Sharing is off: run rustopviewer install on this Mac (System Settings → General → Sharing → Screen Sharing)")
	case errors.Is(err, errRFBNoPassword):
		return errors.New("Screen Sharing takes no VNC password yet: run rustopviewer install on this Mac")
	case errors.Is(err, errRFBRefused):
		return errors.New("Screen Sharing refused RustOp Viewer's password (changed in System Settings?): run rustopviewer install on this Mac")
	}
	return fmt.Errorf("macOS Screen Sharing: %w", err)
}

// readPassword reads the VNC password, refusing a file others can read.
func readPassword() (string, error) {
	path := passwordFile()
	info, err := os.Stat(path)
	if errors.Is(err, os.ErrNotExist) {
		return "", errors.New("this Mac is not set up for RustOp Viewer yet: run rustopviewer install on it once")
	}
	if err != nil {
		return "", err
	}
	if info.Mode().Perm()&0o077 != 0 {
		return "", fmt.Errorf("%s can be read by others: move it aside and run rustopviewer install, which makes a new password", path)
	}
	data, err := os.ReadFile(path)
	if err != nil {
		return "", err
	}
	password := strings.TrimRight(string(data), "\r\n")
	if len(password) == 0 || len(password) > 8 {
		return "", fmt.Errorf("%s holds no VNC password (1 to 8 characters): move it aside and run rustopviewer install", path)
	}
	return password, nil
}

// newPassword is 8 random letters and digits: VNC passwords end at 8.
func newPassword() (string, error) {
	const alphabet = "ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz23456789"
	b := make([]byte, 8)
	for i := range b {
		n, err := rand.Int(rand.Reader, big.NewInt(int64(len(alphabet))))
		if err != nil {
			return "", err
		}
		b[i] = alphabet[n.Int64()]
	}
	return string(b), nil
}

// writePassword writes a new password where only this account reads it.
func writePassword() (string, error) {
	password, err := newPassword()
	if err != nil {
		return "", err
	}
	path := passwordFile()
	if err := os.MkdirAll(filepath.Dir(path), 0o700); err != nil {
		return "", err
	}
	tmp, err := os.CreateTemp(filepath.Dir(path), ".vnc-password-*")
	if err != nil {
		return "", err
	}
	defer os.Remove(tmp.Name())
	if _, err := tmp.WriteString(password); err != nil {
		tmp.Close()
		return "", err
	}
	if err := tmp.Close(); err != nil {
		return "", err
	}
	if err := os.Rename(tmp.Name(), path); err != nil {
		return "", err
	}
	return password, nil
}

// checkPassword signs in to Screen Sharing once and leaves.
func checkPassword(password string) error {
	r, err := DialRFB(screenSharing, password)
	if err != nil {
		return err
	}
	return r.Close()
}

func platformCommand(name string, _ []string) error {
	switch name {
	case "install":
		return macInstall()
	case "uninstall":
		return macUninstall()
	case "host":
		return errors.New("host is the Windows desktop's side of serve")
	}
	return fmt.Errorf("%s: no such command", name)
}

// macInstall makes the password (once) and waits until Screen Sharing
// takes it.
func macInstall() error {
	password, err := readPassword()
	if err == nil && checkPassword(password) == nil {
		fmt.Println("Screen Sharing takes RustOp Viewer's password already: nothing to do.")
		return nil
	}
	if err != nil {
		if _, statErr := os.Stat(passwordFile()); statErr == nil {
			return err // a file we did not make, or one others can read
		}
		if password, err = writePassword(); err != nil {
			return err
		}
	}
	if err := macPasteboard.SetText(password); err != nil {
		return fmt.Errorf("put the password on the clipboard: %w", err)
	}
	if err := exec.Command("/usr/bin/open", sharingSettings).Run(); err != nil {
		fmt.Fprintln(os.Stderr, "rustopviewer: open System Settings yourself:", err)
	}
	fmt.Printf("RustOp Viewer shows this Mac through macOS's own Screen Sharing, with a VNC\npassword only it knows (in %s, for this account only).\n\n%s\n\nWaiting for Screen Sharing to take it (Ctrl+C stops)…\n", passwordFile(), macSteps)
	refused := false
	for deadline := time.Now().Add(installWait); ; {
		err := checkPassword(password)
		if err == nil {
			if text, _ := macPasteboard.Text(); text == password {
				macPasteboard.SetText("")
			}
			fmt.Println("Done: Screen Sharing takes the password, and RustOp Viewer can show this Mac (rustopviewer serve).")
			fmt.Println("Screen Sharing listens on every network this Mac joins, guarded by its passwords.")
			return nil
		}
		if time.Now().After(deadline) {
			return fmt.Errorf("Screen Sharing did not take the password within %s (%v): run rustopviewer install again", installWait, macHint(err))
		}
		wait := 2 * time.Second
		if errors.Is(err, errRFBRefused) {
			wait = 5 * time.Second // a wrong password is not tried in a hurry
			if !refused {
				refused = true
				macPasteboard.SetText(password)
				fmt.Println("Screen Sharing refused it: paste it into the VNC password field again (it is on the clipboard).")
			}
		}
		time.Sleep(wait)
	}
}

// macUninstall moves the password aside; Screen Sharing keeps its own copy
// until it is turned off.
func macUninstall() error {
	path := passwordFile()
	if _, err := os.Stat(path); errors.Is(err, os.ErrNotExist) {
		fmt.Println("RustOp Viewer has no password on this Mac: nothing to do.")
		return nil
	}
	aside := path + ".bak-" + time.Now().Format("20060102-150405")
	if err := os.Rename(path, aside); err != nil {
		return err
	}
	fmt.Printf("Moved RustOp Viewer's password to %s.\nTurn off Screen Sharing, or its VNC password, in System Settings → General → Sharing.\n", aside)
	return nil
}
