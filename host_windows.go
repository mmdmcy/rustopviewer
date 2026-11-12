package main

// Windows shows its screen only to programs in the signed-in user's
// desktop session, and an SSH session runs in session 0, which has none.
// So over SSH, serve hands the desktop to that user's own task (Task
// Scheduler, run only when the user is signed in, no administrator, no
// trigger; serve sets it up the first time): it writes the session's key
// to a file only that user can read and that lasts exactly as long as
// serve (deleted by Windows when serve's handle closes, however it ends),
// starts the task, and prints the ready line once the task's side listens. The task's side (`host`) reads the
// key, serves on 127.0.0.1, and stops within a second of the file going.
// Nothing runs and nothing listens between sessions.
//
// Run in the desktop session itself (a terminal there), serve shows the
// screen directly, as on X11.

import (
	"bytes"
	"errors"
	"fmt"
	"io"
	"net"
	"os"
	"os/exec"
	"os/signal"
	"os/user"
	"path/filepath"
	"strings"
	"syscall"
	"time"
)

const (
	defaultListen = "127.0.0.1:47800"
	socketsWork   = false // OpenSSH for Windows forwards no Unix sockets

	hostProgram = "rustopviewer-host.exe" // the windowless copy the task runs
	sessionFile = "session"
	hostLog     = "host.log"

	fileFlagDeleteOnClose = 0x04000000
	fileAttributeTemp     = 0x100
	errorFileExists       = syscall.Errno(80)
)

// Variables, so a test replaces Task Scheduler.
var (
	// startTask runs the user's task, which starts `host` on the desktop.
	startTask = func(name string) error {
		out, err := exec.Command("schtasks", "/run", "/tn", name).CombinedOutput()
		if err != nil {
			return fmt.Errorf("schtasks /run: %s", orText(strings.TrimSpace(string(out)), err.Error()))
		}
		return nil
	}
	readyWait   = 20 * time.Second
	sessionPoll = time.Second
	// autoInstall: serve sets the task up when it is missing.
	autoInstall = true
)

func openHost(string) (Screen, Clipboard, func(), error) {
	s, err := openWinScreen()
	if err != nil {
		return nil, nil, nil, err
	}
	return s, winClipboard{}, s.Close, nil
}

func hostName(string) string { return "Windows desktop (primary monitor)" }

func platformCommand(name string, args []string) error {
	if len(args) > 0 {
		return fmt.Errorf("%s takes no arguments", name)
	}
	switch name {
	case "install":
		return install(os.Stdout)
	case "uninstall":
		return uninstall(os.Stdout)
	}
	return host()
}

// stateDir is %LOCALAPPDATA%\rustopviewer, which only this user (and the
// PC's administrators) can open.
func stateDir() (string, error) {
	local, err := os.UserCacheDir()
	if err != nil {
		return "", err
	}
	return filepath.Join(local, "rustopviewer"), nil
}

// thisUser is the user's name for the task and SID for its principal.
func thisUser() (name, sid string, err error) {
	u, err := user.Current()
	if err != nil {
		return "", "", err
	}
	return taskName(u.Username), u.Uid, nil
}

// serveElsewhere: outside the desktop's session, the desktop's task shows it.
func serveElsewhere(o serveOptions) (bool, error) {
	if desktopVisible() {
		return false, nil
	}
	if o.open {
		return true, errors.New("--open opens a browser on the desktop serve runs on, and this session has none (over SSH, the browser is on your side of the tunnel)")
	}
	return true, relay(o, os.Stdin, os.Stdout)
}

var errNotInstalled = errors.New("this session cannot see the desktop, and the task that shows it is not installed: rustopviewer install sets it up (as this user, no administrator)")

