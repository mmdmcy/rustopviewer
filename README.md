# rustopviewer

RustOp Viewer, or **ROV**, is a Rust remote desktop viewer/controller for Linux and Windows hosts with built-in browser clients for desktop and mobile use.

ROV currently ships as:

- A cross-platform host TUI for Linux and Windows
- A built-in browser client served by the host itself
- A host-approved pairing flow that exchanges one-time codes for short-lived sessions
- Optional remembered-browser trust for devices you approve once and revisit later
- Loopback-first network exposure with optional Tailscale URL discovery
- Reverse-proxy-friendly browser paths for subpath or hostname-based publishing
- In-terminal readiness checks for local and private-browser access paths

## Why ROV

ROV exists to make a very specific workflow feel good:

- Open a host machine from a browser
- Reach it through loopback, Tailscale, or a private reverse proxy/tunnel
- See the desktop
- Click, drag, scroll, type, and launch shortcuts quickly
- Keep the stack small, understandable, and hackable in Rust

## Product Direction

This repository is intentionally aimed at a focused remote-control workflow rather than a full Remote Desktop replacement.

### Core goals

- Reliable remote access from desktop or mobile browsers to Linux and Windows hosts
- Loopback-first deployment that works well with Tailscale or a reverse proxy/tunnel
- No direct public internet exposure by default
- Security-first defaults, with host-approved pairing and tight session handling
- A browser experience that remains usable on limited or flaky connectivity
- Fast access to the live desktop with the inputs that matter most: pointer, scroll, text, and shortcuts

### Explicit non-goals for now

- Replacing full desktop-sharing suites feature-for-feature
- Audio streaming
- Clipboard sync
- File transfer
- Enterprise desktop-management features
- Chasing extra complexity when the focused browser workflow already feels good enough

### Operational boundaries

- The host session is expected to be awake and unlocked
- Loopback is the default listener; Tailscale and reverse proxies are additive publishing paths
- The browser client is a focused control surface, not a general-purpose desktop protocol
- Security and safe remote use take priority over convenience when the two conflict

## Current Features

- Cross-platform host TUI for Linux and Windows
- Optional `--headless` host runtime for unattended restart-safe deployments after the first approval
- Optional `--print-pair-code` startup flow for one-time first pairing when you are running headless
- Monitor selection
- Browser client that works on desktop and mobile browsers
- Screen streaming from the selected monitor
- Balanced, Data Saver, and Emergency stream profiles for browser-friendly bandwidth use
- Mouse move, click, drag, and wheel input
- Desktop-browser wheel, right-click, and middle-click support
- Keyboard shortcuts and plain-text input
- Touch zoom and panning on mobile browsers
- Pair-approved control that automatically restores pointer and keyboard unless the host is elevated, with host-side toggles for view-only when desired
- Remembered browsers can automatically refresh their short-lived session after host restarts or daily session expiry
- Loopback plus optional Tailscale-tailnet host listeners
- Relative browser API paths so the client can sit behind a stripped reverse-proxy prefix

## Current Limitations

ROV is still early-stage software.

Notable current limitations:

- Linux builds currently depend on desktop capture libraries provided by the host OS
- The host session must already be awake and unlocked
- Remote input is intentionally locked out while ROV runs elevated
- Remembered access still depends on the browser keeping its cookies and the host not revoking that device
- No audio streaming
- No clipboard sync
- No file transfer
- No WebRTC transport yet
- `Ctrl+Alt+Del` is intentionally out of scope for a normal user-space app

## Quick Start

### Requirements

- Linux or Windows host
- Rust toolchain
- A modern desktop or mobile browser
- Tailscale only if you want private tailnet URLs

### Linux build dependencies

On Ubuntu or Linux Mint, install:

```bash
sudo apt install pkg-config libpipewire-0.3-dev libgbm-dev libclang-dev clang
```

Depending on the desktop session and distro, additional capture-related packages may still be required.

### Run locally

```bash
cargo run --release
```

For unattended deployments after you have already approved at least one trusted browser:

```bash
cargo run --release -- --headless
```

To issue one one-time pairing code at startup without opening the TUI:

```bash
cargo run --release -- --headless --print-pair-code
```

