# Changelog

## Unreleased

### Added

- Windows hosts (Windows 10 and 11): the primary monitor through GDI,
  mouse and keyboard through SendInput (a character the host's layout has
  goes as its key, so shortcuts work; any other as the character itself),
  clipboard text both ways as CF_UNICODETEXT, real pixels on scaled
  displays. Over SSH, `serve` hands the screen to a Task Scheduler task of
  the user's that runs only in their signed-in session and only while the
  session lasts; the first session sets it up (`install`, `uninstall`).
  Listens on `127.0.0.1:47800` by default; `install.ps1` installs it in one
  line.
- `serve --open` opens this computer's browser on the viewer, to try it on
  the machine itself.
- macOS hosts, through macOS's own Screen Sharing: `rustopviewer serve`
  on a Mac talks VNC to it on `127.0.0.1:5900` with a password only RustOp
  Viewer knows, and the clipboard goes through `pbpaste` and `pbcopy`.
  `rustopviewer install` sets that up once (it makes the password, opens
  System Settings at Sharing and waits until Screen Sharing takes it).
  RustOp Viewer itself asks macOS for no permission and keeps nothing
  running.
- Control and ⌘ change places between a Mac and a computer that is not
  one, so each keyboard's own shortcuts work on the other side.
- Clipboard both ways, text only, up to 1 MB, on X11 hosts: Ctrl+V or
  **Paste here** in the viewer pastes this computer's clipboard on the
  remote; copying on the remote, or **Copy from remote**, puts the text on
  this computer. When the browser refuses clipboard access, the page says
  so and offers a box to paste into or copy from.

### Fixed

- The viewer's styles were blocked by its own Content-Security-Policy (an
  inline `<style>`), so the screen did not fit the window; they are served
  as `viewer.css` now.
- Input goes out one batch at a time, so a key cannot overtake an earlier
  one on a slow link.
- When a session ends, the connections still open (the screen's stream, a
  kept-alive request) are closed too.

## 0.3.0

Rewritten in Go with no third-party modules: X11 hosts reached over SSH (a
private socket, a key per session), a browser viewer, and `lmice desktop`.
The Rust version (registry, pairing, shared token) is at the tag `rust-era`.


All notable changes to this project will be documented in this file.

The format is inspired by Keep a Changelog and this project aims to follow Semantic Versioning over time.

## [Unreleased]

### Added

- Cross-platform host TUI for RustOp Viewer.
- Browser-based remote web UI served by the host runtime.
- Screen capture pipeline for the selected monitor.
- Mouse, keyboard, scroll, and text injection support.
- Tailscale-friendly connection workflow.
- Initial open-source repository scaffolding and governance docs.
- Linux host support at the code level alongside the existing Windows host support.
- Desktop-browser wheel, right-click, and middle-click controls in the built-in client.
- Persistent trusted-browser records that can reissue normal sessions after a one-time pairing.
- A supported `--headless` runtime for unattended deployments that already trust at least one browser.
- A `--print-pair-code` startup option for issuing one host-approved pairing code while running headless.
- A `--generate-pair-code` command for asking the running local host to mint a fresh code without a restart.
- Support for `ROV_EXTRA_LISTEN_ADDRS` so trusted local reverse proxies can reach the host runtime without widening to a normal LAN bind.
- Form-post browser approval plus explicit approval failure states for cleaner first-pair recovery.
- Browser-bound session and remembered-browser token headers for reverse-proxy deployments where background cookie handling is unreliable.
- Desktop-browser physical keyboard capture, broader special-key coverage, and mobile-only tool chrome.
- Visible desktop keyboard capture/release controls in the browser client.
- Mobile live typing mode alongside the existing compose-and-send text mode.

### Changed

- Replaced the native desktop host window with a terminal-first host control surface.
- Reworked input injection to use the cross-platform `enigo` backend instead of a Windows-only `SendInput` path.
- Generalized host and browser copy from a phone-only Windows workflow to Linux/Windows hosts with desktop or mobile browsers.
- Updated the browser client to use relative API paths so it can sit behind a stripped reverse-proxy prefix.
- Added host-side trust revocation and browser-side remembered-device session restoration.
- Stopped the browser client from polling frame and status endpoints while it is still waiting for approval, which makes first-pair flows quieter and more reliable.
- Switched the browser viewer to fit-to-window scaling first, with manual zoom layered on top.
- Added `Shift+wheel` horizontal-scroll fallback for desktop browsers.
- Added a 30-minute remote-session idle timeout and host-side remembered-browser expiration enforcement.
- Rejected browser state-changing requests when an `Origin` header does not match the request origin.