func relay(o serveOptions, stdin io.Reader, stdout io.Writer) error {
	if err := loopbackOnly(o.address); err != nil {
		return err // before the task starts
	}
	dir, err := stateDir()
	if err != nil {
		return err
	}
	err = refreshHostCopy(dir)
	if errors.Is(err, errNotInstalled) && autoInstall {
		// The first session sets the task up, and says so.
		err = install(os.Stderr)
	}
	if err != nil {
		return err
	}
	name, _, err := thisUser()
	if err != nil {
		return err
	}
	key, err := newKey()
	if err != nil {
		return err
	}
	session, err := createSession(filepath.Join(dir, sessionFile), formatSession(key, o.address))
	if err != nil {
		return err
	}
	defer session.Close() // Windows deletes the file: the desktop's side stops
	logPath := filepath.Join(dir, hostLog)
	os.Remove(logPath)
	if err := startTask(name); err != nil {
		return fmt.Errorf("%w (rustopviewer install sets the task up again)", err)
	}
	if err := waitReady(logPath, o.address); err != nil {
		return err
	}
	fmt.Fprintf(stdout, "ready key=%s\n", key)
	fmt.Fprintf(os.Stderr, "rustopviewer %s: the Windows desktop on %s, shown by its own session\n", version, o.address)
	if o.untilStdin {
		io.Copy(io.Discard, stdin)
		return nil
	}
	stop := make(chan os.Signal, 1)
	signal.Notify(stop, os.Interrupt)
	<-stop
	return nil
}

// waitReady waits for the desktop's side to say it listens (or why not).
func waitReady(logPath, address string) error {
	deadline := time.Now().Add(readyWait)
	for time.Now().Before(deadline) {
		data, _ := os.ReadFile(logPath)
		for _, line := range strings.Split(string(data), "\n") {
			line = strings.TrimSpace(line)
			if line == "ready "+address {
				return nil
			}
			if msg, ok := strings.CutPrefix(line, "error: "); ok {
				return errors.New(msg)
			}
		}
		time.Sleep(100 * time.Millisecond)
	}
	return fmt.Errorf("the desktop's task did not show the screen within %s: is this user signed in on the PC's own screen? The task runs only in a signed-in desktop session", readyWait)
}

// createSession writes the session file and keeps it open: Windows deletes
// it when the handle closes, also when serve is killed.
func createSession(path string, content []byte) (*os.File, error) {
	p, err := syscall.UTF16PtrFromString(path)
	if err != nil {
		return nil, err
	}
	var h syscall.Handle
	for try := 0; ; try++ {
		h, err = syscall.CreateFile(p, syscall.GENERIC_WRITE, syscall.FILE_SHARE_READ|syscall.FILE_SHARE_DELETE, nil,
			syscall.CREATE_NEW, fileAttributeTemp|fileFlagDeleteOnClose, 0)
		if err == nil {
			break
		}
		// A session that just ended may take a moment to go.
		if try == 20 || err != errorFileExists && err != syscall.ERROR_ACCESS_DENIED {
			if err == errorFileExists {
				return nil, errors.New("another rustopviewer session is showing this desktop; end it first")
			}
			return nil, fmt.Errorf("session file: %w", err)
		}
		time.Sleep(100 * time.Millisecond)
	}
	f := os.NewFile(uintptr(h), path)
	if _, err := f.Write(content); err != nil {
		f.Close()
		return nil, err
	}
	return f, nil
}

// readSessionFile reads the session file while serve holds it open.
func readSessionFile(path string) ([]byte, error) {
	p, err := syscall.UTF16PtrFromString(path)
	if err != nil {
		return nil, err
	}
	h, err := syscall.CreateFile(p, syscall.GENERIC_READ, syscall.FILE_SHARE_READ|syscall.FILE_SHARE_WRITE|syscall.FILE_SHARE_DELETE, nil, syscall.OPEN_EXISTING, 0, 0)
	if err != nil {
		return nil, err
	}
	f := os.NewFile(uintptr(h), path)
	defer f.Close()
	return io.ReadAll(io.LimitReader(f, 4096))
}

// host is the task's side: it shows the desktop to the session serve
// started, and writes what happens (never the key) to host.log for serve.
func host() error {
	dir, err := stateDir()
	if err != nil {
		return err
	}
	logFile, err := os.Create(filepath.Join(dir, hostLog))
	if err != nil {
		return err
	}
	defer logFile.Close()
	fail := func(err error) error {
		fmt.Fprintf(logFile, "error: %s\n", strings.ReplaceAll(err.Error(), "\n", " "))
		return err
	}
	path := filepath.Join(dir, sessionFile)
	data, err := readSessionFile(path)
	if err != nil {
		return fail(errors.New("no session to show: rustopviewer serve over SSH starts one"))
	}
	key, address, err := parseSession(data)
	if err != nil {
		return fail(err)
	}
	if !desktopVisible() {
		return fail(errors.New("the task ran outside the signed-in desktop session (its principal must be InteractiveToken: rustopviewer install sets it up again)"))
	}
	screen, clip, closeHost, err := openHost("")
	if err != nil {
		return fail(err)
	}
	defer closeHost()
	var listener net.Listener
	for try := 0; ; try++ { // the last session's side may still be going
		if listener, _, err = listen("", address); err == nil {
			break
		}
		if try == 30 {
			return fail(fmt.Errorf("listen on %s: %v", address, err))
		}
		time.Sleep(100 * time.Millisecond)
	}
	defer listener.Close()
	w, h := screen.Size()
	fmt.Fprintf(logFile, "ready %s\n%dx%d\n", address, w, h)
	go func() {
		for {
			time.Sleep(sessionPoll)
			data, err := readSessionFile(path)
			if now, _, perr := parseSession(data); err != nil || perr != nil || now != key {
				listener.Close()
				return
			}
		}
	}()
	return serveOn(listener, key, screen, clip)
}