On Windows, the repo's Cargo config runs a copied temp executable so a previously opened ROV window does not keep `target\release\rustopviewer.exe` locked during the next rebuild.

### Use it from a browser

1. Launch ROV on the host machine.
2. Open the best available URL shown in the host TUI.
3. If you want a private tailnet URL, start Tailscale and use **Enable Tailscale URL**.
4. If you want a reverse-proxy or tunnel deployment, publish the loopback listener instead of opening a normal LAN listener.
5. Generate a one-time pairing code in the host TUI and enter it in the browser page.
6. Leave **Remember this browser on this device** enabled if you want that browser to reconnect without another code later.
7. Use the remote page to control the desktop. If you want view-only later, turn either input scope off in the host TUI.

### Optional: headless runtime after first approval

If you want ROV to stay available without a terminal window after you have already approved a browser on that device:

1. Start ROV normally once and pair a browser with **Remember this browser on this device** enabled.
2. Stop the TUI.
3. Launch `rustopviewer --headless`.
4. Revisit from the remembered browser and let it restore a fresh short-lived session automatically.

Headless mode is meant for already approved browsers. A brand-new browser still needs a host-generated one-time pairing code first.
If you are deliberately launching headless for that first approval, start it with `--print-pair-code` so the host logs one code at startup.

### Optional: Tailscale private URL

ROV keeps its remote-control server on local loopback and can additionally publish it to private Tailscale clients in two ways:

- Through Tailscale Serve HTTP inside the tailnet
- Directly on the device's Tailscale tailnet address
- Through optional Tailscale Serve HTTPS if you want a browser-trusted URL later

This means the Rust app itself does not need to open a normal LAN-facing remote-control socket and still avoids direct public internet exposure by default.

You can do this from inside the host TUI with the **Enable Tailscale URL** action.

1. In the host TUI, select **Enable Tailscale URL** and press `Enter`.

If you prefer to do it manually on the host, run:

```bash
tailscale serve --bg --yes --http 45080 127.0.0.1:45080
```

2. Tailscale will print an `http://...ts.net:45080` URL for this machine.
3. Open that URL from any browser that is on the same tailnet.

This is the preferred private-path option because it keeps traffic inside the encrypted Tailscale boundary and proxies cleanly back to loopback.

### Optional: browser-trusted HTTPS later

If you eventually want a browser-trusted HTTPS URL, Tailscale HTTPS certificates still need to be enabled for the tailnet first.

### Reverse proxy or tunnel deployment

ROV is designed to be published from loopback through infrastructure you already trust.

- Keep the host TUI runtime listening on `127.0.0.1`.
- Proxy a hostname or subpath back to that loopback listener.
- Preserve cookies and same-origin requests.
- If you mount ROV under a stripped path prefix, the built-in browser client now uses relative API paths so it can still reach `api/*` correctly.

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

```bash
cargo fmt --all -- --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all-targets --all-features
```

## Security Model

- The host runtime listens on loopback and, when available, the current Tailscale tailnet IPs, not on the normal LAN by default.
- Opening the browser page is not enough to gain control. A new browser session must be paired with a one-time code generated in the host TUI.
- A successfully paired browser can optionally become a remembered browser on that device and later refresh its normal session automatically.
- Headless runtime is intended for already approved browsers; a new browser still needs a host-approved one-time code first.
- Approved sessions are cookie-based, single-device, host-revocable, and stay paired for up to 24 hours without an idle timeout.
- Remembered-browser trust survives host restarts until the host revokes it, but it still depends on the browser retaining its cookies.
- A successful pairing restores remote pointer and keyboard control automatically unless ROV is running elevated.
- Running ROV elevated forces remote input back to view-only.
- The host TUI can revoke all remembered browsers immediately.
- Reverse proxies and tunnels should target loopback rather than widening the app's own bind surface.

## Roadmap Highlights

- Better precision and calibration for scaled displays
- Better gesture calibration across touch and desktop browsers
- Lower-latency streaming transport
- Clipboard sync
- File transfer
- Tray mode and startup behavior across Linux and Windows

## License

Licensed under either of:

- MIT license ([LICENSE-MIT](LICENSE-MIT))
- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE))

at your option.
