# rustopviewer

RustOp Viewer, or **ROV**, is a Windows-first remote desktop viewer/controller written in Rust and optimized for controlling a Windows 11 laptop from an iPhone over Tailscale.

ROV currently ships as:

- A native Windows desktop app for the host machine
- A built-in mobile web client served by the host itself
- A secure, token-protected remote-control flow designed for personal tailnet use

## Why ROV

ROV exists to make a very specific workflow feel good:

- Open your laptop from your iPhone
- Reach it over Tailscale, even off-LAN
- See the desktop
- Click, drag, scroll, type, and launch shortcuts quickly
- Keep the stack small, understandable, and hackable in Rust

## Current Features

- Native Windows desktop control window
- Monitor selection
- Mobile Safari-oriented remote UI
- Screen streaming from the selected monitor
- Mouse move, click, drag, and wheel input
- Keyboard shortcuts and plain-text input
- Local view zoom and UI-hide modes on iPhone
- Token-based API authentication
- Tailscale-friendly access model

## Current Limitations

ROV is still early-stage software.

Notable current limitations:

- Windows only for the host
- The Windows session must already be awake and unlocked
- UAC or elevated windows work best when ROV runs as Administrator
- No audio streaming
- No clipboard sync
- No file transfer
- No native iOS app yet
- No WebRTC transport yet
- `Ctrl+Alt+Del` is intentionally out of scope for a normal user-space app

## Quick Start

### Requirements

- Windows 11 host
- Rust toolchain
- iPhone with Safari
- Tailscale on both devices if you want reliable off-LAN access

### Run locally

```powershell
cargo run --release
```

### Use it from iPhone

1. Start Tailscale on the laptop and on the iPhone.
2. Launch ROV on the laptop.
3. Copy one of the Tailscale URLs shown in the desktop app.
4. Open that URL in Safari on the iPhone.
5. Use the remote page to view, tap, drag, scroll, type, and send shortcuts.

### Optional: trusted HTTPS on iPhone

ROV itself serves plain HTTP on the laptop. If you want Safari to stop showing the page as not secure, the recommended setup is to keep ROV on local HTTP and put Tailscale Serve in front of it for TLS.

1. Enable MagicDNS and HTTPS certificates in the Tailscale admin console.
2. On the Windows host, run:

```powershell
tailscale serve --bg 45080
```

3. Tailscale will print an `https://...ts.net` URL for this machine.
4. Open that HTTPS URL in Safari on the iPhone.

This keeps the Rust app simple while still giving you a browser-trusted certificate and encrypted HTTPS at the browser edge.

## Repository Standards

This repository is set up to support long-term open source development:

- Dual licensed under MIT or Apache-2.0
- Issue templates and PR template
- Contributing, security, roadmap, and architecture docs

See:

- [CONTRIBUTING.md](CONTRIBUTING.md)
- [SECURITY.md](SECURITY.md)
- [ROADMAP.md](ROADMAP.md)
- [docs/architecture.md](docs/architecture.md)
- [CHANGELOG.md](CHANGELOG.md)

## Development

Recommended local validation:

```powershell
cargo fmt --all -- --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all-targets --all-features
```

## Security Model

- The host app serves the remote client locally.
- Remote API calls require the `X-Auth-Token` secret.
- Regenerating the secure link invalidates old URLs immediately.
- Tailscale is the recommended network boundary for internet use.
- Direct `http` inside a tailnet is acceptable for personal use because Tailscale still encrypts transport, but browsers will still warn because the page itself is not `https`.
- The recommended browser-trusted HTTPS deployment is Tailscale Serve in front of the local ROV server.

## Roadmap Highlights

- Better precision and calibration for scaled displays
- Pinch-to-zoom and richer mobile navigation
- Lower-latency streaming transport
- Clipboard sync
- File transfer
- Tray mode and startup behavior

## License

Licensed under either of:

- MIT license ([LICENSE-MIT](LICENSE-MIT))
- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE))

at your option.
