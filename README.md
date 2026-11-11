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
  your home folder that only you can open (or on a loopback address).
  You reach it through an SSH tunnel.
- **It runs only while you are connected.** Started inside the SSH
  session (`--until-stdin-closes`), it stops when the session ends; there
  is no background service.
- **A fresh key per session.** The browser trades it for a strict
  same-site, script-proof cookie; every request must name a loopback host
  and input must be a same-origin JSON request, so web pages cannot reach
  it through your browser.
- **Standard library only.** Go, no third-party modules: it speaks the X11
  protocol itself (screen through GetImage, mouse and keyboard through the
  X server's XTEST extension, the clipboard through X selections) and
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
keyboard input: never in a URL, and never logged. On the host RustOp
Viewer owns the X11 CLIPBOARD selection while it holds your text, so it
stays pasteable until another program copies or the session ends. Images
and files do not travel yet. From a Mac, ⌘V gives the remote your text
but reaches it as Super+V; use **Paste here** or Control+V there.

## Supported

- Hosts: Linux with an X11 desktop (Cinnamon, MATE, Xfce, KDE on X11, …).
  Windows hosts and Wayland are planned (see [ROADMAP](ROADMAP.md)).
- Viewers: any current browser.

## Build

```bash
go build -trimpath -ldflags='-s -w' .   # Go 1.24+, no dependencies
go test ./...
```

Versions up to 0.2 were a Rust program with its own registry, pairing and
a shared bearer token; they are kept at the git tag `rust-era`.

## License

MIT or Apache-2.0.
