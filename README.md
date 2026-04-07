# rustopviewer

RustOp Viewer, or **ROV**, is a Windows-first remote desktop viewer/controller written in Rust and optimized for controlling a Windows laptop from an iPhone over Tailscale, including when the phone is on mobile data or another Wi-Fi network.

ROV currently ships as:

- A native Windows desktop app for the host machine
- A built-in mobile web client served by the host itself
- A host-approved pairing flow that exchanges one-time codes for short-lived sessions
- In-app readiness checks for off-LAN access over Tailscale
- Optional one-click Tailscale HTTPS setup for Safari-friendly remote access

## Why ROV

ROV exists to make a very specific workflow feel good:

- Open your laptop from your iPhone
- Reach it over Tailscale, even off-LAN
- See the desktop
- Click, drag, scroll, type, and launch shortcuts quickly
- Keep the stack small, understandable, and hackable in Rust

## Product Direction

This repository is intentionally aimed at a focused remote-control workflow rather than a full Remote Desktop replacement.

### Core goals

- Reliable remote access from a phone to a Windows machine while on the go
- Off-LAN access only through Tailscale
- No direct public internet exposure and no non-Tailscale off-LAN mode
- Support for Windows Home as well as higher Windows editions
- Security-first defaults, with host-approved pairing and tight session handling
- A mobile-first experience that remains usable on limited or flaky phone connectivity
- Fast access to the live desktop with the inputs that matter most: pointer, scroll, text, and shortcuts

### Explicit non-goals for now

- Replacing Windows Remote Desktop feature-for-feature
- Audio streaming
- Clipboard sync
- File transfer
- Enterprise desktop-management features
- Chasing extra complexity when the current phone workflow already feels good enough

### Operational boundaries

- The Windows session is expected to be awake and unlocked
- Tailscale is the network boundary for off-LAN use
- The phone client is a focused control surface, not a general-purpose desktop protocol
- Security and safe remote use take priority over convenience when the two conflict

## Current Features

- Native Windows desktop control window
- Monitor selection
- Mobile Safari-oriented remote UI
- Screen streaming from the selected monitor
- Balanced, Data Saver, and Emergency stream profiles for mobile-friendly bandwidth use
- Mouse move, click, drag, and wheel input
- Keyboard shortcuts and plain-text input
- Local button and two-finger view zoom with panning on iPhone
- View-only by default, with host-side toggles for remote pointer and keyboard control
- Loopback plus Tailscale-tailnet host listeners, with Tailscale Serve HTTPS as an optional browser convenience

## Current Limitations

ROV is still early-stage software.

Notable current limitations:

- Windows only for the host
- The Windows session must already be awake and unlocked
- Remote input is intentionally locked out while ROV runs as Administrator
- No audio streaming
- No clipboard sync
- No file transfer
- No native iOS app yet
- No WebRTC transport yet
- `Ctrl+Alt+Del` is intentionally out of scope for a normal user-space app

## Quick Start

### Requirements

- Windows host
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
3. Copy the tailnet "Phone URL" shown in the desktop app.
4. Open that URL on the iPhone while both devices are connected to the same tailnet.
5. If you later want a browser-trusted HTTPS URL, click **Enable HTTPS for iPhone**.
6. Generate a one-time pairing code on the Windows app and enter it on the phone.
7. Use the remote page to view, and enable input scopes on the Windows app only when you need control.

### Recommended: trusted HTTPS on iPhone

ROV keeps its remote-control server off the normal LAN and can publish it to the phone in two ways:

- Directly on the device's Tailscale tailnet address
- Through optional Tailscale Serve HTTPS if you want a browser-trusted URL

This means the Rust app itself does not open a normal LAN-facing remote-control socket and still avoids public internet exposure by default.

You can now do this from inside the Windows app with the **Enable HTTPS for iPhone** button.

1. Enable MagicDNS and HTTPS certificates in the Tailscale admin console.
2. In the Windows app, click **Enable HTTPS for iPhone**.

If you prefer to do it manually on the Windows host, run:

```powershell
tailscale serve --bg --yes 45080
```

3. Tailscale will print an `https://...ts.net` URL for this machine.
4. Open that HTTPS URL in Safari on the iPhone.

This remains optional. Tailnet-only access already keeps traffic inside the encrypted Tailscale network boundary.

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

- The host app listens on loopback and the current Tailscale tailnet IPs, not on the normal LAN or public internet.
- Opening the phone page is not enough to gain control. A new phone session must be paired with a one-time code generated on the Windows app.
- Approved phone sessions are short-lived, cookie-based, and only one session is kept at a time.
- Remote pointer and keyboard control both default to off; view-only mode is the safe starting state.
- Running ROV as Administrator forces remote input back to view-only.
- Tailscale remains the supported network boundary for off-LAN use; no-Tailscale off-LAN exposure is intentionally out of scope.

## Roadmap Highlights

- Better precision and calibration for scaled displays
- Better gesture calibration for Safari and scaled displays
- Lower-latency streaming transport
- Clipboard sync
- File transfer
- Tray mode and startup behavior

## License

Licensed under either of:

- MIT license ([LICENSE-MIT](LICENSE-MIT))
- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE))

at your option.
