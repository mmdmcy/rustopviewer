# RustOp Viewer

Your own devices' desktops in a browser, over SSH.

```
rustopviewer serve        on the device: shows its screen on a private socket
lmice desktop DEVICE      from anywhere in your fleet: tunnel + browser
```

RustOp Viewer shows a device's screen in a browser tab, sends your mouse
and keyboard back, and carries clipboard text both ways. It is built to be
small and hard to misuse:

- **Nothing listens on the network.** The viewer serves on a Unix socket in
  your home folder that only you can open (or on a loopback address; on
  Windows always `127.0.0.1`). You reach it through an SSH tunnel.
- **It runs only while you are connected.** Started inside the SSH
  session (`--until-stdin-closes`), it stops when the session ends; there
  is no background service.
- **A fresh key per session.** The browser trades it for a strict
  same-site, script-proof cookie; every request must name a loopback host
  and input must be a same-origin JSON request, so web pages cannot reach
  it through your browser.
- **Standard library only.** Go, no third-party modules: on Linux it
  speaks the X11 protocol itself (screen through GetImage, mouse and
  keyboard through the X server's XTEST extension, the clipboard through X
  selections), on Windows it calls Windows' own DLLs (GDI, SendInput, the
  clipboard), on a Mac it speaks VNC to macOS's own Screen Sharing, and it
  streams JPEG to an `<img>`.

## Use

With [LinuxMice](docs/architecture.md#with-linuxmice), from any controller:

```bash
lmice desktop server          # opens your browser on server's desktop
```

By hand, with plain SSH:

```bash
ssh -L 127.0.0.1:47800:/home/me/.local/state/rustopviewer/rov.sock server \
  rustopviewer serve --until-stdin-closes
# it prints: ready key=KEY
# open http://127.0.0.1:47800/?k=KEY
```

## Windows

In PowerShell, as yourself (no administrator):

```powershell
irm https://github.com/mmdmcy/rustopviewer/releases/latest/download/install.ps1 | iex
```

It puts `rustopviewer.exe` in `%LOCALAPPDATA%\rustopviewer` after checking
it against the release's `SHA256SUMS`, adds it to your PATH, and opens this
PC's own screen in your browser as a check (`rustopviewer serve --open`;
Ctrl+C ends it).

From another computer, reach it like a Linux host, with a TCP forward
(OpenSSH for Windows forwards no Unix sockets):

```bash
ssh -L 127.0.0.1:47800:127.0.0.1:47800 pc \
  rustopviewer serve --until-stdin-closes
# it prints: ready key=KEY
# open http://127.0.0.1:47800/?k=KEY
```

Windows shows the screen only to programs in your signed-in desktop
session, and an SSH session has none. So `serve` over SSH hands the screen
to a task of yours in Task Scheduler, which runs only while you are signed
in, without administrator rights and without a trigger; the first session
sets it up (`rustopviewer install` does the same, `rustopviewer uninstall`
removes it). The session's key goes to the task in a file only you can
read, which Windows deletes when `serve` ends, however it ends; the task's
side stops within a second of that. Between sessions nothing runs.

What Windows does not allow: the lock screen, UAC prompts and Ctrl+Alt+Del
are a secure desktop no program can read or type into, and a window
running as administrator takes no input from a program that is not. The
viewer shows the primary monitor, in real pixels on scaled displays.

## A Mac

On a Mac, macOS's own Screen Sharing shows the screen and takes the mouse
and keyboard; RustOp Viewer talks to it on `127.0.0.1:5900` from the SSH
session and serves the same viewer page. macOS does not let an SSH session
capture the screen itself, and a program of its own would need macOS's
Screen Recording and Accessibility permissions, asked again after updates.
Screen Sharing already holds them, works at the lock screen too, and macOS
starts it only when a connection comes. The clipboard goes through
`pbpaste` and `pbcopy`.

Once, on the Mac:

```bash
rustopviewer install
```