// refreshHostCopy keeps the task's windowless copy the same program as
// this one (an update replaces this one only).
func refreshHostCopy(dir string) error {
	target := filepath.Join(dir, hostProgram)
	old, err := os.ReadFile(target)
	if errors.Is(err, os.ErrNotExist) {
		return errNotInstalled
	}
	if err != nil {
		return err
	}
	want, err := windowlessSelf()
	if err != nil {
		return err
	}
	if bytes.Equal(old, want) {
		return nil
	}
	return writeHostCopy(target, want)
}

func windowlessSelf() ([]byte, error) {
	self, err := os.Executable()
	if err != nil {
		return nil, err
	}
	exe, err := os.ReadFile(self)
	if err != nil {
		return nil, err
	}
	return windowless(exe)
}

func writeHostCopy(target string, exe []byte) error {
	next := target + ".new"
	if err := os.WriteFile(next, exe, 0o755); err != nil {
		return err
	}
	if err := os.Rename(next, target); err != nil {
		os.Remove(next)
		return fmt.Errorf("replace %s (a session may still be showing the desktop; end it first): %w", target, err)
	}
	return nil
}

// install sets up the task; run again, it sets it up the same way.
func install(out io.Writer) error {
	dir, err := stateDir()
	if err != nil {
		return err
	}
	if err := os.MkdirAll(dir, 0o700); err != nil {
		return err
	}
	name, sid, err := thisUser()
	if err != nil {
		return err
	}
	exe, err := windowlessSelf()
	if err != nil {
		return err
	}
	target := filepath.Join(dir, hostProgram)
	if err := writeHostCopy(target, exe); err != nil {
		return err
	}
	definition := filepath.Join(dir, "task.xml")
	if err := os.WriteFile(definition, utf16File(taskXML(sid, target)), 0o600); err != nil {
		return err
	}
	defer os.Remove(definition)
	if b, err := exec.Command("schtasks", "/create", "/tn", name, "/xml", definition, "/f").CombinedOutput(); err != nil {
		return fmt.Errorf("schtasks /create: %s", orText(strings.TrimSpace(string(b)), err.Error()))
	}
	fmt.Fprintf(out, "installed: the task %q shows this desktop to an SSH session of yours that runs rustopviewer serve, only while it runs and only while you are signed in here; it has no trigger and no administrator rights (%s). rustopviewer uninstall removes it.\n", name, target)
	return nil
}

// uninstall removes the task and its copy of this program.
func uninstall(out io.Writer) error {
	dir, err := stateDir()
	if err != nil {
		return err
	}
	name, _, err := thisUser()
	if err != nil {
		return err
	}
	b, taskErr := exec.Command("schtasks", "/delete", "/tn", name, "/f").CombinedOutput()
	copyErr := os.Remove(filepath.Join(dir, hostProgram))
	switch {
	case taskErr != nil && errors.Is(copyErr, os.ErrNotExist):
		fmt.Fprintln(out, "not installed: nothing to remove")
		return nil
	case taskErr != nil:
		return fmt.Errorf("schtasks /delete: %s", orText(strings.TrimSpace(string(b)), taskErr.Error()))
	case copyErr != nil && !errors.Is(copyErr, os.ErrNotExist):
		return fmt.Errorf("the task is removed; its copy of this program is not: %w", copyErr)
	}
	fmt.Fprintf(out, "removed the task %q and its copy of this program\n", name)
	return nil
}

func orText(text, fallback string) string {
	if text != "" {
		return text
	}
	return fallback
}
