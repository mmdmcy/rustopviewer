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
	"os/exec"
	"path/filepath"
	"runtime"
	"time"
)

var version = "0.3.0"

const usage = `RustOp Viewer: your own devices' desktops in a browser, over SSH.

Usage:
  rustopviewer serve [--socket PATH | --listen 127.0.0.1:PORT] [--display :0] [--until-stdin-closes] [--open]
  rustopviewer install | uninstall      (Windows: the task that shows the desktop to SSH sessions)
  rustopviewer version

serve prints "ready key=KEY" when it listens. Open http://127.0.0.1:PORT/?k=KEY
through an SSH tunnel to the socket; lmice desktop DEVICE does all of this.
--open opens this computer's browser on it instead, to try it on the machine itself.
Nothing listens on the network: a socket (default ~/.local/state/rustopviewer/rov.sock,
only for this user) or a loopback address (on Windows always, default 127.0.0.1:47800).`

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
	case "install", "uninstall", "host":
		if err := platformCommand(os.Args[1], os.Args[2:]); err != nil {
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
		if err := loopbackOnly(address); err != nil {
			return nil, "", err
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

func loopbackOnly(address string) error {
	host, _, err := net.SplitHostPort(address)
	if err != nil || !isLoopbackHost(host) {
		return fmt.Errorf("--listen must be a loopback address like 127.0.0.1:47800, not %q", address)
	}
	return nil
}

func newKey() (string, error) {
	b := make([]byte, 32)
	if _, err := rand.Read(b); err != nil {
		return "", err
	}
	return hex.EncodeToString(b), nil
}

// serveOptions are serve's flags.
type serveOptions struct {
	socket, address, display string
	untilStdin, open         bool
}

func serve(args []string) error {
	var o serveOptions
	flags := flag.NewFlagSet("serve", flag.ContinueOnError)
	flags.StringVar(&o.socket, "socket", defaultSocket(), "Unix socket to listen on")
	flags.StringVar(&o.address, "listen", defaultListen, "loopback address instead of a socket")
	flags.StringVar(&o.display, "display", envOr("DISPLAY", ":0"), "X display")
	flags.BoolVar(&o.untilStdin, "until-stdin-closes", false, "stop when standard input closes (an SSH session ending)")
	flags.BoolVar(&o.open, "open", false, "open this computer's browser on the viewer")
	if err := flags.Parse(args); err != nil {
		return err
	}
	if o.address == "" && !socketsWork {
		return errors.New("--listen must be a loopback address like 127.0.0.1:47800 here")
	}
	// Where this process cannot see the screen (Windows over SSH), the
	// desktop's own session shows it.
	if handled, err := serveElsewhere(o); handled {
		return err
	}
	screen, clip, closeHost, err := openHost(o.display)
	if err != nil {
		return err
	}
	defer closeHost()
	key, err := newKey()
	if err != nil {
		return err
	}
	listener, where, err := listen(o.socket, o.address)
	if err != nil {
		return err
	}
	defer listener.Close()
	if listener.Addr().Network() == "unix" {
		defer os.Remove(where)
	}
	fmt.Printf("ready key=%s\n", key)
	width, height := screen.Size()
	fmt.Fprintf(os.Stderr, "rustopviewer %s: %dx%d %s on %s\n", version, width, height, hostName(o.display), where)
	if o.untilStdin {
		go func() {
			io.Copy(io.Discard, os.Stdin)
			listener.Close()
		}()
	}
	if o.open {
		if listener.Addr().Network() != "tcp" {
			return errors.New("--open needs --listen 127.0.0.1:PORT (a browser cannot open a socket)")
		}
		go func() {
			if err := openBrowser(fmt.Sprintf("http://%s/?k=%s", where, key)); err != nil {
				fmt.Fprintln(os.Stderr, "rustopviewer: open the browser yourself:", err)
			}
		}()
	}
	return serveOn(listener, key, screen, clip)
}

// serveOn serves the viewer until the listener closes, then drops the
// connections still open (a stream, a kept-alive request).
func serveOn(listener net.Listener, key string, screen Screen, clip Clipboard) error {
	srv := &http.Server{
		Handler:           NewServer(key, screen, clip),
		ReadHeaderTimeout: 10 * time.Second,
	}
	err := srv.Serve(listener)
	srv.Close()
	if errors.Is(err, net.ErrClosed) || errors.Is(err, http.ErrServerClosed) {
		return nil
	}
	return err
}

// openBrowser opens this computer's default browser on url, passed as one
// argument and never through a shell.
func openBrowser(url string) error {
	switch runtime.GOOS {
	case "windows":
		return exec.Command("rundll32", "url.dll,FileProtocolHandler", url).Run()
	case "darwin":
		return exec.Command("open", url).Run()
	}
	return exec.Command("xdg-open", url).Run()
}

func envOr(name, fallback string) string {
	if v := os.Getenv(name); v != "" {
		return v
	}
	return fallback
}