It makes a VNC password only RustOp Viewer knows (8 random characters in
`~/.config/rustopviewer/vnc-password`, readable by this account only), puts
it on the clipboard and opens System Settings → General → Sharing. There:
turn on Screen Sharing, click ⓘ beside it, then Computer Settings…, tick
"VNC viewers may control screen with password", paste the password and
click OK. `install` waits until Screen Sharing takes it, then clears the
clipboard. After that `rustopviewer serve` works on the Mac like anywhere
else. `rustopviewer uninstall` moves the password aside; turning Screen
Sharing off is System Settings' switch.

Screen Sharing listens on every network the Mac joins (Apple's design;
there is no setting to keep it to the Mac itself), guarded by the
account's password and this VNC password; see [SECURITY](SECURITY.md).

## Clipboard

Text goes both ways, up to 1 MB:

- **Paste on the remote:** press Ctrl+V in the viewer. The browser hands
  its clipboard to the page with the keystroke; RustOp Viewer makes that
  text the remote's clipboard first and then sends the keys, so the remote
  app pastes it. **Paste here** (top right) does the same and presses
  Ctrl+V on the remote for you.
- **Copy from the remote:** copy there as usual (Ctrl+C, Ctrl+X,
  Ctrl+Shift+C in a terminal); a moment later the text is on this
  computer's clipboard. Only new text comes over, so Ctrl+C to stop a
  program in a remote terminal leaves your own clipboard alone. **Copy from
  remote** fetches it on demand.

Between a Mac and a computer that is not one, Control and ⌘ change places:
Ctrl+C, Ctrl+V and the other shortcuts of a PC keyboard do the Mac's ⌘C
and ⌘V on a Mac, ⌘C and ⌘V on a Mac viewer do Ctrl+C and Ctrl+V on Linux
and Windows, and the other key (the Windows key, or Control on the Mac) gives what is
left. **Paste here** presses ⌘V on a Mac and Ctrl+V elsewhere.

What the browser asks for: the viewer runs on `127.0.0.1`, which browsers
count as a secure context, so the clipboard API is there. Ctrl+V needs no
permission. **Paste here** reads the clipboard with
`navigator.clipboard.readText()`: Chrome and Edge ask once per address,
Firefox and Safari show a Paste button to click. Writing the remote's text
needs the page focused, and some browsers allow it only right after a key
press or click. When the browser refuses either way, the page says so and
opens a box: paste into it and press Send, or copy the remote's text from
it, already selected.

The text travels only inside same-origin JSON requests, like mouse and
keyboard input: never in a URL, and never logged. On an X11 host RustOp
Viewer owns the X11 CLIPBOARD selection while it holds your text, so it
stays pasteable until another program copies or the session ends; on a
Windows host the text goes on Windows' clipboard (CF_UNICODETEXT, lines
ending in CRLF there and LF here) and stays there after the session; on a
Mac it is on the pasteboard like anything copied there. Images and files
do not travel yet.

## Supported

- Hosts: Linux with an X11 desktop (Cinnamon, MATE, Xfce, KDE on X11, …),
  Windows 10 and 11 (its primary monitor), and macOS 12 or newer through
  Screen Sharing. Wayland is planned (see [ROADMAP](ROADMAP.md)).
- Viewers: any current browser.

## Build

```bash
go build -trimpath -ldflags='-s -w' .   # Go 1.24+, no dependencies
GOOS=windows go build -trimpath -ldflags='-s -w' -o rustopviewer-windows-x86_64.exe .
CGO_ENABLED=0 GOOS=darwin GOARCH=arm64 go build -trimpath -ldflags='-s -w' .   # a Mac's, from any machine
go test ./...
```

A release carries `rustopviewer-windows-x86_64.exe`, `install.ps1` and
`SHA256SUMS` (both files' checksums) as assets.

Versions up to 0.2 were a Rust program with its own registry, pairing and
a shared bearer token; they are kept at the git tag `rust-era`.

## License

MIT or Apache-2.0.
