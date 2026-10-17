# RustOp Viewer

Your own devices' desktops in a browser, over SSH.

```
rustopviewer serve        on the device: shows its screen on a private socket
lmice desktop DEVICE      from anywhere in your fleet: tunnel + browser
```

RustOp Viewer shows a device's screen in a browser tab and sends your mouse
and keyboard back. It is built to be small and hard to misuse:

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
  X server's XTEST extension) and streams JPEG to an `<img>`.

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
