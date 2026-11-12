//go:build !darwin

package main

import (
	"fmt"
	"os"
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

func platformCommand(name string, _ []string) error {
	return fmt.Errorf("%s is for macOS hosts; on Linux nothing needs setting up: an SSH session runs rustopviewer serve", name)
}
