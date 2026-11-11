// RustOp Viewer: your own devices' desktops in a browser, over SSH.
//
// `rustopviewer serve` shows this machine's screen on a private Unix
// socket (or a loopback address); nothing listens on the network. Reach it
// through an SSH tunnel, which `lmice desktop DEVICE` opens for you, and a
// fresh key per session. Standard library only.
package main

import (
	"crypto/rand"
	"encoding/hex"
	"errors"
	"flag"
	"fmt"
	"io"
	"net"
	"net/http"
	"os"
	"path/filepath"
	"time"
)

var version = "0.3.0"

const usage = `RustOp Viewer: your own devices' desktops in a browser, over SSH.

Usage:
  rustopviewer serve [--socket PATH | --listen 127.0.0.1:PORT] [--display :0] [--until-stdin-closes]
  rustopviewer version

serve prints "ready key=KEY" when it listens. Open http://127.0.0.1:PORT/?k=KEY
through an SSH tunnel to the socket; lmice desktop DEVICE does all of this.
Nothing listens on the network: a socket (default ~/.local/state/rustopviewer/rov.sock,
only for this user) or a loopback address.`

func main() {
	if len(os.Args) < 2 {
		fmt.Fprintln(os.Stderr, usage)
		os.Exit(2)
	}
	switch os.Args[1] {
	case "serve":
		if err := serve(os.Args[2:]); err != nil {
			fmt.Fprintln(os.Stderr, "rustopviewer:", err)
			os.Exit(1)
		}
	case "version", "--version", "-V":
		fmt.Println("rustopviewer", version)
	case "help", "--help", "-h":
		fmt.Println(usage)
	default:
		fmt.Fprintln(os.Stderr, usage)
		os.Exit(2)
	}
}

func defaultSocket() string {
	home, _ := os.UserHomeDir()
	return filepath.Join(home, ".local", "state", "rustopviewer", "rov.sock")
}

// listen opens a Unix socket only this user can use, or a loopback TCP
// address; anything reachable from the network is refused.
func listen(socket, address string) (net.Listener, string, error) {
	if address != "" {
		host, _, err := net.SplitHostPort(address)
		if err != nil || !isLoopbackHost(host) {
			return nil, "", fmt.Errorf("--listen must be a loopback address like 127.0.0.1:47800, not %q", address)
		}
		l, err := net.Listen("tcp", address)
		if err != nil {
			return nil, "", err
		}
		return l, l.Addr().String(), nil
	}
	if err := os.MkdirAll(filepath.Dir(socket), 0o700); err != nil {
		return nil, "", err
	}
	if err := os.Chmod(filepath.Dir(socket), 0o700); err != nil {
		return nil, "", err
	}
	// A socket left by an earlier session of ours is replaced; anything
	// else at that path is left alone.
	if info, err := os.Lstat(socket); err == nil {
		if info.Mode()&os.ModeSocket == 0 {
			return nil, "", fmt.Errorf("%s exists and is not a socket", socket)
		}
		if conn, err := net.Dial("unix", socket); err == nil {
			conn.Close()
			return nil, "", errors.New("another rustopviewer session is running here; end it first")
		}
		os.Remove(socket)
	}
	l, err := net.Listen("unix", socket)
	if err != nil {
		return nil, "", err
	}
	os.Chmod(socket, 0o600)
	return l, socket, nil
}

func newKey() (string, error) {
	b := make([]byte, 32)
	if _, err := rand.Read(b); err != nil {
		return "", err
	}
	return hex.EncodeToString(b), nil
}

func serve(args []string) error {
	flags := flag.NewFlagSet("serve", flag.ContinueOnError)
	socket := flags.String("socket", defaultSocket(), "Unix socket to listen on")
	address := flags.String("listen", "", "loopback address instead of a socket")
	display := flags.String("display", envOr("DISPLAY", ":0"), "X display")
	untilStdin := flags.Bool("until-stdin-closes", false, "stop when standard input closes (an SSH session ending)")
	if err := flags.Parse(args); err != nil {
		return err
	}
	screen, err := DialX11(*display)
	if err != nil {
		return err
	}
	defer screen.Close()
	var clip Clipboard
	if c, err := DialClipboard(*display); err != nil {
		fmt.Fprintln(os.Stderr, "rustopviewer: clipboard off:", err)
	} else {
		defer c.Close()
		clip = c
	}
	key, err := newKey()
	if err != nil {
		return err
	}
	listener, where, err := listen(*socket, *address)
	if err != nil {
		return err
	}
	defer listener.Close()
	if listener.Addr().Network() == "unix" {
		defer os.Remove(where)
	}
	srv := &http.Server{
		Handler:           NewServer(key, screen, clip),
		ReadHeaderTimeout: 10 * time.Second,
	}
	fmt.Printf("ready key=%s\n", key)
	fmt.Fprintf(os.Stderr, "rustopviewer %s: %dx%d display %s on %s\n", version, screen.Width, screen.Height, *display, where)
	if *untilStdin {
		go func() {
			io.Copy(io.Discard, os.Stdin)
			listener.Close()
		}()
	}
	err = srv.Serve(listener)
	if errors.Is(err, net.ErrClosed) || errors.Is(err, http.ErrServerClosed) {
		return nil
	}
	return err
}

func envOr(name, fallback string) string {
	if v := os.Getenv(name); v != "" {
		return v
	}
	return fallback
}
