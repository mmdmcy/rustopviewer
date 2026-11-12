//go:build !windows

package main

import (
	"errors"
	"fmt"
	"os"
)

// On X11 the viewer listens on a private socket by default, and runs where
// it is started: an SSH session reaches the display with its cookie.
const (
	defaultListen = ""
	socketsWork   = true
)

// openHost opens an X display: the screen, mouse and keyboard on one
// connection, the clipboard on a second (off, with a message, when that
// fails).
func openHost(display string) (Screen, Clipboard, func(), error) {
	screen, err := DialX11(display)
	if err != nil {
		return nil, nil, nil, err
	}
	c, err := DialClipboard(display)
	if err != nil {
		fmt.Fprintln(os.Stderr, "rustopviewer: clipboard off:", err)
		return screen, nil, func() { screen.Close() }, nil
	}
	return screen, c, func() { c.Close(); screen.Close() }, nil
}

func hostName(display string) string { return "display " + display }

func serveElsewhere(serveOptions) (bool, error) { return false, nil }

func platformCommand(name string, _ []string) error {
	if name == "host" {
		return errors.New("host is the Windows desktop's side of serve")
	}
	return fmt.Errorf("%s is for Windows hosts; on Linux nothing needs installing: an SSH session runs rustopviewer serve", name)
}
