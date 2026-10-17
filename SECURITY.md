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

Version 0.2 and earlier (Rust) used a shared bearer token across devices;
if you ran it, stop its services and rotate that token.

Report a vulnerability through a private security advisory on GitHub.
