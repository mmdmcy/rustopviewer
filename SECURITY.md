# Security

RustOp Viewer controls a desktop, so it is built to be reachable only by
you:

- It never listens on a network interface: a Unix socket (directory 0700,
  socket 0600) or a loopback address; any other address is refused.
- Access goes through SSH, so SSH's keys and host keys are the
  authentication; the viewer adds a random 256-bit key per session.
- The key becomes an HttpOnly, SameSite=Strict cookie. Requests must name a
  loopback host (no DNS rebinding); browsers' cross-site requests and
  foreign origins are refused; input must be JSON (no form posts); pages
  forbid framing and load only their own scripts.
- With `--until-stdin-closes` it stops when the SSH session ends.
- Clipboard text travels only in same-origin JSON POSTs, never in a URL;
  it is text only, at most 1 MB, and never logged. The host keeps the
  viewer's text only while it owns the clipboard.

On a Mac, RustOp Viewer reaches the screen through macOS's own Screen
Sharing on `127.0.0.1:5900`, and only there (any other address is
refused):

- Screen Sharing itself listens on every network the Mac joins, as Apple
  made it; there is no setting to keep it to the Mac. It asks for an
  account's password, or the VNC password. Turn it off in System Settings
  when you stop using RustOp Viewer.
- The VNC password is 8 random characters (about 47 bits; VNC passwords end
  at 8) made by `rustopviewer install`, in
  `~/.config/rustopviewer/vnc-password`, readable by this account only;
  `serve` refuses the file when others can read it. Any program running as
  this account can read it, as it can use RustOp Viewer, so on that Mac
  your account's programs can see and drive the screen without macOS's
  Screen Recording prompt. VNC authentication is DES challenge-response:
  RustOp Viewer uses it only over loopback, never across a network.
- The password is never printed: `install` puts it on the clipboard for
  System Settings and clears it there once Screen Sharing takes it.

Version 0.2 and earlier (Rust) used a shared bearer token across devices;
if you ran it, stop its services and rotate that token.

Report a vulnerability through a private security advisory on GitHub.
